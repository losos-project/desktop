#!/bin/bash
# LosOS Desktop in a VM on a real Mac, with that Mac's apps as LosOS
# windows, live: what .github/workflows/app-windows.yml runs on GitHub's
# macOS runners. macos-app-server runs on the Mac as it would in a macOS
# machine; the newest nightly image boots under QEMU and its app-windows,
# from BUNDLE, talks to the server over QEMU's network. losos-vm.py then
# starts and uses macOS apps from inside LosOS and keeps pictures of both
# screens.
#
# The machines are the other way round from a LosOS PC, where macOS is the
# VM. Apple's licence lets macOS run only on Apple hardware, so here the
# Mac is the real machine and LosOS the guest; the client and the server
# see the same network connection either way.
#
#     tests/losos-vm.sh BUNDLE OUT
#
# Run from src/app-windows after `cargo build --release`. BUNDLE is the
# Linux client with its libraries (the workflow's linux job makes it).
set -u
bundle=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
out=$2
bin=$PWD/target/release
mkdir -p "$out"
work=$(mktemp -d)

sw_vers
sysctl -n machdep.cpu.brand_string
df -h "$work"

# The newest nightly's x86_64 disk image, checked against its SHA256SUMS.
# Images of 2 GiB or more are released in parts.
tag=$(gh release list --repo "$GITHUB_REPOSITORY" --limit 30 --json tagName \
	--jq '[.[].tagName | select(startswith("nightly-"))][0]')
echo "image from $tag"
gh release download "$tag" --repo "$GITHUB_REPOSITORY" --dir "$work" \
	--pattern 'SHA256SUMS-x86_64' --pattern '*_x86_64.raw.xz*'
image=$(cd "$work" && ls *_x86_64.raw.xz* | head -1)
image=${image%.part-*}
version=${image#losos-desktop_}
version=${version%_x86_64.raw.xz}
want=$(grep " $image\$" "$work/SHA256SUMS-x86_64" | cut -d' ' -f1)
# Unpacked sparse, as most of it is empty space.
cat "$work/$image"* | tee >(shasum -a 256 | cut -d' ' -f1 >"$work/got") |
	xz -dc | dd of="$work/x.raw" bs=1m conv=sparse 2>/dev/null
rm -f "$work/$image"*
got=$(cat "$work/got")
if [ "$got" != "$want" ]; then
	echo "::error::$image: sha256 $got, SHA256SUMS says $want"
	exit 1
fi
du -h "$work/x.raw"

brew install qemu swtpm

. tests/macos-server.sh

# Hardware virtualisation where the Mac offers it to a VM inside the
# runner's own VM, which an Intel one may; emulation otherwise.
accel=tcg
[ "$(uname -m)" = x86_64 ] && accel=hvf,tcg

python3 tests/losos-vm.py --image "$work/x.raw" --version "$version" \
	--bundle "$bundle" --token "$token_dir/token" --server-log "$out/server.log" \
	--out "$out" --work "$work/vm" --accel "$accel"
status=$?
screencapture -x "$out/screen.png" || true
exit $status
