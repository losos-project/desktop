# Sourced by the scripts that run macos-app-server on one of GitHub's Macs
# (macos-ci.sh, losos-vm.sh): makes its token, grants what a person would
# grant in System Settings, sets TextEdit up for the tests, and starts it
# in the background, logging to $out/server.log. Leaves $token_dir and
# $server set, and a trap that stops it.
#
# Needs $bin, the folder with the built programs, and $out.

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
