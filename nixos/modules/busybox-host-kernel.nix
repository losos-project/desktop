# The host's whole module tree and firmware in a BusyBox machine
# (busybox-guests.nix), for a machine handed hardware: the network
# machine's cards could need any of the kernel's drivers and any firmware,
# and no initrd could hold them all. service-machines.nix already shares
# the host's store read-only, as the tag nix-store; with
# losos.busyboxGuests.<name>.hostKernelTree set, a step before the
# machine's program mounts it and links the store paths the module tree and
# firmware are made of into the machine's own /nix/store, so the trees'
# symlinks, which name /nix/store, resolve. Only those paths: the host's
# store is not mounted over the machine's, whose own programs the host's
# store need not hold.
#
# Then the kernel's own requests for a module (a filesystem, an nftables
# expression) and mdev's for a device the machine is given find them,
# through /lib/modules and /proc/sys/kernel/modprobe, and the devices the
# machine was started with get their drivers loaded again from the whole
# tree.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  host = config;
  modules = "${host.system.modulesTree}/lib/modules";
  firmware = "${host.hardware.firmware}/lib/firmware";
  closure = pkgs.closureInfo {
    rootPaths = [
      host.system.modulesTree
      host.hardware.firmware
    ];
  };

  step = pkgs.writeScript "losos-guest-host-kernel" ''
    #!/bin/sh
    mkdir -p /run/host-store /nix/store
    if ! mount -t 9p -o trans=virtio,version=9p2000.L,ro nix-store /run/host-store; then
      echo "losos-guest: no host store, so only the initrd's modules" >&2
      exit 0
    fi
    while read -r path; do
      [ -e "$path" ] || ln -s "/run/host-store/''${path#/nix/store/}" "$path"
    done < ${closure}/store-paths
    ln -sfn ${modules} /lib/modules
    echo /bin/modprobe > /proc/sys/kernel/modprobe
    # mdev went on reading uevents with the initrd's modules; this one
    # takes over with the whole tree, and every device there now is
    # offered its driver from it.
    killall mdev 2>/dev/null
    MODULE_DIR=${modules} mdev -d
    for alias in $(cat /sys/bus/*/devices/*/modalias 2>/dev/null | sort -u); do
      MODULE_DIR=${modules} modprobe -b -q "$alias"
    done
    true
  '';
in
{
  options.losos.busyboxGuests = lib.mkOption {
    type = lib.types.attrsOf (
      lib.types.submodule (
        { config, ... }:
        {
          options.hostKernelTree = lib.mkOption {
            type = lib.types.bool;
            default = false;
            description = ''
              Whether the machine reaches the host's whole kernel module tree
              and firmware through the host's shared store.
            '';
          };
          config = lib.mkIf config.hostKernelTree {
            files."/etc/init.d/05-host-kernel" = step;
            firmwarePath = lib.mkDefault firmware;
          };
        }
      )
    );
  };
}
