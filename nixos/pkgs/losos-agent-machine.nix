# The channels between derisk's Agent, in a machine of each person's own,
# and their desktop (nixos/modules/agent.nix, docs/agent.md): opening it,
# derisk's desktop tools, and web links. Both ends, host and machine, are
# this one program.
#
# Built from src/losos-agent-machine. Cargo.lock pins every crate with its
# SHA-256, and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-agent-machine";
  version = "0.1.0";

  src = ../../src/losos-agent-machine;

  cargoLock.lockFile = ../../src/losos-agent-machine/Cargo.lock;

  # Which requests reach the desktop and which links open; no vsock or
  # machine needed.
  doCheck = true;

  meta = {
    description = "Run derisk's Agent in a machine of each person's own";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-agent-machine";
  };
}
