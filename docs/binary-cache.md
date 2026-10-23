# Binary cache

`proxy/` is a Vercel edge function. A WebAssembly module compiled from Rust,
`proxy/src/lib.rs`, makes every decision, and the JavaScript around it only
fetches. It serves four things from this project's GHCR namespace:

- **A Nix binary cache.** `tools/nix-cache-push` pushes each store path as
  one OCI artifact, `nix-cache:<store hash>`, holding its signed narinfo and
  its NAR. The proxy answers `/<hash>.narinfo`, with its `URL:` pointed back
  at the proxy, and redirects `/nar/<hash>/<file>` to GHCR's blob storage, so
  a NAR never passes through it. A real `nix copy` once pulled and verified
  a signed path through it against a fake registry. The automated tests in
  `proxy/test/` run the handler against a fake fetch that answers like the
  registry, with no Nix involved.
  The OCI artifacts do not require a hosted proxy to consume: a client with
  `oras` can pull an individual path directly from GHCR by its store hash,
  and serve the resulting signed narinfo/NAR locally. LosOS's installer uses
  this path, so cache substitution works without deploying `proxy/` to Vercel.
- **Build caches.** `/build-cache/<name>/<arch>/<part>` redirects to one
  part of a compiler cache that CI keeps in GHCR. The one cache, `losos`, is
  for the overlay's patched GTK and Qt and lives at `losos-ccache:<arch>`.
  It is a zstd tarball split into 4 GB parts, starting at
  `ccache.tar.zst.part-00`. `tools/build-cache-fetch` fetches and unpacks a
  cache through the proxy first, and from GHCR with oras when the proxy has
  nothing.
- **Updates.** `/updates/<channel>/<arch>/<file>` serves a file of
  `images:<channel>-<arch>`, which CI's publish job moves to each release
  that passed verification. With `LOSOS_PROXY_URL` set, CI sets
  `losos.update.baseUrl` to `<proxy>/updates/<channel>/<arch>/`, so
  sysupdate fetches manifests and images through the proxy. The channels
  are the [release cadences](releases.md#release-cadences), `nightly`,
  `weekly`, `monthly` and `yearly`. Without the variable, images update from
  each channel's newest GitHub release instead. They go through an index on
  the documentation site and a service on the machine that joins split
  files, as [Releases](releases.md#updating-from-github-releases) explains.
- **The web flasher's files.** `/flasher/<channel>/<arch>/<file>` serves the
  same release's `_gsi-` files and `SHA256SUMS`. It streams them one byte
  range at a time instead of redirecting, because a page on another origin
  cannot read GHCR's storage. [Web flasher](web-flasher.md) has the details.
  Every read the proxy answers carries `Access-Control-Allow-Origin: *`.

![CI pushes store paths, releases and compiler caches to GHCR; the proxy redirects Nix, sysupdate and CI to GHCR's storage and streams the GSI's files to the web flasher; each nightly is also a GitHub release](images/binary-cache.svg)

CI does not read the cache through the public deployment. Each build job
runs the same handler with node on its own runner, `proxy/serve.ts` at
`http://127.0.0.1:8484`, and gives Nix that as its substituter. A run then
gets what earlier runs pushed whether or not the deployment is up. The
packages are public, so it needs no token. Any machine can do the same:

```
cd proxy && sh build.sh
GHCR_REPOSITORY=losos-project/losos-desktop node --experimental-strip-types serve.ts 127.0.0.1:8484
```

To use it for a build:

```
substituters = https://cache.nixos.org/ https://<proxy>/   # or http://127.0.0.1:8484/
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= <the public half of NIX_CACHE_SIGNING_KEY>
```

> [!IMPORTANT]
> Keep cache.nixos.org in the substituter list when adding the project cache.
> Without the optional project cache, Nix's default cache.nixos.org remains
> enabled.

Five things are set up once, outside the repository:

1. **Vercel (optional for the binary cache).** A deployment with root
   directory `proxy/` and `GHCR_REPOSITORY=dasmatus/losos-desktop` hosts the
   proxy's update, flasher, ping and shared-cache HTTP routes. Direct Nix cache
   consumers can pull `nix-cache:<store hash>` from GHCR with `oras` and do
   not need this deployment for cache substitution. `vercel.json` has the
   rest. Its build installs a pinned Rust with rustup when Rust is missing,
   and adds the WebAssembly target when it is there.
2. **GHCR.** Make the `losos-desktop/nix-cache`, `losos-desktop/images` and
   `losos-desktop/losos-ccache` packages public once CI has created them.
   Or put a read-only token in Vercel as `GHCR_TOKEN`, with its owner's
   GitHub login as `GHCR_USERNAME`.
3. **A signing key.** Run `nix key generate-secret --key-name
   losos-desktop-1` and store the result as the secret
   `NIX_CACHE_SIGNING_KEY`. Store its public half, from `nix key
   convert-secret-to-public`, as the variable `NIX_CACHE_PUBLIC_KEY`.
4. **`LOSOS_PROXY_URL` (optional).** Set the deployment URL without a
   trailing slash when images should fetch updates from the hosted proxy.
   It is not required to publish or consume Nix cache artifacts: CI reads the
   GHCR cache through `proxy/serve.ts` on its own runner, and appliances can
   pull store-path artifacts directly with `oras`. Flake evaluation reads
   this variable for the optional update URL, so CI passes `--impure` when
   it builds, checks and collects the release. CI uploads signed cache paths
   to GHCR whenever `NIX_CACHE_SIGNING_KEY` is set, whether or not this URL
   is configured.
5. **The update signing key**, which every image trusts and every release
   is signed with. Make it on a machine you trust, with no passphrase,
   because CI signs unattended:

   ```
   export GNUPGHOME=$(mktemp -d)
   mkdir -p nixos/keys
   gpg --batch --pinentry-mode loopback --passphrase '' --quick-gen-key 'LosOS Desktop updates' ed25519 sign never
   gpg --armor --export > nixos/keys/update-signing.asc
   gpg --armor --export-secret-keys   # paste into the secret UPDATE_SIGNING_KEY
   rm -rf "$GNUPGHOME"                # once the secret and an offline copy are saved
   ```

   Commit `nixos/keys/update-signing.asc`, which `losos.update.pubring`
   reads. sysupdate verifies every update against it. Put the key's
   fingerprint, from `gpg --show-keys --with-colons
   nixos/keys/update-signing.asc`, in `UPDATE_SIGNING_FPR` in `ci.yml`. The
   `push` job fails without the secret, or with a secret for a different key,
   instead of publishing a release the images would refuse. Keep an offline
   copy of the secret half. An image only ever trusts the key it shipped
   with, so if the key is lost, every installed machine stops taking updates
   until it is reinstalled.

> [!WARNING]
> Before setting `LOSOS_PROXY_URL`, check the public deployment without a
> Vercel login. `/nix-cache-info` must return the cache metadata, an absent
> store hash must return 404 rather than `502 token: 403`, and a published
> release's `/updates/<channel>/<arch>/SHA256SUMS` must be readable. A ready
> deployment alone does not prove GHCR access works. The variable also changes
> the pm image's update source, so leave it unset while the registry is
> inaccessible. Vercel deployment protection must allow anonymous clients.
