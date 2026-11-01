# Turns focus mode on and off: freezes the background work the desktop
# does while a game or an editor runs (nixos/modules/focus.nix,
# docs/focus-mode.md).
#
# Built from src/losos-focus. Cargo.lock pins every crate with its
# SHA-256, and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "losos-focus";
  version = "0.1.0";

  src = ../../src/losos-focus;

  cargoLock.lockFile = ../../src/losos-focus/Cargo.lock;

  # GameMode runs its scripts with a PATH of pkexec alone (NixOS's
  # gamemode module), where systemctl and id are not found. The system's
  # are appended rather than this package's own systemd: systemctl must
  # match the systemd it talks to.
  nativeBuildInputs = [ makeWrapper ];
  postInstall = ''
    wrapProgram $out/bin/losos-focus --suffix PATH : /run/current-system/sw/bin
  '';

  # Which unit holds whose focus and which units are running, from text;
  # no systemd needed.
  doCheck = true;

  meta = {
    description = "Pause background work while a game or an editor runs";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-focus";
  };
}
