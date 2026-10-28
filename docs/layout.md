# Where the OS lives

The biggest structural change from the pm tree is where the operating system
lives.

In the pm tree the OS was the root partition, `/usr` and `/etc` together,
which sysupdate replaced as a whole, with `/home` beside it. Here the OS is
the Nix store, on its own `/usr` partition, protected by dm-verity. The root
partition holds only state, which is `/etc`'s writable layer, `/var` and the
journal.

```
BIOS boot    GRUB's core, for a legacy BIOS (x86_64)  image
ESP          GRUB, the UKIs                          image
usr-verity A dm-verity hash tree for slot A          image
usr A        the Nix store, erofs with Zstandard     image, grown on first boot
usr-verity B                                         first boot, label _empty
usr B        where the next update is written        first boot, label _empty
root         state only                              first boot, FactoryReset=yes
home         homed's LUKS images                     first boot, FactoryReset=yes
swap         RAM-sized, a new key each boot          first boot (losos-swap)
```

![The disk from left to right: BIOS boot, ESP, usr-verity A and usr A come from the image; usr-verity B, usr B, root, home and swap are created on the first boot. The UKI's usrhash= names slot A's verity tree, sysupdate writes whichever slot is not in use, and a factory reset empties root and home](images/layout.svg)

> [!IMPORTANT]
> `/usr` is compressed with Zstandard. Each slot is `losos.usrSize` (8G) once
> the first boot has grown it and can never grow again, so every later
> release's store has to fit in it, and a release build fails when it does
> not. Uncompressed, the store had reached 11.7G by October 2026, 7.3G
> compressed; with LZ4HC it was 8.2G, and installs stopped at 97%.

On x86_64, GRUB's boot code is also in the MBR, so a PC without UEFI starts
the same disk through the same [boot loader](boot-loaders.md).

Swap's key is random and nobody keeps it, so it is new every boot and nothing
swapped out survives a power cut. That is also one reason nothing
[hibernates](hibernation.md).

This layout fixes two things the pm tree listed as limits:

- **`/usr` really is immutable.** The pm tree's boot documentation named
  `systemd-veritysetup` in the boot chain but never built a verity partition.
  Here the root hash is `usrhash=` on the UKI's command line, so a changed
  block in `/usr` fails verification, and a different `/usr` needs a
  different UKI.
- **A factory reset resets the whole machine.** The pm tree's factory-reset
  notes explained that marking only `/home` left `/etc` and `/var` behind,
  and that the fix was to move state off the partition the OS lives on. This
  layout does that, so root is marked too, and a reset returns the machine to
  exactly what the image contained.
