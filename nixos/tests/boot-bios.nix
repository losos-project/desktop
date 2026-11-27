# Boot the real image on a legacy BIOS: SeaBIOS, Limine from the MBR, the
# UKI's kernel and initrd (modules/bios.nix). boot.nix checks what the system
# does once it is up, which is the same under either firmware; this checks
# what is particular to a BIOS boot, and that Limine's boot counting ends
# where systemd-boot's would.
{ self }:

{ lib, ... }:

let
  image = import ./image.nix { inherit self; };
in
{
  name = "losos-desktop-boot-bios";

  # The module adds its own overlay, which the test framework's shared,
  # read-only package set would refuse.
  node.pkgsReadOnly = false;

  # Merged beside the shared node rather than importing it, so the system
  # is boot.nix's to the byte and the two tests share one disk image: a
  # third image is more than CI's runner has room for.
  nodes.machine = lib.mkMerge [
    image.node
    {
      # Started by the BIOS from the image's own disk, with no EFI firmware.
      virtualisation.useEFIBoot = false;
      # A second disk, made to look like another LosOS disk's root below.
      virtualisation.emptyDiskImages = [ 256 ];
    }
  ];

  testScript =
    { nodes, ... }:
    image.disk nodes.machine
    + ''

      machine.wait_for_unit("multi-user.target")

      with subtest("Limine started the system, and first boot found its disk"):
          machine.fail("test -d /sys/firmware/efi")
          # Limine named the ESP it started the kernel from, and the initrd
          # mounted root from that ESP's disk (bios.nix).
          machine.succeed("grep -q 'root=/dev/disk/by-loader/root' /proc/cmdline")
          machine.succeed("grep -q 'losos.esp=' /proc/cmdline")
          machine.succeed('test "$(findmnt --noheadings --output SOURCE /)" = "$(readlink -f /dev/disk/by-loader/root)"')
          machine.succeed("grep -q 'usrhash=' /proc/cmdline")
          machine.succeed("findmnt --kernel --types ext4 /")
          machine.succeed("findmnt --kernel /home")
          assert "ACTIVE" in machine.succeed("dmsetup info --target verity usr")

      with subtest("The ESP is mounted without EFI to name it"):
          esp = machine.succeed("bootctl --print-esp-path").strip()
          machine.succeed(f"test -f {esp}/EFI/Linux/losos-desktop_1.efi")
          machine.succeed(f"test -f {esp}/EFI/Linux/losos-desktop_1.linux")
          machine.succeed(f"test -f {esp}/EFI/Linux/losos-desktop_1.cmdline")
          machine.succeed(f"test -f {esp}/limine.conf")
          machine.succeed(f"grep -qx 'serial: yes' {esp}/limine.conf")
          machine.succeed(
              "grep -q 'path: boot():/EFI/Linux/losos-desktop_1.linux' "
              f"{esp}/limine.conf"
          )
          machine.fail(f"test -e {esp}/EFI/losos/grubenv")
          # The bless runs after boot-complete.target, which comes after
          # multi-user.target, so it can still be queued here; waiting
          # still fails if its job was dropped or it failed.
          machine.wait_for_unit("losos-limine-bless.service")

      with subtest("An update rewrites Limine's BIOS boot code"):
          # Without the record of what it installed, losos-limine-update
          # writes the boot code again, so a BIOS boot partition emptied
          # here is whole again for the reboot below.
          machine.wait_for_unit("losos-limine-update.service")
          machine.succeed(f"test -f {esp}/EFI/losos/installed")
          bios = machine.succeed(
              "lsblk --raw --noheadings --output PATH,PARTTYPE /dev/vda"
              " | awk '$2 == \"21686148-6449-6e6f-744e-656564454649\" { print $1 }'"
          ).strip()
          machine.succeed(f"dd if=/dev/zero of={bios} bs=1M count=1 conv=fsync")
          machine.succeed(f"rm {esp}/EFI/losos/installed")
          machine.succeed("systemctl restart losos-limine-update.service")
          machine.fail(f"cmp --bytes=512 {bios} /dev/zero")

      with subtest("Another disk's root partition is not mounted in its place"):
          machine.succeed(
              "printf 'label: gpt\\nsize=64M, type=4f68bce3-e8cd-4db1-96e7-fbcaf984b709, name=root-x86-64\\n'"
              " | sfdisk /dev/vdb"
          )
          machine.succeed("udevadm settle")
          machine.succeed("mkfs.ext4 -q /dev/vdb1")
          machine.succeed("mount /dev/vdb1 /mnt && touch /mnt/other-disk && umount /mnt")

      with subtest("A counted UKI is blessed once it boots"):
          # The name sysupdate gives a new UKI (update.nix).
          machine.succeed(
              f"mv {esp}/EFI/Linux/losos-desktop_1.efi '{esp}/EFI/Linux/losos-desktop_1+3-0.efi'"
          )
          machine.shutdown()
          machine.start()
          machine.wait_for_unit("multi-user.target")
          machine.wait_for_unit("losos-limine-bless.service")
          machine.succeed(f"test -f {esp}/EFI/Linux/losos-desktop_1.efi")
          machine.fail(f"ls {esp}/EFI/Linux/ | grep -q '+'")
          machine.fail(f"test -e {esp}/EFI/losos/grubenv")
          # Both disks now have a root partition by that label.
          machine.succeed("findmnt --noheadings --output SOURCE / | grep -q '^/dev/vda'")
          machine.fail("test -e /other-disk")
    '';
}
