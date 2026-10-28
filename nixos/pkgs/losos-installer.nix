# The installer's backend: `derisk installer` draws the pages and joins the
# network, and runs `losos-installer serve`, which lists the disks and runs
# systemd-repart and systemd-sysupdate on the one picked.
#
# Built from src/losos-installer, pinned by its Cargo.lock as losos-security is.
# The repart definitions and sysupdate transfers it runs with are not in here:
# they are generated from the OS's own configuration (installer.nix) and
# passed on its command line, so this package knows nothing about the layout.
{
  lib,
  rustPlatform,
  curl,
}:

rustPlatform.buildRustPackage {
  pname = "losos-installer";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    src = ../../src/losos-installer;
    # A local `cargo build` leaves target/ beside the sources; it is gitignored,
    # but a path source would still copy all of it into the store.
    filter = path: _type: baseNameOf path != "target";
  };

  cargoLock.lockFile = ../../src/losos-installer/Cargo.lock;

  # Fixture trees and the JSON protocol: no disk and no root. The disk filter is what stands between someone and the wrong drive,
  # so it is tested on every build.
  doCheck = true;
  # A report is sent with curl, and its tests send one to a stand-in for
  # GitHub on 127.0.0.1.
  nativeCheckInputs = [ curl ];

  meta = {
    description = "Installer backend for LosOS Desktop, driven by derisk installer";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-installer";
  };
}
