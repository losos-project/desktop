# The LosOS commit the losos client and the box test are built from
# (losos.nix). The LosOS repository's desktop-pin workflow rewrites each
# value here in a pull request when it tags a release; the hashes are the
# client's sparse fetch, its vendored crates and the box's sparse fetch.
{
  rev = "88bbd5d65c69fee33b8a6a2c276d94f7d0a1c290";
  # The release, or the release this commit follows and the commit's date.
  version = "0.6.1-unstable-2026-10-28";
  # What `losos --version` says: the tag, or the newest tag before it.
  release = "v0.6.1";
  hash = "sha256-T8mxBPNh8n6TYd5EaMlRsLmU6Zj+lZynJrgf4oMPpew=";
  cargoHash = "sha256-0VQK6reYxlHCgLQuyAb0ttNCIRVy3PBvs9nFSAqj/7U=";
  boxHash = "sha256-9PgLckZtdgpWnn2j9P3Tc4B68Yx80q1tEUnBTbAkErs=";
}
