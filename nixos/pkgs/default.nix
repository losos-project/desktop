# The three programs this repository writes rather than fetches, pm, the
# system manager, and the Halium hardware layer nixpkgs does not carry, plus
# GTK and Qt with the patches that make their applications fit a phone and
# keep the system's icon theme. Everything else the OS runs comes from
# nixpkgs unchanged.
final: prev:
let
  # nixpkgs without this overlay: the packages that would rebuild against
  # the patched GTK below come from here, as cache.nixos.org has them.
  plain = import final.path { inherit (final.stdenv.hostPlatform) system; };
  # A directory of patches, applied in file name order, which the numeric
  # prefixes make the order they were written to apply in.
  patchesIn =
    dir:
    map (name: dir + "/${name}") (
      builtins.sort builtins.lessThan (builtins.attrNames (builtins.readDir dir))
    );
  # A package with a directory of our patches, compiled through ccache and
  # linked by mold. Every package this overlay patches goes through here, so
  # whatever gets patches next gets both, with no list to keep (docs/nixos.md,
  # "GTK and Qt on a phone"). They no longer substitute from cache.nixos.org
  # and CI compiles them anyway, so neither costs a cache hit; their
  # dependents keep nixpkgs' stdenv. useMoldLinker puts ld.mold in the cc
  # wrapper's bintools and adds -fuse-ld=mold, so configure probes and
  # libtool link with mold too, not only the final link. ccache goes on
  # second: the mold adapter adds its flag only for a compiler it knows to be
  # clang or GCC 12 or later, and ccache's wrapper does not say which it is.
  withPatches =
    pkg: dir:
    (pkg.override (args: {
      stdenv = withCcache (final.stdenvAdapters.useMoldLinker args.stdenv);
    })).overrideAttrs
      (old: {
        patches = (old.patches or [ ]) ++ patchesIn dir;
      });
  # The compiler behind ccache when the builder offers a cache at
  # /var/cache/losos-ccache (Nix's extra-sandbox-paths, which CI's prebuild
  # job sets and fills from GHCR), and directly otherwise. The derivation is the same either way, so a build that used
  # the cache is the one the image asks for. ccache returns an object only
  # for the same preprocessed input and compiler, so a GTK or Qt point
  # release, or a changed patch, recompiles only the files it touched.
  # Paths are made relative to the build directory, which differs per build,
  # and a source file's time is not held against it, because patching leaves
  # every patched file newer than the cache entry.
  withCcache =
    stdenv:
    final.ccacheStdenv.override {
      inherit stdenv;
      extraConfig = ''
        if [ -d /var/cache/losos-ccache ] && [ -w /var/cache/losos-ccache ]; then
          export CCACHE_DIR=/var/cache/losos-ccache
          export CCACHE_BASEDIR="$NIX_BUILD_TOP"
          export CCACHE_NOHASHDIR=1
          export CCACHE_SLOPPINESS=include_file_mtime,include_file_ctime,time_macros
          export CCACHE_MAXSIZE=8G
          # Each build runs as whichever nixbld user is free, and ccache
          # makes its subdirectories and temporary files with the build's
          # umask of 022, so the next build user could not write beside them.
          export CCACHE_UMASK=000
        else
          export CCACHE_DISABLE=1
        fi
      '';
    };
  # What this overlay builds itself links with mold too, Rust and C alike:
  # callWithMold hands a package nixpkgs' stdenv and a rustPlatform on it,
  # both through useMoldLinker, so buildRustPackage's cc, which rustc links
  # with, carries -fuse-ld=mold as GTK's does. Only the arguments a package
  # asks for are passed, so a new one needs nothing beyond being called
  # through here. Nothing else in nixpkgs is touched, so it all still
  # substitutes from cache.nixos.org.
  moldStdenv = final.stdenvAdapters.useMoldLinker final.stdenv;
  callWithMold = final.lib.callPackageWith (
    final
    // {
      stdenv = moldStdenv;
      rustPlatform = final.makeRustPlatform {
        inherit (final) rustc cargo;
        stdenv = moldStdenv;
      };
    }
  );
in
{
  pm = callWithMold ./pm.nix { };
  pm-plugins = callWithMold ./pm-plugins.nix { };
  derisk = callWithMold ./derisk.nix { };
  losos-installer = callWithMold ./losos-installer.nix { };
  losos-security = callWithMold ./losos-security.nix { };
  losos-hardware = callWithMold ./losos-hardware.nix { };
  # The Windows installer's Linux build, for its tests and for installing
  # into a disk image; the .exe is windows-installer.nix's.
  losos-windows-installer = callWithMold ./losos-windows-installer.nix { };
  losos-swap = callWithMold ./losos-swap.nix { };
  # losos, the terminal client for LosOS boxes (docs/losos-boxes.md),
  # written in the LosOS repository.
  losos = callWithMold ./losos.nix { };
  # Finds those boxes over mDNS and offers each one as an application.
  losos-boxes = callWithMold ./losos-boxes.nix { };
  # Windows app support (windows-apps.nix): WinBoat, from nixpkgs' own set
  # since it runs on an Electron that would link the patched GTK, renamed
  # and with the setup that installs Windows without asking anything.
  windows-app-support = final.callPackage ./windows-app-support.nix { inherit (plain) winboat; };
  # A guest's apps and windows on the desktop, from macOS's app server or
  # any other that speaks its protocol (docs/app-windows.md).
  app-windows = callWithMold ./app-windows.nix { };
  # This repository's documentation, for reading offline; built by Node, so
  # nothing to link.
  losos-docs = final.callPackage ./losos-docs.nix { };
  # Halium (nixos/halium/): Android's HAL headers, libhybris, which loads
  # the vendor's bionic-linked GPU and HAL libraries into glibc processes,
  # and the generic Android system image that starts those HALs. That image
  # is unpacked from Droidian's build, not linked, so it needs no mold.
  android-headers = callWithMold ./android-headers.nix { };
  libhybris = callWithMold ./libhybris.nix { };
  halium-gsi = final.callPackage ./halium-gsi.nix { };
  # Android app support (android-apps.nix): a virtual Pixel of each
  # person's own and its apps as windows. scrcpy and the emulator's sandbox
  # come from nixpkgs' own set: SDL's window decorations would link the
  # patched GTK.
  android-app-support = final.callPackage ./android-app-support.nix {
    inherit (plain) scrcpy buildFHSEnv;
  };
  # Danube (danube.nix), the web browser, on WPE WebKit (wpewebkit.nix),
  # which nixpkgs does not carry. WebKit is built from nixpkgs' own package
  # set: from this one, GStreamer's plugins would link the patched GTK and
  # need compiling too. It still compiles here, since nothing upstream
  # substitutes it, but not through ccache: nixpkgs' ccache wrapper around
  # clang loses the C++ standard library's headers. Its output is in the
  # project's cache from the first CI run on, so it compiles again only
  # when nixpkgs moves WebKit or its dependencies.
  #
  # Danube replaced Uranium, which was Chromium under the OS's name with a
  # native messaging host for derisk's menus (docs/danube.md, "What it
  # replaced"); Chromium could not be drawn inside an mcsapi window, so it
  # is not coming back.
  wpewebkit = plain.callPackage ./wpewebkit.nix { webkitgtk = plain.webkitgtk_6_0; };
  # QEMU from nixpkgs' own package set too, so it substitutes from
  # cache.nixos.org. From this one its GTK and VTE front end would link the
  # patched GTK, and CI would compile QEMU twice, once for the host's
  # machines (qemu_kvm, docs/hypervisor.md) and once for the OVMF the VM
  # tests boot, which ran a runner's disk out of space. Nothing shows that
  # front end: virt-viewer draws a machine's screen, and the VM tests run
  # headless.
  qemu = plain.qemu;
  # QEMU's vmapple machine under TCG, an M1 Mac's arm64 macOS guest on any
  # PC (docs/macos-arm64.md): steelbrain's 27on86 changes, which boot
  # macOS 13 there, then an apple-m1 CPU and Apple's paravirtual pointer
  # authentication, so signatures are checked and macOS 26 gets past its
  # first PAC hypercall. Only the aarch64 system emulator and nothing to
  # draw with, since the guest runs headless until a GPU joins it.
  qemu-vmapple =
    (withPatches (plain.qemu.override {
      minimal = true;
      hostCpuTargets = [ "aarch64-softmmu" ];
    }) ./patches/qemu-vmapple).overrideAttrs
      { pname = "qemu-vmapple"; };
  reims-vgpu-backend = callWithMold ./reims-vgpu-backend.nix { };
  # libsecret too. Only its test suite reaches the patched GTK, through gjs,
  # so ours differed from nixpkgs' in a check input alone, compiled in CI,
  # and failed there in test-collection when the test's D-Bus service
  # dropped off the bus. nixpkgs' library is the same and substitutes.
  libsecret = plain.libsecret;
  danube = callWithMold ./danube.nix { };
  # The system-wide ad blocker (docs/adblock.md).
  losos-adblock = callWithMold ./losos-adblock.nix { };
  # Backups to a LosOS box (docs/backup.md).
  losos-backup = callWithMold ./losos-backup.nix { };
  x2mcsapi = callWithMold ./x2mcsapi.nix { };
  # sonne.nix is gone: Sonne, the project's fork of Zed, took about half a
  # gigabyte of /usr for an editor, and its agent, the part the OS wanted,
  # is derisk's Agent now (derisk.nix, docs/agent.md).
  # Not a package: every package in nixpkgs, as a pm package's contents.
  pm-payloads = import ./pm-payloads.nix { inherit (final) lib pkgsStatic runCommand; };

  # Mobile-friendly toolkits that keep the system's icons (docs/nixos.md,
  # "GTK and Qt on a phone" and "One icon theme").
  # Replacing them here, rather than patching each application, gives every
  # package that links them the patched build, on the PC and Halium alike.
  # Each patch switches on only when every screen is phone-sized (under 600
  # logical pixels on its short side, derisk's own phone line), so a PC sees
  # stock behaviour; Qt's touch scrolling alone follows a touchscreen instead. The cost is that GTK, Qt and everything built against
  # them no longer substitute from cache.nixos.org and are built by CI.
  #
  # GTK3: Purism's adaptive series, which PureOS and Mobian ship on phones:
  # an adaptive file chooser, about, print and shortcuts windows built from
  # libhandy widgets copied into GTK, maximized dialogs, a back button in
  # dialog header bars, and touch event fixes. 0033 is ours and turns it on
  # from the screen size instead of a per-device setting. 0034, also ours,
  # is the icon theme patch every toolkit here gets: an application can no
  # longer name its own icon theme, through GtkSettings or its GTK theme, or
  # put its icon directories ahead of the system's.
  gtk3 = withPatches prev.gtk3 ./patches/gtk3;
  # GTK4: Purism's three adaptive patches (postmarketOS carried the same two
  # behaviour changes until libadwaita 1.5 made its own dialogs adaptive):
  # resizable dialogs and transient windows open maximized, and get only a
  # close button. Plain GTK4 windows still need them; libadwaita's
  # AdwDialog and breakpoints already adapt and are left alone. 0004 and
  # 0005 are ours, as in GTK3.
  gtk4 = withPatches prev.gtk4 ./patches/gtk4;
  # Qt 6: nobody ships Qt phone patches (Plasma Mobile adapts in Kirigami
  # and its own shell), so both are ours. A finger scrolls Qt Widgets'
  # scroll areas kinetically, and resizable dialogs open maximized with
  # their minimum size capped at the screen. Since Qt 6.10 the Wayland
  # client lives in qtbase, so qtwayland, now only the compositor library,
  # needs nothing. 0003 is the icon theme patch: QIcon::setThemeName()
  # cannot replace the theme QT_QPA_SYSTEM_ICON_THEME names, which derisk
  # exports, and the system's directories lead the search path.
  qt6 = prev.qt6.overrideScope (
    _: qtPrev: {
      qtbase = withPatches qtPrev.qtbase ./patches/qtbase;
    }
  );
  # Qt 5 gets only the icon theme patch, Qt 6's 0003 ported. Nothing in the
  # image links Qt 5, so it costs CI nothing until an app the user installs
  # through pm brings it in; libsForQt5 takes its qtbase from qt5.
  qt5 = prev.qt5.overrideScope (
    _: qtPrev: {
      qtbase = withPatches qtPrev.qtbase ./patches/qtbase5;
    }
  );
}
