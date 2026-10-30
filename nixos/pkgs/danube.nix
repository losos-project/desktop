# Danube, the web browser (docs/danube.md): WPE WebKit (wpewebkit.nix)
# drawn inside an mcsapi window, and danube-sandbox, the Hakoniwa launcher
# that sandboxes the browser and each of WebKit's web processes.
#
# Built from src/danube against the mcsapi components/mcsapi records, by
# path, so the window draws with the same components and theme files as
# derisk at the pinned commit.
{
  lib,
  rustPlatform,
  pkg-config,
  wpewebkit,
  libseccomp,
  udev,
  libxkbcommon,
  wayland,
  libGL,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
  xdg-dbus-proxy,
  aria2,
  glib-networking,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "danube";
  version = "0.1.0";

  # src/danube and the mcsapi submodule it takes its components from, at
  # the paths Cargo.toml names them by.
  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../src/danube
      ../../components/mcsapi/Cargo.toml
      ../../components/mcsapi/crates/mcsapi
      ../../components/mcsapi/crates/mcsapi-ui
      ../../components/mcsapi/crates/mcsapi-theme
      ../../components/mcsapi/crates/mcsapi-components
    ];
  };
  cargoRoot = "src/danube";
  buildAndTestSubdir = "src/danube";
  cargoLock.lockFile = ../../src/danube/Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
    # build.rs writes the C API's declarations from WPE's headers with
    # bindgen, which needs libclang.
    rustPlatform.bindgenHook
  ];
  buildInputs = [
    wpewebkit
    # Hakoniwa's seccomp filters.
    libseccomp
    # The authenticator crate finds security keys through libudev
    # (src/danube's webauthn.rs).
    udev
    libxkbcommon
    wayland
  ];

  # Unit tests only: nothing here starts WebKit or opens a window.
  doCheck = true;

  # winit and glutin dlopen the windowing and GL libraries rather than
  # linking them, as derisk.nix explains; X11's for a session without
  # Wayland. xdg-dbus-proxy filters the session bus the sandbox sees, and
  # aria2c fetches downloads (src/danube's downloads.rs); the sandbox
  # keeps PATH, so both are found from inside it too.
  # GIO finds its TLS backend only through GIO_EXTRA_MODULES, which nothing
  # in the image sets, and WebKit's network process inherits it from here:
  # without it every https page says "TLS support is not available".
  postFixup = ''
    patchelf --add-rpath ${
      lib.makeLibraryPath [
        wayland
        libxkbcommon
        libGL
        libx11
        libxcursor
        libxi
        libxrandr
      ]
    } $out/bin/danube
    wrapProgram $out/bin/danube \
      --suffix PATH : ${
        lib.makeBinPath [
          xdg-dbus-proxy
          aria2
        ]
      } \
      --prefix GIO_EXTRA_MODULES : ${glib-networking}/lib/gio/modules
  '';

  postInstall = ''
    install -Dm644 ${./danube/danube.svg} $out/share/icons/hicolor/scalable/apps/org.losos.Danube.svg
    install -Dm644 ${./danube/org.losos.Danube.desktop} $out/share/applications/org.losos.Danube.desktop
    substituteInPlace $out/share/applications/org.losos.Danube.desktop \
      --replace-fail "Exec=danube" "Exec=$out/bin/danube"
  '';

  meta = {
    description = "The LosOS web browser, on WPE WebKit";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "danube";
  };
}
