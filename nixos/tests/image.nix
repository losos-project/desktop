# What boot.nix and boot-bios.nix share: a node that boots
# the real image as firmware would, from its own disk, and the start of a
# test script that gives it that disk.
{ self }:

rec {
  node =
    { lib, pkgs, ... }:
    {
      imports = [ self.nixosModules.default ];

      losos.version = "1";

      # The layout a release signed with LosOS's key has, shim and
      # MokManager in front of GRUB, signed with a key made for the tests.
      # The firmware here has Secure Boot off, so shim starts GRUB without
      # checking it; secure-boot.nix boots the same image with it on.
      losos.secureBoot =
        let
          key = import ./secure-boot-key.nix { inherit pkgs; };
        in
        {
          certificate = "${key}/cert.pem";
          privateKey = "${key}/key.pem";
        };

      # The boot tests build three disks from the image, and the guest
      # template in each was what ran CI's runner out of space; booting the
      # image does not use it, and tests/machines.nix tests it on its own.
      losos.machines.templates = false;

      # First-boot setup waits on the seat for a person, which a test has
      # none of. The user is created by the test script instead.
      services.homed.promptOnFirstBoot = lib.mkForce false;
      systemd.services.derisk-setup.enable = false;

      # The image's own disk, instead of the test driver's kernel with the
      # host's store mounted in.
      virtualisation = {
        directBoot.enable = false;
        mountHostNixStore = false;
        useEFIBoot = lib.mkDefault true;
        fileSystems = lib.mkVMOverride { };
        memorySize = lib.mkDefault 3072;
      };

      # The VM's first-boot disk is /dev/vda. Repart cannot infer it from
      # /sysroot before the root partition exists.
      boot.initrd.systemd.repart.device = "/dev/vda";
      boot.initrd.systemd.services.systemd-repart.after = lib.mkForce [ "dev-vda.device" ];

      # The VM module empties swapDevices with the same override it uses on
      # fileSystems. Put back what disk.nix gives the real image, so the test
      # boots the same fstab and zswap has the backing device it asserts on.
      swapDevices = lib.mkVMOverride [
        {
          device = "/dev/mapper/swap";
          options = [ "nofail" ];
        }
      ];
    };

  # A copy-on-write disk backed by the image, grown so first boot has room
  # for slot B, root, /home and swap.
  disk = diskOfSize "32G";
  diskOfSize = size: machine: ''
    import os
    import subprocess
    import tempfile

    tmp = tempfile.NamedTemporaryFile()
    subprocess.run([
      "${machine.virtualisation.qemu.package}/bin/qemu-img", "create",
      "-f", "qcow2", "-F", "raw",
      "-b", "${machine.system.build.disk}/${machine.image.filePath}",
      tmp.name, "${size}",
    ], check=True)
    os.environ["NIX_DISK_IMAGE"] = tmp.name
  '';
}
