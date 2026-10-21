# The Windows installer

`losos-desktop_<v>_x86_64-windows-installer.exe` installs LosOS beside
Windows, on the same disk, without a USB stick. It is a console program in
`src/losos-windows-installer`. `nixos/modules/windows-installer.nix` builds it
from the OS's own configuration with nixpkgs' mingw-w64 cross toolchain, and
every [nightly](releases.md) carries it beside the installer ISO. It does the
job of the [ISO's installer](installer.md) on a machine that keeps Windows. It
writes what an update would, from the same channel, checked against the same
key.

## What it does

It runs as administrator, and asks for elevation itself if it was started
without it. Then it:

1. Checks the machine. It refuses a Windows started in legacy BIOS mode, a
   machine of another architecture, Secure Boot turned on, and a drive that
   BitLocker is still encrypting or decrypting, as explained
   [below](#secure-boot-bitlocker-fast-startup).
2. Finds the newest release on the channel compiled into it, which is the one
   the OS updates from. That is a proxy's `losos.update.baseUrl`, or, for an
   OS that updates from GitHub Releases, the channel's index on the
   documentation site, as
   [Releases](releases.md#updating-from-github-releases) explains. From the
   index it downloads split files part by part and joins them. It checks
   `SHA256SUMS.gpg` against the key in `nixos/keys/update-signing.asc`.
   `--release DIR` takes a release from a folder instead, such as a
   `LOSOS-RELEASE` disk.
3. Asks how much space LosOS gets, 64 GB unless the user types another
   number. The minimum is what first boot needs: the boot partition, two
   `/usr` slots, root's and `/home`'s minimums from `disk.nix`, and swap the
   size of RAM. The maximum leaves Windows its used space plus 10 GB, and
   never goes past what `Get-PartitionSupportedSize` says C: can shrink to.
4. Shows what it will do and waits for the word `install`. Nothing changes
   before that, and `--dry-run` stops there.
5. Downloads the UKI, `/usr` and its hash tree into
   `%ProgramData%\LosOS\Downloads` with Windows' own `curl.exe`, which
   resumes an interrupted download when the program is run again. Each file
   is checked against its line of `SHA256SUMS`.
6. Turns fast startup off, suspends BitLocker, and shrinks C: with
   `Resize-Partition`.
7. Adds three partitions in the space. The first is a 512 MB FAT32
   XBOOTLDR partition, "LosOS boot", holding the UKI. Then come slot A's
   verity and `/usr` partitions, written straight from the `.raw.xz` files.
   They are named, typed, flagged read-only and given the UUIDs the
   release's file names carry, exactly as sysupdate would write them,
   because that is how the initrd finds `/usr` from `usrhash=`.
8. Copies GRUB to `\EFI\losos\` on Windows' EFI system partition, with an
   empty `grubenv` beside it, and adds a firmware boot entry for it at the
   front of the boot order. The entry is a copy of Windows Boot Manager's
   own with the file swapped, so it points at the same disk in the same way.
   Some Windows updates and some firmware put Windows Boot Manager first
   again. Every LosOS start that reaches `boot-complete.target` moves the
   LosOS entry back to the front, through `losos-boot-order` in `grub.nix`,
   and GRUB's menu still offers Windows.
9. Offers to restart.

If any step after the shrink fails, it removes what it added and gives C:
its space back.

At the next start, GRUB offers LosOS from the XBOOTLDR partition, and Windows
Boot Manager, which it [finds on the ESP](boot-loaders.md#grubs-menu) by
itself. LosOS's first boot then does what it does after `dd`. `disk.nix`
creates slot B, root, `/home` and swap in the rest of the space, and
first-boot setup asks for an account. Windows' partitions are never touched
again.

`--uninstall` undoes the install. It deletes the partitions behind C: whose
types are LosOS's, the files on the ESP it recorded in
`\EFI\losos\losos-windows-installer.txt`, and the boot entry, then grows C:
back into the space.

## The layout

```
ESP                  Windows' own, ~100M: Windows Boot Manager, GRUB
MSR                  Windows' own
C:                   shrunk
LosOS boot           XBOOTLDR, 512M: the UKIs              installer
usr-verity A                                               installer
usr A                                                      installer
usr-verity B                                               first boot, _empty
usr B                                                      first boot, _empty
root, home, swap                                           first boot
Recovery             Windows' own, left where it was
```

![The disk before, after the installer and after the first boot: C: shrinks; the installer adds LosOS boot, verity A and usr A and puts GRUB on Windows' ESP; the first boot fills the rest with slot B, root, home and swap; Recovery stays where it was](images/windows-installer.svg)

Windows makes its ESP 100 MB, with its own partitions right behind it, so
the ESP cannot grow and has no room for two UKIs of about 45 MB each. So the
UKIs go on an XBOOTLDR partition, which GRUB reads beside the ESP.
`update.nix` writes them to `$BOOT`, which is that partition where there is
one and the ESP otherwise. `disk.nix` accepts whatever ESP the disk has and
no longer asks for 512 MB of it. With that minimum, repart could not grow
Windows' ESP and refused the whole disk on first boot, so the system never
found its root.

> [!IMPORTANT]
> A release built before that change cannot be installed beside Windows.

## Secure Boot, BitLocker, fast startup

- **Secure Boot** must be off. Neither GRUB nor the UKI is
  [signed](not-done.md), so firmware with Secure Boot on would refuse to start
  them. A shim signed by Microsoft would need a key this project does not
  have. The installer says so and stops before changing anything.
- **BitLocker** seals its key to the boot chain the TPM measured, and adding
  a boot manager in front of Windows' changes that chain. So the installer
  suspends protection on C: with `DisableKeyProtectors` until Windows has
  started twice. That lets BitLocker reseal itself to the new chain the first
  time Windows comes up through GRUB. The installer still tells the user to
  keep the recovery key at hand. It refuses a drive that is encrypting or
  decrypting, because such a drive cannot be shrunk safely.
- **Fast startup** hibernates Windows' kernel instead of shutting it down,
  with C: still mounted. LosOS's first boot changes the partition table
  around it, so the installer sets `HiberbootEnabled` to 0 before anything
  else. LosOS never mounts Windows' partitions, so this is about the
  partition table, not shared files.

## Testing it without Windows

The Linux build, `pkgs.losos-windows-installer`, is the same program except
for `windows.rs`, the part that talks to Windows. Its tests check the
release's real signature and manifest, the layout, the FAT and xz writers,
and the bytes of the firmware boot entry. With `--image DISK` it installs
into a disk image laid out like a Windows disk, using `sfdisk` instead of
Windows' disk IOCTLs, and leaves out the firmware entry. That is how the
install was checked. It installed into a 64 GB image with a 100 MB ESP, an
MSR, a C: and a recovery partition. The image then booted under QEMU into
the boot loader's menu with LosOS and Windows Boot Manager, ran first boot,
and left C: and recovery byte-for-byte as they were.

> [!NOTE]
> That check was made when the loader was still systemd-boot; with GRUB it
> has not been run again.

Only a Windows machine can test what `windows.rs` does: shrinking C:,
writing the partition table through `IOCTL_DISK_SET_DRIVE_LAYOUT_EX`,
mounting the ESP, writing firmware variables, suspending BitLocker, and how
a firmware treats the new boot entry.
