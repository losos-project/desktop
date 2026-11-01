# The portal machine's pieces (docs/portal-machine.md): the backend that
# hands the portals' dialogs to it, on the host and in every machine; the
# host's relay, which starts it and passes files; and its server and the
# screenshot agent inside it.
#
# Built from src/losos-portal. Cargo.lock pins every crate with its SHA-256,
# and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
  dbus,
}:

rustPlatform.buildRustPackage {
  pname = "losos-portal";
  version = "0.1.0";

  src = ../../src/losos-portal;

  cargoLock.lockFile = ../../src/losos-portal/Cargo.lock;

  # The wire format, the grants and the file copies, and a request's body
  # through a real session bus.
  doCheck = true;
  nativeCheckInputs = [ dbus ];

  meta = {
    description = "The portal machine's backend, relay and server";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-portal";
  };
}
