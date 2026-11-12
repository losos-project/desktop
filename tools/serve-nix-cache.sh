#!/usr/bin/env bash
# Local GHCR Nix cache proxy (docs/binary-cache.md). Listens on 127.0.0.1:8484.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
addr=${1:-127.0.0.1:8484}
export GHCR_REPOSITORY=${GHCR_REPOSITORY:-losos-project/desktop}
sh "$root/proxy/build.sh"
exec node --experimental-strip-types "$root/proxy/serve.ts" "$addr"
