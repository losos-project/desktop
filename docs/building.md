# Building

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
nix build .#installer --impure  # the installer ISO; grafts the local release
                               # payload and try.qcow2. Pass UPDATE_SIGNING_KEY_FILE
                               # on the runner (as CI does) to sign the manifest.
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

> [!IMPORTANT]
> The build user has to be able to make user namespaces: `systemd-repart`
> runs under `unshare --map-root-user`. Ubuntu 23.10 and later refuse them
> to unprivileged processes through AppArmor, and the image then fails to
> build. Lift that with
> `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`, as CI
> does, or keep it across reboots in a file under `/etc/sysctl.d/`.

## Building on a host running systemd-oomd

On the usual multi-user install every builder runs under
`nix-daemon.service`, inside `system.slice`, and the heavy derivations —
WPE WebKit compiling, derisk's and pm's LTO links, `systemd-repart`
assembling the image, the VM tests' guests — ask for more memory than a
desktop keeps free. systemd-oomd watches only the cgroups a slice opts in
to it, and a desktop distro's defaults watch `system.slice` (Fedora's kill
line there is 80% memory pressure), so the build's pressure registers
there. Past the line oomd kills the cgroup in that slice with the worst
pressure, which is `nix-daemon.service` — every running build dies at
once — or an innocent service that was merely standing beside the build.

Give oomd a target that is the build, and stop the build before the
machine swaps until it stops answering:

```ini
# /etc/systemd/system/nix-daemon.service.d/oomd.conf
[Service]
# Past the high line the kernel reclaims from the builders instead of
# from the machine: a build that crosses it slows down, a desktop does
# not. MemoryHigh takes no percentage; pick a byte count that leaves the
# host's own work half of the RAM (10G of a 16 GB machine).
MemoryHigh=10G
# oomd watches this cgroup on its own, and at a line lower than its
# slice's, so it trips first and what dies is the build: the running
# builds are lost, the machine is not.
ManagedOOMMemoryPressure=kill
ManagedOOMMemoryPressureLimit=50%
```

`systemctl daemon-reload` makes systemd read it, and the next start of
the daemon applies it. `systemctl show system.slice -p
ManagedOOMMemoryPressure
-p ManagedOOMMemoryPressureLimit` says what a host watches today. A NixOS
host writes the same through `systemd.services.nix-daemon.serviceConfig`,
and a single-user install, whose builders live in the session, gets the
same target from `systemd-run --user --scope -p
ManagedOOMMemoryPressure=kill -p ManagedOOMMemoryPressureLimit=50% -- nix
build …`. CI builds one derivation at a time (`--max-jobs 1`) for the
same reason: parallel derivations filled a 16 GB runner until it stopped
answering.

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
