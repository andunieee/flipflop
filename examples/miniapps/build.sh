#!/bin/sh
# Builds each miniapp as its own wasm module into dist/. Needs the wasm32-unknown-unknown target.
set -eu
cd "$(dirname "$0")"
target=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
mkdir -p dist
for app in checklist expenses poll availability calendar; do
  cargo build --release --target wasm32-unknown-unknown \
    --no-default-features --features "$app" --quiet
  cp "$target/wasm32-unknown-unknown/release/flipflop_miniapps.wasm" "dist/$app.wasm"
  printf '%s: %s bytes\n' "$app" "$(wc -c <"dist/$app.wasm")"
done
