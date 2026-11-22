# Legacy BIOS PCs, started by Limine from the MBR (docs/boot-loaders.md).
#
# UEFI firmware starts Limine from the ESP (limine.nix). This is for x86_64
# PCs whose firmware has no UEFI, or has it turned off: the same disk, the
# same /usr slots, the same menu and the same updates, started by Limine
# from the MBR instead. Every disk the image or the installer lays out
# carries both, so one disk starts on either kind of firmware.
#
#   BIOS -> MBR -> Limine's stage in the BIOS boot partition
#     -> kernel + initrd + command line of the newest UKI
#
# Limine cannot run a UKI, which is an EFI program, so on BIOS it starts
# each UKI's kernel, initrd and command line (uki-files.nix).
# losos-limine-menu writes that into limine.conf. image.nix and the
# installer run that program on the disk, and on x86_64 it also installs
# Limine's BIOS boot code.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos;
  inherit (cfg) arch;

  # systemd-gpt-auto-generator mounts the ESP only on an EFI boot: without
  # LoaderDevicePartUUID it cannot tell which ESP the boot came from. A BIOS
  # boot came from the disk root is on, so this mounts that disk's ESP at
  # /efi, where gpt-auto would have, and sysupdate and bootctl find $BOOT
  # there. On a UEFI boot it writes nothing and gpt-auto does the job.
  espGenerator = pkgs.writeShellScript "losos-bios-esp-generator" ''
    [ -d /sys/firmware/efi ] && exit 0
    PATH=${lib.makeBinPath [ pkgs.util-linux ]}
    root=$(findmnt --noheadings --output SOURCE --nofsroot /) || exit 0
    disk=$(lsblk --noheadings --nodeps --output PKNAME "$root") || exit 0
    [ -n "$disk" ] || exit 0
    esp=$(lsblk --noheadings --list --output PARTTYPE,PARTUUID "/dev/$disk" \
      | while read -r type uuid; do
          if [ "$type" = c12a7328-f81f-11d2-ba4b-00a0c93ec93b ]; then
            echo "$uuid"
            break
          fi
        done)
    [ -n "$esp" ] || exit 0
    cat > "$1/efi.mount" <<EOF
    # Written by losos-bios-esp-generator (bios.nix).
    [Unit]
    Description=EFI System Partition
    Documentation=man:systemd-gpt-auto-generator(8)
    Before=local-fs.target

    [Mount]
    What=/dev/disk/by-partuuid/$esp
    Where=/efi
    Type=vfat
    Options=umask=0077,noexec,nosuid,nodev
    EOF
    mkdir -p "$1/local-fs.target.wants"
    ln -sf ../efi.mount "$1/local-fs.target.wants/efi.mount"
  '';
in
lib.mkIf (arch.name == "x86_64") {
  # Where Limine's BIOS stage goes: 1M, more than the stage needs. Before
  # slot A's /usr, which grows into the
  # space behind it on first boot (disk.nix). disk.nix has no definition of
  # its own for it, and repart leaves alone a partition no definition
  # matches.
  image.repart.partitions."05-bios-boot".repartConfig = {
    Type = "21686148-6449-6e6f-744e-656564454649";
    Label = "BIOS boot";
    SizeMinBytes = "1M";
    SizeMaxBytes = "1M";
  };

  systemd.generators.losos-bios-esp = espGenerator;
}
