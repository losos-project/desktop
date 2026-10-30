# Carries the schizo gateway's connections out through the person's LosOS
# edge, over Tor: Tor to the edge's onion service, then the edge's SOCKS5
# exit to the address each connection was meant for (docs/schizo-mode.md,
# "Through an edge"). Runs in the Tor gateway machine.
#
# Built from src/losos-edge-chain. Cargo.lock pins every crate with its
# SHA-256, and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-edge-chain";
  version = "0.1.0";

  src = ../../src/losos-edge-chain;

  cargoLock.lockFile = ../../src/losos-edge-chain/Cargo.lock;

  # The edge file and both handshakes against a stand-in for Tor and the
  # edge; the redirect itself needs the gateway's firewall.
  doCheck = true;

  meta = {
    description = "Carry a Tor gateway's connections out through a LosOS edge's onion exit";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-edge-chain";
  };
}
