# derisk, the desktop: an adaptive, agent-first Wayland shell on mcsapi's
# Smithay compositor, with its core apps (Files, Settings, Text Editor, System
# Monitor, Calculator) built into the same binary. `derisk-gpui` goes beside
# it: the same core apps as GPUI windows, and the portal's consent dialog
# (`derisk-gpui ask`), which xdg-desktop-portal-derisk runs instead of GTK's
# access dialog when it finds the binary next to `derisk` (docs/index.md).
# `derisk.terminal` is the desktop's terminal, a package of its own,
# `derisk.index` the user service indexing home for the palette, and
# `derisk.agent` the Agent, the window that runs Claude Code on the desktop
# (docs/agent.md).
#
# Pinned by the components/derisk submodule and moved by hand, like pm, because both derisk and the
# mcsapi it builds against are moving fast. mcsapi comes in through
# Cargo.lock's git entries, which the cargo vendor step fetches and cargoHash
# covers.
{
  lib,
  rustPlatform,
  pkg-config,
  lld,
  libxkbcommon,
  wayland,
  libGL,
  libinput,
  udev,
  libgbm,
  seatd,
  libdrm,
  linux-pam,
  libpwquality,
  libvirt,
  # derisk-gpui's GPUI (Zed's) links these, and finds the rest below at
  # run time.
  fontconfig,
  libx11,
  libxcb,
  libxcursor,
  libxi,
  libxrandr,
  vulkan-loader,
  graphene-hardened-malloc,
  # mcsapi-hardened-malloc as the global allocator (nixos/modules/allocator.nix
  # turns it on where the system preloads hardened_malloc).
  withHardenedMalloc ? false,
}:

let
  version = "0.1.0-unstable-2026-10-28";

  # The components/derisk submodule, at the commit this tree records for it
  # (flake.nix, `self.submodules`). cleanSource drops the submodule's .git
  # file, so the source hash covers only the tree.
  src = lib.cleanSourceWith {
    name = "derisk-source";
    src = lib.cleanSource ../../components/derisk;
  };

  cargoHash = "sha256-XobWBflaOFiArgHf/BJm7kzbhPIGe1VIaqvQ5QHUTIY=";

  # Fat LTO (Cargo.toml: lto = true) compiles every dependency as bitcode
  # (-C linker-plugin-lto) and links it in one rustc. That link is killed on
  # a 4GB machine, and stripping -C lto only from the final rustc then leaves
  # mold with undefined symbols, because the rlibs contain no object code.
  # These override the release profile, and the wasm-plugin profile that
  # inherits it, so the crates ship ordinary object code.
  cargoEnv = {
    CARGO_PROFILE_RELEASE_LTO = "off";
    CARGO_PROFILE_WASM_PLUGIN_LTO = "off";
  };

  # derisk-gpui, as a cargo build of its own. It cannot share derisk's: the
  # portal talks to xdg-desktop-portal through ashpd on tokio, and Sonne's
  # GPUI through the same ashpd on async-io, and cargo unifies features
  # across one build, where ashpd refuses both at once. Two builds in one
  # derivation would still share a target directory and fight over it, so
  # this is a second package, vendoring the same Cargo.lock under the same
  # name so the vendor derivation is one store path.
  gpui = rustPlatform.buildRustPackage {
    pname = "derisk-gpui";
    inherit version src cargoHash;
    env = cargoEnv;
    cargoDepsName = "derisk-${version}";

    # `gpui` is what makes it a binary at all: the crate is otherwise only
    # its list of ported apps, which derisk reads.
    cargoBuildFlags = [
      "-p"
      "derisk-gpui"
    ];
    buildFeatures = [ "derisk-gpui/gpui" ];

    # derisk-plugin is among its dependencies, so its build.rs compiles the
    # bundled WebAssembly plugins here too, with lld as below.
    nativeBuildInputs = [
      pkg-config
      lld
    ];
    # Its X11 backend (x11rb, and zed-scap's xcb for screen capture) links
    # libxcb and libX11; libxkbcommon-x11 is in libxkbcommon. Its font list
    # comes from fontconfig, which yeslogic-fontconfig-sys asks pkg-config
    # for at build time and then loads by name. FreeType is compiled in
    # (freetype-sys builds its own).
    buildInputs = [
      libxkbcommon
      libxcb
      libx11
      fontconfig
    ];

    # GPUI dlopens libwayland-client, fontconfig (for the system's font
    # list), Vulkan's loader (the ICDs come from hardware.graphics) and,
    # under XWayland or an X11 session, the X11 libraries x11rb and its
    # cursor code look up by name.
    postFixup = ''
      patchelf --add-rpath ${
        lib.makeLibraryPath [
          wayland
          libxkbcommon
          libGL
          fontconfig
          vulkan-loader
          libxcb
          libx11
          libxcursor
          libxi
          libxrandr
        ]
      } $out/bin/derisk-gpui
    '';

    meta = {
      description = "derisk's core apps as GPUI windows, and the portal's consent dialog";
      homepage = "https://github.com/dasmatus/derisk";
      license = lib.licenses.gpl3Only;
      platforms = lib.platforms.linux;
      mainProgram = "derisk-gpui";
    };
  };

  # derisk-terminal, the desktop's terminal, as a build of its own like
  # derisk-gpui: the machine template ships it without the rest of derisk
  # (nixos/guest), and it links none of derisk's compositor.
  terminal = rustPlatform.buildRustPackage {
    pname = "derisk-terminal";
    inherit version src cargoHash;
    env = cargoEnv;
    cargoDepsName = "derisk-${version}";

    cargoBuildFlags = [
      "-p"
      "derisk-terminal"
    ];
    # Its tests open ptys and read the system's fonts, which the build
    # sandbox has neither of; derisk's own CI runs them.
    doCheck = false;

    postInstall = ''
      install -Dm644 -t $out/share/applications crates/derisk-terminal/data/org.derisk.terminal.desktop
      substituteInPlace $out/share/applications/org.derisk.terminal.desktop \
        --replace-fail "Exec=derisk-terminal" "Exec=$out/bin/derisk-terminal"
    '';

    # winit, glutin and softbuffer dlopen everything they draw with:
    # libwayland-client and libxkbcommon, libEGL (whose vendor library,
    # Mesa's, comes from hardware.graphics), and under X11 the libraries
    # winit and softbuffer look up by name.
    postFixup = ''
      patchelf --add-rpath ${
        lib.makeLibraryPath [
          wayland
          libxkbcommon
          libGL
          libxcb
          libx11
          libxcursor
          libxi
          libxrandr
        ]
      } $out/bin/derisk-terminal
    '';

    meta = {
      description = "derisk's terminal, with Nerd Font glyphs, drawn on the GPU or in software";
      homepage = "https://github.com/dasmatus/derisk";
      license = lib.licenses.gpl3Only;
      platforms = lib.platforms.linux;
      mainProgram = "derisk-terminal";
    };
  };
  # derisk-index, the user service that indexes the names and text of the
  # files under home for the palette (docs/index.md), as a build of its own
  # like the terminal: derisk links only its client, and a build of both
  # would unify their features and link the search engine into derisk too.
  index = rustPlatform.buildRustPackage {
    pname = "derisk-index";
    inherit version src cargoHash;
    env = cargoEnv;
    cargoDepsName = "derisk-${version}";

    cargoBuildFlags = [
      "-p"
      "derisk-index"
    ];
    cargoTestFlags = [
      "-p"
      "derisk-index"
    ];

    # derisk-index.socket and derisk-index.service, calling the binary by
    # its store path.
    postInstall = ''
      install -Dm644 -t $out/lib/systemd/user crates/derisk-index/data/systemd/user/*
      substituteInPlace $out/lib/systemd/user/derisk-index.service \
        --replace-fail "ExecStart=derisk-index" "ExecStart=$out/bin/derisk-index"
    '';

    meta = {
      description = "derisk's file index: the names and text of the files under home, for the command palette";
      homepage = "https://github.com/dasmatus/derisk";
      license = lib.licenses.gpl3Only;
      platforms = lib.platforms.linux;
      mainProgram = "derisk-index";
    };
  };
  # derisk-agent, the Agent: Sonne's agent window, moved into derisk when
  # Sonne left the image (docs/agent.md). A build of its own like the
  # terminal: it links wasmtime with WASI for its previews, which derisk's
  # own wasmtime deliberately goes without, and cargo would unify the two.
  agent = rustPlatform.buildRustPackage {
    pname = "derisk-agent";
    inherit version src cargoHash;
    env = cargoEnv;
    cargoDepsName = "derisk-${version}";

    cargoBuildFlags = [
      "-p"
      "derisk-agent"
    ];
    cargoTestFlags = [
      "-p"
      "derisk-agent"
    ];

    # mcsapi-ui brings Smithay's xkbcommon bindings, which link libxkbcommon
    # through pkg-config, as x2mcsapi's do.
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ libxkbcommon ];

    # Its desktop entry claims the derisk-agent: links the palette opens.
    postInstall = ''
      install -Dm644 -t $out/share/applications crates/derisk-agent/data/org.derisk.agent.desktop
      substituteInPlace $out/share/applications/org.derisk.agent.desktop \
        --replace-fail "Exec=derisk-agent" "Exec=$out/bin/derisk-agent"
    '';

    # eframe draws through winit and glutin, which dlopen what they draw
    # with, as the terminal's do.
    postFixup = ''
      patchelf --add-rpath ${
        lib.makeLibraryPath [
          wayland
          libxkbcommon
          libGL
          libxcb
          libx11
          libxcursor
          libxi
          libxrandr
        ]
      } $out/bin/derisk-agent
    '';

    meta = {
      description = "derisk's Agent: build apps, change the desktop and run routines with Claude Code";
      homepage = "https://github.com/dasmatus/derisk";
      license = lib.licenses.gpl3Only;
      platforms = lib.platforms.linux;
      mainProgram = "derisk-agent";
    };
  };
in

rustPlatform.buildRustPackage {
  pname = "derisk";
  inherit version src cargoHash;
  env = cargoEnv;

  # derisk and its portal backend, xdg-desktop-portal-derisk. `host` is the
  # compositor; without it the binary has only the headless commands. It is
  # named through its package because cargo refuses a bare feature name when
  # more than one package is built. derisk-faces is the privacy display's
  # face counter, which the session runs from beside its own executable
  # while one of the display's conditions holds (docs/sign-in.md).
  cargoBuildFlags = [
    "-p"
    "derisk"
    "-p"
    "derisk-portal"
    "-p"
    "derisk-faces"
  ];
  buildFeatures = [ "derisk/host" ] ++ lib.optional withHardenedMalloc "derisk/hardened-malloc";

  # build.rs compiles the palette's and the overview's plugins (plugins/ in
  # the same workspace and Cargo.lock, so the vendored crates cover them) for
  # wasm32-unknown-unknown. nixpkgs' rustc carries that target's std but links
  # it with `lld` from PATH rather than a bundled rust-lld.
  nativeBuildInputs = [
    pkg-config
    lld
    # libfacedetection-sys compiles libfacedetection's vendored C++ and
    # generates its bindings with bindgen, which needs libclang and the C++
    # headers' include paths.
    rustPlatform.bindgenHook
  ];
  buildInputs = [
    libxkbcommon
    wayland
    libGL
    libinput
    udev
    libgbm
    seatd
    libdrm
    # The lock screen checks passwords through PAM.
    linux-pam
    # derisk-machines (the Machines app, the window gate and the palette's
    # machines) talks to libvirtd through libvirt's own C library, which the
    # virt crate's bindings link and pkg-config finds. derisk-gpui does not
    # build that crate, so it goes without.
    libvirt
  ]
  # Linked, and put on the RUNPATH, by the cc wrapper.
  ++ lib.optional withHardenedMalloc graphene-hardened-malloc;

  # winit and glutin dlopen libwayland-client, libxkbcommon and libEGL at run
  # time rather than linking them, so nothing puts them on the RUNPATH and the
  # session would die looking for them. First-boot setup dlopens libpwquality,
  # as homed does, to say a password homed would refuse is too weak while it
  # is typed (docs/first-boot.md).
  postFixup = ''
    patchelf --add-rpath ${
      lib.makeLibraryPath [
        wayland
        libxkbcommon
        libGL
        libpwquality
      ]
    } $out/bin/derisk
  '';

  # The units call plain `derisk`; give them the store path so they work
  # without the user's PATH.
  postInstall = ''
    # derisk and the portal look for derisk-gpui beside their own executable
    # (derisk's host::gpui_apps, derisk-portal's access::dialog_program).
    ln -s ${gpui}/bin/derisk-gpui $out/bin/derisk-gpui

    install -Dm644 -t $out/lib/systemd/user crates/derisk/data/systemd/user/* crates/derisk-portal/data/systemd/user/*
    substituteInPlace $out/lib/systemd/user/derisk-agent.service \
      --replace-fail "ExecStart=derisk " "ExecStart=$out/bin/derisk "

    # The portal backend. xdg.portal links share/xdg-desktop-portal from
    # extraPortals and configPackages, and D-Bus wants an absolute Exec=.
    install -Dm644 -t $out/share/xdg-desktop-portal/portals crates/derisk-portal/data/portal/derisk.portal
    install -Dm644 -t $out/share/xdg-desktop-portal crates/derisk-portal/data/portal/derisk-portals.conf
    install -Dm644 -t $out/share/dbus-1/services crates/derisk-portal/data/dbus-1/services/*
    substituteInPlace $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.derisk.service \
      --replace-fail "Exec=xdg-desktop-portal-derisk" "Exec=$out/bin/xdg-desktop-portal-derisk"
    substituteInPlace $out/lib/systemd/user/xdg-desktop-portal-derisk.service \
      --replace-fail "ExecStart=xdg-desktop-portal-derisk" "ExecStart=$out/bin/xdg-desktop-portal-derisk"
  '';

  passthru = {
    inherit
      gpui
      terminal
      index
      agent
      ;
  };

  meta = {
    description = "Adaptive, agent-first Wayland desktop environment built on mcsapi";
    homepage = "https://github.com/dasmatus/derisk";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux;
    mainProgram = "derisk";
  };
}
