# The gaming machine (docs/gaming-machine.md): a Windows machine given a
# graphics card of its own, so a game runs on the card at full speed while
# the desktop draws on the other card, or on the processor where there is
# none (mcsapi's compositor leaves a card on vfio-pci alone and falls back
# to llvmpipe).
#
# A card goes at boot, not when the machine starts: losos-gaming-bind puts
# it on vfio-pci before udev's coldplug, so no host driver, and no program
# that opens every GPU it finds, ever holds it, and taking it from them
# later is the step that leaves cards in a state only a reboot clears.
# Which card goes is the person's choice, from Settings or
# `pm system start losos-gaming-reserve@<address>`, and takes effect at
# the next boot. Handing a PC's only card over and back while the desktop
# waits is not done yet (docs/not-done.md).
#
# Windows and the games this is for are x86-64 only.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  gaming = lib.getExe pkgs.losos-gaming;
  # Secure Boot firmware with Microsoft's keys enrolled in its variables'
  # template, which Windows 11 and several games' anti-cheat insist on.
  # QEMU's own firmware has Secure Boot but no keys, so Windows' boot loader
  # would be refused.
  ovmf = pkgs.OVMFFull.fd;
in
lib.mkIf pkgs.stdenv.hostPlatform.isx86_64 {
  environment.systemPackages = [
    pkgs.losos-gaming
    # The window the machine's picture is shown in. Its half in Windows,
    # Looking Glass's host, must be the same version (docs/gaming-machine.md).
    pkgs.looking-glass-client
  ];

  # Where src/losos-gaming's domain.rs points libvirt.
  environment.etc."losos-gaming/OVMF_CODE.fd".source = "${ovmf}/FV/OVMF_CODE.fd";
  environment.etc."losos-gaming/OVMF_VARS.ms.fd".source = "${ovmf}/FV/OVMF_VARS.ms.fd";

  # Early, like losos-hardware: after the initrd mounted the root, where the
  # choice is kept, and before udev's coldplug or systemd-modules-load could
  # load a driver for the card. A USB-C controller on the card that the
  # initrd's xhci already took is unbound instead. After the peripherals'
  # keep list is written, which says what must never leave the host.
  systemd.services.losos-gaming-bind = {
    description = "Move the gaming machine's graphics card to vfio-pci";
    wantedBy = [ "sysinit.target" ];
    after = [
      "systemd-remount-fs.service"
      "losos-peripherals-keep.service"
    ];
    before = [
      "sysinit.target"
      "systemd-modules-load.service"
      "systemd-udev-trigger.service"
      "losos-hardware.service"
      "shutdown.target"
    ];
    conflicts = [ "shutdown.target" ];
    unitConfig = {
      DefaultDependencies = false;
      ConditionPathExists = "/var/lib/losos-gaming/gpu";
    };
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStartPre = "${pkgs.kmod}/bin/modprobe vfio-pci";
      ExecStart = "${gaming} bind";
    };
  };

  # `pm system start losos-gaming-reserve@0000:01:00.0` (or @none), which
  # is what Settings runs: pm's polkit grant covers starting units, and
  # this one only records the choice.
  systemd.services."losos-gaming-reserve@" = {
    description = "Give the gaming machine the graphics card %I from the next boot";
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${gaming} reserve %I";
      StateDirectory = "losos-gaming";
    };
  };

  # Looking Glass's frames, shared between QEMU, which runs as
  # qemu-libvirtd, and the client in the session of whoever runs the
  # machine: an administrator, the only people libvirt lets manage
  # machines (libvirt.nix), so wheel's. Made at boot, before libvirt would
  # make it its own user's alone.
  systemd.tmpfiles.rules = [
    "f /dev/shm/looking-glass 0660 qemu-libvirtd wheel -"
  ];
}
