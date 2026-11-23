# The sysupdate payload embedded on the installer ISO so an install needs no
# network. It is the same file set an online install pulls from GHCR's
# initial-update layer (docs/installer.md), squashed and read from
# /iso/release.squashfs on the medium. The xz images come from
# system.build.updateImages, which the GitHub release uses too, so they are
# compressed once. UPDATE_SIGNING_KEY_FILE on the runner signs the manifest
# in CI; without it, the installer checks hashes only.
{
  lib,
  pkgs,
  payloads,
  id,
  version,
  signingKeyFile,
  signingFpr,
}:

let
  prefix = "${id}_${version}";
in
pkgs.runCommand "${prefix}-installer-embedded-release"
  {
    nativeBuildInputs = with pkgs.buildPackages; [
      gnupg
      squashfsTools
    ];
  }
  ''
    payload=$(mktemp -d)
    cd "$payload"
    cp -a --no-preserve=ownership --link ${payloads}/. . || cp -a --no-preserve=ownership ${payloads}/. .

    sha256sum -- * > SHA256SUMS

    if [ -n ${lib.escapeShellArg signingKeyFile} ] && [ -f ${lib.escapeShellArg signingKeyFile} ]; then
      gpg --batch --import < ${lib.escapeShellArg signingKeyFile}
      signer=$(gpg --batch --with-colons --show-keys ${lib.escapeShellArg signingKeyFile} \
        | awk -F: '$1 == "fpr" { print $10; exit }')
      if [ "$signer" != ${lib.escapeShellArg signingFpr} ]; then
        echo "The signing key is ${"signer:-none"}, but the images trust ${signingFpr}." >&2
        exit 1
      fi
      gpg --batch --yes --local-user ${lib.escapeShellArg signingFpr} --detach-sign \
        --output SHA256SUMS.gpg SHA256SUMS
    fi

    # The members are already xz (or a small EFI binary). Level 19 spent
    # minutes to save almost nothing on data xz had already compressed.
    mksquashfs . $out -comp zstd -Xcompression-level 3 -no-progress
    rm -rf "$payload"
  ''
