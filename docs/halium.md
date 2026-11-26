# Halium GSI

`nixos/halium/` builds one generic system image (GSI) for every Treble phone
and tablet that takes generic system images and runs Linux 5.10 or newer. It
runs through [Halium](https://halium.org), the Android hardware layer that
Ubuntu Touch and Droidian build on. The [web flasher](web-flasher.md) installs
it.

`nixosModules.gsi` imports `nixos/modules/base.nix`, which holds everything a
PC and a phone share: the derisk desktop, homed accounts, NetworkManager, pm,
no Nix on the device, the `/etc` overlay and sysusers. In place of the PC's
UEFI, verity `/usr`, sysupdate and installer, it adds its own boot (`boot.nix`)
and Android layer (`android.nix`).

There are no device ports. The target used to be a configuration each port
filled in with its kernel, device tree, `mkbootimg` offsets, partition labels
and udev rules. Two things Android made generic replaced all of that:

- Treble. A device's vendor HALs run under any newer Android system image, so
  Halium's generic Android 14 image starts them on every device. It is pinned
  in `nixos/pkgs/halium-gsi.nix` and lives in the store, so it updates with the
  OS.
- The ramdisk is unpacked last. A device that launched with Android 13 or later
  boots its maker's kernel from `boot`, its first drivers from `vendor_boot`,
  and the generic ramdisk from `init_boot`, and the flasher replaces only that
  ramdisk. An older device keeps kernel and ramdisk together in `boot`, so the
  flasher writes that boot image back with this ramdisk after the device's own,
  and the kernel unpacks both. Either way the kernel, modules and device tree
  stay the device's, and nothing is built per device.

## How a device boots it

![The phone's partitions beside its start-up: LosOS writes init_boot, vbmeta and userdata and keeps boot, vendor_boot and super; the bootloader, the initrd, losos-gsi-super, losos-gsi-modules, Halium's system image, Android's init in LXC, and then derisk](images/halium.svg)

1. The bootloader loads the device's kernel and unpacks the device's ramdisk,
   from `vendor_boot`, or from `boot` without `init_boot`. Then it unpacks
   NixOS's systemd initrd over it.
2. In the initrd, `losos-gsi-first-stage-modules` loads the modules listed in
   the device ramdisk's `/lib/modules/modules.load`: the storage controller and
   what it hangs off. Without them there is no `userdata`.
3. All of `userdata` is the root filesystem, mounted by partition label and
   grown on first boot by `x-systemd.growfs`. The command line comes from the
   device's own boot images, so there is no `init=`. The initrd boots
   `/nix/var/nix/profiles/system` from the flashed filesystem.
4. After switch-root, `losos-gsi-super` reads `super`'s logical partition
   metadata for the booted slot with `lpdump` and maps `vendor`, `odm`,
   `vendor_dlkm` and `system_dlkm` with device-mapper, without the slot suffix.
   They mount read-only at `/vendor`, `/odm`, `/vendor_dlkm` and `/system_dlkm`.
5. `losos-gsi-modules` points the firmware loader at `/vendor/firmware` and
   loads the generic kernel's modules from `system_dlkm`, then the vendor's.
   These partitions have no `modules.dep.bin`, so `load-modules.sh` runs
   `insmod` in passes until a pass loads nothing new, applying
   `modules.options` and `modules.blocklist`. A module that never loads is
   logged and skipped.
6. Halium's system image is loop-mounted read-only from the store at
   `/android/system`, and `/android/system/system` at `/system`, where
   libhybris's linker looks.
7. `losos-android.service` starts Android's init with `lxc-start`, before the
   display manager. The container shares the host's `/dev` and network, gets
   the vendor partitions bind-mounted, and gets `/var/lib/android/data` as
   `/data`. It uses LXC because `systemd-nspawn` always gives a container a
   private `/dev`, and the host must open the same HAL device nodes ueventd
   creates.
8. libhybris is a GLVND EGL vendor in `hardware.graphics`, next to Mesa.

The system carries no kernel (`boot.kernel.enable = false`), because the
device's kernel boots. Every device in scope runs 5.10 or newer, systemd's
minimum. The flasher reads the kernel version from any boot image it repacks
and refuses older ones.

Every Android-side mount is `nofail`, so a device whose Android half is missing
or broken still reaches the login screen.

> [!WARNING]
> The container is not a security boundary. Android's init and the vendor HALs
> run as host root with the whole host `/dev`, every block device included, so
> the vendor partition's closed blobs are trusted as much as the kernel, and
> nothing here plays the part of the PC's verity `/usr`. The container drops
> `sys_module`, `sys_rawio`, `sys_time` and the MAC capabilities. A `/dev` with
> only the HAL nodes would narrow it further, but those nodes differ per
> device, so that waits for a real device.

## What it builds

`nix build .#packages.aarch64-linux.gsi` produces the three files the flasher
writes, with `SHA256SUMS`:

| File | What it is |
| --- | --- |
| `losos-desktop_<version>_gsi-init_boot.img` | The initrd as an Android boot image: header v4, a ramdisk and nothing else, LZ4-compressed in the legacy format the generic kernel reads |
| `losos-desktop_<version>_gsi-vbmeta.img` | An empty vbmeta with verification off, so a bootloader that would check `init_boot` against the stock vbmeta boots this one |
| `losos-desktop_<version>_gsi-userdata.simg.gz` | The root filesystem as an Android sparse image, gzipped because browsers decompress gzip natively |

The aarch64 release carries the same files under its `SHA256SUMS` and
signature, and the flasher downloads them from there. Without the flasher, on a
device with `init_boot`:

```sh
fastboot flash vbmeta losos-desktop_<version>_gsi-vbmeta.img
fastboot flash init_boot losos-desktop_<version>_gsi-init_boot.img
gunzip losos-desktop_<version>_gsi-userdata.simg.gz
fastboot flash userdata losos-desktop_<version>_gsi-userdata.simg
fastboot reboot
```

## Which devices

A device is in scope when its bootloader can be unlocked, it takes GSIs
(Treble, on every device that launched with Android 9 or later), and its kernel
is Linux 5.10 or newer, which in practice means it launched with Android 12 or
later.

- A device with `init_boot` (launched with Android 13 or later) gets only
  `init_boot` written.
- A device without it (launched with Android 12) gets `boot` written: its own
  boot image, read back in fastbootd or taken from its factory image, with this
  ramdisk appended. The flasher refuses kernels older than 5.10, as on devices
  that launched with Android 11 or earlier.

Every such device has `super`, because devices launched with Android 10 or
later must have dynamic partitions, so `losos-gsi-super` has no fallback for
physical vendor partitions. A device without `vendor_dlkm` or `system_dlkm`, as
on Android 12, skips those mounts.

> [!NOTE]
> A device with a 4.x or 5.4 kernel can't run this image: systemd doesn't run
> on it, and a generic image can't bring a newer kernel.

`nixos/pkgs/` builds `libhybris` from upstream master, and Halium's
`android-headers`, where one tree serves Halium 11 to 16.

## GTK and Qt apps on a phone

GTK and Qt apps are written for a desktop: dialogs open at desktop size, the
GTK3 file chooser puts a sidebar next to its file list, and a finger on a Qt
list selects instead of scrolling. On a phone, windows run off the screen.

The overlay doesn't patch `gtk3`, `gtk4` or Qt, so they and everything built
against them substitute from cache.nixos.org. A patched toolkit used to drop
about thirty derivations per architecture out of that cache for CI to compile.
Phone-sized layout is whatever the upstream toolkit does.

![A GTK4 dialog on a phone-sized screen, its search field focused and derisk's on-screen keyboard open beneath it](images/gtk-phone-keyboard.png)

The on-screen keyboard is derisk's. GTK3, GTK4 and Qt 6 speak Wayland's
`text-input-unstable-v3` unpatched, so the keyboard follows text focus once
derisk's compositor offers that protocol. Flatpak apps use their runtime's GTK
and Qt.
