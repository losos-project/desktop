#!/usr/bin/env bash
# Point the nix-daemon at tools/serve-nix-cache.sh (docs/binary-cache.md).
# NIX_CONFIG alone is not enough on multi-user installs: only root is trusted.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=/dev/null
source "$root/tools/nix-local-cache.env"
marker='# losos-project local GHCR cache'
if sudo grep -qF "$marker" /etc/nix/nix.conf 2>/dev/null; then
  echo "Local losos cache already in /etc/nix/nix.conf"
  exit 0
fi
sudo tee -a /etc/nix/nix.conf >/dev/null <<EOF
$marker
substituters = $NIX_CACHE_URL
trusted-public-keys = $NIX_CACHE_PUBLIC_KEY
EOF
sudo systemctl restart nix-daemon
echo "nix-daemon restarted; substituter $NIX_CACHE_URL is active."
