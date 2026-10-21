# What this trusts from outside

Everything the flake builds from, besides this repository:

- **nixpkgs `nixos-unstable`**, the channel tarball from `channels.nixos.org`,
  pinned by
  `narHash` in `flake.lock`. Its expressions are read, not trusted blindly:
  a different tarball fails the hash.
- **Stock package binaries from cache.nixos.org.** nixpkgs' normal glibc
  package set is used without libc-specific package overrides, so available
  package closures can be substituted rather than rebuilt locally.
- **Upstream sources nixpkgs fetches**, each pinned by hash in nixpkgs, for
  packages that must be built locally.
- **Fixed-output inputs in the build closure.** CI preloads these from
  cache.nixos.org where available (`tools/nix-fetch-sources`) to avoid making
  a cold build depend on every upstream being available. The selection includes
  source archives and can also include pinned bootstrap tools or upstream
  prebuilt compilers; it is not source-only. Each copied flat or NAR output is
  checked against its derivation's hash, and outputs using other hash methods
  are rejected.
- **pm**, the `components/pm` submodule (`github.com/losos-project/pm`) at the
  commit this tree records (`nixos/pkgs/pm.nix`), and **crates.io**, for its,
  losos-security's, losos-installer's and losos-hardware's dependencies, each pinned by
  `Cargo.lock` and checked by hash.
- **Purism's adaptive GTK patches**, copied into
  `nixos/pkgs/patches/gtk3` and `gtk4` from PureOS's packaging
  (source.puri.sm, `Librem5/debs/gtk4` and `sebastian.krzyszkowiak/gtk`,
  branch `pureos/latest`). They are reviewed source in this repository, not
  fetched at build time.
- **Device firmware**, from nixpkgs' `linux-firmware`, pinned by hash like any
  source. It is prebuilt by the hardware vendors, and nothing can compile it.
  `hardware.nix` ships it because amdgpu and nouveau cannot start current
  GPUs without it.
- **NVIDIA's driver**, the production branch nixpkgs pins by hash, fetched
  from `download.nvidia.com`: the userspace libraries and GSP firmware are
  NVIDIA's binaries, which its licence lets the image and the binary cache
  redistribute unmodified. They are the image's only unfree package
  (`nixos/modules/nvidia.nix`), loaded only on a machine with a card the
  driver supports ([Hardware and NVIDIA](drivers.md)). The open kernel module
  is built from NVIDIA's MIT/GPL source.
- **derisk**, the `components/derisk` submodule
  (`github.com/losos-project/derisk`) at the commit this tree records
  (`nixos/pkgs/derisk.nix`), with its crates, mcsapi among them, pinned by its
  `Cargo.lock` and `cargoHash`. It is the desktop, the display manager and the
  portal backend. The `components/mcsapi` submodule is the same mcsapi commit,
  and builds `x2mcsapi`.
- **The LosOS terminal client**, `losos`, built from the LosOS repository
  (`github.com/dasmatus/losos`) at the commit `nixos/pkgs/losos.nix` names:
  a sparse `fetchgit` of the directories it builds from, checked by hash,
  with its crates pinned by its `Cargo.lock` and `cargoHash`
  ([LosOS boxes](losos-boxes.md)).
- **Home Manager**, `github.com/nix-community/home-manager`, pinned by
  revision and `narHash` in `flake.lock`. Only `nix flake check` reads it, to
  build `homeModules.pm` into a Home Manager generation
  (`nixos/tests/home-manager-pm.nix`); the image and the release never do.
- **Halium's generic system image** (`nixos/pkgs/halium-gsi.nix`), the
  Android 14 system the [Halium GSI](halium.md) runs the vendor's HALs
  under: UBports' build of Halium's AOSP tree, as Droidian publishes it in
  its apt repository, pinned by the hash that repository's index gives for
  the package. It is prebuilt bionic code nothing here can compile, and it
  runs as host root in the Android container, so it is trusted as much as
  the vendor blobs beside it. The phone's own kernel, vendor partitions and
  bootloader are the device maker's, and the GSI trusts them as Android does.
- **Flathub**, for apps installed after the fact. Its repo file, with the
  signing key every install is checked against, is
  `nixos/modules/flathub.flatpakrepo` in this tree; the image adds the remote
  from that copy at boot and never fetches the key. The apps themselves are
  Flathub's builds, sandboxed by Flatpak and verified against that key, and
  nothing in the image depends on one.
- **The project's binary cache** in GHCR, through `proxy/` ([Binary cache](binary-cache.md)): optional
  paths the CI job stores and signs. A client checks every narinfo against
  the public key it was told to trust, so the proxy and GHCR carry bytes but
  cannot vouch for them.
- **In CI only:** the official Nix installer from `releases.nixos.org`,
  pinned by version. Nix is what builds Nix, so this binary is the one
  bootstrap; `.#nix` is the nixpkgs Nix package for a build host after it.

## The components/ submodules

derisk, mcsapi and pm are git submodules under `components/`, and the flake
builds them from there (`self.submodules = true` in `flake.nix`, which Nix 2.27
and later honour). The commit recorded for each submodule is the pin: a clone
needs `git clone --recurse-submodules` (or `git submodule update --init`), and
`git -C components/<name> log` shows exactly what the image builds. Moving one
is `git -C components/<name> checkout <rev>`, a commit here, and a new
`cargoHash` in its `nixos/pkgs/*.nix` when its `Cargo.lock` changed. There is
no source hash to update: the submodule commit already names the tree.
`tools/nix-refresh-hashes --list > before.txt` before the move and
`tools/nix-refresh-hashes before.txt` after it finds and rewrites that
`cargoHash`.
derisk's own `Cargo.lock` still fetches mcsapi by git revision, so move
`components/mcsapi` to the revision that lock names. Every Monday the
lockfile update moves all three to releases ([Keeping the pins
current](#keeping-the-pins-current)); a move by hand in between stays.

## Keeping the pins current

Every Monday the `lockfiles` workflow (`.github/workflows/lockfiles.yml`)
runs `tools/update-lockfiles`, which a person can run the same way:

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
  agree. A pin only moves forward: one moved past the release by hand stays
  until a newer release passes it. Inside the submodules nothing else
  moves, since each repository updates its own lockfiles the same way.
- `tools/nix-refresh-hashes` then re-fetches every fixed output pinned in
  this repository whose derivation those changes touched, and writes back
  the hash it really has. Re-fetching is the point: a stale hash still names
  the old store path, so a machine or cache that has the old output builds
  with it and never notices. losos-docs' npm hash moves this way whenever
  `website/package-lock.json` does.

The result is a pull request on an `automation/lockfiles-<date>` branch that
merges itself with a merge commit once every check on it has passed and no
VM test warned; otherwise it waits for a person, with a comment saying
which check stopped it. A newer week's pull request closes the older one.

> [!IMPORTANT]
> The workflow opens and merges with `LOCKFILE_UPDATE_TOKEN`, a fine-grained
> token with Contents and Pull requests write. GitHub starts no workflow for
> a pull request or push made with Actions' own token, so with that token
> neither the pull request's checks nor main's run after the merge would
> start. Without the secret the branch is still pushed, and the run's summary
> links the page that opens its pull request.

android_translation_layer stays a `fetchgit` pin: it is an upstream fork, off
by default, and its tree is close to half a gigabyte, which the flake would
otherwise copy on every evaluation.

The LosOS client is a sparse `fetchgit` pin, its repository being
about 140 MB, of which the client needs 3.
