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
- **pm**, the `components/pm` submodule from `github.com/losos-project/pm`,
  at the commit this tree records, built by `nixos/pkgs/pm.nix`.
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
- **derisk**, the `components/derisk` submodule from
  `github.com/losos-project/derisk`, at the commit this tree records, built
  by `nixos/pkgs/derisk.nix`. Its crates, mcsapi among them, are pinned by
  its `Cargo.lock` and `cargoHash`. It is the desktop, the display manager
  and the portal backend. The `components/mcsapi` submodule is the same
  mcsapi commit, and builds `x2mcsapi`.
- **The LosOS terminal client**, `losos`, built from the LosOS repository at
  `github.com/dasmatus/losos`, at the commit `nixos/pkgs/losos.nix` names. It
  is a sparse `fetchgit` of the directories it builds from, checked by hash,
  with its crates pinned by its `Cargo.lock` and `cargoHash`. See
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

## The components/ submodules

derisk, mcsapi and pm are git submodules under `components/`, and the flake
builds them from there, through `self.submodules = true` in `flake.nix`, which
Nix 2.27 and later honour. The commit recorded for each submodule is the pin.
A clone needs `git clone --recurse-submodules`, or `git submodule update
--init`, and `git -C components/<name> log` shows exactly what the image
builds. To move one, run `git -C components/<name> checkout <rev>`, commit
here, and set a new `cargoHash` in its `nixos/pkgs/*.nix` if its `Cargo.lock`
changed. There is no source hash to update, because the submodule commit
already names the tree. Running `tools/nix-refresh-hashes --list > before.txt`
before the move and `tools/nix-refresh-hashes before.txt` after it finds and
rewrites that `cargoHash`. derisk's own `Cargo.lock` still fetches mcsapi by
git revision, so move `components/mcsapi` to the revision that lock names.
Every Monday the lockfile update moves all three to releases ([Keeping the
pins current](#keeping-the-pins-current)), and a move by hand in between
stays.

## Keeping the pins current

Every Monday the `lockfiles` workflow, `.github/workflows/lockfiles.yml`,
runs `tools/update-lockfiles`. A person can run it the same way. It does
three things:

- `nix flake update` moves nixpkgs and Home Manager in `flake.lock`.
- `cargo update` moves every `Cargo.lock` outside `components/`, and
  `npm update --package-lock-only` every `package-lock.json`, each only as
  far as its manifest allows, with cargo and npm from the new nixpkgs.
- The submodules move to releases. derisk, mcsapi and pm publish GitHub
  releases on the same cadences as the OS ([Release
  cadences](releases.md#release-cadences)), and cut their weekly ones at
  02:17 UTC, before this runs. `components/derisk` and `components/pm` move
  to their repository's newest weekly release, and `components/mcsapi` to
  the revision derisk's `Cargo.lock` names at that release, so the two
  agree. A pin only moves forward, so one moved past the release by hand stays
  until a newer release passes it. Inside the submodules nothing else
  moves, since each repository updates its own lockfiles the same way.
- `tools/nix-refresh-hashes` then re-fetches every fixed output pinned in
  this repository whose derivation those changes touched, and writes back
  the hash it really has. It has to re-fetch, because a stale hash still
  names the old store path, so a machine or cache that has the old output
  builds with it and never notices. losos-docs' npm hash moves this way
  whenever `website/package-lock.json` does.

The result is one commit, `chore(deps): update lockfiles`, pushed straight
to main with no pull request. Main's own `ci` run on it is the check, so a
lockfile that breaks the build shows there as any other push would, and a
green one publishes that night's release. If main moved while the update
ran, the commit is rebased onto it first. If the two touched the same lock,
nothing is pushed and the run fails, and running it again starts afresh.

> [!IMPORTANT]
> The workflow pushes with `LOCKFILE_UPDATE_TOKEN`, a fine-grained token with
> Contents write. GitHub starts no workflow for a push made with Actions' own
> token, so without the secret the update still lands on main but `ci` does
> not run on it until the next push. A branch rule that refuses direct pushes
> to main has to let the token's owner, or Actions, bypass it.

android_translation_layer stays a `fetchgit` pin. It is an upstream fork,
off by default, and its tree is close to half a gigabyte, which the flake
would otherwise copy on every evaluation.

The LosOS client is a sparse `fetchgit` pin, because its repository is about
140 MB and the client needs 3 MB of it.
