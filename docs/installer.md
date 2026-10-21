# The installer

The pm tree's installer was `systemd-sysinstall`, new in systemd v261.
nixpkgs 26.05 shipped 260.4, so the installer does the same job by hand with
the same tools. nixos-unstable now carries 261, so sysinstall is available,
but the installer has not moved onto it yet. The installer is its own small
live system, `nixos/installer/`, on its own ISO, and
`nixos/modules/installer.nix` evaluates it from the OS's configuration.

> [!TIP]
> A machine that keeps Windows uses the [Windows installer](windows-installer.md)
> instead, which installs the same release beside it.

Under UEFI, the ISO's appended FAT partition holds GRUB as
`EFI/BOOT/BOOT<ARCH>.EFI`, which chainloads the installer's UKI from the ISO
9660 filesystem. On x86_64 a legacy BIOS starts it too, through GRUB, from a
disc or a USB stick. Either way, [GRUB's menu and the Plymouth
splash](boot-loaders.md) after it look the same as on an installed system and
say "LosOS Desktop installer". The initrd then mounts the ISO by its volume
label, and the Nix store from a squashfs on it.

The live system runs NetworkManager with NixOS's defaults, as the installed
system does. NetworkManager runs DHCP on every wired port, built in or USB,
as soon as a cable is plugged in, at boot or later, and prefers it over
Wi-Fi when both are up. A machine with a cable in is online without being
asked anything. Otherwise derisk scans for a Wi-Fi network and joins it
through NetworkManager with `nmcli`. Of the firmware NixOS would add, only
`linux-firmware` is on the medium, because most Wi-Fi cards do not start
without it.

![The installer's Disk page, listing the one disk it may install to, and the Erase this disk? page, which names the disk once more behind an Erase and install button](images/installer-pages.png)

The installer is `derisk installer` on tty1, a logind session run as root,
with a root shell on tty2 for anything it does not cover. It draws the same
pages first-boot setup uses, so it works with a mouse, a touchscreen or a
phone-sized screen. It starts `losos-installer serve`, from
`src/losos-installer`, as its backend and talks to it in JSON lines on stdin
and stdout. derisk owns the screen and the network, and the backend owns the
disks. The Network page shows each wired port as having no cable, getting an
address, or online, and is skipped when the machine is already online. Then
the backend lists the disks. It leaves out the disk the ISO booted from and
refuses any disk it did not list. Before anything is touched, the installer
names the chosen disk, its size and the source once more, behind an "Erase
and install" button. Then:

1. The installer wipes the disk's old partition table and tells the kernel to
   forget its partitions. The kernel refuses while one of them is mounted,
   and that stops the install before anything is written. This step matters
   because repart adds the new ESP to the kernel itself to format it, and it
   could not do that while the kernel still knew an old partition with the
   same number.
2. `systemd-repart --empty=force` lays out the ESP, with GRUB and an empty
   `grubenv` copied in, a BIOS boot partition for GRUB on x86_64, and both
   `/usr` slots at full size, labelled `_empty`. sysupdate refuses a disk
   with only one slot. These are `disk.nix`'s own definitions, so the disk is
   laid out the way the installed system expects to find it.
3. `systemd-sysupdate update` fills the slots with the GitHub release of the
   version the ISO was built as. It uses the same transfers as `update.nix`,
   aimed at the chosen disk instead of `auto` and at the new ESP, mounted
   under `/run/losos-installer`. With `losos.update.pubring` set, the
   installer checks `SHA256SUMS.gpg` against it, as an update does. On
   x86_64, `losos-grub-bios-install` then writes GRUB into the MBR and the
   BIOS boot partition, so the disk starts on a legacy BIOS as well.
4. The machine reboots into the installed system. Its first boot creates
   root, `/home` and swap from `disk.nix`, the same way an image written with
   `dd` does.

![derisk installer talks JSON lines to losos-installer serve, which runs systemd-repart, then systemd-sysupdate from the channel or a LOSOS-RELEASE disk, then reboots; the disk ends with the ESP, BIOS boot and slot A written, slot B laid out empty, and root, home and swap left to the first boot](images/installer.svg)

So an install is an update into an empty slot, from the
[release](releases.md) the ISO came with. An ISO from
`nightly-20261010T2127` installs that nightly. One from
`weekly-20261012T0300` installs that weekly release, and the installed
system then follows the weekly releases. The medium carries no copy of the
OS.

sysupdate cannot read a GitHub release directly. There, each architecture's
manifest is `SHA256SUMS-<arch>`, and a file of 2 GiB or more is split into
`.part-NN` assets. So `losos-installer` serves the release to sysupdate from
`127.0.0.1`. The `SHA256SUMS` and signature it serves are the
architecture's, and a split file comes back as its parts one after the
other. sysupdate checks the signature and every hash itself, as it does for
the channel. The backend asks GitHub's API for the release by its tag, which
it makes from the cadence and version the ISO was built as. When there is no
such release, as for an ISO built from a branch or a local build, the install
reads the channel and gets its newest release instead. When the OS updates
from GitHub Releases, it reads the channel through the channel's index on the
documentation site, as [Releases](releases.md) explains.

The files come down with [aria2](https://aria2.github.io/) before sysupdate
reads them, in `src/losos-installer/src/stage.rs`. aria2 fetches one file at
a time and splits each over one connection per hardware thread, as
`aria2c -x $(nproc)` would, up to 16, which is the most aria2 opens to one
server. GitHub's release storage caps each connection's speed, so the image
arrives that many times faster than over curl's single connection, which is
how it used to come. A connection that breaks is opened again and carries on
from the byte where it stopped. Only the manifest, its signature and the
files the transfers name are fetched. The ISO, the disk image and the other
architecture's files are not.

aria2 writes each connection's range into its place in the file, so a file
is whole only once every range is in, and it cannot go to sysupdate while it
is still arriving. It has to be stored somewhere first, and the live
system's memory cannot hold a `/usr` of over 3 GiB on every machine. The disk
being installed can. Until the first boot creates root, `/home` and swap,
everything after the `/usr` slots is free. So repart adds an ext4 partition
labelled `losos-download` there, the size of the files plus some room, and
aria2 fetches into it. Once sysupdate has written the slots, the installer
wipes and deletes the partition, so the first boot finds the free space it
expects. Updates on the installed system still stream through curl, in
`update-source.nix`, because its free space is not a spare partition.

The installed system then updates from the channel like any other. The
repository comes from `GITHUB_REPOSITORY` in CI, as
`losos.release.repository`, so a fork's ISO reads the fork's releases. The
image used to be its own installer, with a second UKI that booted it into a
tmpfs root and copied its own `/usr` with `CopyBlocks=`. That is gone, along
with the `losos.install` condition it needed in `disk.nix`. When nixpkgs
reaches v261, the installer should be compared against `systemd-sysinstall`
again.

A machine that cannot reach the channel installs from a release disk
instead. That is any filesystem labelled `LOSOS-RELEASE` that holds a
release's `SHA256SUMS`, `SHA256SUMS.gpg` and the `/usr`, verity and UKI files
they list. udev mounts it at `/run/losos/release` whenever it appears, and
the installer then turns the transfers' sources into local files. sysupdate
verifies signatures only on downloads, so the installer checks the release
first. It checks `SHA256SUMS` against the signing key with `gpg`, as
`systemd-pull` does, then checks every file on the disk against its line
there and refuses any file not listed. The release disk is for a VM on a
network that re-signs TLS with its own authority, where the channel's
certificate never verifies. With the disk attached, the installer does not
need the network at all. The backend's `hello` names the disk as `release`,
and derisk then skips its Network page.
