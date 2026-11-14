# WPE WebKit, the WebKit port with no toolkit of its own, which the Danube
# browser (src/danube) embeds. nixpkgs packages only WebKitGTK, so this is
# its WebKitGTK of the same version built as the WPE port instead: the same
# dependencies and patches, wpewebkit.org's tarball, and no GTK.
#
# Only the WPEPlatform API is built (docs/danube.md). Its headless platform
# hands each rendered frame to the embedder, which is how Danube draws the
# page inside a window it renders with mcsapi. The legacy libwpe API, the
# DRM platform (a kiosk with no compositor) and the Qt API are not needed,
# and each would add dependencies.
{
  lib,
  stdenv,
  webkitgtk,
  fetchurl,
  fontconfig,
  libdrm,
  libgbm,
  libjpeg,
  libpng,
  pcre2,
  woff2,
  zlib,
}:

webkitgtk.overrideAttrs (old: {
  pname = "wpewebkit";
  version = "2.54.2";
  name = "wpewebkit-2.54.2";

  # The WPE release of the WebKit version nixpkgs' WebKitGTK is at; move
  # them together so nixpkgs' patches keep applying.
  src = fetchurl {
    url = "https://wpewebkit.org/releases/wpewebkit-2.54.2.tar.xz";
    hash = "sha256-8yhOGeR/y1rIxjwuHBE9vWWyQj64pUX1s0VweCkAT1Y=";
  };

  # One patch of ours: the program that sandboxes each web process can be
  # named at run time, which is how Danube puts them in Hakoniwa
  # containers instead of bwrap's (docs/danube.md, "Sandbox and
  # hardening"). nixpkgs' WebKitGTK patches come first.
  patches = old.patches ++ [ ./patches/wpewebkit/0001-sandbox-launcher-from-the-environment.patch ];

  # No documentation output: gi-docgen is off below.
  outputs = [
    "out"
    "dev"
  ];

  # What WebKitGTK found through GTK and WPE has to be given: the image
  # decoders, fontconfig, WOFF2 fonts, and glib's pcre2, which its pkg-config file
  # names. libdrm and GBM are how the headless platform shares the web
  # process's GPU buffers.
  buildInputs = old.buildInputs ++ [
    fontconfig
    libdrm
    libgbm
    libjpeg
    libpng
    pcre2
    woff2
    zlib
  ];
  # WebKitGTK propagates GTK 4; WPE links none, and libsoup is what its
  # headers still need.
  propagatedBuildInputs = lib.filter (p: (p.pname or "") != "gtk4") old.propagatedBuildInputs;

  cmakeFlags =
    lib.filter (
      f:
      !(lib.hasPrefix "-DPORT=" f)
      && !(lib.hasPrefix "-DENABLE_INTROSPECTION=" f)
      && !(lib.hasPrefix "-DUSE_GTK4=" f)
      && !(lib.hasPrefix "-DENABLE_EXPERIMENTAL_FEATURES=" f)
    ) old.cmakeFlags
    ++ [
      "-DPORT=WPE"
      "-DENABLE_WPE_PLATFORM=ON"
      "-DENABLE_WPE_PLATFORM_HEADLESS=ON"
      "-DENABLE_WPE_PLATFORM_WAYLAND=ON"
      "-DENABLE_WPE_PLATFORM_DRM=OFF"
      "-DENABLE_WPE_LEGACY_API=OFF"
      "-DENABLE_WPE_QT_API=OFF"
      "-DENABLE_COG=OFF"
      # Danube is written in Rust against the C API, so neither GObject
      # introspection data nor the API reference is used.
      "-DENABLE_INTROSPECTION=OFF"
      "-DENABLE_DOCUMENTATION=OFF"
      # Hardening (docs/danube.md, "Sandbox and hardening"): no way in
      # from outside the browser. WebDriver and the remote inspector each
      # listen for a program that drives pages; WPE has no local inspector
      # for the remote one to serve anyway.
      "-DENABLE_WEBDRIVER=OFF"
      "-DENABLE_REMOTE_INSPECTOR=OFF"
      # Features WebKit has not shipped yet, among them WebXR, WebDriver
      # BiDi and WebKit's own extension API: each is more code reachable
      # from a page. Off by default too; stated for the same reason as
      # the sandbox below.
      "-DENABLE_EXPERIMENTAL_FEATURES=OFF"
      # Web processes always run in WebKit's sandbox: its seccomp filter
      # inside a container from the launcher Danube names (bwrap if none
      # is named). Already the default on Linux; stated so a nixpkgs change
      # cannot turn it off unnoticed.
      "-DENABLE_BUBBLEWRAP_SANDBOX=ON"
    ];

  # nixpkgs' hardening already gives format checks, PIE, the strong stack
  # protector, _FORTIFY_SOURCE=2 and RELRO; on top of that, the level-3
  # fortify checks, stack clash probes, zeroed automatic variables, eager
  # binding so the whole GOT goes read-only, and control-flow protection
  # where the CPU has it (CET on x86_64, BTI and PAC on aarch64).
  hardeningEnable = (old.hardeningEnable or [ ]) ++ [
    "fortify3"
    "stackclashprotection"
    "trivialautovarinit"
    "bindnow"
  ];
  env = (old.env or { }) // {
    NIX_CFLAGS_COMPILE = toString (
      lib.optional (old ? env.NIX_CFLAGS_COMPILE) old.env.NIX_CFLAGS_COMPILE
      ++ lib.optional stdenv.hostPlatform.isx86_64 "-fcf-protection=full"
      ++ lib.optional stdenv.hostPlatform.isAarch64 "-mbranch-protection=standard"
    );
  };

  postFixup = "";

  # WebKitGTK's mainProgram is WebKitWebDriver, which is not built here.
  meta = old.meta // {
    description = "Web content rendering engine, WPE port";
    homepage = "https://wpewebkit.org/";
    pkgConfigModules = [
      "wpe-webkit-2.0"
      "wpe-platform-2.0"
      "wpe-platform-headless-2.0"
    ];
    platforms = lib.platforms.linux;
  };
})
