# GrapheneOS's hardened_malloc as the allocator of every process.
#
# nixpkgs' glibc reads /etc/ld-nix.so.preload where upstream glibc reads
# /etc/ld.so.preload, and loads what it names into every dynamically linked
# process ahead of libc, so malloc and free in all of them, C, C++ and Rust,
# are hardened_malloc's: slab canaries, zero-on-free, guard slabs, randomized
# slot reuse and quarantines, with metadata kept away from the heap it
# describes. Statically linked programs keep their own allocator, and so do
# Android's bionic-linked HALs on the GSI, which nothing here loads into.
#
# On a PC, derisk is built with mcsapi-hardened-malloc as its Rust global
# allocator, which links the same libhardened_malloc.so and frees through
# free_sized, so a free with a corrupted length aborts too.
#
# The Halium GSI (losos.allocator.deviceKernel) boots the phone's own kernel,
# so this cannot know its address space or page size when it is built:
#
#   - hardened_malloc reserves 32 GiB per size class per arena up front,
#     about 12 TiB, which needs a 48-bit address space. Most Android kernels
#     are built with 39-bit virtual addresses, 512 GiB, where that
#     reservation fails and with it every process. The GSI's build reserves
#     512 MiB per size class in one arena, 49 GiB.
#   - It is built for 4 KiB pages, and a few newer phones run 16 KiB ones,
#     where its first mprotect fails.
#
# So on the GSI the initrd tries it first, in a process of its own, and only
# when that process runs is it preloaded into the system the initrd switches
# to. derisk does not link it there: a linked copy would take the desktop
# down on a phone where the trial failed. It gets it through the preload
# like everything else, without free_sized's length check.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.allocator;

  package =
    if cfg.deviceKernel then
      pkgs.graphene-hardened-malloc.overrideAttrs (old: {
        pname = "graphene-hardened-malloc-39bit";
        # 2 x 512 MiB per size class (half of it guard space), 49 size
        # classes, 1 arena: 49 GiB of a 39-bit address space's 512 GiB,
        # where the default's 4 arenas of 32 GiB took 12 TiB. Each size
        # class can still hold 512 MiB of a process's small allocations;
        # anything over 128 KiB is mapped on its own, outside these regions.
        makeFlags = (old.makeFlags or [ ]) ++ [
          "CONFIG_CLASS_REGION_SIZE=536870912"
          "CONFIG_N_ARENA=1"
        ];
      })
    else
      pkgs.graphene-hardened-malloc;

  lib' = "${package}/lib/libhardened_malloc.so";

  # Where the initrd says whether the GSI preloads it; /run survives the
  # switch to the system.
  decided = "/run/losos/ld-nix.so.preload";

  # The trial: a program with hardened_malloc preloaded, which reserves its
  # regions as it loads and then allocates and frees. A kernel that cannot
  # give it the room, or pages of another size, kills it.
  trial = pkgs.writeShellScript "losos-allocator-trial" ''
    ${pkgs.coreutils}/bin/mkdir -p ${dirOf decided}
    if LD_PRELOAD=${lib'} ${pkgs.coreutils}/bin/sort --version > /dev/null; then
      echo ${lib'} > ${decided}
      echo "hardened_malloc runs on this kernel; every process gets it."
    else
      : > ${decided}
      echo "hardened_malloc failed on this kernel; processes keep glibc's malloc." >&2
    fi
  '';
in
{
  options.losos.allocator.deviceKernel = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = ''
      The device's own kernel boots, as on the Halium GSI, rather than one
      this OS built: hardened_malloc is built for a 39-bit address space, and
      the initrd tries it before it is preloaded.
    '';
  };

  config = lib.mkMerge [
    {
      # Not environment.memoryAllocator.provider = "graphene-hardened": that
      # preloads a copy of the library in a store path of its own. The
      # library has no DT_SONAME, so ld.so tells loaded objects apart by
      # file, and derisk, linking the package's copy, would map and
      # initialize a second hardened_malloc beside the preloaded one. Naming
      # the package's own file here makes the two one mapping.
      environment.etc."ld-nix.so.preload" =
        if cfg.deviceKernel then
          { source = decided; }
        else
          {
            text = ''
              ${lib'}
            '';
          };

      # hardened_malloc puts a guard mapping between slabs, and a process
      # with a large heap needs far more mappings than the kernel's default
      # 65530. NixOS already raises vm.max_map_count to 1048576, upstream's
      # recommendation; this fails the build if that default ever goes away.
      assertions = [
        {
          assertion = (config.boot.kernel.sysctl."vm.max_map_count" or 0) >= 1048576;
          message = "hardened_malloc needs vm.max_map_count of at least 1048576.";
        }
      ];
    }

    (lib.mkIf (!cfg.deviceKernel) {
      # After base.nix's overlay, which defines derisk.
      nixpkgs.overlays = lib.mkAfter [
        (final: prev: {
          derisk = prev.derisk.override { withHardenedMalloc = true; };
        })
      ];
    })

    (lib.mkIf cfg.deviceKernel {
      # Before the switch to the system, so its PID 1 is the first process
      # the preload reaches. A glibc that finds the file empty, or finds no
      # file, preloads nothing.
      boot.initrd.systemd = {
        # The script and everything it names: the initrd carries only what
        # is listed and the libraries ELF files among it link.
        storePaths = [
          trial
          pkgs.runtimeShell
          lib'
          "${pkgs.coreutils}/bin/mkdir"
          "${pkgs.coreutils}/bin/sort"
        ];
        services.losos-allocator-trial = {
          description = "Try hardened_malloc on this kernel";
          wantedBy = [ "initrd.target" ];
          before = [
            "initrd.target"
            "initrd-switch-root.target"
          ];
          unitConfig.DefaultDependencies = false;
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = trial;
          };
        };
      };
    })
  ];
}
