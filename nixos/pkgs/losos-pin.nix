# The LosOS commit the losos client and the box test are built from
# (losos.nix). The LosOS repository's desktop-pin workflow rewrites each
# value here in a pull request when it tags a release; the hashes are the
# client's sparse fetch, its vendored crates and the box's sparse fetch.
{
  rev = "282ffdcaa37dfc93224572e02b07c7d4c1819ea0";
  # The release, or the release this commit follows and the commit's date.
  version = "0.6.1-unstable-2026-10-28";
  # What `losos --version` says: the tag, or the newest tag before it.
  release = "v0.6.1";
  hash = "sha256-7qEELJ+8rJtCI0Qc9GMTdSTGRLClzfVLsrxcW310DwU=";
  cargoHash = "sha256-0VQK6reYxlHCgLQuyAb0ttNCIRVy3PBvs9nFSAqj/7U=";
  boxHash = "sha256-mhr55EGPnro7KuRc9ixfFMDia+g9sx7wNWt0VEuYhJw=";
}
