# Danube, the web browser (docs/danube.md): WPE WebKit drawn inside an
# mcsapi window, and danube-sandbox, the Hakoniwa launcher that sandboxes
# the browser and each of WebKit's web processes. The overlay does not
# call this file. The image's browser is the Danube machine, against
# Arch's wpewebkit, so a Nix build here would compile WebKit.
#
# Built from src/danube against the mcsapi input, whose components and theme
# crates Cargo.toml reaches by path, so the window draws with the same
# components and theme files as derisk at the pinned commit.
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
  # mcsapi's source, at the commit flake.lock pins (sources.nix).
  mcsapiSrc,
}:

rustPlatform.buildRustPackage {
  pname = "danube";
  version = "0.1.0";

  # src/danube; the mcsapi crates Cargo.toml reaches by path join it in
  # postPatch below, at the paths it names them by.
  src = lib.fileset.toSource {
    root = ../..;
    fileset = ../../src/danube;
  };
  cargoRoot = "src/danube";
  buildAndTestSubdir = "src/danube";
  cargoLock.lockFile = ../../src/danube/Cargo.lock;

  # The mcsapi crates are path dependencies of danube
  # (src/danube/crates/danube/Cargo.toml), named by their path out of this
  # repository. In a checkout the path is the developer's own mcsapi clone;
  # in the build it is the store path the mcsapi input fetched to, which is
  # substituted before cargo reads the manifest. Path dependencies are not
  # entries of Cargo.lock, so the vendor hash does not follow them.
  postPatch = ''
    substituteInPlace src/danube/crates/danube/Cargo.toml \
      --replace-fail "../../../../components/mcsapi" "${mcsapiSrc}"
  '';

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
