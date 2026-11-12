# The same driver choice the installed system uses (hardware.nix,
# nvidia.nix), on the live installer medium. Without it, Turing-and-newer
# NVIDIA cards (such as a GTX 16xx) stay on nouveau while derisk runs the
# same GLES compositor as on an installed session, which is painfully slow.
#
# The full redistributable-firmware package set stays off for ISO size;
# linux-firmware for Wi-Fi and the driver's GSP firmware for NVIDIA open
# are enough here.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  nvidiaProduction = config.boot.kernelPackages.nvidiaPackages.production;
in
{
  imports = [
    ../modules/hardware.nix
    # Same driver stack as nvidia.nix, without kernel-hardening (the live
    # UKI uses nixpkgs' stock kernel) or nixpkgs.config (pkgs is injected).
    ./nvidia-live.nix
  ];

  hardware.enableRedistributableFirmware = lib.mkForce false;
  hardware.firmware = lib.mkForce [
    pkgs.linux-firmware
    nvidiaProduction.firmware
  ];

  # hardware.nix turns zswap on; the live medium has no swap partition.
  boot.zswap.enable = lib.mkForce false;
}
