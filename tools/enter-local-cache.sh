#!/usr/bin/env bash
# Start the GHCR cache proxy and try once to register it with nix-daemon.
# Called from devenv's enterShell (and optionally nix develop's shellHook).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=/dev/null
source "$root/tools/nix-local-cache.env"

if ! curl -sf "$NIX_CACHE_URL/nix-cache-info" >/dev/null; then
  log="${XDG_STATE_HOME:-$HOME/.local/state}/losos-nix-cache.log"
  mkdir -p "$(dirname "$log")"
  echo "Starting project Nix cache at $NIX_CACHE_URL (log: $log)"
  nohup "$root/tools/serve-nix-cache.sh" >>"$log" 2>&1 &
  for _ in $(seq 1 30); do
    if curl -sf "$NIX_CACHE_URL/nix-cache-info" >/dev/null; then
      break
    fi
    sleep 0.2
  done
  if ! curl -sf "$NIX_CACHE_URL/nix-cache-info" >/dev/null; then
    echo "warning: local cache did not answer at $NIX_CACHE_URL (see $log)" >&2
    exit 0
  fi
fi

if nix show-config 2>/dev/null | grep -qF "$NIX_CACHE_URL"; then
  exit 0
fi

if sudo -n true 2>/dev/null; then
  "$root/tools/configure-nix-local-cache.sh"
else
  echo "Nix is not using $NIX_CACHE_URL yet. Run: $root/tools/configure-nix-local-cache.sh"
fi
