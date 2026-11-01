# Flatpak on a PC: a machine of its own (docs/hypervisor.md, "The Flatpak
# machine"), and nothing of Flatpak on the host. Flatpak and Flathub run
# in the machine made from nixos/guest/flatpak.nix, which this module adds
# to the templates as losos-flatpak and has the host make as the machine
# `flatpak`, labelled purple. Its apps are on the desktop as that
# machine's, in the palette whether it runs or not; picking one starts it.
# The host's Store installs them there, and Settings' Privacy page changes
# their permissions there.
#
# Flatpak ran on the host before, so a host that updates into this may
# have apps and their data where the host kept them. They move into the
# machine once: the apps installed for everyone at the first boot that has
# the machine, and each person's own apps and data at their next sign-in,
# when their home is open.
#
# A phone has no machines; the GSI imports flatpak.nix itself and keeps
# Flatpak on the host.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  machine = "flatpak";

  moveHost = pkgs.writeShellApplication {
    name = "losos-flatpak-move-host";
    runtimeInputs = [
      config.virtualisation.libvirtd.package
      pkgs.coreutils
      pkgs.findutils
      pkgs.e2fsprogs
      pkgs.jq
    ];
    text = builtins.readFile ./flatpak-move-host.sh;
  };
in
{
  losos.machines.guests.losos-flatpak = ../guest/flatpak.nix;

  losos.machines.system.${machine} = {
    template = "losos-flatpak";
    label = "purple";
    # The OS's: out of the Machines app, its apps in the palette.
    service = true;
  };

  # The apps the host had installed for everyone. /var/lib/flatpak is
  # what flatpak.nix left there; once its contents are in the machine it
  # goes, since nothing on the host reads it any more. A machine that is
  # running is not moved into; the next boot tries again.
  systemd.services.losos-flatpak-move = {
    description = "Move the apps Flatpak installed on the host into the Flatpak machine";
    wantedBy = [ "multi-user.target" ];
    requires = [ "losos-system-machines.service" ];
    after = [ "losos-system-machines.service" ];
    unitConfig.ConditionDirectoryNotEmpty = "/var/lib/flatpak/app";
    environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
    serviceConfig = {
      Type = "oneshot";
      SuccessExitStatus = [ 75 ];
    };
    script = ''
      if ! ${config.virtualisation.libvirtd.package}/bin/virsh -q dominfo ${machine} >/dev/null 2>&1; then
        exit 0
      fi
      ${lib.getExe moveHost} ${machine} system /var/lib/flatpak
      rm -rf /var/lib/flatpak
    '';
  };

  # Each person's: the apps they installed for themselves
  # (~/.local/share/flatpak) and every app's data (~/.var/app), at sign-in,
  # when a homed home is open. Administrators only, whom polkit lets manage
  # machines without asking; anyone else would be asked for a password by a
  # dialog they did not open, and moves theirs by hand (docs/hypervisor.md).
  # The apps go from the host once moved; their data stays until its owner
  # deletes it, and a mark says it was moved.
  systemd.user.services.losos-flatpak-move = {
    description = "Move your Flatpak apps and their data into the Flatpak machine";
    unitConfig.ConditionUser = "!@system";
    wantedBy = [ "graphical-session.target" ];
    after = [ "graphical-session.target" ];
    path = [
      config.virtualisation.libvirtd.package
      pkgs.coreutils
      pkgs.gnugrep
    ];
    environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
    serviceConfig = {
      Type = "oneshot";
      SuccessExitStatus = [ 75 ];
    };
    script = ''
      moved="''${XDG_STATE_HOME:-$HOME/.local/state}/losos/flatpak-moved"
      if [ -e "$moved" ] || ! id -nG | tr ' ' '\n' | grep -qx wheel; then
        exit 0
      fi
      if ! virsh -q dominfo ${machine} >/dev/null 2>&1; then
        exit 0
      fi
      if [ -n "$(ls -A "$HOME/.local/share/flatpak/app" 2>/dev/null)" ]; then
        ${lib.getExe moveHost} ${machine} user-flatpak "$HOME/.local/share/flatpak"
        rm -rf "$HOME/.local/share/flatpak"
      fi
      if [ -n "$(ls -A "$HOME/.var/app" 2>/dev/null)" ]; then
        ${lib.getExe moveHost} ${machine} var-app "$HOME/.var/app"
      fi
      mkdir -p "$(dirname "$moved")"
      touch "$moved"
    '';
  };
}
