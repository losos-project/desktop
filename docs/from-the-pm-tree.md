# What maps to what

| pm tree (removed) | NixOS | Notes |
|---|---|---|
| `recipes/10-systemd/linux`, `losos.config` | nixpkgs' kernel, `hardware.nix` | nixpkgs' `common-config.nix` already sets what systemd needs, `CONFIG_HIDRAW` included. zswap is built in and off, and `boot.zswap.enable` turns it on |
| `recipes/10-systemd/nvidia-open` | `nvidia.nix`: nixpkgs' NVIDIA open module and userspace | chosen per machine at boot from nixos-facter's report; older cards keep nouveau and NVK ([Hardware and NVIDIA](drivers.md)) |
| `recipes/00-*` through `30-gnome`, `manifest/`, `share/` | nixpkgs | every package the recipes built. Their build-system patches existed only for the CFI toolchain |
| `recipes/30-gnome/gnome-control-center` patches | `nixos/pkgs/patches/gnome-control-center/` | kept and not applied, see [What is not done](not-done.md) |
| `recipes/90-image/losos-image` (mkosi), `plugins/` | `nixos/modules/image.nix` (`image/repart.nix`) | the same `systemd-repart`; no fingerprint table to get past, so no plugins |
| `mkuki.py`, `files/cmdline` | `boot.uki`, verity-store module | the UKI carries `usrhash=` |
| `overlay/usr/lib/repart.d/` | `systemd.repart.partitions` in `disk.nix` | runs in the initrd, as before |
| `overlay/usr/lib/sysupdate.d/` | `systemd.sysupdate.transfers` in `update.nix` | UKI plus both `/usr` halves |
| `overlay/usr/lib/repart.sysinstall.d/`, `systemd-sysinstall` | `installer.nix`, `nixos/installer/` | see [The installer](installer.md) |
| `overlay/usr/lib/systemd/system-preset/10-losos.preset` | the module options themselves | on NixOS a unit is enabled by being wanted, not by a preset |
| `overlay/usr/lib/sysusers.d/losos.conf` | `systemd.sysusers.enable` over `users.users` | the upstream tool, not NixOS's perl script |
| `overlay/usr/lib/tmpfiles.d/losos.conf` | `systemd.tmpfiles.settings` in `services.nix` | |
| `overlay/usr/lib/systemd/network/20-wired.network` | `systemd.network.networks."20-wired"` | same match and settings |
| `overlay/etc/crypttab`, `overlay/etc/fstab` | `environment.etc.crypttab`, `swapDevices` | |
| `user@.service.d/10-oomd.conf` | `systemd.oomd`, `systemd.slices.user` | |
| `losos-security.service`, its D-Bus files | `services.nix` | same sandbox, line for line |
| `50-losos-factory-reset.rules` | `security.polkit.extraConfig` in `disk.nix` | same rule |
| `losos-selftest.service`, `losos-ota-test.service` | nothing | no test booted with either; the selftest made an ordering cycle that could drop `boot-complete.target`, see `nixos/modules/default.nix` |
| `recipes/10-core/losos-release` | `system.nixos.distroId`, `system.image.*` | NixOS writes os-release itself, with `IMAGE_VERSION` |
| `manifest/architectures.yaml` | `losos.arch` in `options.nix` | x86_64 and aarch64 |
| `tools/configure --version --channel` | `losos.version`, `losos.channel` | the flake derives the version from the commit date |
| `tools/vm-test` | `nixos/tests/boot.nix` | boots the real image under UEFI |
| `tools/` gates, `Containerfile`, `./do` | `nix flake check`, `nix fmt` | the gates checked generated recipes against pm's fingerprint table, and neither exists now |
| pm, the system manager | pm, the system manager (`pm.nix`) | the `pm` flake input |
