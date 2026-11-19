# What this trusts from outside

Everything the flake builds from, besides this repository:

- **nixpkgs `nixos-unstable`**, the channel tarball from
  `channels.nixos.org`, pinned by `narHash` in `flake.lock`. A different
  tarball fails the hash.
- **Stock package binaries from cache.nixos.org.** The image uses nixpkgs'
  normal glibc package set without libc-specific package overrides, so Nix
  can substitute the closures the cache has instead of rebuilding them
  locally.
- **Upstream sources nixpkgs fetches**, each pinned by hash in nixpkgs, for
  packages that must be built locally.
- **Fixed-output inputs in the build closure.** CI preloads these from
  cache.nixos.org where it can, with `tools/nix-fetch-sources`, so that a cold
  build does not depend on every upstream being up. The selection includes
  source archives, and it can also include pinned bootstrap tools or prebuilt
  upstream compilers, so it is not only source. Each copied flat or NAR output
  is checked against its derivation's hash, and outputs that use other hash
  methods are rejected.
- **pm**, the `pm` flake input from `github.com/losos-project/pm`, pinned by
  the revision `flake.lock` records, built by `nixos/pkgs/pm.nix`.
- **crates.io**, for the dependencies of pm, losos-security,
  losos-installer and losos-hardware, each pinned by `Cargo.lock` and checked
  by hash.
- **Purism's adaptive GTK patches**, copied into
  `nixos/pkgs/patches/gtk3` and `gtk4` from PureOS's packaging on
  source.puri.sm, `Librem5/debs/gtk4` and `sebastian.krzyszkowiak/gtk`,
  branch `pureos/latest`. They are reviewed source in this repository, not
  fetched at build time.
- **Device firmware**, from nixpkgs' `linux-firmware`, pinned by hash like any
  source. It is prebuilt by the hardware vendors, and nothing can compile it.
  `hardware.nix` ships it because amdgpu and nouveau cannot start current
  GPUs without it.
- **NVIDIA's driver**, the production branch nixpkgs pins by hash, fetched
  from `download.nvidia.com`. The userspace libraries and GSP firmware are
  NVIDIA's binaries, which its licence lets the image and the binary cache
  redistribute unmodified. They are the image's only unfree package, added
  by `nixos/modules/nvidia.nix`, and they load only on a machine with
  [a card the driver supports](drivers.md). The
  open kernel module is built from NVIDIA's MIT/GPL source.
- **derisk**, the `derisk` flake input from `github.com/losos-project/derisk`,
  pinned by the revision `flake.lock` records, built by
  `nixos/pkgs/derisk.nix`. Its crates, mcsapi among them, are pinned by its
  `Cargo.lock` and `cargoHash`. It is the desktop, the display manager and
  the portal backend. The `mcsapi` flake input is the same mcsapi commit,
  and builds `x2mcsapi`.
- **ocrs' OCR models**, `text-detection.onnx` and `text-recognition.onnx`,
  fetched by `nixos/pkgs/derisk.nix` from ocrs' own bucket,
  `ocrs-models.s3-accelerate.amazonaws.com`, and pinned by hash. The URLs
  carry no version, so a changed file fails the hash rather than changing
  the build. derisk reads every screenshot's text with them to black out
  its secrets ([Screenshots](screenshots.md)). They are data that
  [rten](https://github.com/robertknight/rten) runs, with no code in them.
- **The LosOS terminal client**, `losos`, built from the LosOS repository at
  `github.com/dasmatus/losos`, at the commit `nixos/pkgs/losos-pin.nix`
  names. It is a sparse `fetchgit` of the directories it builds from, checked
  by hash, with its crates pinned by its `Cargo.lock` and `cargoHash`. A
  second sparse fetch of the same commit, the box's modules and lososd's
  crates, is read only by `nix flake check`, to boot a box in
  `nixos/tests/losos-box.nix`; the image and the release never do. The LosOS
  repository's release workflow proposes each new pin as a pull request. See
  [LosOS boxes](losos-boxes.md).
- **Home Manager**, `github.com/nix-community/home-manager`, pinned by
  revision and `narHash` in `flake.lock`. Only `nix flake check` reads it, to
  build `homeModules.pm` into a Home Manager generation in
  `nixos/tests/home-manager-pm.nix`. The image and the release never do.
- **Halium's generic system image**, from `nixos/pkgs/halium-gsi.nix`. It is
  the Android 14 system that the [Halium GSI](halium.md) runs the vendor's
  HALs under, UBports' build of Halium's AOSP tree as Droidian publishes it in
  its apt repository. It is pinned by the hash that repository's index gives
  for the package. It is prebuilt bionic code that nothing here can compile,
  and it runs as host root in the Android container, so it is trusted as much
  as the vendor blobs beside it. The phone's own kernel, vendor partitions and
  bootloader are the device maker's, and the GSI trusts them as Android does.
- **Fedora's signed shim**, from `nixos/pkgs/shim-signed.nix`: shim and
  MokManager 16.1 as Fedora's build system published them, pinned by hash,
  and signed by Microsoft after shim-review. They are prebuilt, since
  rebuilding them would lose Microsoft's signature, and they are the first
  code a PC with [Secure Boot](secure-boot.md) on runs from the ESP. shim
  also trusts Fedora's own certificate, as any machine that trusts
  Microsoft's third-party CA already does through Fedora's shim.
- **Flathub**, for apps installed later. Its repo file, with the signing key
  every install is checked against, is `nixos/modules/flathub.flatpakrepo` in
  this tree. The image adds the remote from that copy at boot and never
  fetches the key. The apps themselves are Flathub's builds, sandboxed by
  Flatpak and verified against that key, and nothing in the image depends on
  any of them.
- **The project's [binary cache](binary-cache.md)** in GHCR, through `proxy/`.
  It holds optional paths that the CI job stores and signs. A client checks
  every narinfo against the public key it was told to trust, so the proxy and
  GHCR carry bytes but cannot vouch for them.
- **In CI only**, the official Nix installer from `releases.nixos.org`,
  pinned by version. Nix is needed to build Nix, so this binary is the one
  bootstrap. After it, `.#nix` is the nixpkgs Nix package for a build host.

## What may be built from source

Only the project's own repositories are compiled here: this one, and derisk,
mcsapi and pm, which `flake.lock` pins under `losos-project`. Everything else
the image runs is one of two things.

- **A nixpkgs package**, which substitutes from cache.nixos.org. That is the
  whole of nixpkgs, with the packages this overlay patches — GTK 3 and 4 and
  Qt 6, which get the adaptive and icon-theme patches ([GTK and Qt on a
  phone](gtk-qt-phone.md)), and GRUB, whose menu editor is patched out
  ([Kernel hardening](kernel-hardening.md)). Their source and their hash are
  nixpkgs', so they trust nothing outside it; they compile because a patched
  package is not the one the cache holds, and no cache can hold it.
- **A distributor's prebuilt package**, unpacked as it was published and
  never compiled: Fedora's signed shim, Droidian's generic system image,
  Google's Android SDK and emulator images, and Arch's cloud image that the
  Danube machine boots. Each is pinned by the distributor's own version and
  a hash in this tree, as the list above says.

What would otherwise need a patch on someone else's code is set at run time
instead: a sysctl, a kernel command line, an environment variable, a
configuration file. `kernel-hardening.nix` is the model — nixpkgs' kernel,
from cache.nixos.org, with lockdown and the IOMMU asked for on the command
line — and `tools/source-policy` is the check.

It reads every fetch in the tree and fails on one whose origin is neither
`losos-project`, nor nixpkgs, nor a distributor's package, and it runs as
`checks.<system>.source-policy` in `nix flake check`. It prints the debt it
knows about on every run, each entry with its reason: the boxes client
(`losos`) and the admin panes, whose repository is not yet under
`losos-project` and which publish no binaries; Reims' GPU backend, which no
distributor packages; and libhybris with its Android headers, which no
distributor builds with the GLVND vendor layout [Halium](halium.md) needs. A
new fetch has to be argued for in that script, and the list is meant to
shrink.

## The component inputs

derisk, mcsapi and pm are flake inputs (`flake.nix`), and the flake builds
them from the revisions `flake.lock` pins each at, which `nixos/pkgs/
sources.nix` fetches with `builtins.fetchGit`. The lock's revision and
`narHash` are the pin: a fetch that yields a different tree fails it, and
`nix flake metadata` shows exactly what the image builds. A clone needs no
flags, since the sources are fetched at evaluation, not checked out. To move
one, run `tools/update-lockfiles`, or by hand `nix flake lock --override-input
<name> git+https://github.com/losos-project/<name>?rev=<rev>`, and set a new
`cargoHash` in its `nixos/pkgs/*.nix` if its `Cargo.lock` changed. There is no
source hash to update, because the locked commit already names the tree.
Running `tools/nix-refresh-hashes --list > before.txt` before the move and
`tools/nix-refresh-hashes before.txt` after it finds and rewrites that
`cargoHash`. derisk's own `Cargo.lock` still fetches mcsapi by git revision,
so keep the `mcsapi` input at the revision that lock names.
Every Monday the lockfile update moves all three to releases ([Keeping the
pins current](#keeping-the-pins-current)), and a move by hand in between
stays.

The sources are read from the lock rather than passed through the overlay,
because the overlay is imported standalone — by the OS's own module set
(`nixos/modules/base.nix`) and by every VM test (`nixos/tests/*.nix`) — and
threading three arguments through all of those would buy the store path the
lock already names.

## Keeping the pins current

Every Monday the `lockfiles` workflow, `.github/workflows/lockfiles.yml`,
runs `tools/update-lockfiles`. A person can run it the same way. It does
four things:

- `nix flake update` moves nixpkgs and Home Manager in `flake.lock`, named,
  so it cannot move the component inputs below a second way.
- `cargo update` moves every `Cargo.lock`, and
  `npm update --package-lock-only` every `package-lock.json`, each only as
  far as its manifest allows, with cargo and npm from the new nixpkgs.
- The component inputs move to releases. derisk, mcsapi and pm publish
  GitHub releases on the same cadences as the OS ([Release
  cadences](releases.md#release-cadences)), and cut their weekly ones at
  02:17 UTC, before this runs. The `derisk` and `pm` inputs move to their
  repository's newest weekly release, and `mcsapi` to the revision derisk's
  `Cargo.lock` names at that release, so the two agree. A pin only moves
  forward, so one moved past the release by hand stays until a newer
  release passes it. Inside those repositories nothing else moves, since
  each repository updates its own lockfiles the same way.
- `tools/update-packages` moves the custom packages' own upstream pins,
  each forward only and with the hash its upstream source lists, so a
  fetch no runner can reach never leaves a stale hash behind:
  libhybris to its master's head, android-headers to the head of
  the halium-16.0 branch that every supported Halium base shares,
  halium-gsi to Droidian's newest `android-system-gsi-34` in its trixie
  repository, and the Android emulator to Google's stable channel, whose
  zip is downloaded once to hash and checked against the SHA-1 Google
  lists. The emulator's system image, shim-signed's Fedora build and
  reims' chosen commit stay hand decisions, and the losos client's pin is
  the losos repository's own desktop-pin workflow's, which raises its own
  pull request here on each release.
- `tools/nix-refresh-hashes` then re-fetches every fixed output pinned in
  this repository whose derivation those changes touched, and writes back
  the hash it really has. It has to re-fetch, because a stale hash still
  names the old store path, so a machine or cache that has the old output
  builds with it and never notices. losos-docs' npm hash moves this way
  whenever `website/package-lock.json` does.

The result is one pull request, `chore(deps): update lockfiles`, pushed to
a `lockfiles/update` branch and set to merge when its checks pass. The
checks are the same CI a pull request of any kind gets, so a lockfile that
breaks the build shows there before it reaches main, and the squash merge
lands as that commit on main, from which a green `ci` publishes that
night's release.

> [!IMPORTANT]
> The workflow needs `LOCKFILE_UPDATE_TOKEN`, a fine-grained token with
> Contents and Pull requests write. Main's rules refuse a direct push, and
> they wait for the pull request's checks to pass before they let a merge
> through — which is exactly what the pull request is for. The token, and
> not Actions' own, is what makes this work at all: a branch or merge
> pushed with Actions' token starts no workflow, so the checks would never
> run and the rules would never let the merge through.

android_translation_layer stays a `fetchgit` pin. It is an upstream fork,
off by default, and its tree is close to half a gigabyte, which the flake
would otherwise copy on every evaluation.

The LosOS client is a sparse `fetchgit` pin, because its repository is about
140 MB and the client needs 3 MB of it.
