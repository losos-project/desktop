# How the GSI boots and what it ships as: the initrd that init_boot carries,
# userdata as the root filesystem, and the three images the flasher writes.
# default.nix says why it is built this way.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  inherit (config.losos) version;
  id = "losos-desktop";
  prefix = "${id}_${version}_gsi";

  toplevel = config.system.build.toplevel;
  initrdPath = "${config.system.build.initialRamdisk}/${config.system.boot.loader.initrdFile}";

  loadModules = pkgs.writeShellApplication {
    name = "losos-gsi-load-modules";
    runtimeInputs = [
      pkgs.kmod
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = builtins.readFile ./load-modules.sh;
  };

  # The partition every Android device names its data partition, which the
  # flasher writes this OS's root filesystem to.
  userdata = "/dev/disk/by-partlabel/userdata";
in
{
  options.losos.halium.loadModules = lib.mkOption {
    type = lib.types.package;
    default = loadModules;
    readOnly = true;
    internal = true;
    description = "The module loader, shared with android.nix.";
  };

  config = {
    # The device's kernel is the one that boots, so the system carries none:
    # no kernel image, no modules for a kernel that never runs, and no
    # firmware path pointing into one. The vendor's modules come from the
    # device's own partitions (load-modules.sh).
    boot.kernel.enable = false;
    # The systemd initrd still reads a kernel's config, to decide whether it
    # needs kmod. Every Android kernel loads modules, and so does the initrd
    # here (load-modules.sh); nixpkgs' kernel says the same, and its image
    # goes nowhere.
    system.build.kernel = config.boot.kernelPackages.kernel;

    boot = {
      # The Android bootloader is the only bootloader; there is nothing for
      # NixOS to install.
      loader.grub.enable = false;

      initrd = {
        # NixOS's initrd modules are for a kernel this system does not boot.
        # The ones a device needs to reach its storage are in vendor_boot's
        # ramdisk, which the bootloader unpacks in front of this one.
        includeDefaultModules = false;
        availableKernelModules = lib.mkForce [ ];
        kernelModules = lib.mkForce [ ];
        allowMissingModules = true;

        # The generic kernel image decompresses LZ4 ramdisks in the legacy
        # format, which is what Android's own generic ramdisk is.
        compressor = "lz4";
        compressorArgs = [
          "-l"
          "-9"
        ];

        systemd.storePaths = [ loadModules ];
        # NixOS makes the initrd's /lib a link to the modules of the kernel
        # it built, and there is none. /lib/modules is vendor_boot's,
        # unpacked before this ramdisk, so the initrd has no /lib of its own.
        systemd.contents."/lib".enable = false;

        # vendor_boot's first-stage modules (UFS or eMMC, the regulators and
        # clocks they hang off) sit at /lib/modules with their modules.load.
        # Until they are in, there is no userdata to mount.
        systemd.services.losos-gsi-first-stage-modules = {
          description = "Load vendor_boot's kernel modules";
          unitConfig.DefaultDependencies = false;
          wantedBy = [ "initrd.target" ];
          before = [
            "initrd-root-device.target"
            "systemd-udev-trigger.service"
          ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = "${lib.getExe loadModules} /lib/modules";
          };
        };

        # A bootloader passes the command line from `boot` and `vendor_boot`,
        # which this image does not write, so no init= names the system to
        # boot. The flasher's root filesystem has the one system it was
        # built with at /nix/var/nix/profiles/system, as a NixOS root does,
        # and this finds it there. The rest is nixpkgs' service as it is.
        systemd.services.initrd-find-nixos-closure.script = lib.mkForce ''
          set -uo pipefail
          export PATH="/bin:${
            lib.makeBinPath [
              config.boot.initrd.systemd.package.util-linux
              config.system.nixos-init.package
            ]
          }"

          closure="$(resolve-in-root /sysroot /nix/var/nix/profiles/system)"
          if [ ! -x "/sysroot$closure/prepare-root" ]; then
            echo "/nix/var/nix/profiles/system on userdata is not a NixOS system" >&2
            exit 1
          fi
          ln --symbolic "$closure" /nixos-closure
          echo 'NEW_INIT=' > /etc/switch-root.conf
        '';
      };
    };

    # userdata is the root filesystem, whole. The flasher writes it with an
    # image as large as the system closure, and the first boot grows it to
    # the partition. Android's own data, which it is on a phone, is a
    # directory of it (android.nix).
    fileSystems."/" = {
      device = userdata;
      fsType = "ext4";
      options = [
        "noatime"
        "x-systemd.growfs"
      ];
    };

    system.image = {
      inherit id version;
    };

    # The root filesystem: the system closure on ext4, and the profile link
    # the initrd boots. Stage 2 creates /etc, /var and the rest on first
    # boot, as on the PC image's empty root partition.
    system.build.gsiRootfs = pkgs.callPackage "${modulesPath}/../lib/make-ext4-fs.nix" {
      storePaths = [ toplevel ];
      volumeLabel = "losos-root";
      populateImageCommands = ''
        mkdir -p ./files/nix/var/nix/profiles
        ln -s ${toplevel} ./files/nix/var/nix/profiles/system
      '';
    };

    # The generic ramdisk in the format of Android's own init_boot.img: boot
    # image header v4 with a ramdisk and nothing else.
    system.build.gsiInitBoot =
      pkgs.runCommand "${prefix}-init_boot.img"
        { nativeBuildInputs = [ pkgs.buildPackages.android-tools ]; }
        ''
          mkbootimg --header_version 4 --ramdisk ${initrdPath} --output $out
        '';

    # A vbmeta with verification turned off, so a bootloader that checks
    # init_boot against the stock vbmeta's hash boots this one. Every
    # bootloader that can be unlocked accepts it; none can be relocked over
    # it (docs/web-flasher.md).
    system.build.gsiVbmeta =
      pkgs.runCommand "${prefix}-vbmeta.img" { nativeBuildInputs = [ pkgs.buildPackages.android-tools ]; }
        ''
          avbtool make_vbmeta_image --flags 2 --padding_size 4096 --output $out
        '';

    # What the flasher writes, under the names the release publishes them
    # by. userdata is an Android sparse image, which fastboot sends as
    # chunks no larger than the device asks for and which skips the free
    # space, gzipped because gzip is what a browser decompresses natively.
    system.build.gsiImages =
      pkgs.runCommand "${prefix}"
        {
          nativeBuildInputs = with pkgs.buildPackages; [
            android-tools
            pigz
          ];
        }
        ''
          mkdir -p $out
          cp ${config.system.build.gsiInitBoot} $out/${prefix}-init_boot.img
          cp ${config.system.build.gsiVbmeta} $out/${prefix}-vbmeta.img
          img2simg ${config.system.build.gsiRootfs} userdata.simg
          pigz -p $NIX_BUILD_CORES -9 -c userdata.simg > $out/${prefix}-userdata.simg.gz
          cd $out
          sha256sum -- * > SHA256SUMS
        '';
  };
}
