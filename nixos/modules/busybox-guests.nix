# Service machines with no operating system of their own
# (docs/peripherals.md, "BusyBox machines"): BusyBox, the one program a machine is there
# to run, and the host's own kernel. The network, files, USB and Bluetooth
# machines, and the disposable machines devices are given, are each
# nothing but that, so there is no distribution in them to update, no
# service manager, no shell anyone logs in to, and nothing a compromised
# device driver could persist in: a machine's whole system is its initrd,
# in the host's verity /usr, and it starts over from it at every boot.
#
# Each losos.busyboxGuests.<name> builds `build.kernel`, `build.initrd`
# and `build.cmdline`, which service-machines.nix boots directly
# (libvirt's <os><kernel/><initrd/><cmdline/>). The kernel is the host's,
# the file the host booted, so a guest runs whatever kernel the host
# runs and an update moves both; the initrd holds BusyBox, the closure of
# the machine's packages (a dynamically linked program brings its glibc,
# not a NixOS), the host kernel's modules the machine names with what they
# depend on, and /init, which mounts the kernel's filesystems, loads those
# modules, mounts the machine's 9p shares, runs /etc/init.d/* and then
# replaces itself with the machine's program, which is PID 1 from then on.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  kernelFile = "${config.boot.kernelPackages.kernel}/${config.system.boot.loader.kernelFile}";

  # The console QEMU's machine type has as its first serial port.
  console = if pkgs.stdenv.hostPlatform.isx86 then "ttyS0" else "ttyAMA0";

  guest =
    { name, config, ... }:
    {
      options = {
        init = lib.mkOption {
          type = lib.types.str;
          description = ''
            The command line /init execs once the machine is up, as PID 1:
            the program the machine is for. It reaps no children unless it
            does so itself, so one that starts others should.
          '';
        };
        packages = lib.mkOption {
          type = lib.types.listOf lib.types.package;
          default = [ ];
          description = "Packages whose bin is on PATH, with their closures.";
        };
        modules = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ ];
          description = ''
            Host kernel modules /init loads, with what they need; virtio's,
            vsock's and 9p's are always there. A device the machine is given later
            loads its own driver through mdev, if the driver is among
            these or a share brings the host's modules.
          '';
        };
        shares = lib.mkOption {
          type = lib.types.listOf (
            lib.types.submodule {
              options = {
                source = lib.mkOption {
                  type = lib.types.str;
                  description = "The host directory.";
                };
                writable = lib.mkOption {
                  type = lib.types.bool;
                  default = false;
                  description = ''
                    Whether the machine may write to it, for state it keeps
                    across starts, such as the Bluetooth machine's pairings.
                  '';
                };
                target = lib.mkOption {
                  type = lib.types.str;
                  description = "Where the machine mounts it.";
                };
                tag = lib.mkOption {
                  type = lib.types.str;
                  description = "The 9p mount tag, at most 31 bytes.";
                };
              };
            }
          );
          default = [ ];
          description = ''
            Host directories the machine mounts over 9p, read-only unless
            they say otherwise: the host's firmware, which no initrd could
            hold all of, or a directory for state.
            service-machines.nix adds the matching <filesystem>s.
          '';
        };
        moduleDir = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Where modprobe looks once the shares are mounted, such as a
            share of the host's modules, for a machine given devices whose
            drivers nobody can name at build time. The initrd's own modules
            load first either way, 9p's among them.
          '';
        };
        firmwarePath = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "Where the kernel looks for firmware first (firmware_class.path).";
        };
        fromHostStore = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = ''
            Whether the machine runs its programs from the host's store,
            which service-machines.nix shares read-only as `nix-store`,
            rather than from copies in its initrd. Its initrd is then a
            static BusyBox and the few modules that mount the share, a few
            MiB, where a closure would be unpacked into memory the machine
            never gets back.
          '';
        };
        files = lib.mkOption {
          type = lib.types.attrsOf lib.types.path;
          default = { };
          description = ''
            Files linked into the initrd by their path there, such as
            "/etc/init.d/10-network" for a step /init runs before the
            machine's program.
          '';
        };
        build = lib.mkOption {
          type = lib.types.attrs;
          readOnly = true;
          description = ''
            The machine's kernel, initrd and command line, and `hostPaths`:
            what it runs from the host's store, which the host's closure
            keeps.
          '';
        };
      };

      config.build =
        let
          modules = pkgs.makeModulesClosure {
            kernel = config'.system.modulesTree;
            firmware = config'.hardware.firmware;
            rootModules = [
              # The bus every one of QEMU's virtio devices is on.
              "virtio_pci"
              "vmw_vsock_virtio_transport"
              "9p"
              "9pnet_virtio"
            ]
            ++ lib.optionals (!config.fromHostStore) config.modules;
            # A module this kernel builds in rather than as a module.
            allowMissing = true;
          };

          # BusyBox under everything: kmod's modprobe, which reads the
          # kernel's compressed modules, and a machine's own programs win
          # where both have one.
          path = pkgs.buildEnv {
            name = "losos-guest-${name}-path";
            paths = [ (lib.lowPrio pkgs.busybox) ] ++ map lib.hiPrio ([ pkgs.kmod ] ++ config.packages);
            pathsToLink = [ "/bin" ];
          };

          # mdev loads a hotplugged device's driver by its modalias.
          mdevConf = pkgs.writeText "mdev.conf" ''
            $MODALIAS=.* 0:0 0660 @modprobe -b -q "$MODALIAS"
          '';

          # From the host's store, the initrd holds a static BusyBox, which
          # needs nothing else, and the modules that mount the store,
          # uncompressed, as BusyBox's modprobe reads them, where it looks:
          # /lib/modules. Everything after that is the host's.
          busybox = pkgs.pkgsStatic.busybox;
          early =
            pkgs.runCommand "losos-guest-${name}-early-modules"
              {
                nativeBuildInputs = [
                  pkgs.kmod
                  pkgs.xz
                  pkgs.zstd
                ];
              }
              ''
                cp -r --no-preserve=mode ${modules} $out
                find $out -name '*.ko.xz' -exec xz -d {} +
                find $out -name '*.ko.zst' -exec zstd -q -d --rm {} +
                for dir in $out/lib/modules/*; do
                  depmod -b $out "$(basename "$dir")"
                done
              '';
          # What a machine run from the host's store runs. /init names
          # these without its initrd holding them, so the host's closure
          # must.
          hostPaths = [
            path
            mdevConf
            config'.system.modulesTree
          ]
          ++ lib.attrValues config.files;

          storeInit = ''
            #!${pkgs.busybox}/bin/sh
            export PATH=${path}/bin MODULE_DIR=${modules}/lib/modules
            mkdir -p /proc /sys /dev /run /tmp
            mount -t proc proc /proc
            mount -t sysfs sysfs /sys
            mount -t devtmpfs devtmpfs /dev
          '';

          hostInit = ''
            #!${busybox}/bin/sh
            PATH=${busybox}/bin
            mkdir -p /proc /sys /dev /run /tmp /nix/store /etc /bin
            mount -t proc proc /proc
            mount -t sysfs sysfs /sys
            mount -t devtmpfs devtmpfs /dev
            for m in virtio_pci vmw_vsock_virtio_transport 9pnet_virtio 9p; do
              modprobe "$m" || echo "losos-guest: cannot load $m" >&2
            done
            mount -t 9p -o ro,trans=virtio,version=9p2000.L,msize=1048576,cache=loose nix-store /nix/store \
              || { echo "losos-guest: cannot mount the host's store" >&2; exit 1; }
            ${lib.concatStrings (
              lib.mapAttrsToList (target: object: ''
                mkdir -p "$(dirname ${lib.escapeShellArg target})"
                ln -sfn ${object} ${lib.escapeShellArg target}
              '') (config.files // { "/etc/mdev.conf" = mdevConf; })
            )}
            for tool in ${path}/bin/*; do
              ln -sf "$tool" /bin/
            done
            export PATH=${path}/bin MODULE_DIR=${config'.system.modulesTree}/lib/modules
          '';

          mainInit = ''
            mount -t tmpfs -o mode=0755 tmpfs /run
            mount -t tmpfs tmpfs /tmp
            mkdir -p /dev/pts /dev/shm
            mount -t devpts devpts /dev/pts
            hostname ${name}
            for m in virtio_pci vmw_vsock_virtio_transport 9pnet_virtio 9p ${lib.escapeShellArgs config.modules}; do
              modprobe -q "$m" || echo "losos-guest: cannot load $m" >&2
            done
            ${lib.concatMapStrings (s: ''
              mkdir -p ${lib.escapeShellArg s.target}
              mount -t 9p -o trans=virtio,version=9p2000.L,${
                if s.writable then "rw" else "ro"
              } ${lib.escapeShellArg s.tag} ${lib.escapeShellArg s.target} \
                || echo "losos-guest: cannot mount ${s.target}" >&2
            '') config.shares}
            ${lib.optionalString (config.moduleDir != null) ''
              export MODULE_DIR=${lib.escapeShellArg config.moduleDir}
            ''}
            # The kernel has no uevent helper, so mdev reads its events
            # itself, in the background, after making what is there now.
            mdev -s
            mdev -d
            for step in /etc/init.d/*; do
              [ -x "$step" ] && "$step"
            done
            exec ${config.init}
          '';

          # From the host's store, the script's text names its store paths
          # without depending on them, so they stay out of the initrd.
          init = pkgs.writeScript "losos-guest-${name}-init" (
            if config.fromHostStore then
              builtins.unsafeDiscardStringContext (hostInit + mainInit)
            else
              storeInit + mainInit
          );

          storeContents = [
            {
              object = init;
              symlink = "/init";
            }
            {
              # For programs that run /bin/sh.
              object = pkgs.runCommand "losos-guest-${name}-bin" { } "ln -s ${path}/bin $out";
              symlink = "/bin";
            }
            {
              object = mdevConf;
              symlink = "/etc/mdev.conf";
            }
          ]
          ++ lib.mapAttrsToList (target: object: {
            inherit object;
            symlink = target;
          }) config.files;

          hostContents = [
            {
              object = init;
              symlink = "/init";
            }
            {
              object = busybox;
              symlink = "/early/busybox";
            }
            {
              object = "${early}/lib/modules";
              symlink = "/lib/modules";
            }
          ];
        in
        {
          kernel = kernelFile;
          initrd = "${
            pkgs.makeInitrd {
              name = "losos-guest-${name}";
              compressor = "zstd";
              contents = if config.fromHostStore then hostContents else storeContents;
            }
          }/initrd";
          hostPaths = lib.optionals config.fromHostStore hostPaths;
          cmdline = lib.concatStringsSep " " (
            [
              "console=${console}"
              "rdinit=/init"
              # A machine that cannot go on stops rather than waits.
              "panic=-1"
              "loglevel=4"
            ]
            ++ lib.optional (config.firmwarePath != null) "firmware_class.path=${config.firmwarePath}"
          );
        };
    };

  # The host's configuration, under the submodule's own `config`.
  config' = config;
in
{
  config.system.extraDependencies = lib.concatMap (guest: guest.build.hostPaths) (
    lib.attrValues config.losos.busyboxGuests
  );

  options.losos.busyboxGuests = lib.mkOption {
    type = lib.types.attrsOf (lib.types.submodule guest);
    default = { };
    description = ''
      Machines that are BusyBox, one program and the host's kernel, by
      name. Each gives `build.kernel`, `build.initrd` and `build.cmdline`.
    '';
  };
}
