# LosOS Desktop as a generic system image (GSI) for Android phones and
# tablets, through Halium.
#
# Halium is the Android hardware layer, cut down to what a GNU/Linux system
# needs from it: the device's own kernel, its vendor partition of
# bionic-linked HAL libraries and firmware, and a minimal Android system
# image whose init starts those HALs. Everything above the kernel that a
# person sees is still this OS: base.nix's desktop, accounts, network and
# pm, the same as on a PC.
#
# There is one image for every device, and no device ports. That rests on
# two things Android itself made generic:
#
#   - Treble: a device's vendor HALs run under any system image of a newer
#     Android, so Halium's generic system image (pkgs.halium-gsi) starts
#     them on every device.
#   - The ramdisk is the device's last unpacked: a device that launched
#     with Android 13 or later boots its maker's kernel from `boot`, its
#     drivers' first modules from `vendor_boot`, and the generic ramdisk
#     from a partition of its own, `init_boot`, which this image replaces.
#     An older device keeps kernel and ramdisk together in `boot`; the
#     flasher writes its own boot image back with this ramdisk after the
#     device's (docs/web-flasher.md). Either way the kernel, its modules and
#     its device tree stay as they were, so nothing here is built per
#     device.
#
#   bootloader -> the device's kernel + vendor_boot's or boot's own ramdisk
#     + this ramdisk (NixOS's systemd initrd), concatenated
#     -> the initrd loads vendor_boot's storage modules and mounts userdata,
#        which is this OS's root filesystem
#     -> switch-root -> systemd maps `super`, mounts the vendor partitions,
#        loads their modules and starts Android's init in an LXC container
#        (android.nix) beside the desktop
#
# A device in scope runs kernel 5.10 or later, systemd's minimum baseline
# (the flasher refuses an older one), and the kernel nixpkgs would build is
# never booted, so it is left out.
#
# There is no UEFI, no verity /usr and no sysupdate: the bootloader loads
# what the flasher (docs/web-flasher.md) wrote and nothing else. docs/halium.md
# says how it boots and what is not done.
{
  imports = [
    ../modules/base.nix
    # Flatpak on the host: a phone's kernel has no KVM for the machine a PC
    # runs it in (modules/flatpak-machine.nix).
    ../modules/flatpak.nix
    # hardened_malloc for a 39-bit address space, tried by the initrd before
    # it is preloaded, since the kernel is the phone's.
    ../modules/allocator.nix
    ./boot.nix
    ./android.nix
  ];

  losos.allocator.deviceKernel = true;
}
