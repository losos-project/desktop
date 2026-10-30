# Boot loader and boot screen

LosOS boots with GRUB on every PC. On UEFI it starts from the ESP. On an
x86_64 PC that starts by legacy BIOS, or by UEFI's Compatibility Support
Module with UEFI turned off, it starts from the MBR. One disk carries both,
so the same image, the same install and the same updates start on either
kind of firmware. Both draw the same menu with the salmon LosOS boots behind,
and both hand over to the same Plymouth splash. arm64 has no BIOS, so only
the UEFI half applies there.

```
UEFI   firmware -> ESP: GRUB -> chainloads the newest UKI -> systemd-stub
BIOS   firmware -> MBR: GRUB's boot.img -> BIOS boot partition: GRUB's core
              -> the newest UKI's kernel, initrd and command line
then   the systemd initrd, under Plymouth -> the login screen
```

![UEFI firmware starts GRUB, which chainloads the UKI; a legacy BIOS starts GRUB from the MBR and the BIOS boot partition, which starts the UKI's kernel, initrd and command line from files beside it. Both reach the same systemd initrd and the /usr slot usrhash= names](images/boot-loaders.svg)

Under either firmware, GRUB finds the same versions on the ESP, counts the
tries of a new one, and falls back to the older one. The two `/usr` slots and
rollback work the same way on both.

## What it looks like

![GRUB's menu on UEFI: the salmon on the dark ground, "LosOS Desktop" under it, the newest version highlighted in the accent colour above the older one and Firmware settings, and "Starting in 2 s" below](images/boot-grub-menu.png)

The menu, the splash systemd-stub shows while the kernel loads, and the
Plymouth splash all come from one picture, `nixos/branding/salmon.png`. It
is the same coho salmon that LosOS itself boots behind, cut out of a NOAA
Fisheries photo, and `nixos/branding/CREDITS.md` credits it.
`nixos/branding/default.nix` makes every picture from it at build time, on
the dark palette that derisk and LosOS's WebUI share. Replace that one file
and all of them change.

- **GRUB**, from the `grub/` output of `nixos/branding/default.nix`, shows
  the salmon, the system's name, the menu with the chosen entry in the
  accent colour, and a countdown. On UEFI, GRUB uses the screen's own mode.
  On a BIOS it uses 1024x768, which Linux keeps as its framebuffer. If GRUB
  cannot set a mode, the menu is text. GRUB also writes it to the first
  serial port.
- **systemd-stub** shows the UKI's `.splash`, the salmon and the name on
  black, centred while the kernel and initrd load. That keeps the screen
  from going dark between GRUB and Plymouth. `nixos/modules/splash.nix` sets
  it up.
- **Plymouth**, scripted in `nixos/branding/splash.script`, shows the salmon
  swimming in the middle of the screen, with "LosOS Desktop is starting"
  under it and a line that grows as the boot goes on. The swim is 16 frames
  drawn at build time from the same picture. A wave runs from head to tail,
  one stroke a second, and the fish rises and sinks a little. A password
  prompt or a question takes the caption's place. At shutdown the caption
  says "is restarting" or "is shutting down". Plymouth runs from the initrd
  until the login screen takes the display. Nothing on it appears abruptly.
  The screen fades in from the dark ground when Plymouth starts, the caption
  and a prompt cross-fade, and just before the login screen everything fades
  back to the ground except the line, which fills and stays. The login screen
  starts from that frame. It draws the same line in the same place and closes
  it into a dot that opens the screen, as the launch film shows. Esc shows
  the boot log.

![Plymouth while the system starts: the salmon, "LosOS Desktop is starting" and the progress line in the accent colour under it](images/boot-splash.png)

The start has a sound too, the launch film's closing three seconds, which
are the two bars under its end card, in `nixos/branding/end-track.flac`. It
plays as soon as the kernel finds a sound card, while Plymouth still shows
the salmon. At shutdown or restart it plays backwards, after the sessions
and the login screen have closed, under Plymouth's "is shutting down"
screen. `nixos/modules/boot-sound.nix` does this. No PipeWire runs at either
moment, so the sound goes straight to ALSA's default card at whatever volume
the machine was left at. A machine without a sound card plays nothing.

> [!TIP]
> The kernel prints errors only (`loglevel=3`), systemd prints its status
> lines only when a unit fails or the boot stalls (`systemd.show_status=auto`),
> and Plymouth keeps even those behind Esc; every message is still in the
> journal.

The [installer ISO](installer.md) looks the same, but says "LosOS Desktop
installer". Halium phones boot through Android's boot loader and draw
nothing until the compositor starts, so none of this applies to them.

## What is on the disk

```
MBR, first 440 bytes                GRUB's boot.img (UEFI ignores it)
BIOS boot partition, 1 MiB          GRUB's core.img, its modules, menu and theme inside
ESP
  EFI/BOOT/BOOTX64.EFI              GRUB, its modules, menu and theme inside
  EFI/losos/grubx64.efi             the same, where a boot entry can name it
  EFI/losos/grubenv                 GRUB's boot counter
  EFI/Linux/losos-desktop_<v>.efi     the UKI, version <v>
  EFI/Linux/losos-desktop_<v>.linux   its kernel (x86_64 only)
  EFI/Linux/losos-desktop_<v>.initrd  its initrd (x86_64 only)
  EFI/Linux/losos-desktop_<v>.grub    its command line, as a line of GRUB script
```

The image, `system.build.disk` in `image.nix`, and the
[installer ISO](installer.md) both lay out the BIOS boot partition and write
GRUB into it with `losos-grub-bios-install` from `nixos/modules/bios.nix`.
The [Windows installer](windows-installer.md) does not, because it only adds
LosOS beside Windows on a UEFI PC.

UEFI GRUB chainloads the UKI, so systemd-stub runs as it would under any
loader. It measures the UKI into the TPM, shows its splash and starts the
kernel with the built-in command line. GRUB's `bli` module sets the EFI
variables systemd-boot used to set, `LoaderDevicePartUUID` and
`LoaderInfo`. `systemd-gpt-auto-generator` reads them to find the root disk
and mount the ESP, as before.

BIOS GRUB cannot run a UKI, because a UKI is an EFI program. So every x86_64
release also carries each UKI's kernel, initrd and command line as separate
files, cut from the finished UKI. sysupdate installs them beside the UKI as
part of the same version, through the transfers in `uki-files.nix`. An
aarch64 release carries only the command line, which the recovery shell
below needs. The command line is
the UKI's with one change. systemd finds the root partition from
`LoaderDevicePartUUID`, and a BIOS has no EFI variables. Instead, GRUB passes
the partition UUID of the ESP it started the kernel from as `losos.esp=`,
the way `bli` would set that variable. `root=gpt-auto` becomes
`root=/dev/disk/by-loader/root`. A udev rule in the initrd links that path
to the partition labelled `root-x86-64` (`root-arm64` on aarch64) on the
same disk as that ESP, so a second LosOS disk in the machine is never
mounted by mistake. A GRUB older than `losos.esp=` passes nothing, and the
command line falls back to `root=PARTLABEL=root-x86-64`. `systemd-gpt-auto-generator` mounts the root
disk's ESP at `/efi` only under UEFI, so a small generator does the same on
a BIOS boot.

## Updating GRUB

The GRUB on a disk is the one the image or the installer put there, and
`/usr` carries the GRUB its version was built with. After an update,
`losos-grub-update.service` copies the new GRUB over the old one, the way
`systemd-boot-update.service` does for systemd-boot. It is defined in
`grub.nix`. It runs after `losos-grub-bless`, so a version's GRUB reaches
the disk only after that version has started cleanly. It never goes
backwards. A rollback to an older version keeps the newer GRUB, which reads
the same file names and `grubenv`. It writes each file beside the old one and
renames it into place. `EFI/losos/installed` records which version's GRUB it
installed last, so a boot with nothing new writes nothing.

It replaces only files that already belong to LosOS. `EFI/losos/grub<arch>.efi`
is always LosOS's. `EFI/BOOT/BOOT<ARCH>.EFI` is replaced only when it holds
LosOS's GRUB or systemd-boot, so on a disk the
[Windows installer](windows-installer.md) set up, it stays Windows'. On an
x86_64 disk whose MBR holds GRUB's boot code, the service also rewrites the
MBR and the BIOS boot partition with `losos-grub-bios-install`. Any file it
adds to a disk the Windows installer set up goes on that installer's list,
so uninstalling LosOS from Windows removes it as well.

NixOS's own GRUB module installs GRUB and regenerates its menu on every
rebuild. It stays off, because this OS is never rebuilt in place and its menu
is the same for every version.

UEFI used to boot with systemd-boot. It draws a text menu in the firmware's
font and nothing else, and two loaders counting tries in two places meant one
more thing to keep in step, so GRUB replaced it. A disk installed before the
switch starts systemd-boot until its first update after it. Then
`losos-grub-update` puts GRUB at every path systemd-boot was started from,
with an empty `grubenv` beside it. Those paths are `EFI/BOOT/BOOT<ARCH>.EFI`
and `EFI/systemd/systemd-boot<arch>.efi`, which the firmware entry from the
Windows installer names. GRUB reads the tries left from the same file names
systemd-boot did, so a trial in progress carries on. Until that update,
`losos-grub-bless` leaves such a boot to systemd-boot's own tools.

## GRUB's menu

GRUB's menu, `nixos/modules/grub.cfg`, is built into GRUB's image together
with the theme and only the modules they use, by
`nixos/modules/grub-image.nix`. So the only file GRUB reads from a
filesystem is `grubenv`. The menu is not generated per machine. Each time
GRUB draws it, it looks for `EFI/Linux/losos-desktop_*.efi` on every
partition of the disk GRUB started from. On a BIOS it counts only UKIs with a
kernel beside them. It then offers:

1. the newest UKI that still has tries left, as the default;
2. every other one, with "(failed to start)" beside any that used up its
   tries;
3. on UEFI, Windows Boot Manager when it is on the same disk, and the
   firmware's own settings when the firmware can be asked to open them.

> [!TIP]
> Kernel arguments typed at GRUB's prompt into `losos_extra`, and exported,
> are added to whichever entry starts next, on x86_64 under UEFI as on a BIOS.
> That is the recovery shell ([When something goes
> wrong](troubleshooting.md#no-one-can-administer-the-machine)). With Secure
> Boot on, GRUB ignores them ([Secure Boot](secure-boot.md#while-secure-boot-is-on)).

A UKI takes kernel arguments only in place of its whole command line, so on
UEFI, GRUB puts the UKI's own command line from the `.grub` file in front of
`losos_extra`. On x86_64 it then starts the kernel and initrd beside the UKI
with them, as a BIOS boot does. aarch64 has no copies beside its UKI. Its
kernel is an uncompressed Image of about 70 MiB, and two versions of it
beside two UKIs would nearly fill the 512 MiB ESP. So GRUB chainloads the
UKI and passes the line to it, and systemd-stub starts the kernel with that
line instead of its own. systemd-stub ignores such a line while Secure Boot
is on.

## Boot counting

sysupdate installs a UKI with three tries in its name,
`losos-desktop_<v>+3-0.efi`, as `update.nix` sets up. GRUB cannot rename a
file, so it keeps the count in `grubenv`:

- The first boot of a counted UKI starts its trial. GRUB records its name in
  `losos_entry` and the number of tries its name gives in `losos_left`.
- Every boot of it lowers `losos_left` by one before GRUB starts it.
- At zero, GRUB passes over it and starts the next-newest version instead.

![A new UKI arrives with +3-0 in its name, three tries; each start uses one; reaching boot-complete.target blesses it, and running out of tries starts the older version and renames the new one +0-3](images/boot-counting.svg)

`losos-grub-bless.service` is the userspace half. Like
`systemd-bless-boot.service`, it is ordered after `boot-complete.target`. It
records the trial's result in the file name, where sysupdate reads it:

- if the UKI on trial is the one that booted, it is renamed to
  `losos-desktop_<v>.efi` and counts as blessed;
- if its tries ran out and an older version booted, it is renamed to
  `losos-desktop_<v>+0-3.efi`. That is the name systemd-boot gives an entry
  that used up its tries, and it keeps the entry last in the menu.

Either way `grubenv` is emptied, so a disk moved to another machine
mid-trial carries on from the file names.

`grubenv` also holds `losos_oneshot`, a version to start once at the next
boot instead of the newest, as `bootctl set-oneshot` did. Nothing sets it any
more. The hibernation hook that set it went away with
[hibernation](hibernation.md), and GRUB still reads a value an older version
left behind.

## The installer ISO

The [installer ISO](installer.md) starts with GRUB too, with the same theme
and one entry, which finds the medium by its label. On UEFI, GRUB is the
ISO's removable-media boot path and chainloads the installer's UKI from the
ISO 9660 filesystem. On an x86_64 BIOS, GRUB's El Torito image starts it from
a disc, and GRUB's hybrid MBR starts the same image from a USB stick. There
it starts the installer's kernel and initrd, kept beside its UKI in `boot/`,
with the UKI's command line. Plymouth then shows "LosOS Desktop installer is
starting" until the installer takes the screen.

## Secure Boot

A release signed with LosOS's key starts with Secure Boot on. Microsoft-signed
shim goes in front of GRUB on the ESP, GRUB and every UKI are signed, and
the key is enrolled once in shim's MokManager. [Secure Boot](secure-boot.md)
has the details, and how to enroll the key. Secure Boot is UEFI's and does
not apply to a BIOS boot.

> [!IMPORTANT]
> An image built without the key, such as a fork's or a local build's, has
> GRUB alone on the ESP, unsigned, and starts only with Secure Boot off.

## Tested

`nix flake check` boots the image under UEFI in `checks.x86_64-linux.boot`.
GRUB chainloads the UKI, `LoaderInfo` names GRUB, and first boot lays out the
disk. The test then renames the UKI to the name sysupdate gives a new one,
reboots, and checks that the trial ended with the file blessed and `grubenv`
empty. `boot-bios` does the same under QEMU's SeaBIOS, where GRUB starts the
UKI's kernel and the ESP is mounted without EFI variables to name it. Both
tests also replace an older GRUB with `losos-grub-update` and boot what it
wrote. Under UEFI that is an older GRUB and a systemd-boot, and under SeaBIOS
an emptied BIOS boot partition. `boot-bios` also gives the machine a second
disk with a root partition labelled as LosOS's, and checks that root still
comes from the disk GRUB started from. `hibernate` checks that the one-shot
request is used up on resume. `installer-boot` and `installer-boot-bios`
start the installer ISO under each firmware.

Both GRUB images were also started under QEMU, with OVMF and with
SeaBIOS, from a disk holding two versions, one of them on trial. Each drew
the themed menu, counted the trial down in `grubenv` and honoured a one-shot
request. On UEFI each left `LoaderInfo` as "GRUB 2.14" and
`LoaderDevicePartUUID` as the ESP's for the UKI it chainloaded.
`secure-boot` boots the image with Secure Boot on ([Secure
Boot](secure-boot.md#tested)).
