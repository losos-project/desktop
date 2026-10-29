# Reports a problem on the desktop as a GitHub issue: derisk runs
# `losos-report serve` when a window stops responding, a service fails or an
# action never finishes, shows what it wrote, and it signs in with GitHub's
# device flow and files it (docs/reporting.md). The installer files a failed
# install with the same crate.
#
# Built from src/losos-report, pinned by its Cargo.lock as losos-installer is.
# Where reports go is not in here: the image writes /etc/losos/report.json
# (modules/report.nix).
{
  lib,
  rustPlatform,
  curl,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "losos-report";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../../src/losos-report;
    # A local `cargo build` leaves target/ beside the sources; it is
    # gitignored, but a path source would still copy all of it into the store.
    fileset = lib.fileset.difference ../../src/losos-report (
      lib.fileset.maybeMissing ../../src/losos-report/target
    );
  };

  cargoLock.lockFile = ../../src/losos-report/Cargo.lock;

  # curl on its own PATH, as nothing else in the session needs it there.
  # journalctl and systemctl stay the system's, which must match its systemd.
  nativeBuildInputs = [ makeWrapper ];
  postInstall = ''
    wrapProgram $out/bin/losos-report --prefix PATH : ${lib.makeBinPath [ curl ]}
  '';

  doCheck = true;
  # A report is sent with curl, and its tests send one to a stand-in for
  # GitHub on 127.0.0.1.
  nativeCheckInputs = [ curl ];

  meta = {
    description = "Files a problem on LosOS Desktop as a GitHub issue, for derisk";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-report";
  };
}
