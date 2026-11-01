# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

## What this is

`losos-desktop` is a derisk desktop OS written as a NixOS configuration. It
installs as an image, uses systemd for everything it can, and uses nixpkgs'
stock glibc packages.
`flake.nix` and `nixos/` are the OS. nixpkgs `nixos-unstable`, pinned in `flake.lock`,
provides stock packages, normally substituted from cache.nixos.org.
[`pm`](https://github.com/losos-project/pm) ships in the image as the system
manager. derisk, mcsapi and pm are git submodules under `components/`, and
their recorded commits are the pins (`docs/trust.md`, "The components/
submodules"); clone with `--recurse-submodules`.

`README.md` points at the documentation. `docs/` is the documentation, one
page per subject: how each piece is built, which outside inputs the build
trusts (`docs/trust.md`), and what is not done (`docs/not-done.md`). Read the
pages a change touches before changing anything in `nixos/`. `docs/nixos.md`
was once all of it in one file; comments still cite its section names, and
it now maps each one to its page.

## Build & develop

```sh
nix build              # the disk image (.#image)
nix build .#release    # what a release uploads, with SHA256SUMS
nix flake check        # both architectures, losos-security's tests, VM boot with KVM
nix fmt                # nixfmt; CI runs `nix fmt -- --ci`
just plugins           # pm's plugin components, for a pm outside the image
```

Evaluate both architectures before trusting a change:

```sh
nix eval --raw .#nixosConfigurations.losos-desktop-x86_64.config.system.build.toplevel.drvPath
nix eval --raw .#nixosConfigurations.losos-desktop-aarch64.config.system.build.toplevel.drvPath
nix eval --raw .#nixosConfigurations.losos-desktop-gsi-aarch64.config.system.build.toplevel.drvPath
```

That runs every module and assertion in a few minutes. A full image build
still takes time, so don't start one to check a module edit.

## Layout

- `nixos/modules/` has one file per concern: `boot`, `disk`, `update`,
  `accounts`, `setup`, `desktop`, `services`, `hardware`, `installer`,
  `windows-installer`, `pm` and a few more. `base.nix` imports what every
  target shares; `default.nix` adds the PC half (UEFI, verity `/usr`, sysupdate, installer).
- `nixos/halium/` is the Halium GSI, one image for every Treble phone that
  takes GSIs and runs Linux 5.10 or newer: `base.nix` plus NixOS's initrd as
  `init_boot` (or after the phone's own ramdisk in `boot`),
  userdata as the root, the vendor's partitions mapped out of `super`, and
  its HALs under Halium's generic system image in an LXC container. No device
  ports. `docs/halium.md` says how it boots.
- `nixos/installer/` is the installer ISO's live system: NetworkManager
  and `derisk installer` on tty1, with `losos-installer
  serve` as its backend. `modules/installer.nix`
  evaluates it with the OS's own repart definitions and sysupdate transfers.
- `nixos/guest/` holds the machine templates the image ships for its
  virtual machine desktop (`docs/hypervisor.md`): `common.nix`, a NixOS
  guest with the QEMU guest agent, a waypipe server over vsock and
  `losos-guest-run`, under `default.nix`, the LosOS guest, `boxes.nix`,
  the LosOS boxes machine that holds `losos` and is the one machine that
  talks to LosOS boxes (`docs/losos-boxes.md`), and `flatpak.nix`, the
  Flatpak machine, where a PC's Flatpak apps run (`modules/flatpak-machine.nix`).
  `modules/machines.nix` evaluates each and installs it as
  `share/losos/machines/<name>.qcow2` with its app list, `<name>.apps`, and
  makes `losos.machines.system`'s machines on the host.
- `nixos/home/pm.nix` is the flake's `homeModules.pm`: pm and its plugins
  installed and signed by a user's own Home Manager (`docs/pm.md`).
- `nixos/pkgs/` is the overlay, and holds only what nixpkgs lacks: pm, its
  plugins, `losos-installer`, `losos-security`, `losos-swap`, `losos-backup`
  (`docs/backup.md`), `losos-docs`
  (`website/` built for reading offline), `losos`, the LosOS boxes'
  terminal client (`docs/losos-boxes.md`), the Danube browser and the WPE
  WebKit it draws (`docs/danube.md`), `losos-adblock` (`docs/adblock.md`),
  the pm payload builder, and Halium's `libhybris`, `android-headers` and generic
  system image (`halium-gsi`).
  It also replaces `gtk3`, `gtk4` and Qt 6's `qtbase` with builds carrying
  `patches/gtk3`, `patches/gtk4` and `patches/qtbase`, which make their apps
  fit a phone (`docs/gtk-qt-phone.md`); those and their
  dependents rebuild in CI instead of substituting. `patches/` also holds
  the two gnome-control-center patches, which target 51.0 and nothing
  applies.
- `components/` holds derisk, mcsapi and pm as git submodules. The flake
  builds them from there; `nixos/pkgs/{derisk,x2mcsapi,pm}.nix` keep only
  their `cargoHash` and build flags.
- `src/` holds the programs this repository writes. The overlay builds
  them; `losos-windows-installer` is also cross-built to a Windows `.exe` by
  `modules/windows-installer.nix` (`docs/windows-installer.md`).
- `plugins/` holds pm plugins, written in Rust and compiled to WebAssembly
  components. pm asks a plugin about a command only when no built-in
  fingerprint matched it.
- `proxy/` is an edge function with its logic in Rust compiled to
  WebAssembly. It serves the Nix binary cache and sysupdate's files from GHCR,
  streams the GSI's files to the web flasher (`/flasher/`), and counts
  active users (`/ping`, `docs/choice-screens.md`). `proxy/serve.ts` runs it
  with node, which is how CI reads the Nix cache. A hosted deployment is
  optional (`LOSOS_PROXY_URL`): without it images update from GitHub
  Releases through `website/release-index.js`'s index on Pages and
  `modules/update-source.nix` on the machine (`docs/releases.md`).
- `website/` is the Docusaurus site that publishes `docs/` on GitHub Pages,
  with the web flasher (`website/static/flasher/`, `docs/web-flasher.md`) beside it;
  `website/wiki.js` writes the same pages for the GitHub wiki, and
  `DOCS_OFFLINE=1` builds the copy the image carries (`modules/docs.nix`),
  which derisk's error dialogs open a section of (`docs/troubleshooting.md`). A new page in
  `docs/` goes in `website/sidebars.js` too, or both fail. Pages link each
  other as `page.md`, which GitHub, Docusaurus and the wiki all follow.
  Callouts are GitHub's `> [!NOTE]` (also TIP, IMPORTANT, WARNING,
  CAUTION), which GitHub and the wiki draw themselves and
  `website/callouts.js` turns into Docusaurus admonitions; `:::` would show
  as text on GitHub. Keep each one a paragraph a reader must not miss.
  Pictures live in `docs/images/` and pages show them as `images/<file>`;
  the diagrams there are hand-written SVG in the OS's dark theme.
  The site's root is a landing page, `website/src/pages/index.js`, which
  lists every page from `sidebars.js` (`website/landing-data.js`), and
  `docs/index.md` is served at `/overview` instead.
- `tools/nix-cache-push` pushes built store paths into that cache.
- `tools/nix-fetch-sources` preloads fixed-output inputs in CI; copied outputs
  are hash-checked and can include bootstrap tools as well as source archives.

## Gotchas that bite silently

- **Use nixpkgs' standard platforms and packages.** `nixpkgs.hostPlatform` is
  `x86_64-linux` or `aarch64-linux`; avoid libc-specific platform or package
  overrides so stock closures substitute from cache.nixos.org.
- **`/usr` is the Nix store, on dm-verity.** The root hash is `usrhash=` on the
  UKI's command line, so a different `/usr` needs a different UKI, and root
  holds only state. An update is a new `/usr` from systemd-sysupdate. Nothing
  ever switches a generation, which is why the image sets `nix.enable = false`.
- **pm's jail mounts `/nix/store` read-only and has no daemon socket.** A pm
  step that runs nix uses `nix --store /build/nix ...`. The `losos-nix` plugin
  denies network to a step that passes `--offline`. pm grants network per
  build file, so one networked step puts the whole file on the host network.

## What was here, and is not coming back

This repository used to build the same OS a second time, as a from-source
distribution built by pm. It had ninety-odd recipes under `recipes/`, Python
under `tools/` that composed them into layer bundles, a sysroot assembled by
`share/`, OS files in `overlay/`, a `Containerfile` for the build host, and
gates that re-implemented pm's fingerprint table. nixpkgs builds every one of
those packages and the flake already wired them together, so the second build
was two of everything to maintain. It did one thing nixpkgs doesn't: it
compiled the whole tree with cross-DSO CFI and ThinLTO. That went with it, and
`docs/index.md` says so. The history before the removal has all of it,
including `docs/pm-constraints.md` and the C1 to C11 constraints its comments
cited.

## CI

`.github/workflows/ci.yml` is the main workflow. `changes` runs first and
reads the paths a change touches through `tools/ci-changes`, and every other
job runs only when that says it can be affected: a pull request that edits
only `docs/` builds the offline documentation (`docs`) instead of the
image, one that edits only the proxy runs `proxy`, a draft builds no image,
and a push to main that changes the image runs everything. A path the
script does not name counts as the image's. `prebuild` builds every
package the release needs (the patched GTK and Qt, WPE WebKit, what links them, and this
repository's programs) for each architecture first. Nothing lists them:
`tools/nix-prebuild-plan` keeps the derivations that do not change when
`losos.version` does. It pushes them to the GHCR cache outside a pull
request and hands what it compiled to `flake` as an artifact. `flake`
imports that, fails if any of the plan would still compile, then builds
`.#release` within a time budget and pushes project-built paths to
the GHCR cache. When
a build completes it checks the flake, boots the VM tests (a failed one is a
warning and a line in the job summary, never a red job), and outside a pull
request hands the release to `push` as an artifact. `format` runs `nix fmt -- --ci` on its own
in a few minutes, so run `nix fmt` before pushing any `.nix` edit, including
one made in GitHub's web editor. `push` uploads it to
GHCR. It is the only job holding `packages: write` beside release files, and
it checks out no code. `publish` moves the `images:nightly-<arch>` tag that
`proxy/` serves updates from, creates that run's own release,
`<cadence>-<YYYYMMDD>T<HHMM>` (a prerelease for a nightly), with both
architectures' files and a name such as "Quiet Queue", and starts
`docs.yml` so the site's release index follows it; a run started by hand
takes a `cadence` and an optional `title` (`docs/releases.md`).
`cadence.yml` runs daily, reads the GitHub releases' tags
(`nightly-`, `weekly-`, `monthly-`, `yearly-`) and starts `ci` on the newest
nightly's tag for each cadence due a release; only a yearly release is marked
latest (`docs/releases.md`, "Release cadences").
`proxy` tests the proxy and deploys nothing. `ci` is the check branch
protection reads; it fails when a job `changes` asked for did not pass, and
accepts the skipped rest. `docs.yml` builds the site and the wiki pages on a pull
request, and from main deploys the site to Pages and pushes the wiki.

## Conventions

- Commits are Conventional Commits (`feat:`, `fix:`, `docs:`), written from the
  diff. **No attribution trailers of any kind** — no `Co-Authored-By`, no
  "Generated with", no session link, no tool name in a comment or doc header.
  The sibling `losos` enforces this with a commit-msg hook.
- Licence is AGPL-3.0-or-later via the blanket `REUSE.toml`. **No per-file SPDX
  headers**: a module's header comment is scarce space, spent on what it does
  and why.
- Comment density follows `losos`: every non-obvious line carries the reason it
  is there, in prose, at the point of use. Removed things get a tombstone
  comment saying why they are not coming back.
