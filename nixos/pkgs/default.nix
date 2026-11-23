# Packages this repository builds itself. They are the programs under
# github.com/losos-project (this tree, plus the derisk, mcsapi and pm
# inputs). GTK, Qt, GRUB, QEMU, WebKit and WinBoat stay nixpkgs' builds, so
# they substitute from cache.nixos.org. A patch of one of those used to
# rebuild it and everything callPackage'd against it.
final: prev:
let
  # The component sources, at the commits flake.lock pins (sources.nix).
  sources = import ./sources.nix;
  # What this overlay builds itself links with mold too, Rust and C alike:
  # callWithMold hands a package nixpkgs' stdenv and a rustPlatform on it,
  # both through useMoldLinker, so buildRustPackage's cc, which rustc links
  # with, carries -fuse-ld=mold. Only the arguments a package
  # asks for are passed, so a new one needs nothing beyond being called
  # through here. Nothing else in nixpkgs is touched, so it all still
  # substitutes from cache.nixos.org.
  moldStdenv = final.stdenvAdapters.useMoldLinker final.stdenv;
  rustPlatformMold =
    let
      rp = final.makeRustPlatform {
        inherit (final) rustc cargo;
        stdenv = moldStdenv;
      };
      # Force every Cargo build to one job. pm's checkPhase and other Rust
      # builds can saturate all cores and OOM the host; this keeps them
      # serialized. Individual packages may still set their own cargo flags,
      # but CARGO_BUILD_JOBS=1 wins for cargo's -j default.
      forceCargoJobs1 =
        args:
        let
          env =
            (args.cargoEnv or { })
            // (args.env or { })
            // {
              CARGO_BUILD_JOBS = "1";
              RUST_TEST_THREADS = "1";
              NIX_BUILD_CORES = "1";
            };
        in
        builtins.removeAttrs args [ "cargoEnv" ]
        // {
          inherit env;
          dontUseCargoParallelTests = true;
        };
    in
    rp
    // {
      buildRustPackage =
        args:
        rp.buildRustPackage (
          if builtins.isFunction args then
            finalAttrs: forceCargoJobs1 (args finalAttrs)
          else
            forceCargoJobs1 args
        );
    };
  callWithMold = final.lib.callPackageWith (
    final
    // {
      stdenv = moldStdenv;
      rustPlatform = rustPlatformMold;
    }
  );
in
{
  pm = callWithMold ./pm.nix { source = sources.pm; };
  pm-plugins = callWithMold ./pm-plugins.nix { };
  derisk = callWithMold ./derisk.nix { source = sources.derisk; };
  losos-installer = callWithMold ./losos-installer.nix { };
  # Reports a problem on the desktop as a GitHub issue (docs/reporting.md).
  losos-report = callWithMold ./losos-report.nix { };
  losos-security = callWithMold ./losos-security.nix { };
  losos-hardware = callWithMold ./losos-hardware.nix { };
  # Peripherals in machines of their own: the host's router and the
  # machines' halves (docs/peripherals.md).
  losos-peripherals = callWithMold ./losos-peripherals.nix { };
  # The gaming machine: a Windows machine with a graphics card of its own
  # (docs/gaming-machine.md).
  losos-gaming = callWithMold ./losos-gaming.nix { };
  # Focus mode: background work waits while a game or an editor runs
  # (docs/focus-mode.md).
  losos-focus = callWithMold ./losos-focus.nix { };
  # The Windows installer's Linux build, for its tests and for installing
  # into a disk image; the .exe is windows-installer.nix's.
  losos-windows-installer = callWithMold ./losos-windows-installer.nix { };
  losos-swap = callWithMold ./losos-swap.nix { };
  # losos, the terminal client for LosOS boxes (docs/losos-boxes.md),
  # written in the LosOS repository.
  losos = callWithMold ./losos.nix { };
  # Admin/panes web export, pinned to losos channel/daily; not in the image yet.
  losos-admin-panes = final.callPackage ./losos-admin-panes.nix { };
  # Finds those boxes over mDNS and offers each one as an application.
  losos-boxes = callWithMold ./losos-boxes.nix { };
  # Company accounts: LosOS Login on a box, as homed records here
  # (docs/company.md).
  losos-login-bridge = callWithMold ./losos-login-bridge.nix { };
  # The host's security keys and smart card readers, lent to its machines
  # (docs/tokens.md).
  # Fetches the Slovak eID klient, which no image may carry.
  losos-eid-fetch = final.callPackage ./losos-eid-fetch.nix { };
  # The Agent's machine's channels to the desktop, both ends (docs/agent.md).
  losos-agent-machine = callWithMold ./losos-agent-machine.nix { };
  # The host's way into its service machines: `nmcli` on the host runs in
  # the network machine (docs/network-machine.md).
  losos-vm-call = callWithMold ./losos-vm-call.nix { };
  # The network machine's init and daemon, in place of NetworkManager there.
  losos-netd = callWithMold ./losos-netd.nix { };
  # The files machine's init and daemon: the storage it is given, mounted
  # there and served to the host (docs/files-machine.md).
  losos-filesd = callWithMold ./losos-filesd.nix { };
  # sys-vpn's program (docs/vpn.md): the providers, the check of their
  # WireGuard files on the host, and the tunnel in the machine.
  losos-vpn = callWithMold ./losos-vpn.nix { };
  # The portals' dialogs in a machine of their own, sys-portal, for the
  # host's apps and every machine's (docs/portal-machine.md).
  losos-portal = callWithMold ./losos-portal.nix { };
  # The host's security keys and smart card readers, lent to its machines
  # (docs/tokens.md).
  losos-tokens = callWithMold ./losos-tokens.nix { };
  # Keeps machines apart on libvirt's network by their label
  # (machine-labels.nix).
  losos-machine-labels = callWithMold ./losos-machine-labels.nix { };
  # Windows app support (windows-apps.nix): nixpkgs' WinBoat, renamed, with
  # the setup that installs Windows without asking anything. No source
  # patch, so Electron stays the one cache.nixos.org already built.
  windows-app-support = final.callPackage ./windows-app-support.nix { inherit (prev) winboat; };
  # A guest's apps and windows on the desktop, from macOS's app server or
  # any other that speaks its protocol (docs/app-windows.md).
  app-windows = callWithMold ./app-windows.nix { };
  # Schizo mode's way out through the person's LosOS edge: in the Tor
  # gateway, each connection over Tor to the edge's onion exit.
  losos-edge-chain = callWithMold ./losos-edge-chain.nix { };
  # macOS app support (macos-apps.nix): macOS in dockur/macos on a Mac, and
  # its windows through app-windows.
  macos-app-support = final.callPackage ./macos-app-support.nix { };
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
  # Fedora's Microsoft-signed shim, which starts LosOS's signed GRUB with
  # Secure Boot on (secure-boot.nix). Unpacked, not built.
  shim-signed = final.callPackage ./shim-signed.nix { };
  # Android app support (android-apps.nix): a virtual Pixel of each
  # person's own and its apps as windows. scrcpy and the emulator's sandbox
  # are nixpkgs' packages.
  android-app-support = final.callPackage ./android-app-support.nix {
    inherit (prev) scrcpy buildFHSEnv;
  };
  # Danube is not built here. The browser runs in the Danube machine and
  # links Arch's WPE WebKit (danube-machine.nix). Compiling WebKit in this
  # overlay is a project outside losos-project, and nothing on cache.nixos.org
  # substitutes that build.
  #
  # Danube replaced Uranium, which was Chromium under the OS's name with a
  # native messaging host for derisk's menus (docs/danube.md, "What it
  # replaced"); Chromium could not be drawn inside an mcsapi window, so it
  # is not coming back.
  # QEMU is nixpkgs' build. virt-viewer draws a machine's screen, and the
  # VM tests run headless, so nothing here needs a second QEMU.
  qemu = prev.qemu;
  # The system-wide ad blocker (docs/adblock.md).
  losos-adblock = callWithMold ./losos-adblock.nix { };
  # Backups to a LosOS box (docs/backup.md).
  losos-backup = callWithMold ./losos-backup.nix { };
  x2mcsapi = callWithMold ./x2mcsapi.nix { source = sources.mcsapi; };
  # sonne.nix is gone: Sonne, the project's fork of Zed, took about half a
  # gigabyte of /usr for an editor, and its agent, the part the OS wanted,
  # is derisk's Agent now (derisk.nix, docs/agent.md).
  # Not a package: every package in nixpkgs, as a pm package's contents.
  pm-payloads = import ./pm-payloads.nix { inherit (final) lib pkgsStatic runCommand; };

}
