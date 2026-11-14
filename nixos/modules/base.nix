# What every build of LosOS Desktop is, whatever it boots on: the desktop, its
# accounts, network and services, pm, and the rule that the system is an
# image nothing rebuilds in place.
#
# default.nix adds the PC half (UEFI, the verity /usr, sysupdate, the
# installer). nixos/halium/ adds the Halium half instead: an Android boot
# image and the device's vendor layer in an Android container.
{
  config,
  lib,
  pkgs,
  ...
}:
{
  imports = [
    ./options.nix
    ./accounts.nix
    ./home-folders.nix
    ./sign-in.nix
    ./setup.nix
    ./network.nix
    ./desktop.nix
    ./assistant.nix
    ./danube.nix
    ./adblock.nix
    # flatpak.nix is not here: a PC runs Flatpak in a machine of its own
    # (flatpak-machine.nix), and the GSI, which has no machines, imports it
    # itself.
    ./services.nix
    ./kernel-hardening.nix
    ./pm.nix
    ./ping.nix
    ./focus.nix
    ./report.nix
    ./backup.nix
    ./docs.nix
    ./losos-boxes.nix
    ./company.nix
    ./app-windows.nix
    ./userland.nix
  ];

  nixpkgs.overlays = [ (import ../pkgs) ];

  # linuxPackages_latest, not the channel's default: the kernel is this
  # project's own build (kernel-hardening.nix) and is rebuilt by CI anyway, so
  # it never substitutes from cache.nixos.org, and tracking the newest series
  # is what nixos-unstable is here for (flake.nix). The channel's default
  # kernel can sit a whole series behind linuxPackages_latest for weeks. The
  # Halium GSI boots its device's vendor kernel (halium/boot.nix), so the
  # choice costs it nothing but an evaluation.
  boot.kernelPackages = lib.mkDefault pkgs.linuxPackages_latest;

  system.nixos = {
    distroId = "losos-desktop";
    distroName = "LosOS Desktop";
  };

  # The system is an image. It cannot be rebuilt in place, and it carries no
  # Nix: an update is a new image, not a switch.
  nix.enable = false;
  system.switch.enable = false;

  # /etc is an overlay of the generated tree rather than files written by an
  # activation script, which is what lets it be assembled without perl and
  # makes it read the same on every boot. Mutable, because NetworkManager,
  # homed and hostnamed all keep state under /etc and the root partition is
  # where state lives here.
  system.etc.overlay = {
    enable = true;
    mutable = true;
  };

  # System users come from systemd-sysusers, reading sysusers.d generated from
  # users.users -- the upstream tool, not NixOS's perl script and not userborn.
  # Human users are not here at all: see accounts.nix.
  systemd.sysusers.enable = true;
  users.mutableUsers = false;

  # The image does not include Nix or nixos-rebuild, so the manual is not useful
  # on the installed system.
  documentation.nixos.enable = false;

  # The NixOS release the option defaults above were written against.
  system.stateVersion = "26.05";
}
