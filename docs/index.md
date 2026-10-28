# LosOS Desktop

LosOS Desktop is a [derisk](https://github.com/losos-project/derisk) desktop
operating system. It uses systemd for everything it can, installs as an
image, updates with `systemd-sysupdate`, and makes every user a
`systemd-homed` LUKS volume.

The OS is a NixOS configuration, `flake.nix` plus the modules in `nixos/`.
nixpkgs from the `nixos-unstable` channel, pinned by `flake.lock`, provides the
stock glibc packages for `x86_64-linux` and `aarch64-linux`. This repository
decides how those packages fit together. Nix fetches them from
cache.nixos.org. Paths CI has already built come from this project's GHCR
cache, which `proxy/`'s code reads; [Binary cache](binary-cache.md) explains
it. [pm](https://github.com/losos-project/pm) ships in the image as the system
manager. A user of the running system builds and runs software with it, as
[pm on LosOS](pm.md) describes.

![The derisk overview: the open windows as tiles, workspaces along the top, and the clock, suggested apps, services, calendar and notes beside them](images/overview.png)

Apps the image does not carry are Flatpaks from
[Flathub](https://flathub.org), installed through derisk's own
[Store](store.md). On a PC they run in [a machine of their
own](hypervisor.md#the-flatpak-machine), and that machine's GTK portal backend
serves their dialogs. On a phone they run on the host. There derisk's own
portal backend gives them its theme, screenshots and the wallpaper, and asks
for consent in its own dialog, `derisk-gpui ask`, drawn with the same toolkit
as derisk's GPUI apps. GTK's backend covers the file chooser and the rest.

The desktop is lettered in Inter, the font of the LosOS web interface. It is
derisk's default theme font and the system's sans-serif, so apps that never
read derisk's theme, web pages among them, use it too. A theme can name any
installed font instead.

The web browser is [Danube](danube.md), WPE WebKit inside an mcsapi window.
[Ad blocking](adblock.md) works from filter lists and covers every app.
[Schizo mode](schizo-mode.md) sends every connection through Tor by way of a
gateway machine like Whonix's, and lets nothing out while that gateway is
down. The terminal is [derisk's own](terminal.md), `derisk-terminal`. It
draws the theme's monospace font from its Nerd Font build, so the icons that
prompts and TUIs print show up. It renders on the GPU, or in software where there is
no GPU. [The Agent](agent.md), on the PC image, runs Claude Code in its own
window with live previews of the apps it builds, and derisk's palette can
hand it a request.

derisk's command palette finds files by name and by content, and shows the
line that matched. It searches an index of each person's home that
`derisk-index.service` updates as files change. The palette also takes
requests in plain words. [Ask derisk](assistant.md) handles the ones its
rules don't cover with a small language model that runs on the computer,
through Ollama.

derisk moves the way the launch film does, and speeds up as each person gets
used to it. It counts the time they spend at the desktop, unlocked and not
dimmed, in `~/.local/state/derisk/familiarity`. For the first two hours
every animation plays at full length. After that they speed up, and after a
few days of use they settle at four times the original speed. Animations get
shorter but never disappear, so a familiar desktop still shows where things
went. Settings › Appearance › Familiar motion turns this off or restarts the
count. Reduce motion still replaces the motion with cross-fades.

The same OS also builds as one generic system image for Treble phones and
tablets that take GSIs and run Linux 5.10 or newer, over
[Halium](https://halium.org), in `nixos/halium/`. NixOS's systemd initrd goes
in the phone's `init_boot`, or beside its own ramdisk in `boot`. The phone
keeps its own kernel, and its vendor HALs run in an LXC container. The
[web flasher](web-flasher.md) installs it from a browser.

> [!WARNING]
> The GSI has not run on a device yet; [Halium GSI](halium.md) says how it boots.

[losos](https://codeberg.org/dasmatus/losos) is the headless sibling. It is an
appliance with no desktop, no graphical session and no seat management, by
design. This repository copies its house style and none of its technical
decisions.

## What went away

This repository used to build the same OS a second time, as a from-source
distribution of ninety-odd pm recipes. [What maps to what](from-the-pm-tree.md)
says where each of its pieces went. One thing went with it and has no
replacement. The recipe tree compiled every package with cross-DSO
control-flow integrity and ThinLTO, from a toolchain it configured itself.
nixpkgs on this platform enables fortify, the stack protector, stack clash
protection, RELRO, bind-now and zeroed call-used registers, going by a
derivation's `NIX_HARDENING_ENABLE`. It enables no CFI.

## What this repository writes itself

nixpkgs provides the packages. This repository provides the rest:

- `nixos/`, the modules that put nixpkgs' packages together into this OS.
- `src/losos-security`, which serves the OS half of the
  [security report](security-report.md) on the system bus.
- `src/losos-swap`, which writes a swap partition definition sized to the
  machine's RAM for `systemd-repart`.
- `src/losos-installer`, the backend that `derisk installer` runs on the
  installer ISO. It lists the disks, partitions the chosen one with
  `systemd-repart` and installs the channel's newest release with
  `systemd-sysupdate`. See [The installer](installer.md).
- `plugins/`, pm plugins that let pm build files call `nix` and the image
  tools.
- `proxy/`, the GHCR binary cache and update proxy. CI also runs it on its
  runners to reach the cache.
- `tools/nix-cache-push`, which CI uses to fill that cache.

## Where to start

- [Building](building.md) the image, the installer ISO and a release.
- [Everything systemd, and the exceptions](systemd.md): which systemd
  component does which job.
- [What this trusts from outside](trust.md): every input the build takes
  from somewhere else.
- [What is not done](not-done.md): the known gaps.

LosOS Desktop is licensed AGPL-3.0-or-later.
