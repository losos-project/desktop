#!/bin/sh
# Compile the proxy's decisions to WebAssembly, where api/proxy.ts imports them.
#
# Vercel's build image may have Rust without the WebAssembly target. Install
# a pinned toolchain when absent, and add the target to an existing toolchain.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain 1.95.0 \
            --target wasm32-unknown-unknown
    . "$HOME/.cargo/env"
fi

if command -v rustup >/dev/null 2>&1; then
    rustup target add wasm32-unknown-unknown
fi

CARGO_BUILD_JOBS=1 cargo build --locked --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/losos_proxy.wasm api/losos_proxy.wasm

# Vercel rejects an empty output directory even when every route is a function.
mkdir -p public
printf '%s\n' 'LosOS GHCR proxy' > public/index.txt
