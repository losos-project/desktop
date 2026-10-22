#!/bin/bash
# The whole path on a real Mac, as .github/workflows/app-windows.yml runs it
# on GitHub's macOS runners: macos-app-server inside macOS, app-windows
# driving it from the same machine with each *.steps script, every window's
# pictures decoded and saved. Everything lands in the folder given, which
# the workflow keeps: pictures, each script's report.json and session.rec,
# the server's log, and a picture of the whole screen at the end.
#
#     tests/macos-ci.sh OUT
#
# Run from src/app-windows after `cargo build --release`.
set -u
out=$1
bin=$PWD/target/release
mkdir -p "$out"

sw_vers
sysctl -n machdep.cpu.brand_string
csrutil status || true

. tests/macos-server.sh

"$bin/macos-app-server" apps >"$out/apps.json"
echo "$(grep -c '"id"' "$out/apps.json") apps listed"

# This Mac stands in for the host as well: a folder for the host's
# Downloads, which `watch` hands Mac packages from, as the image's
# app-windows-downloads unit does. packages.steps downloads into it.
work=$(mktemp -d)
export WORK=$work DOWNLOADS=$work/downloads
mkdir -p "$DOWNLOADS"
"$bin/app-windows" --token-file "$token_dir/token" watch "$DOWNLOADS" >"$out/watch.log" 2>&1 &
watcher=$!
trap 'kill $server $watcher 2>/dev/null' EXIT

failed=0
for script in tests/*.steps; do
	name=$(basename "$script" .steps)
	echo "::group::$name"
	if ! "$bin/app-windows" --token-file "$token_dir/token" record "$script" "$out/$name"; then
		echo "::error::$name failed"
		failed=1
	fi
	echo "::endgroup::"
done

screencapture -x "$out/screen.png" || true
exit $failed
