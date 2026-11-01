# Picks the graphics card the gaming machine gets, moves it to vfio-pci at
# boot, and defines that machine (nixos/modules/gaming.nix,
# docs/gaming-machine.md).
#
# Built from src/losos-gaming. Cargo.lock pins every crate with its
# SHA-256, and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-gaming";
  version = "0.1.0";

  src = ../../src/losos-gaming;

  cargoLock.lockFile = ../../src/losos-gaming/Cargo.lock;

  # Which cards can go and the machine's definition, from a made-up sysfs;
  # no devices, libvirt or root needed.
  doCheck = true;

  meta = {
    description = "Give a Windows gaming machine a graphics card of its own";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-gaming";
  };
}
