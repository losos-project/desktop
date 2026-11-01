# Releases and GHCR

`nix build .#release` writes a flat directory:

```
losos-desktop_<v>_x86_64.efi                        the UKI sysupdate installs
losos-desktop_<v>_usr-x86-64_<uuid>.raw.xz          /usr for a sysupdate slot
losos-desktop_<v>_usr-x86-64-verity_<uuid>.raw.xz   its hash tree
losos-desktop_<v>.linux, .initrd, .grub             the UKI's kernel, initrd and
                                                    command line, for GRUB on a
                                                    BIOS PC, x86_64 only
losos-desktop_<v>_aarch64.grub                      the UKI's command line, for
                                                    the recovery shell, aarch64
                                                    only
losos-desktop_<v>_x86_64.raw.xz                     the disk image
losos-desktop_<v>_x86_64-installer.iso              the installer
losos-desktop_<v>_x86_64-windows-installer.exe      the installer for Windows, x86_64 only
SHA256SUMS
```

CI pushes it to GHCR with `oras`, as one OCI artifact per architecture.
Alongside the individual files, the OCI artifact carries
`initial-update.tar.xz`: the signed manifest and only the files named by
the sysupdate transfers, with update payloads individually XZ-compressed
before they are bundled into the XZ archive. The installer downloads this
single archive for the initial version and extracts to a scratch partition
on the target disk instead of tmpfs; the archive and extracted files are
removed after installation. sysupdate cannot fetch from an OCI registry
itself, but a deployment of [`proxy/`](binary-cache.md) serves the
individual file layers to installed systems. The publish job then creates a
GitHub release for that run, tagged with its cadence and version.
Version `20261010.175322` becomes `nightly-20261010T1753` for a nightly, which
is marked as a [prerelease](#release-cadences). The release carries both
architectures' files: the update images, disk images, installer ISOs and the
[Windows installer](windows-installer.md). Each architecture's signed manifest
goes beside them as `SHA256SUMS-<arch>` and `SHA256SUMS-<arch>.gpg`.

GitHub rejects a release asset of 2 GiB or more, and `/usr` and the disk
image are larger than that. So such a file is uploaded in 2000 MiB parts,
starting at `<file>.part-00`. `cat <file>.part-* > <file>` gives back the
file that its line of `SHA256SUMS-<arch>` names. sysupdate cannot read that
as a channel, so an image reads the release through its own service that
joins the parts as it streams them, described
[below](#updating-from-github-releases).

The installer ISO pulls the versioned OCI release artifact directly from GHCR
with `oras`, falling back to the channel's moving OCI tag for branch builds.
It stages only the manifest, signature and files its transfers need, verifies
them, and gives them to sysupdate locally. It does not need GitHub release
asset downloads or a hosted proxy to install. [The installer](installer.md)
has the details. GitHub releases remain available for people downloading
artifacts manually and for the web flasher.

> [!NOTE]
> A release is created once and never edited, so the repository can make
> releases immutable. A re-run of the same commit finds its tag taken and
> publishes nothing. There is no moving `nightly` release: GitHub refuses to
> reuse the tag of a deleted immutable release, so it could not be replaced.

## Release cadences

Every release belongs to one of four cadences, and its tag says which:
`nightly-`, `weekly-`, `monthly-` or `yearly-`, then the version's
`<YYYYMMDD>T<HHMM>`. A nightly is a prerelease. A weekly, monthly or yearly
release is a full release, and only a yearly one is marked as the
repository's latest release on GitHub.

Each green run on main publishes a nightly. Once a day,
`.github/workflows/cadence.yml` lists the repository's releases and checks
each slower cadence. If that cadence's newest release came out in an earlier
ISO week, month or year than today, or there is none yet, it starts `ci` on
the newest nightly's tag for that cadence. So a weekly comes out on Monday, a
monthly on the 1st and a yearly on 1 January. If that cadence has already
released the newest nightly, it waits for the first day after that with a
nightly it has not released yet. The run builds the same commit again with
`LOSOS_CHANNEL` set to the cadence, which `flake.nix` reads as
`losos.channel`. Every package comes from the cache, and only `/usr`, the UKI
and the images built from them change, because the cadence a system follows by
default is part of `/usr`. A system installed from a weekly release's ISO or
disk image therefore follows the weekly releases. A cadence whose run failed
has no release for that period yet, so the next day starts it again.

A run of `ci` started by hand, from Actions, ci, Run workflow, takes a
cadence and an optional title, such as `LosOS 2027`. It releases
the commit it ran on for that cadence and moves that cadence's tag in GHCR,
`images:<cadence>-<arch>`, to it.

CI also gives each release made without a title a name. The name is an
adjective and a verb that start with the same letter, such as "Quiet Queue",
picked at random from the [Oxford 3000 and
5000](https://www.oxfordlearnersdictionaries.com/wordlists/oxford3000-5000)
word lists. It goes after a ` · ` in the release's title. A verb that an
earlier release's name used is not picked again until every verb has been
used. If the word list cannot be fetched, the release is published without a
name.

derisk, mcsapi and pm release on the same cadences, each through its own
`.github/release`: every commit on its default branch that passes CI is a
nightly, and at 02:17 UTC the newest nightly becomes the weekly, monthly or
yearly release that is due, with the same commit and files, since nothing
in them depends on the cadence. Their releases are named the same way, from
their own earlier names. Every Monday the OS's lockfile update moves its
submodules to their weekly releases ([Keeping the pins
current](trust.md#keeping-the-pins-current)).

## Choosing a cadence

Settings, System, Updates offers all four cadences. The choice applies to the
whole machine. `losos-update-channel set <cadence>` writes it to
`/var/lib/losos/update-channel`. Settings runs that as
`losos-update-channel@<cadence>.service`, so that polkit asks an administrator
first. `losos-update-channel` on its own prints the cadence the machine
follows, which is the image's own until someone picks another. The update
source below reads that file again for each request, so a new choice applies
from the next update. sysupdate never installs a version older than the one
running, so moving to a slower cadence keeps the current version until that
cadence has a newer one. A factory reset forgets the choice.

![CI pushes store paths, releases and compiler caches to GHCR; the proxy redirects Nix, sysupdate and CI to GHCR's storage and streams the GSI's files to the web flasher; each nightly is also a GitHub release](images/binary-cache.svg)

The `/usr` halves are cut out of the finished disk image at the offsets
repart reported, not built a second time, so the bytes sysupdate installs are
the bytes the image boots. Each file name carries its partition's UUID, which
sysupdate reads with `@u` and gives to the partition it writes. The initrd
finds `/usr` by the UUIDs repart derived from `usrhash=`. A slot that kept
the random UUID repart created it with would hold the right bytes and still
never be found.

## The channel index on the site

The documentation site on GitHub Pages carries each channel's newest release
as a file, `updates/<channel>/<arch>/index`. `website/release-index.js`
writes it when the site is built. It has one line per file sysupdate would
ask a channel for, with the name, the size, and the release asset URLs the
file is made of. So the `SHA256SUMS` line points at `SHA256SUMS-<arch>`, and
a split file lists its parts in order. A channel's newest release is its
cadence's newest release by tag. The site reads as many pages of the API as
it takes to find it, since a yearly release sits behind hundreds of
nightlies.

```
SHA256SUMS 1096 https://github.com/<repo>/releases/download/nightly-20261016T1200/SHA256SUMS-x86_64
losos-desktop_<v>_usr-x86-64_<uuid>.raw.xz 2621440000 https://...raw.xz.part-00 https://...raw.xz.part-01
```

`docs.yml` builds the site when the docs change, and the publish job starts
it once a release is up, so the index moves to a release only after the
release has every file. Pages serves files and nothing else, which is why
the index lists URLs instead of redirecting to them. Nothing in it is
trusted. Whatever reads it still checks `SHA256SUMS.gpg` and each file's
hash.

## Updating from GitHub Releases

An image built without `LOSOS_PROXY_URL` updates from the newest GitHub
release of the cadence it follows. Its sysupdate transfers read
`http://127.0.0.1:8485/`, set as `losos.update.baseUrl`. There a socket,
`losos-update-source.socket` from `nixos/modules/update-source.nix`, starts a
short script for each request. The script reads the channel's index from
`losos.update.index`, with the machine's cadence in place of `{channel}`,
finds the name asked for, and streams the release assets listed for it one
after the other with curl. A connection that breaks is resumed from the byte
where it stopped, up to ten times per part. sysupdate gets `SHA256SUMS`, its
signature and whole files under one URL, as from any channel. It checks the
signature against the image's key in `/etc/systemd/import-pubring.pgp`, with
gpg on the update unit's PATH, and checks every file against its line. No
service outside GitHub is involved.

If a download still fails, it leaves its slot marked partial, and sysupdate
will not install over a partial slot. The update unit vacuums such slots
before it starts, so the next timer run downloads that version again.

[The installer ISO](installer.md) reads the same index itself when it installs
from the channel, which it does when the backend finds no release of the ISO's
own version. It fetches the parts listed there with aria2, as it does for a
release. The [Windows installer](windows-installer.md) also reads the index
itself, downloads each part with resume, and joins the parts before it checks
the file.

The index is read again for each request, so a release published in the
middle of an update can change it between `SHA256SUMS` and the files that
manifest names. Every file name carries its version, so the older release's
files are then not found. That attempt fails, and the next timer run starts
over from the new manifest.

Systems installed from an image that names a proxy keep reading that proxy.
The URL is part of `/usr`, so only an update from that proxy, or a reinstall,
moves them to GitHub Releases. Their sysupdate transfers name the proxy's
`/updates/<cadence>/<arch>/` for the cadence they were built for, so
Settings' choice does not move them, as [What is not done](not-done.md)
notes.
