# losos-desktop

A [derisk](https://github.com/losos-project/derisk) desktop operating system.
It uses systemd for everything it can and installs as an image. The OS is a
NixOS configuration, `flake.nix` plus the modules in `nixos/`, built on
nixpkgs' stock glibc packages for `x86_64-linux` and `aarch64-linux`. It also
builds as one generic system image for Android phones and tablets over
[Halium](https://halium.org), and the
[web flasher](https://losos-project.github.io/losos-desktop/flasher/) installs
that.

The documentation is on the
[website](https://losos-project.github.io/losos-desktop/) and in the
[wiki](https://github.com/losos-project/losos-desktop/wiki). Both are
generated from [`docs/`](docs/index.md), so edit it there.

## Building

Clone with `git clone --recurse-submodules`, then:

```sh
nix build --impure              # the disk image; dd it to a disk and boot
nix build .#installer --impure  # the installer ISO
nix run .#vm --impure           # boot the image in QEMU with UEFI firmware
```

[Building](docs/building.md) has the rest.

## Licence

AGPL-3.0-or-later. See `LICENSES/` and `REUSE.toml`.
