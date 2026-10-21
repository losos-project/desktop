# Building

> [!IMPORTANT]
> Clone with `git clone --recurse-submodules`: derisk, mcsapi and pm are git
> submodules under `components/` ([What this trusts from
> outside](trust.md#the-components-submodules)).

The flake reads a few environment variables to decide where an image updates
from, so pass `--impure` when you build an image, installer, QCOW2, release,
UKI or VM. With none set, the image updates from the channel's newest GitHub
release, as [Updating from GitHub
Releases](releases.md#updating-from-github-releases) describes.
`LOSOS_PROXY_URL` is the base URL of a deployment of `proxy/`, covered in
[Binary cache](binary-cache.md). When it is set, the image updates from that
deployment instead, and pings it.

```sh
nix build --impure              # the disk image: dd it to a disk and boot
nix build .#installer --impure  # the installer ISO
nix build .#release --impure    # the files CI publishes
nix run .#vm --impure           # boot the image in QEMU with UEFI firmware
nix flake check        # both architectures, losos-security's and
                       # losos-installer's tests, and
                       # (on a builder with KVM) a VM boot test
```

Nix substitutes the stock nixpkgs closure from cache.nixos.org when it can.
It builds the image and the project's own packages locally unless the
project's [binary cache](binary-cache.md) has them. `systemd-repart` builds
the image inside the build sandbox, with no loop device, no root and no KVM.
Only the VM tests need KVM.

> [!WARNING]
> CI builds every other check first and boots the VM tests last. A VM test
> that fails there is a warning and a line in the run's summary, not a red
> run, so a boot that breaks under the runner's KVM does not hold back the
> nightly. Run `nix flake check` locally to see one fail for real.

`just` has short names for the same commands. `just plugins` builds pm's
plugin components into `plugins/dist/share/pm/plugins`, for a pm installed
outside the image ([pm on LosOS](pm.md)).

Before trusting a change to `nixos/`, evaluate every target. That runs every
module and assertion without building an image:

```sh
nix eval --raw .#nixosConfigurations.losos-desktop-x86_64.config.system.build.toplevel.drvPath
nix eval --raw .#nixosConfigurations.losos-desktop-aarch64.config.system.build.toplevel.drvPath
nix eval --raw .#nixosConfigurations.losos-desktop-gsi-aarch64.config.system.build.toplevel.drvPath
```

`nix fmt` formats the Nix files, and CI runs `nix fmt -- --ci`.
