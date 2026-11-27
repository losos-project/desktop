# What this trusts from outside

Everything the flake builds from, other than this repository, and how each
input is pinned.

- nixpkgs `nixos-unstable`: the channel tarball from `channels.nixos.org`,
  pinned by `narHash` in `flake.lock`. A different tarball fails the hash.
- Stock binaries from cache.nixos.org. The image uses nixpkgs' normal glibc
  package set with no libc-specific overrides, so Nix substitutes cached
  closures instead of rebuilding them.
- Upstream sources that nixpkgs fetches, each pinned by hash in nixpkgs, for
  packages built locally.
- Fixed-output inputs in the build closure. CI preloads these from
  cache.nixos.org where it can, with `tools/nix-fetch-sources`, so a cold build
  doesn't depend on every upstream being up. They include source archives and
  can include pinned bootstrap tools or prebuilt upstream compilers. Each
  copied flat or NAR output is checked against its derivation's hash, and
  outputs using other hash methods are rejected.
- pm: the `pm` flake input from `github.com/losos-project/pm`, pinned by
  revision in `flake.lock`, built by `nixos/pkgs/pm.nix`.
- derisk: the `derisk` flake input from `github.com/losos-project/derisk`,
  pinned by revision in `flake.lock`, built by `nixos/pkgs/derisk.nix`, with
  its crates (mcsapi among them) pinned by its `Cargo.lock` and `cargoHash`.
  It is the desktop, the display manager and the portal backend. The `mcsapi`
  flake input is the same mcsapi commit and builds `x2mcsapi`.
- crates.io, for the dependencies of pm, losos-security, losos-installer and
  losos-hardware, each pinned by `Cargo.lock` and checked by hash.
- Device firmware from nixpkgs' `linux-firmware`, pinned by hash. Vendors ship
  it prebuilt and nothing can compile it. `hardware.nix` ships it because
  amdgpu and nouveau can't start current GPUs without it.
- NVIDIA's driver: the production branch nixpkgs pins by hash, from
  `download.nvidia.com`. The userspace libraries and GSP firmware are NVIDIA
  binaries that its licence lets the image and binary cache redistribute
  unmodified. They are the image's only unfree package, added by
  `nixos/modules/nvidia.nix`, and load only with
  [a supported card](drivers.md). The open kernel module is built from
  NVIDIA's MIT/GPL source.
- ocrs' OCR models, `text-detection.onnx` and `text-recognition.onnx`, fetched
  by `nixos/pkgs/derisk.nix` from `ocrs-models.s3-accelerate.amazonaws.com` and
  pinned by hash. The URLs carry no version, so a changed file fails the hash.
  derisk reads every screenshot's text with them to black out secrets
  ([Screenshots](screenshots.md)). They are data that
  [rten](https://github.com/robertknight/rten) runs, with no code in them.
- The LosOS terminal client, `losos`, from `github.com/dasmatus/losos` at the
  commit in `nixos/pkgs/losos-pin.nix`: a sparse `fetchgit` of the directories
  it builds from, checked by hash, with its crates pinned by `Cargo.lock` and
  `cargoHash`. A second sparse fetch of the same commit, the box's modules and
  lososd's crates, is read only by `nix flake check` to boot a box in
  `nixos/tests/losos-box.nix`. The LosOS repository's release workflow proposes
  each new pin as a pull request ([LosOS boxes](losos-boxes.md)).
- Home Manager, `github.com/nix-community/home-manager`, pinned by revision and
  `narHash` in `flake.lock`. Only `nix flake check` reads it, to build
  `homeModules.pm` in `nixos/tests/home-manager-pm.nix`.
- Halium's generic system image (`nixos/pkgs/halium-gsi.nix`): the Android 14
  system the [Halium GSI](halium.md) runs vendor HALs under, UBports' build of
  Halium's AOSP tree as Droidian publishes it, pinned by the hash in Droidian's
  apt index. It is prebuilt bionic code that runs as host root in the Android
  container, so it is trusted as much as the vendor blobs next to it. The
  phone's kernel, vendor partitions and bootloader are the device maker's, and
  the GSI trusts them as Android does.
- Fedora's signed shim (`nixos/pkgs/shim-signed.nix`): shim and MokManager
  16.1 as Fedora's build system published them, pinned by hash and signed by
  Microsoft after shim-review. They stay prebuilt, because a rebuild would lose
  Microsoft's signature, and they are the first code a PC with
  [Secure Boot](secure-boot.md) runs from the ESP. shim also trusts Fedora's
  certificate, as any machine trusting Microsoft's third-party CA already does.
- Flathub, for apps installed later. Its repo file, with the signing key every
  install is checked against, is `nixos/modules/flathub.flatpakrepo` in this
  tree. The image adds the remote from that copy at boot and never fetches the
  key. Apps are Flathub's builds, sandboxed by Flatpak, and nothing in the
  image depends on them.
- The project's [binary cache](binary-cache.md) in GHCR, through `proxy/`. It
  holds optional paths that CI stores and signs. Clients check every narinfo
  against the public key they trust, so the proxy and GHCR carry bytes but
  can't vouch for them.
- In CI only, the official Nix installer from `releases.nixos.org`, pinned by
  version. Nix is needed to build Nix, so this binary is the one bootstrap.
  After it, `.#nix` is nixpkgs' Nix package for a build host.

## The component inputs

derisk, mcsapi and pm are flake inputs (`flake.nix`). `nixos/pkgs/sources.nix`
fetches each at the revision `flake.lock` pins with `builtins.fetchGit`. The
revision and `narHash` are the pin: a fetch that yields a different tree fails,
and `nix flake metadata` shows exactly what the image builds. A clone needs no
flags, because the sources are fetched at evaluation.

To move one:

1. Run `tools/nix-refresh-hashes --list > before.txt`.
2. Run `tools/update-lockfiles`, or by hand
   `nix flake lock --override-input <name> git+https://github.com/losos-project/<name>?rev=<rev>`.
3. Run `tools/nix-refresh-hashes before.txt`, which rewrites the `cargoHash` in
   `nixos/pkgs/*.nix` if the component's `Cargo.lock` changed. There is no
   source hash to update, because the locked commit names the tree.

derisk's `Cargo.lock` still fetches mcsapi by git revision, so keep the
`mcsapi` input at the revision that lock names. The Monday lockfile update
moves all three to releases ([Keeping the pins current](#keeping-the-pins-current)),
and a manual move in between stays.

The sources come from the lock, not through the overlay, because the overlay is
imported standalone by the OS's module set (`nixos/modules/base.nix`) and by
every VM test (`nixos/tests/*.nix`). Threading three arguments through all of
those would only produce the store path the lock already names.

## Keeping the pins current

Every Monday the `lockfiles` workflow (`.github/workflows/lockfiles.yml`) runs
`tools/update-lockfiles`, which you can also run by hand. It:

- Runs `nix flake update` on nixpkgs and Home Manager by name, so the component
  inputs don't move a second way.
- Runs `cargo update` on every `Cargo.lock` and
  `npm update --package-lock-only` on every `package-lock.json`, within their
  manifests, using cargo and npm from the new nixpkgs.
- Moves the component inputs to releases. derisk, mcsapi and pm publish GitHub
  releases on the OS's cadences ([Release cadences](releases.md#release-cadences))
  and cut weekly ones at 02:17 UTC, before this runs. `derisk` and `pm` move to
  their newest weekly release, and `mcsapi` to the revision derisk's
  `Cargo.lock` names at that release. Pins only move forward, so one moved past
  the release by hand stays until a newer release passes it.
- Runs `tools/update-packages` to move the custom packages' upstream pins,
  forward only, with the hash each upstream lists, so an unreachable fetch
  never leaves a stale hash:
  - wpewebkit to the WPE release matching nixpkgs' WebKitGTK version.
  - libhybris to its master's head.
  - android-headers to the head of the halium-16.0 branch shared by every
    supported Halium base.
  - halium-gsi to Droidian's newest `android-system-gsi-34` in trixie.
  - The Android emulator to Google's stable channel, downloaded once to hash
    and checked against Google's SHA-1.

  The emulator's system image, shim-signed's Fedora build and reims' commit
  stay hand decisions. The losos client's pin belongs to the losos
  repository's desktop-pin workflow.
- Runs `tools/nix-refresh-hashes` to re-fetch every fixed output pinned here
  whose derivation changed, and write back its real hash. It must re-fetch,
  because a stale hash still names the old store path, and a machine or cache
  with the old output would build with it unnoticed. losos-docs' npm hash
  moves this way when `website/package-lock.json` does.

The result is one pull request, `chore(deps): update lockfiles`, on the
`lockfiles/update` branch, set to merge when its checks pass. It gets the same
CI as any pull request, so a lockfile that breaks the build fails there. The
squash merge lands on main, and a green `ci` publishes that night's release.

> [!IMPORTANT]
> The workflow needs `LOCKFILE_UPDATE_TOKEN`, a fine-grained token with
> Contents and Pull requests write. Main's rules refuse direct pushes and wait
> for the pull request's checks before a merge. A branch or merge pushed with
> Actions' own token starts no workflow, so without this token the checks never
> run and the merge never goes through.

android_translation_layer stays a `fetchgit` pin. It is an upstream fork, off
by default, and its tree is close to half a gigabyte, which the flake would
otherwise copy on every evaluation. The LosOS client is a sparse `fetchgit`
pin too, because the client needs 3 MB of a much larger repository.
