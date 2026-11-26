# Building

Pass `--impure` whenever you build an image, installer, QCOW2, release, UKI or
VM. The flake reads environment variables to decide where the image updates
from. With none set, it updates from the channel's newest GitHub release
([Updating from GitHub Releases](releases.md#updating-from-github-releases)).
Set `LOSOS_PROXY_URL` to a deployment of `proxy/` ([Binary cache](binary-cache.md))
and the image updates from, and pings, that deployment instead.

```sh
nix build --impure              # the disk image: dd it to a disk and boot
nix build .#installer --impure  # the installer ISO, with the local release payload
                               # and try.squashfs grafted in. Set
                               # UPDATE_SIGNING_KEY_FILE to sign the manifest, as CI does.
nix build .#release --impure    # the files CI publishes
nix run .#vm --impure           # boot the image in QEMU with UEFI firmware
nix flake check        # both architectures, the losos-security and
                       # losos-installer tests, and a VM boot test on a KVM builder
```

Nix takes the stock nixpkgs closure from cache.nixos.org and builds the image
and the project's own packages locally, unless the project's
[binary cache](binary-cache.md) has them. `systemd-repart` builds the image
inside the sandbox, with no loop device, root or KVM. Only the VM tests need KVM.

> [!IMPORTANT]
> The build user must be able to create user namespaces, because
> `systemd-repart` runs under `unshare --map-root-user`. Ubuntu 23.10 and later
> block them for unprivileged processes through AppArmor, and the image build
> fails. Run `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`,
> as CI does, or set it in a file under `/etc/sysctl.d/` to keep it.

> [!WARNING]
> CI boots the VM tests last, and a VM test failure there is only a warning in
> the run summary, so it never holds back the nightly. Run `nix flake check`
> locally to see a real failure.

## Check a change to `nixos/`

Evaluate every target. This runs every module and assertion without building an
image:

```sh
nix eval --raw .#nixosConfigurations.losos-desktop-x86_64.config.system.build.toplevel.drvPath
nix eval --raw .#nixosConfigurations.losos-desktop-aarch64.config.system.build.toplevel.drvPath
nix eval --raw .#nixosConfigurations.losos-desktop-gsi-aarch64.config.system.build.toplevel.drvPath
```

`nix fmt` formats the Nix files. CI runs `nix fmt -- --ci`.

`just` has short names for these commands. `just plugins` builds pm's plugin
components into `plugins/dist/share/pm/plugins`, for a pm installed outside the
image ([pm on LosOS](pm.md)).

## Building on a host running systemd-oomd

The heavy derivations, such as the derisk and pm LTO links, image assembly and
the VM test guests, need more memory than a desktop keeps free. On a multi-user
Nix install they all run inside `nix-daemon.service` in `system.slice`. Desktop
distributions often have systemd-oomd watch `system.slice` (Fedora kills at 80%
memory pressure). When the build crosses that line, oomd kills the worst cgroup
in the slice. That is `nix-daemon.service`, which takes every running build
with it, or an unrelated service next to it.

Give oomd the build itself as its target:

```ini
# /etc/systemd/system/nix-daemon.service.d/oomd.conf
[Service]
# Above this the kernel reclaims memory from the builders, not the desktop.
# MemoryHigh takes no percentage: leave the host half the RAM (10G of 16 GB).
MemoryHigh=10G
# A lower line than the slice's, so oomd kills the build, not the machine.
ManagedOOMMemoryPressure=kill
ManagedOOMMemoryPressureLimit=50%
```

Run `systemctl daemon-reload`. The setting applies at the daemon's next start.
To see what your host watches today, run
`systemctl show system.slice -p ManagedOOMMemoryPressure -p ManagedOOMMemoryPressureLimit`.

On a NixOS host, set the same keys in
`systemd.services.nix-daemon.serviceConfig`. On a single-user install, where
builds run in your session, wrap the build:

```sh
systemd-run --user --scope -p ManagedOOMMemoryPressure=kill \
	-p ManagedOOMMemoryPressureLimit=50% -- nix build …
```

CI builds one derivation at a time (`--max-jobs 1`) for the same reason:
parallel builds filled a 16 GB runner until it stopped responding.
