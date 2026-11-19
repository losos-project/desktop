# Goal: source builds only from the losos-project namespace

## Goal statement (measurable)

No package in this repository's build closure compiles a project whose source
lives outside the `losos-project` GitHub namespace, except this repository
itself. Everything else is either

1. a nixpkgs package that substitutes from `cache.nixos.org`, or
2. a prebuilt package pulled from another distributor (Debian, Droidian,
   Fedora, upstream release artifacts) when nixpkgs has no prebuilt copy,
   configured at run time (sysctl, kernel cmdline, env, config file) instead
   of patched.

Measured by: `tools/source-policy` (also wired into `nix flake check` as
`checks.<system>.source-policy`) exiting 0 on the finished tree, and by each
replacement package building/`nix eval`ing from its prebuilt input.

## Milestones

| # | Milestone | Status |
|---|-----------|--------|
| M1 | Audit every source declaration in `nixos/`, classify by origin, record violations | pending |
| M2 | `tools/source-policy` gate + `checks.<system>.source-policy`, with a named exception list | pending |
| M3 | `wpewebkit`: replace the wpewebkit.org source build with Debian's prebuilt 2.54.3 WPE package | pending |
| M4 | `libhybris` + `android-headers`: replace Halium source builds with Droidian's prebuilt debs | pending |
| M5 | `reims-vgpu-backend` and `losos` client: resolve namespace violation (prebuilt or exception) | pending |
| M6 | Docs (`docs/trust.md`, `docs/packages.md`, `docs/danube.md`, `docs/halium.md`) match the new policy | pending |

## Key decisions

- The gate is a static check over `nixos/` plus an explicit exception list,
  because the policy is about *source declarations*: every `src =` in
  `nixos/pkgs` is a literal local path or a fetch, so the tree is the ground
  truth and the check runs in seconds without evaluating the image.
- Prebuilt repacks are `stdenv.mkDerivation` + `dpkg-deb -x` + `autoPatchelf`,
  pinned by the distributor's own version and hash, like `shim-signed`
  (Fedora RPM) and `halium-gsi` (Droidian deb) already are.
- Patches on nixpkgs packages (GTK, Qt, GRUB, QEMU) are not third-party
  source builds: the source is nixpkgs', so they stay, but they are listed
  in the policy's allowlist as `nixpkgs` origin.

## Blockers / open questions

- `aquarat/reims-vgpu` publishes no releases and no distributor packages it,
  so there is no prebuilt artifact to pull.
- `dasmatus/losos` (the boxes client) is not in `losos-project` and publishes
  no releases either; `losos-project/losos` does not exist.
