# The files machine

A USB stick's filesystem is whatever its last owner put on it, and the
kernel's filesystem code is some of its most attacked. On a PC, drives and
SD cards are mounted in sys-files, a machine of their own, and only their
files cross to the host. sys-files has no network card at all, so nothing
on a drive can reach anything but the host.

Like the [network machine](network-machine.md), sys-files is BusyBox, one
program (`losos-filesd`) and the host's own kernel, hidden from the
Machines app.

## How a drive gets there

- A USB drive is moved there by the [peripherals](peripherals.md) router,
  over USB/IP. The router starts sys-files if it is not running.
- An SD card reader on the PCI bus is handed to sys-files at boot, and
  sys-files starts at boot to hold it.

`losos-filesd` mounts each filesystem it recognises: FAT, exFAT, NTFS,
ext2 to ext4, Btrfs, XFS, F2FS, and CD and DVD formats. Nothing on a drive
can run, and nothing on it can be a device. It leaves other things alone,
such as swap or an encrypted volume.

## In Files

On the host, `losos-files-host` mounts each volume for the person whose
session is in front, under `/run/media/<user>/<name>`. It uses sshfs, with
OpenSSH's `sftp-server` in the machine at the other end of a
`losos-vm-call` connection. The name is the volume's label. The files show
as the person's own, and the kernel checks access as it would for their
own files.

`losos-files eject <name>` unmounts a volume and tells the kernel the drive
is going, so it can be pulled out. `losos-files list` lists what is
mounted.

## Lazy

sys-files powers itself off five minutes after the last drive goes, and
gives its memory back to the host while it runs. The next drive starts it
again.

## Network shares

SMB, NFS and WebDAV shares are mounted in a second machine, sys-shares,
which is sys-files with a network card. That card is on the host's link to
the network machine, which routes it as it routes the host: through the
ad blocker's DNS, and through Tor in schizo mode. sys-shares keeps the
shares and their passwords on a small disk of its own, which only it
reads. The host only remembers that shares were added, so it can start
sys-shares at boot.

```sh
printf '%s\n%s\n' "$user" "$password" | losos-shares add Photos smb://nas/photos
losos-shares list
losos-shares remove Photos
```

The shares appear under `/run/media/<user>/<name>` like drives.

[Backups to a LosOS box](backup.md) do not use sys-shares. They talk HTTPS
to the box through the LosOS boxes machine and check the box's
certificate on every connection.

## The pieces

| Piece | Where |
| --- | --- |
| The files machine and its host half | `nixos/modules/files-machine.nix`, `losos-files-host.sh` |
| The network shares machine | `nixos/modules/shares-machine.nix` |
| The machines' program | `src/losos-filesd` |
