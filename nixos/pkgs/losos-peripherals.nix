# Moves USB devices into the machines that drive them, on the host and in
# those machines (nixos/modules/peripherals.nix, docs/peripherals.md).
#
# Built from src/losos-peripherals. Cargo.lock pins every crate with its
# SHA-256, and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-peripherals";
  version = "0.1.0";

  src = ../../src/losos-peripherals;

  cargoLock.lockFile = ../../src/losos-peripherals/Cargo.lock;

  # Routes, descriptors, input capabilities and the virtual controller's
  # ports, from what sysfs would say; no devices or root needed.
  doCheck = true;

  meta = {
    description = "Move USB devices into the machines that drive them";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-peripherals";
  };
}
