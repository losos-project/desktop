# The host's whole module tree and firmware in a BusyBox machine
# (busybox-guests.nix), for a machine handed hardware: the network
# machine's cards could need any of the kernel's drivers and any firmware,
# and no initrd could hold them all. With
# losos.busyboxGuests.<name>.hostKernelTree set, the machine runs from the
# host's store (fromHostStore), where the host's module tree and firmware
# already are, and a step before the machine's program points the kernel
# at them.
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
  # Named, not depended on: a store path the step's text depends on would
  # be copied into the initrd with everything it refers to, every module
  # and firmware file, over a gigabyte. The host's closure has them.
  path = p: builtins.unsafeDiscardStringContext (toString p);
  modules = "${path host.system.modulesTree}/lib/modules";
  firmware = "${path host.hardware.firmware}/lib/firmware";

  step = pkgs.writeScript "losos-guest-host-kernel" ''
    #!/bin/sh
    ln -sfn ${modules} /lib/modules
    echo /bin/modprobe > /proc/sys/kernel/modprobe
    # mdev went on reading uevents with the early modules; this one takes
    # over with the whole tree, and every device there now is offered its
    # driver from it.
    killall mdev 2>/dev/null
    mdev -d
    for alias in $(cat /sys/bus/*/devices/*/modalias 2>/dev/null | sort -u); do
      modprobe -b -q "$alias"
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
              Whether the machine loads modules and firmware from the host's
              whole kernel module tree and firmware, through the host's
              store.
            '';
          };
          config = lib.mkIf config.hostKernelTree {
            fromHostStore = true;
            files."/etc/init.d/05-host-kernel" = step;
            firmwarePath = lib.mkDefault firmware;
          };
        }
      )
    );
  };
}
