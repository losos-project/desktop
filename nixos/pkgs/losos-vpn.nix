# sys-vpn's program, on the host and in the machine (docs/vpn.md): the
# providers it takes, the check of their WireGuard files, and the tunnel.
#
# Built from src/losos-vpn. Cargo.lock pins every crate with its SHA-256,
# and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-vpn";
  version = "0.1.0";

  src = ../../src/losos-vpn;

  cargoLock.lockFile = ../../src/losos-vpn/Cargo.lock;

  # The providers and the WireGuard file's rules.
  doCheck = true;

  meta = {
    description = "sys-vpn: a VPN provider's WireGuard tunnel for the whole PC";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-vpn";
  };
}
