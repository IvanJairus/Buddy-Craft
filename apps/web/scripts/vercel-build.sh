#!/usr/bin/env bash
# Vercel build entry point.
#
# `npm run build` generates the wasm bridge with cargo before Vite runs. The
# Vercel Node image has no Rust, so install the pinned toolchain here. The
# repository's rust-toolchain.toml pulls 1.87.0 and wasm32-unknown-unknown.
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  echo "vercel-build: cargo missing, installing rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
else
  echo "vercel-build: cargo found at $(command -v cargo)"
fi

echo "vercel-build: ensuring wasm32 target"
rustup target add wasm32-unknown-unknown

echo "vercel-build: building web client"
npm run build
