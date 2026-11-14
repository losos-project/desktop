# The sysupdate payload embedded on the installer ISO so an install needs no
# network. It is the same file set an online install pulls from GHCR's
# initial-update layer (docs/installer.md), squashed and read from
# /iso/release.squashfs on the medium. Built from the local image outputs.
# UPDATE_SIGNING_KEY_FILE on the runner signs the manifest in CI; without it,
# the installer checks hashes only.
{
  lib,
  pkgs,
  image,
  raw,
  id,
  version,
  arch,
  ukiFile,
  uki,
  ukiReleaseFiles,
  usrSize,
  signingKeyFile,
  signingFpr,
}:

let
  prefix = "${id}_${version}";
in
pkgs.runCommand "${prefix}-installer-embedded-release"
  {
    nativeBuildInputs = with pkgs.buildPackages; [
      jq
      xz
      gnupg
      squashfsTools
    ];
  }
  ''
    payload=$(mktemp -d)
    cd "$payload"

    extract() {
      local type=$1
      local offset size uuid
      field() {
        jq -er --arg t "$type" ".[] | select(.type==\$t) | .$1" ${image}/repart-output.json
      }
      offset=$(field offset)
      size=$(field raw_size)
      uuid=$(field uuid)
      dd if=${image}/${raw} of="${prefix}_''${type}_$uuid.raw" bs=1M iflag=skip_bytes,count_bytes \
        skip="$offset" count="$size" status=none
      xz --threads=$NIX_BUILD_CORES "${prefix}_''${type}_$uuid.raw"
    }

    usr=$(jq -er '.[] | select(.type=="${arch.usr}") | .raw_size' ${image}/repart-output.json)
    slot=$(numfmt --from=iec ${usrSize})
    if [ "$usr" -gt "$slot" ]; then
      echo "/usr is $(numfmt --to=iec "$usr"), larger than its ${usrSize} update slot (losos.usrSize)" >&2
      exit 1
    fi

    cp ${uki}/${ukiFile} ${prefix}_${arch.name}.efi
    cp ${ukiReleaseFiles}/* .
    extract ${arch.usr}
    extract ${arch.usrVerity}

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

    ${lib.getExe pkgs.squashfsTools}/bin/mksquashfs . $out -comp zstd -Xcompression-level 19 -no-progress
    rm -rf "$payload"
  ''
