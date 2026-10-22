# macOS app support (docs/macos-apps.md): macOS apps as windows of their
# own on the desktop. Each person who opens it on a Mac gets a macOS of
# their own, in a rootless Podman container of theirs (dockur/macos, a QEMU
# with KVM and Reims vGPU), and app-windows shows its windows here
# (nixos/pkgs/macos-app-support.nix). Podman and its subordinate IDs are
# windows-apps.nix's, which every x86-64 PC has.
#
# dockur/macos runs the Intel macOS, so this is x86-64 only. An Apple
# silicon Mac runs arm64 macOS in QEMU's vmapple machine instead
# (docs/macos-arm64.md).
{
  config,
  lib,
  pkgs,
  ...
}:

lib.mkIf pkgs.stdenv.hostPlatform.isx86_64 {
  environment.systemPackages = [ pkgs.macos-app-support ];

  # The setup, as each person's own unit, which the app starts when macOS
  # is not up and which carries on while nothing is open: macOS downloads
  # from Apple and installs itself for most of an hour the first time.
  # Podman and newuidmap come from the PATH the user manager gives every
  # unit.
  systemd.user.services.macos-app-support-setup = {
    description = "Set up macOS for macOS app support";
    serviceConfig = {
      Type = "exec";
      ExecStart = lib.getExe pkgs.macos-app-support.setup;
    };
  };

  assertions = [
    {
      assertion = config.virtualisation.podman.enable;
      message = "macos-apps.nix runs macOS in rootless Podman, which windows-apps.nix sets up.";
    }
  ];
}
