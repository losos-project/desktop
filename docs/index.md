# LosOS Desktop

LosOS Desktop is a desktop operating system built around the
[derisk](https://github.com/losos-project/derisk) desktop. It installs as an
image, updates with `systemd-sysupdate`, and gives every user an encrypted
`systemd-homed` home. Wherever systemd has a component for a job, LosOS uses it.

The OS is a NixOS configuration: `flake.nix` plus the modules in `nixos/`. It
uses stock glibc packages from nixpkgs `nixos-unstable`, pinned by
`flake.lock`, for `x86_64-linux` and `aarch64-linux`. Nix fetches them from
cache.nixos.org, and paths that CI already built come from the project's GHCR
cache ([Binary cache](binary-cache.md)). [pm](https://github.com/losos-project/pm)
ships in the image as the system manager, and you use it to build and run
software on the running system ([pm on LosOS](pm.md)).

![The derisk overview: the open windows as tiles, workspaces along the top, and the clock, suggested apps, services, calendar and notes beside them](images/overview.png)

## What you get

Apps that the image does not carry come from [Flathub](https://flathub.org)
through derisk's [Store](store.md). On a PC they run in
[their own machine](hypervisor.md#the-flatpak-machine), and their file dialogs
run in [the portal machine](portal-machine.md). On a phone they run on the
host, and derisk's own portal backend draws their consent dialogs with
`derisk-gpui ask`.

- [Danube](danube.md) is the web browser: WPE WebKit in an mcsapi window.
- [Ad blocking](adblock.md) uses filter lists and covers every app.
- [Schizo mode](schizo-mode.md) sends every connection through a Tor gateway
  machine, like Whonix, and blocks traffic while the gateway is down.
- [`derisk-terminal`](terminal.md) renders on the GPU, or in software without
  one, and uses the Nerd Font build of the theme's monospace font.
- [The Agent](agent.md) runs Claude Code in its own window, with live previews
  of the apps it builds. It is on the PC image only.

The command palette finds files by name and by content, PDFs and office
documents included. It searches an index of your home and any folders you add
on the **Privacy** page of Settings, which `derisk-index.service` keeps up to
date. Turning the index off on that page deletes it. The palette also takes
plain-language requests, and [Ask derisk](assistant.md) answers the ones its
rules don't cover with a small local model through Ollama.

The system font is Inter, the font of the LosOS web interface. Apps that never
read derisk's theme use it too. A theme can pick any installed font.

Animations play at full length for your first two hours at the desktop, then
speed up over a few days until they run four times faster. They never
disappear. derisk counts the time in `~/.local/state/derisk/familiarity`.
**Settings › Appearance › Familiar motion** turns this off or restarts the
count, and **Reduce motion** still swaps motion for cross-fades.

## Phones

The same OS builds as one generic system image (GSI) for Treble phones and
tablets on Linux 5.10 or newer, over [Halium](https://halium.org), in
`nixos/halium/`. The phone keeps its own kernel, its vendor HALs run in an LXC
container, and NixOS's systemd initrd goes in `init_boot`, or next to the
phone's ramdisk in `boot`. The [web flasher](web-flasher.md) installs it from a
browser.

> [!WARNING]
> The GSI has not run on a device yet. [Halium GSI](halium.md) explains how it boots.

## What this repository writes itself

nixpkgs provides the packages. This repository adds:

- `nixos/`, the modules that assemble the OS.
- `src/losos-security`, the OS half of the [security report](security-report.md), on the system bus.
- `src/losos-swap`, which sizes a swap partition to the machine's RAM for `systemd-repart`.
- `src/losos-installer`, the backend behind `derisk installer` on the
  [installer](installer.md) ISO.
- `plugins/`, pm plugins that let pm build files call `nix` and the image tools.
- `proxy/`, the GHCR binary cache and update proxy, which CI also uses.
- `tools/nix-cache-push`, which CI uses to fill that cache.

This repository used to build the OS a second time from about ninety pm
recipes. [What maps to what](from-the-pm-tree.md) says where each piece went.
One thing has no replacement: the recipe tree built everything with cross-DSO
control-flow integrity (CFI) and ThinLTO. nixpkgs enables fortify, the stack
protector, stack clash protection, RELRO, bind-now and zeroed call-used
registers, but no CFI.

[losos](https://codeberg.org/dasmatus/losos) is the headless sibling: an
appliance with no desktop. This repository copies its house style but none of
its technical decisions.

## Where to start

- [Building](building.md) the image, the installer ISO and a release.
- [Everything systemd, and the exceptions](systemd.md).
- [What this trusts from outside](trust.md).
- [What is not done](not-done.md).

LosOS Desktop is licensed AGPL-3.0-or-later.
