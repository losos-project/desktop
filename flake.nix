# LosOS Desktop as NixOS.
#
# A derisk desktop that uses systemd for everything it can, installs as an
# image, updates with systemd-sysupdate and makes every user a systemd-homed
# LUKS volume, written as a NixOS configuration. nixos/ holds the modules
# and docs/ says how each piece is built.
#
#   nix build                      the disk image (.#image)
#   nix build .#installer          the installer ISO: Wi-Fi, a disk, sysupdate
#   nix build .#windows-installer  x86_64: the .exe that installs beside Windows
#   nix build .#release            the files CI publishes through the proxy
#   nix run .#vm                   boot the image in QEMU with UEFI firmware
#   nix build .#pm-payloads.<pkg>  any nixpkgs package, static, for a pm build file
#   nix flake check                evaluate both architectures, test this repository's
#                                  programs, and (with KVM) boot the image
{
  description = "LosOS Desktop: a systemd-native, image-based derisk desktop on NixOS";

  inputs = {
    # The channel tarball rather than github:NixOS/nixpkgs. It is the same
    # tree at the revision the channel's Hydra jobset tested, and resolves to
    # an immutable releases.nixos.org URL that flake.lock pins by narHash
    # exactly as it would a git revision. It is also the form that
    # fetches without api.github.com, which rate-limits anonymous clients and
    # which some build hosts cannot reach at all. `nix flake update nixpkgs`
    # moves it.
    #
    # nixos-unstable, the channel rather than nixpkgs master: Hydra has built
    # and tested every revision it points at, so the stock closures still
    # substitute from cache.nixos.org. An update to this OS is a whole new
    # image that CI builds and the VM test boots before it is published, so
    # a breaking change upstream stops at CI rather than on a machine, and
    # tracking unstable buys the newer kernel, Mesa and systemd that the
    # desktop and the Halium target (nixos/halium/) want.
    nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";

    # derisk, mcsapi and pm are git submodules under components/, so the
    # commit this tree carries for each of them is the one the image builds.
    # Moving one is `git -C components/<name> checkout <rev>` and a commit
    # here, plus a new cargoHash in nixos/pkgs when its Cargo.lock changed.
    # Nix 2.27 and later read this and fetch the submodules with the flake.
    self.submodules = true;

    # Only for the check that builds the Home Manager module (nixos/home/pm.nix)
    # into a real Home Manager generation. A user's own Home Manager is what
    # imports the module, so nothing the image or a release builds reads this.
    # git+https rather than github:, for the same api.github.com reason as
    # nixpkgs above; shallow, because the history is not needed.
    home-manager = {
      url = "git+https://github.com/nix-community/home-manager?ref=master&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      home-manager,
    }:
    let
      inherit (nixpkgs) lib;

      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = f: lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});

      # strverscmp(3) orders this the way time does, which is what
      # systemd-sysupdate needs from a version: a later commit compares greater.
      # A dirty tree reuses the last commit's date, so a local build never
      # claims to be newer than the release it was built from.
      version =
        let
          d = self.lastModifiedDate or "19700101000000";
        in
        "${lib.substring 0 8 d}.${lib.substring 8 6 d}";

      # A hosted proxy (proxy/) to update from and ping, when there is one.
      # Without it the images update from GitHub Releases through the
      # documentation site's index (nixos/modules/update-source.nix) and
      # send no ping.
      proxyUrl = builtins.getEnv "LOSOS_PROXY_URL";

      # The cadence CI builds for (ci.yml): `nightly`, or `weekly`,
      # `monthly` or `yearly` for a run cadence.yml started. Empty outside
      # Actions, where options.nix's default, nightly, stands.
      channel = builtins.getEnv "LOSOS_CHANNEL";

      # The repository CI runs in, which is the one its releases go to.
      # Empty outside Actions, where options.nix's default stands.
      releaseRepo = builtins.getEnv "GITHUB_REPOSITORY";

      # The GitHub App the installer signs in with to report a failed
      # install (docs/installer.md), a repository variable in CI. Empty
      # outside Actions, where the ISO offers no report.
      reportClientId = builtins.getEnv "LOSOS_REPORT_CLIENT_ID";

      # requireProxy refused to build an image without LOSOS_PROXY_URL, back
      # when the proxy was the only place an image could update from. It
      # went when GitHub Releases became the default source, and is not
      # coming back.

      # What the images built for release take from the proxy's URL, which
      # is a repository variable in CI rather than anything written here.
      proxySettings =
        lib.optionalAttrs (proxyUrl != "") {
          losos.ping.url = lib.mkDefault "${lib.removeSuffix "/" proxyUrl}/ping";
        }
        // lib.optionalAttrs (releaseRepo != "") {
          losos.release.repository = lib.mkDefault releaseRepo;
        }
        // lib.optionalAttrs (channel != "") {
          losos.channel = lib.mkDefault channel;
        }
        // lib.optionalAttrs (reportClientId != "") {
          losos.report.clientId = lib.mkDefault reportClientId;
        };

      mkSystem =
        system:
        lib.nixosSystem {
          modules = [
            self.nixosModules.default
            proxySettings
            {
              # Use nixpkgs' standard glibc platform so its stock package
              # closures can substitute from cache.nixos.org.
              nixpkgs.hostPlatform = system;
              losos.version = lib.mkDefault version;
            }
            (
              { config, ... }:
              lib.optionalAttrs (proxyUrl != "") {
                # The image's update source and the artifact published to the
                # proxy must agree.
                losos.update.baseUrl = lib.mkDefault "${lib.removeSuffix "/" proxyUrl}/updates/${config.losos.channel}/${archOf system}/";
              }
            )
          ];
        };

      # The GSI: the same OS as one image for every Treble phone and tablet
      # that takes generic system images and runs Linux 5.10 or newer, over
      # Halium (nixos/halium/).
      # arm64 only, since that is what those devices are.
      gsiSystem = lib.nixosSystem {
        modules = [
          self.nixosModules.gsi
          proxySettings
          {
            nixpkgs.hostPlatform = "aarch64-linux";
            losos.version = lib.mkDefault version;
          }
        ];
      };

      archOf = system: lib.head (lib.splitString "-" system);
      configOf = system: self.nixosConfigurations."losos-desktop-${archOf system}".config;
    in
    {
      nixosModules.default = ./nixos/modules;
      nixosModules.gsi = ./nixos/halium;

      overlays.default = import ./nixos/pkgs;

      # pm with its plugins installed by the user's Home Manager
      # (docs/pm.md, "pm's plugins"): `programs.pm.enable = true`.
      # homeManagerModules is the older name Home Manager's docs still use.
      homeModules.pm = import ./nixos/home/pm.nix { inherit self; };
      homeManagerModules.pm = self.homeModules.pm;

      nixosConfigurations =
        lib.listToAttrs (
          map (system: lib.nameValuePair "losos-desktop-${archOf system}" (mkSystem system)) systems
        )
        // {
          losos-desktop-gsi-aarch64 = gsiSystem;
        };

      packages = forAllSystems (
        system: pkgs:
        let
          build = (configOf system).system.build;
        in
        {
          image = build.disk;
          installer = build.installerIso;
          qcow2 = build.qcow2;
          # On arm64 the release also carries the GSI's files, under the
          # same SHA256SUMS and signature, which is where the web flasher
          # (website/static/flasher/) downloads them from.
          release =
            if system == "aarch64-linux" then
              pkgs.runCommand "${build.releaseArtifacts.name}-with-gsi" { } ''
                mkdir -p $out
                # Both directories carry a SHA256SUMS, and one cp refuses to
                # overwrite a file it just created, so each copy drops its own
                # before the combined list is written.
                cp ${build.releaseArtifacts}/* $out/
                rm $out/SHA256SUMS
                cp ${gsiSystem.config.system.build.gsiImages}/* $out/
                cd $out
                rm SHA256SUMS
                sha256sum -- * > SHA256SUMS
              ''
            else
              build.releaseArtifacts;
          uki = build.uki;
          toplevel = build.toplevel;
          # Packages of their own (derisk, pm, danube, ...) are reached
          # through legacyPackages below, which `nix build .#<name>` falls
          # back to.
          default = build.disk;
        }
        // lib.optionalAttrs (system == "aarch64-linux") {
          # init_boot.img, vbmeta.img and userdata.simg.gz, with SHA256SUMS:
          # what the flasher writes. No update URL is baked into them.
          gsi = gsiSystem.config.system.build.gsiImages;
        }
        // lib.optionalAttrs (build ? windowsInstaller) {
          # The channel URL is compiled in, as in the UKI.
          windows-installer = build.windowsInstaller;
        }
      );

      # The whole of nixpkgs at the pinned revision, as this OS builds it:
      # glibc, with this repository's overlay. `nix build .#<package>`
      # reaches any of it, and `.#pm-payloads.<package>` is the same package
      # static and laid out for a pm build file to install (docs/nixos.md,
      # "nixpkgs in a pm build"). It is the system's own package set, so a
      # package here and the same package in the image are one store path.
      legacyPackages = forAllSystems (
        system: _: self.nixosConfigurations."losos-desktop-${archOf system}".pkgs
      );

      checks = forAllSystems (
        system: pkgs:
        {
          # Building the toplevel evaluates every module and every assertion in
          # them, which is most of what can go wrong in a configuration.
          toplevel = self.packages.${system}.toplevel;
        }
        # Each program's own test suite runs as part of its build.
        // lib.genAttrs [
          "losos-installer"
          "losos-report"
          "losos-hardware"
          "losos-security"
          "losos-swap"
          "losos-windows-installer"
          "derisk"
          "pm"
          "pm-plugins"
          "losos-adblock"
          "losos-backup"
          "losos-boxes"
          "losos-tokens"
          "danube"
        ] (name: self.legacyPackages.${system}.${name})
        // {
          home-manager-pm = import ./nixos/tests/home-manager-pm.nix {
            inherit self home-manager pkgs;
          };
          # qemu-vmapple's M1 CPU and Apple's pointer authentication, under
          # TCG with no Apple software (docs/macos-arm64.md).
          apple-m1 = import ./nixos/tests/apple-m1.nix {
            inherit pkgs;
            inherit (self.legacyPackages.${system}) qemu-vmapple;
          };
          # Reims' GPU backend for those guests. Nothing in the image uses
          # it, so without this nothing would build it.
          reims-vgpu-backend = self.legacyPackages.${system}.reims-vgpu-backend;
        }
        // lib.optionalAttrs (system == "aarch64-linux") {
          # Every GSI module and assertion, libhybris's build, and the
          # images themselves: mkbootimg, avbtool, the ext4 rootfs and its
          # sparse, compressed form only run when the images are built.
          gsi = self.packages.${system}.gsi;
          libhybris = self.legacyPackages.${system}.libhybris;
        }
        # Boots the image under QEMU and checks the systemd pieces are actually
        # in place. Needs KVM, which the test driver asks for, so a builder
        # without it refuses rather than running for hours.
        // lib.optionalAttrs (system == "x86_64-linux") {
          boot = pkgs.testers.runNixOSTest (import ./nixos/tests/boot.nix { inherit self; });
          # Driver choice from facter's report (hardware.nix, nvidia.nix).
          hardware = pkgs.testers.runNixOSTest ./nixos/tests/hardware.nix;
          # The same image on a legacy BIOS, started by GRUB (bios.nix).
          boot-bios = pkgs.testers.runNixOSTest (import ./nixos/tests/boot-bios.nix { inherit self; });
          # hibernate is gone with hibernate.nix: the locked-down kernel
          # refuses to hibernate (kernel-hardening.nix).
          # The start-up sound reaches a sound card, and plays backwards at
          # shutdown (boot-sound.nix).
          boot-sound = pkgs.testers.runNixOSTest ./nixos/tests/boot-sound.nix;
          # LosOS boxes, one announcing _losos._tcp and one from before
          # that, found over mDNS and offered as applications (losos-boxes.nix).
          losos-boxes = pkgs.testers.runNixOSTest ./nixos/tests/losos-boxes.nix;
          # A real LosOS box, from the commit the losos client is pinned to,
          # beside the desktop: found by its announcement, signed in to with
          # losos, and backed up to (losos-box.nix).
          losos-box = pkgs.testers.runNixOSTest ./nixos/tests/losos-box.nix;
          # pm makes a machine from the guest template, which boots, lists its
          # apps and sends an app's window to the host over vsock
          # (machines.nix).
          machines = pkgs.testers.runNixOSTest ./nixos/tests/machines.nix;
          # A security key and a smart card reader on the host, used from a
          # machine's half of the relay over vsock's loopback (tokens.nix).
          tokens = pkgs.testers.runNixOSTest ./nixos/tests/tokens.nix;
          # Signing in with an eID card: homed enrols the certificate that
          # may decrypt and logs a person in with the PIN, through the
          # relay and the eID machine's half, with SoftHSM for the card
          # (eid-signin.nix).
          eid-signin = pkgs.testers.runNixOSTest ./nixos/tests/eid-signin.nix;
          # derisk-terminal draws in software with no GPU and through GL when
          # asked, and both show its text and Nerd Font icons (terminal.nix).
          terminal = pkgs.testers.runNixOSTest ./nixos/tests/terminal.nix;
          # A person's backups reach a Nextcloud and a Forgejo laid out as a
          # LosOS box lays them out, files to one and a repository to the
          # other, then again after changes (backup.nix).
          backup = pkgs.testers.runNixOSTest ./nixos/tests/backup.nix;
          # Windows app support's containers: a person's rootless Podman from
          # the image's subordinate IDs, a container run through WinBoat's
          # pm and its podman plugin, and none of either for root
          # (windows-apps.nix).
          windows-apps = pkgs.testers.runNixOSTest ./nixos/tests/windows-apps.nix;
          installer-boot = pkgs.testers.runNixOSTest (
            import ./nixos/tests/installer-boot.nix { inherit self; }
          );
          installer-boot-bios = pkgs.testers.runNixOSTest (
            import ./nixos/tests/installer-boot.nix {
              inherit self;
              bios = true;
            }
          );
        }
      );

      # VM tests that need the internet, which the sandbox `checks` build
      # in has none of. CI's android-desktop job builds each one's driver
      # and runs it outside the sandbox.
      networkTests = forAllSystems (
        system: pkgs:
        lib.optionalAttrs (system == "x86_64-linux") {
          # Android app support in the booted image: the Pixel set up and
          # booted inside it, and an app as a window of the desktop's
          # (android-apps.nix).
          android-apps = pkgs.testers.runNixOSTest (import ./nixos/tests/android-apps.nix { inherit self; });
        }
      );

      apps = forAllSystems (
        system: pkgs:
        let
          config = configOf system;
          machine =
            {
              x86_64-linux = "q35,accel=kvm:tcg";
              aarch64-linux = "virt,accel=kvm:tcg -cpu max";
            }
            .${system};
          vm = pkgs.writeShellApplication {
            name = "losos-desktop-vm";
            runtimeInputs = [
              pkgs.qemu
              pkgs.coreutils
            ];
            # A writable copy, grown to a size first boot can lay a whole disk
            # out on: repart needs room for slot B, root, /home and swap.
            text = ''
              disk=''${LOSOS_DISK:-losos-desktop.qcow2}
              vars=''${disk%.qcow2}-efivars.fd
              if [ ! -e "$disk" ]; then
                qemu-img create -f qcow2 -F qcow2 -b ${config.system.build.qcow2} "$disk" 40G
              fi
              # The firmware's variable store is per machine and writable:
              # GRUB records which loader started and from which
              # partition there.
              if [ ! -e "$vars" ]; then
                install -m 0644 ${pkgs.OVMF.variables} "$vars"
              fi
              exec qemu-system-${config.losos.arch.name} -machine ${machine} \
                -drive if=pflash,format=raw,readonly=on,file=${pkgs.OVMF.firmware} \
                -drive if=pflash,format=raw,file="$vars" \
                -m 4096 -smp 4 \
                -device virtio-vga -device virtio-net-pci,netdev=n0 -netdev user,id=n0 \
                -drive if=virtio,file="$disk" "$@"
            '';
          };
        in
        {
          vm = {
            type = "app";
            program = lib.getExe vm;
            meta.description = "Boot the LosOS Desktop image in QEMU with UEFI firmware";
          };
        }
      );

      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            packages = with pkgs; [
              nixfmt
              qemu
              jq
              xz
              # repart, ukify and sysupdate, to inspect what an image contains
              # with the same tools that built it.
              systemd
              systemdUkify
            ];
          };
        }
      );

      formatter = forAllSystems (system: pkgs: pkgs.nixfmt-tree);
    };
}
