# What every service machine has in common (modules/service-machines.nix):
# a machine the OS runs a part of itself in, such as schizo mode's Tor
# gateway (tor-gateway.nix), rather than one a person makes and uses.
#
# None has a disk image. The host boots it from its own store: the kernel
# and initrd by path, and the store itself shared read-only over 9p, the way
# NixOS's test VMs boot. So a service machine is part of the image, covered
# by /usr's verity hash like the rest of the OS, costs /usr only what it
# does not share with the host, and an update brings a new one the next
# time it starts. Its root is a tmpfs; what it keeps is on its state disk,
# /dev/vda, when it asks for one, which the host makes empty and the
# machine formats the first time.
#
# Nobody signs in to one: it has no password, no guest agent and no screen.
# Its console is the serial port `virsh console` shows, for its journal.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

{
  imports = [
    "${modulesPath}/profiles/minimal.nix"
    # No perl in its closure, as on the host (base.nix): the /etc overlay
    # instead of the activation script that needs it.
    "${modulesPath}/profiles/perlless.nix"
  ];

  options.losos.service = {
    name = lib.mkOption {
      type = lib.types.str;
      description = "The machine's name, its attribute in losos.serviceMachines.";
    };
    state = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/var/lib/tor";
      description = ''
        Where the state disk is mounted, when the host gives the machine one
        (losos.serviceMachines.<name>.stateSize).
      '';
    };
  };

  config = {
    system.stateVersion = "26.05";
    networking.hostName = config.losos.service.name;
    nix.enable = false;
    system.switch.enable = false;
    # sysusers, as on the host, rather than perlless.nix's userborn.
    services.userborn.enable = false;
    systemd.sysusers.enable = true;
    documentation.enable = false;

    # Booted by its kernel and initrd: no boot loader, no ESP.
    boot.loader.grub.enable = false;
    boot.initrd.availableKernelModules = [
      "virtio_pci"
      "virtio_blk"
      "virtio_net"
      "9p"
      "9pnet_virtio"
    ];
    boot.kernelParams = [
      (if pkgs.stdenv.hostPlatform.isAarch64 then "console=ttyAMA0" else "console=ttyS0")
    ];

    fileSystems = {
      "/" = {
        device = "none";
        fsType = "tmpfs";
        options = [
          "mode=0755"
          "size=64M"
        ];
      };
      # The host's store, which service-machines.nix shares as `nix-store`.
      # Read-only here as well as there: on a LosOS host it is the verity
      # /usr anyway.
      "/nix/store" = {
        device = "nix-store";
        fsType = "9p";
        neededForBoot = true;
        options = [
          "ro"
          "trans=virtio"
          "version=9p2000.L"
          "msize=1048576"
          "cache=loose"
        ];
      };
    }
    // lib.optionalAttrs (config.losos.service.state != null) {
      ${config.losos.service.state} = {
        device = "/dev/vda";
        fsType = "ext4";
        autoFormat = true;
      };
    };

    # A clock from the host, through kvm-clock and the RTC.
    services.timesyncd.enable = lib.mkDefault false;
    services.nscd.enable = false;
    system.nssModules = lib.mkForce [ ];
  };
}
