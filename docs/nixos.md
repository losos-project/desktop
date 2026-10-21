# Where the sections of nixos.md went

This file used to hold all of the build documentation on one page. Each of
its sections is now a page of its own. Comments in `nixos/`, `tools/` and the
workflows still cite it as `docs/nixos.md`, "Section", and this table says
where each section went.

| Section | Now |
|---|---|
| (the introduction) | [LosOS Desktop](index.md) |
| What this trusts from outside | [What this trusts from outside](trust.md) |
| The components/ submodules | [What this trusts from outside](trust.md#the-components-submodules) |
| Building | [Building](building.md) |
| glibc and stock packages | [glibc and stock packages](packages.md) |
| The memory allocator | [The memory allocator](allocator.md) |
| Binary cache | [Binary cache](binary-cache.md) |
| pm's plugins | [pm on LosOS](pm.md#pms-plugins) |
| Driving the flake from pm | [pm on LosOS](pm.md#driving-the-flake-from-pm) |
| nixpkgs in a pm build | [pm on LosOS](pm.md#nixpkgs-in-a-pm-build) |
| Where the OS lives | [Where the OS lives](layout.md) |
| What maps to what | [What maps to what](from-the-pm-tree.md) |
| Everything systemd, and the exceptions | [Everything systemd, and the exceptions](systemd.md) |
| First-boot setup | [First-boot setup](first-boot.md) |
| The installer | [The installer](installer.md) |
| Releases and GHCR | [Releases and GHCR](releases.md) |
| Halium | [Halium GSI](halium.md) |
| What a device port supplies | [Halium GSI](halium.md#which-devices): there are no device ports any more |
| GTK and Qt on a phone | [GTK and Qt on a phone](gtk-qt-phone.md) |
| Uranium | [Danube](danube.md), which replaced it |
| Active users and the choice screens | [Active users and the choice screens](choice-screens.md) |
| One icon theme | [One icon theme](icon-theme.md) |
| What is not done | [What is not done](not-done.md) |
