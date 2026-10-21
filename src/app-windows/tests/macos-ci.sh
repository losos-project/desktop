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

token_dir="$HOME/Library/Application Support/LosOS App Server"
mkdir -p "$token_dir"
head -c 32 /dev/urandom | base64 >"$token_dir/token"

# Screen Recording and Accessibility, which a person allows once in System
# Settings. A runner has nobody to click, so they are written into TCC's
# database, which GitHub's images leave writable; where that fails, the
# server says what is missing and the scripts do not run.
grant() {
	sudo sqlite3 "/Library/Application Support/com.apple.TCC/TCC.db" \
		"INSERT OR REPLACE INTO access (service, client, client_type, auth_value, auth_reason, auth_version, flags, last_modified)
		 VALUES ('$1', '$bin/macos-app-server', 1, 2, 4, 1, 0, CAST(strftime('%s','now') AS INTEGER));" ||
		echo "could not grant $1"
}
grant kTCCServiceScreenCapture
grant kTCCServiceAccessibility
"$bin/macos-app-server" permissions || true

# TextEdit opens a file dialog instead of a document unless told not to,
# and a held letter shows accents instead of repeating.
defaults write com.apple.TextEdit NSShowAppCentricOpenPanelInsteadOfUntitledFile -bool false
defaults write com.apple.TextEdit RichText -bool true
defaults write -g ApplePressAndHoldEnabled -bool false

"$bin/macos-app-server" >"$out/server.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null' EXIT
for _ in $(seq 50); do
	nc -z 127.0.0.1 7180 && break
	sleep 0.2
done

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
