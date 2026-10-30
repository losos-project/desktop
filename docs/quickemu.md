# Trying it in quickemu

[quickemu](https://github.com/quickemu-project/quickemu) runs a VM from a
short config file. `quickemu/` has one for each architecture. Both describe
the PC LosOS expects, not the smallest one it boots on:

| | |
| --- | --- |
| Firmware | UEFI, Secure Boot off |
| TPM | 2.0, from `swtpm` |
| CPU | 4 cores |
| RAM | 8 GB |
| Disk | 64 GB, sparse |
| Graphics | virtio GPU with GL (virgl) |

> [!IMPORTANT]
> The firmware has to be UEFI. Secure Boot is off so that a local build,
> which is not signed, starts as well as a release, which is
> ([Secure Boot](secure-boot.md)). Only a UEFI boot goes through
> systemd-stub, which measures the UKI into the TPM, so with the TPM the
> [security report](security-report.md) finds both and the VM is the machine a
> laptop would be. Like every machine it never [hibernates](hibernation.md).

First boot lays out the disk as [Where the OS lives](layout.md) describes.
It makes the ESP, two 8 GB `/usr` slots with their verity halves, root from
8 GB, `/home` from 4 GB, and swap the size of RAM, about 29 GB plus RAM in
all. 64 GB is what the [Windows installer](windows-installer.md) offers by
default, and it leaves most of the rest to `/home`. 8 GB of RAM is twice what
`nix run .#vm` gives. More RAM needs more disk, since swap grows with it.

## Running it

quickemu needs `swtpm` beside QEMU for the TPM. The installer ISO is in every
nightly release on [the releases
page](https://github.com/losos-project/desktop/releases), as
`losos-desktop_<version>_<arch>-installer.iso`. Save it next to the config,
under the name the config gives it:

```sh
cd quickemu
mkdir -p losos-desktop-x86_64
mv ~/Downloads/losos-desktop_*_x86_64-installer.iso losos-desktop-x86_64/installer.iso
quickemu --vm losos-desktop-x86_64.conf
```

The first start creates the disk and boots [the installer](installer.md),
which installs the channel's newest release over the network. quickemu
attaches the ISO only while the disk is still empty, so every later start
boots the installed system. On an aarch64 host, use
`losos-desktop-aarch64.conf` and the aarch64 ISO the same way.

> [!WARNING]
> quickemu will also run it on an x86_64 host, emulated, but that is too
> slow to install from. In one try, the installer's initrd gave up
> waiting for its CD after 90 seconds.

The TPM only counts if the firmware can use it too. quickemu takes OVMF
from the host, and nixpkgs builds the OVMF that quickemu picks for Secure
Boot off without TPM support. The system then finds the TPM, but nothing
measured the boot, and `bootctl status` in the VM says
`TPM2 Support: driver only, firmware unavailable` and `Measured UKI: no`. On
NixOS, run a quickemu built with a TPM-enabled OVMF instead:

```nix
pkgs.quickemu.override {
  OVMF = pkgs.OVMF.override { tpmSupport = true; };
}
```

With that, `bootctl status` reads `TPM2 Support: yes`, `Measured UKI: yes`
and `Measured OS: yes`.

> [!TIP]
> GL needs a host that has it, under quickemu's default GTK window or
> `--display sdl` or `spice-app`. On a host without it QEMU refuses to start,
> saying `OpenGL is not supported by display backend`; `gl="off"` in the
> config starts it with a plain virtio GPU, and the desktop draws with
> llvmpipe instead.

> [!TIP]
> A network that re-signs TLS with its own authority, as some corporate ones
> do, fails the download. The installer then installs from a `LOSOS-RELEASE`
> disk instead ([The installer](installer.md)), attached with
> `--extra_args "-drive if=virtio,format=raw,readonly=on,file=/path/to/release.img"`.
