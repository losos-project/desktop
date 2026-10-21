# app-windows, which shows a guest's apps and windows on the desktop
# (docs/app-windows.md): each guest app in the app grid and the palette, and
# each guest window as a window of its own, from a guest that runs an app
# server such as macos-app-server.
#
# Built from src/app-windows, a cargo workspace of the protocol, the macOS
# server and this client; only the client is built here, since the server
# runs inside macOS. Cargo.lock pins every crate with its SHA-256, and
# importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
  pkg-config,
  ffmpeg-headless,
  libxkbcommon,
  copyDesktopItems,
  makeDesktopItem,
}:

rustPlatform.buildRustPackage {
  pname = "app-windows";
  version = "0.1.0";

  src = ../../src/app-windows;

  cargoLock.lockFile = ../../src/app-windows/Cargo.lock;

  cargoBuildFlags = [
    "-p"
    "app-windows"
  ];
  cargoTestFlags = [
    "-p"
    "app-windows-protocol"
    "-p"
    "app-windows"
  ];

  # bindgen reads FFmpeg's headers for the decoder's bindings.
  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
    copyDesktopItems
  ];
  # The headless FFmpeg: no SDL or X, nothing that would link the overlay's
  # GTK, and still VA-API, which is what decodes on the GPU.
  buildInputs = [
    ffmpeg-headless
    libxkbcommon
  ];

  # "Open" on a disk image or installer package in Files hands it to macOS,
  # which mounts or installs it (docs/app-windows.md, "Mac packages").
  # Not an app of its own, so not in the app grid; modules/app-windows.nix
  # makes it those types' default.
  desktopItems = [
    (makeDesktopItem {
      name = "app-windows-open-macos";
      desktopName = "Open in macOS";
      exec = "app-windows send %F";
      icon = "computer";
      mimeTypes = [
        "application/x-apple-diskimage"
        "application/x-xar"
      ];
      noDisplay = true;
    })
  ];

  # The protocol, the desktop entries, the key tables, recordings played
  # back, files handed over and a stand-in derisk; no guest or Wayland
  # needed. The macOS
  # workflow (.github/workflows/app-windows.yml) drives real Macs.
  doCheck = true;

  meta = {
    description = "Show a guest system's apps and windows on the desktop";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "app-windows";
  };
}
