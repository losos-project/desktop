#!/usr/bin/env bash
# derisk installer "Update the installer" page with a fake backend (no ISO).
set -euo pipefail
outdir=${1:-docs/images/installer-liveproof}
mkdir -p "$outdir"

backend=$(mktemp)
cat >"$backend" <<'EOF'
#!/usr/bin/env bash
echo '{"event":"hello","name":"LosOS Desktop","source":"https://example.invalid/channel","livepatch":{"current":"20261101.120000","version":"20261112.220000","channel":"nightly"}}'
while IFS= read -r line; do
  case "$line" in
    *disks*) echo '{"event":"disks","disks":[]}'
  esac
done
EOF
chmod +x "$backend"

socket=$(mktemp -u /tmp/weston-proof-XXXX)
weston --socket="$socket" --width=1024 --height=640 --backend=headless-backend.so 2>/dev/null &
weston_pid=$!
trap 'kill $weston_pid 2>/dev/null; rm -f "$backend"' EXIT
sleep 2

derisk=$(nix build --no-link --print-out-paths .#legacyPackages.x86_64-linux.derisk 2>/dev/null | sed 's|$|/bin/derisk|')
if [ ! -x "$derisk" ]; then
  derisk=$(command -v derisk || true)
fi
[ -x "$derisk" ] || { echo "derisk not built" >&2; exit 1; }

WAYLAND_DISPLAY=$socket XDG_SESSION_TYPE=wayland \
  "$derisk" installer -- "$backend" &
derisk_pid=$!
sleep 5

if command -v grim >/dev/null; then
  WAYLAND_DISPLAY=$socket grim "$outdir/03-livepatch-page.png"
elif command -v import >/dev/null; then
  import -window root "$outdir/03-livepatch-page.png"
fi

kill "$derisk_pid" 2>/dev/null || true
kill "$weston_pid" 2>/dev/null || true
echo "wrote $outdir/03-livepatch-page.png"
