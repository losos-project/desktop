# losos-desktop

A desktop operating system built around the
[derisk](https://github.com/losos-project/derisk) desktop. It installs as an
image, updates with `systemd-sysupdate`, and uses systemd wherever systemd has
a component for the job. The OS is a NixOS configuration (`flake.nix` and the
modules in `nixos/`) on nixpkgs' stock glibc packages for `x86_64-linux` and
`aarch64-linux`. The same OS builds as one generic system image for Android
phones and tablets over [Halium](https://halium.org), which the
[web flasher](https://losos-project.github.io/desktop/flasher/) installs.

The documentation is on the
[website](https://losos-project.github.io/desktop/) and in the
[wiki](https://github.com/losos-project/desktop/wiki). Both are
generated from [`docs/`](docs/index.md), so edit the pages there.

## Building

Clone the repository, then:

```sh
nix build --impure              # the disk image; dd it to a disk and boot
nix build .#installer --impure  # the installer ISO
nix run .#vm --impure           # boot the image in QEMU with UEFI firmware
```

[Building](docs/building.md) has the rest.

## Licence

AGPL-3.0-or-later. See `LICENSES/` and `REUSE.toml`.
