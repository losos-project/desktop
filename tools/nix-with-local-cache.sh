#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=/dev/null
source "$root/tools/nix-local-cache.env"

if ! curl -sf "$NIX_CACHE_URL/nix-cache-info" >/dev/null; then
  echo "Local cache is not up. Start it in another terminal:"
  echo "  $root/tools/serve-nix-cache.sh"
  exit 1
fi

if ! nix show-config 2>/dev/null | grep -qF "$NIX_CACHE_URL"; then
  echo "Nix is not using $NIX_CACHE_URL (untrusted substituter)."
  echo "Run once with sudo:"
  echo "  $root/tools/configure-nix-local-cache.sh"
  exit 1
fi

exec nix "$@"
