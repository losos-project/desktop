# Pin for derisk's prebuilt desktop bundles (losos-project/derisk GitHub
# releases). tools/update-lockfiles rewrites this from the weekly release's
# SHA256SUMS; CI may override the tag with LOSOS_DERISK_TAG for the cadence it
# builds. While enable is false, nixos/pkgs/derisk-from-source.nix is used.
{
  enable = false;
  repository = "losos-project/derisk";
  tag = "weekly-19700101T0000";
  version = "0.1.0-unstable-2026-10-29";
  hashes = {
    x86_64-linux = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    aarch64-linux = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
  };
}
