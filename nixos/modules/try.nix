# Direct, ephemeral Try LosOS Desktop boot.
#
# The installer ISO carries the desktop's disk, compressed, as `try.squashfs`.
# A Try boot starts the desktop's own kernel and initrd; this unit turns that
# read-only image into a writable block device with a device-mapper snapshot
# whose copy-on-write store lives in tmpfs. The normal repart, dm-verity and
# gpt-auto units then find their disk exactly as they do on an installed
# machine, and nothing on the machine's own disks is touched.
#
# No daemon has to survive the switch into the desktop: loop devices, the
# squashfs mount and the device-mapper target are all in the kernel, so the
# snapshot is still there after the initrd's systemd re-execs as the desktop's
# PID 1.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  attach = pkgs.writeShellApplication {
    name = "losos-try-attach";
    runtimeInputs = with pkgs; [
      coreutils
      util-linux
      lvm2
      systemd
    ];
    text = ''
      set -euo pipefail

      cmdline=$(< /proc/cmdline)
      label=
      for word in $cmdline; do
        case "$word" in
          losos.iso=*) label=''${word#losos.iso=} ;;
        esac
      done
      if [ -z "$label" ]; then
        echo "losos-try-attach: no losos.iso= on the command line" >&2
        exit 1
      fi

      work=/run/losos-try
      mkdir -p "$work/iso" "$work/image"
      mount -t iso9660 -o ro "/dev/disk/by-label/$label" "$work/iso"
      squashfs="$work/iso/try.squashfs"
      if [ ! -f "$squashfs" ]; then
        echo "losos-try-attach: $squashfs is missing" >&2
        exit 1
      fi

      # Keep the raw disk in a compressed filesystem. A loop device can read
      # a file in squashfs directly, and no userspace NBD daemon has to stay
      # alive across the switch into the desktop.
      imageLoop=$(losetup --find --show --read-only "$squashfs")
      mount -t squashfs -o ro "$imageLoop" "$work/image"
      image=
      for candidate in "$work/image"/*.raw; do
        if [ -f "$candidate" ]; then
          image=$candidate
          break
        fi
      done
      if [ -z "$image" ]; then
        echo "losos-try-attach: no raw disk in $squashfs" >&2
        exit 1
      fi

      origin=$(losetup --find --show --read-only "$image")
      sectors=$(blockdev --getsize "$origin")
      truncate --size=$((sectors * 512)) "$work/cow"
      cow=$(losetup --find --show "$work/cow")
      dmsetup create losos-try --table "0 $sectors snapshot $origin $cow N 64"

      # A dm device holding a GPT does not get partition nodes by itself.
      # kpartx makes /dev/mapper/losos-try1 (the ESP), ...3 (usr-verity),
      # ...4 (/usr), and later the slots repart creates.
      kpartx -a /dev/mapper/losos-try
      udevadm settle
    '';
  };
in
{
  boot = {
    initrd = {
      # The medium's ISO 9660 filesystem, the compressed raw disk, the loop
      # devices that make both into block devices, and dm-snapshot itself.
      # The dm-verity and erofs halves come from image.nix's verityStore.
      availableKernelModules = [
        "iso9660"
        "sr_mod"
        "usb_storage"
        "uas"
      ];
      kernelModules = [
        "squashfs"
        "loop"
        "dm_mod"
        "dm_snapshot"
      ];
      supportedFilesystems.iso9660 = lib.mkDefault true;
      supportedFilesystems.squashfs = lib.mkDefault true;

      systemd = {
        storePaths = [ attach ];

        services.losos-try-attach = {
          description = "Attach the ephemeral Try LosOS Desktop disk";
          wantedBy = [ "initrd.target" ];
          after = [ "systemd-udev-settle.service" ];
          # The verity mapping, repart and gpt-auto all have to see the
          # snapshot and its partition nodes.
          before = [
            "systemd-veritysetup@usr.service"
            "initrd-usr-fs.target"
            "systemd-repart.service"
            "initrd-root-fs.target"
          ];
          unitConfig.ConditionKernelCommandLine = "losos.try=1";
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = "${attach}/bin/losos-try-attach";
          };
        };
      };
    };
  };
}
