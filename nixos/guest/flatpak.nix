# The Flatpak machine (docs/hypervisor.md, "The Flatpak machine"): where
# Flatpak and Flathub run on a PC, instead of on the host. Its apps are on
# the host's desktop as this machine's, framed in its colour; the host's
# Store installs them, and Settings' Privacy page changes what they may
# reach, both through losos-guest-flatpak, below.
#
# modules/flatpak-machine.nix adds it to losos.machines.guests as
# losos-flatpak, which is the template name derisk looks for, and has the
# host make a machine from it.
{ pkgs, ... }:

let
  user = "user";

  guestFlatpak = pkgs.writeShellApplication {
    name = "losos-guest-flatpak";
    runtimeInputs = with pkgs; [
      coreutils
      gnused
      systemd
      util-linux
    ];
    text = builtins.replaceStrings [ "@user@" "@flatpak@" ] [ user (pkgs.lib.getExe pkgs.flatpak) ] (
      builtins.readFile ./losos-guest-flatpak.sh
    );
  };

  move = pkgs.writeShellApplication {
    name = "losos-flatpak-move";
    runtimeInputs = with pkgs; [
      coreutils
      findutils
      systemd
      util-linux
    ];
    text = builtins.replaceStrings [ "@user@" ] [ user ] (builtins.readFile ./losos-flatpak-move.sh);
  };
in
{
  imports = [
    ./common.nix
    # Flathub as the remote, added at boot from the repo file the image
    # pins: what the host had before. The store is the host's.
    ../modules/flatpak.nix
  ];

  # flatpak's own rule lets an active, local wheel member install for the
  # whole system. The guest's user is never in an active session (nobody
  # logs in; its apps run under its systemd manager), so it is given
  # Flatpak's actions here instead. The machine is its owner's to fill.
  security.polkit.extraConfig = ''
    polkit.addRule(function (action, subject) {
      if (action.id.indexOf("org.freedesktop.Flatpak.") === 0 && subject.user === "${user}") {
        return polkit.Result.YES;
      }
      return polkit.Result.NOT_HANDLED;
    });
  '';

  environment.systemPackages = [
    guestFlatpak
    move
  ];

  # Lazy, as the OS's other machines are: with no app open and nothing
  # being installed for ten minutes, it powers off and gives the host its
  # memory back. Picking one of its apps in the palette, or installing one
  # from the Store, starts it again. Every Flatpak app runs under bwrap; an
  # install or update is a flatpak or losos-guest-flatpak process.
  systemd.services.losos-flatpak-idle = {
    description = "Power off when no Flatpak app has been open for a while";
    wantedBy = [ "multi-user.target" ];
    path = [
      pkgs.procps
      pkgs.systemd
    ];
    script = ''
      idle=0
      while :; do
        # Names as the kernel keeps them, cut at 15 characters.
        if pgrep '^(bwrap|flatpak|flatpak-system-|losos-guest-fla|losos-flatpak-m)$' >/dev/null; then
          idle=0
        else
          idle=$((idle + 1))
        fi
        if [ "$idle" -ge 10 ]; then
          echo "no app open for ten minutes: powering off"
          exec systemctl poweroff
        fi
        sleep 60
      done
    '';
  };
}
