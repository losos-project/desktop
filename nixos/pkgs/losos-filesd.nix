# The files machine's one program (docs/files-machine.md): its PID 1
# and the daemon that mounts the storage it is given and serves it to
# the host.
#
# Built from src/losos-filesd. Cargo.lock pins every crate with its SHA-256,
# and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-filesd";
  version = "0.1.0";

  src = ../../src/losos-filesd;

  cargoLock.lockFile = ../../src/losos-filesd/Cargo.lock;

  # blkid's answers, the volumes' names and the mount options.
  doCheck = true;

  meta = {
    description = "The files machine's init and storage daemon";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-filesd";
  };
}
