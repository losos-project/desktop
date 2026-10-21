# Halium GSI

`nixos/halium/` is the phone and tablet target. It builds one generic system
image, a GSI, for every Treble device that takes generic system images and
runs Linux 5.10 or newer. It runs through [Halium](https://halium.org), the
Android hardware layer that ports such as Ubuntu Touch and Droidian build on.
`nixosModules.gsi` imports `nixos/modules/base.nix`, which holds everything a
PC and a phone share: the derisk desktop, homed accounts, NetworkManager, pm,
no Nix on the device, the `/etc` overlay and sysusers. It then adds its own
boot, `boot.nix`, and Android layer, `android.nix`, in place of the PC's UEFI,
verity `/usr`, sysupdate and installer. The [web flasher](web-flasher.md)
installs it.

There are no device ports. The target used to be a configuration that a port
filled in with its kernel, device tree, `mkbootimg` offsets, partition labels
and udev rules. All of that is gone, because two things Android made generic
cover it:

- **Treble.** A device's vendor HALs run under any system image of a newer
  Android, so Halium's generic Android 14 system image starts them on every
  device. It is pinned in `nixos/pkgs/halium-gsi.nix` and lives in the store,
  so it updates with the OS.
- **The ramdisk is unpacked last.** A device that launched with Android 13
  or later boots its maker's kernel from `boot`, its first drivers from
  `vendor_boot`, and the generic ramdisk from `init_boot`, and the flasher
  replaces only that ramdisk. An older device keeps kernel and ramdisk
  together in `boot`. There the flasher reads that boot image and writes it
  back with this ramdisk after the device's own, and the kernel unpacks both
  in turn. Either way the kernel, its modules and its device tree stay the
  device's own, and nothing here is built per device.

## How a device boots it

![The phone's partitions beside its start-up: LosOS writes init_boot, vbmeta and userdata and keeps boot, vendor_boot and super; the bootloader, the initrd, losos-gsi-super, losos-gsi-modules, Halium's system image, Android's init in LXC, and then derisk](images/halium.svg)

1. The bootloader loads the device's kernel and unpacks the device's own
   ramdisk, from `vendor_boot`, or from `boot` on a device without
   `init_boot`. It then unpacks this one, NixOS's systemd initrd, over it.
2. In the initrd, `losos-gsi-first-stage-modules` loads the modules the
   device's ramdisk lists in `/lib/modules/modules.load`, which are the
   storage controller and what it hangs off. Without them there is no
   `userdata`.
3. All of `userdata` is the root filesystem. It is mounted by partition label,
   and `x-systemd.growfs` grows it to fill the partition on the first boot.
   The command line comes from the device's own boot images, which keep
   theirs, so there is no `init=`. The initrd boots
   `/nix/var/nix/profiles/system`, which the flashed filesystem carries.
4. After switch-root, `losos-gsi-super` reads `super`'s logical partition
   metadata for the booted slot with `lpdump` and maps `vendor`, `odm`,
   `vendor_dlkm` and `system_dlkm` with device-mapper under their names
   without the slot suffix. They mount read-only at `/vendor`, `/odm`,
   `/vendor_dlkm` and `/system_dlkm`.
5. `losos-gsi-modules` points the kernel's firmware loader at
   `/vendor/firmware` and loads the generic kernel's modules from
   `system_dlkm`, then the vendor's. None of these partitions carries a
   `modules.dep.bin`, so the loader, `load-modules.sh`, runs `insmod` in
   passes until a pass loads nothing new, applying `modules.options` and
   `modules.blocklist`. A module that never loads is logged and skipped.
6. Halium's system image is loop-mounted read-only from the store at
   `/android/system`, and `/android/system/system` at `/system`, where
   libhybris's linker looks.
7. `losos-android.service` starts Android's init with `lxc-start`. The
   container shares the host's `/dev` and network, gets the vendor partitions
   bind-mounted, and gets `/var/lib/android/data` as `/data`. It uses LXC
   instead of `systemd-nspawn` because nspawn always gives a container a
   private `/dev`, and the host has to open the same HAL device nodes that
   ueventd creates. It starts before the display manager.
8. libhybris is a GLVND EGL vendor in `hardware.graphics`, beside Mesa.

The system carries no kernel, with `boot.kernel.enable = false`. The kernel
that boots is the device's, so nixpkgs' would be dead weight in `rootfs`.
Every device in scope runs 5.10 or newer, which is systemd's minimum
baseline. The flasher reads the kernel's version out of any boot image it
repacks and refuses an older one.

Every mount on the Android side is `nofail`, so a device whose Android half is
missing or broken still reaches the login screen.

> [!WARNING]
> The container is not a security boundary. Android's init and the vendor HALs
> run as host root with the host's whole `/dev`, every block device included,
> so the vendor partition's closed blobs are trusted as much as the kernel, and
> nothing here plays the part of the PC build's verity `/usr`. It drops
> `sys_module`, `sys_rawio`, `sys_time` and the MAC capabilities; a `/dev`
> holding only the HAL nodes would narrow it further, but which nodes those are
> differs per device, so it waits for a device to find out.

## What it builds

`nix build .#packages.aarch64-linux.gsi` produces the three files the
flasher writes, with `SHA256SUMS`:

- `losos-desktop_<version>_gsi-init_boot.img` is the initrd as an Android
  boot image. It has header v4, a ramdisk and nothing else, compressed with
  LZ4 in the legacy format the generic kernel decompresses.
- `losos-desktop_<version>_gsi-vbmeta.img` is an empty vbmeta with
  verification turned off, so a bootloader that would check `init_boot`
  against the stock vbmeta boots this one.
- `losos-desktop_<version>_gsi-userdata.simg.gz` is the root filesystem as an
  Android sparse image. It is gzipped because a browser can decompress gzip
  natively.

The aarch64 release carries the same three files under its `SHA256SUMS` and
signature, which is where the flasher downloads them from.

Without the flasher, the same three go on with fastboot on a device with
`init_boot`:

```sh
fastboot flash vbmeta losos-desktop_<version>_gsi-vbmeta.img
fastboot flash init_boot losos-desktop_<version>_gsi-init_boot.img
gunzip losos-desktop_<version>_gsi-userdata.simg.gz
fastboot flash userdata losos-desktop_<version>_gsi-userdata.simg
fastboot reboot
```

## Which devices

A device is in scope when three things hold. Its bootloader can be unlocked.
It takes GSIs, which means Treble, and every device that launched with
Android 9 or later has it. And its kernel is Linux 5.10 or newer, which in
practice means it launched with Android 12 or later.

- On a device with `init_boot`, which launched with Android 13 or later,
  only `init_boot` is written.
- On a device without it, which launched with Android 12, `boot` is written.
  That is the device's own boot image, read back from the device in
  fastbootd or chosen from its factory image, with this ramdisk appended to
  its own. The flasher checks the kernel's version there and refuses a
  kernel older than 5.10, as on devices that launched with Android 11 or
  earlier.

Every such device has `super`, since devices that launched with Android 10
or later must have dynamic partitions. So `losos-gsi-super` has no fallback
for physical vendor partitions. A device without `vendor_dlkm` or
`system_dlkm`, as on Android 12, skips those mounts.

> [!NOTE]
> A device this image cannot carry is one with a 4.x or 5.4 kernel: systemd
> does not run on it, and a generic image cannot bring a newer kernel.

`nixos/pkgs/` builds `libhybris` from its upstream master, and Halium's
`android-headers`, where one tree serves Halium 11 to 16.
