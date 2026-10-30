# Relays the host's security keys and smart card readers into the machines
# (nixos/modules/tokens.nix, nixos/guest/tokens.nix, docs/tokens.md).
#
# Built from src/losos-tokens. Cargo.lock pins every crate with its SHA-256,
# and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-tokens";
  version = "0.1.0";

  src = ../../src/losos-tokens;

  cargoLock.lockFile = ../../src/losos-tokens/Cargo.lock;

  # The frames, CTAPHID's channels, uhid's layout and the readers' lease;
  # no devices needed. nixos/tests/tokens.nix relays a key and a reader.
  doCheck = true;

  meta = {
    description = "Lend the host's security keys and smart card readers to its machines";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-tokens";
  };
}
