# Runs a command in one of the OS's service machines as if it ran on the
# host: the guest's server, the host's relay and the client a host command
# such as `nmcli` becomes (docs/network-machine.md, "Settings and nmcli").
#
# Built from src/losos-vm-call. Cargo.lock pins every crate with its
# SHA-256, and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-vm-call";
  version = "0.1.0";

  src = ../../src/losos-vm-call;

  cargoLock.lockFile = ../../src/losos-vm-call/Cargo.lock;

  # The wire format and who may call; the vsock half needs a machine.
  doCheck = true;

  meta = {
    description = "Run a command in one of the OS's service machines as if it ran on the host";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-vm-call";
  };
}
