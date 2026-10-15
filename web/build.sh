#!/bin/sh
# Builds the browser link's page on the web into web/dist (or $1): the
# app's own page (src/guest/page.html), the service worker and iroh in wasm.
# Needs the wasm32-unknown-unknown target and a wasm-bindgen CLI matching
# the version pinned in Cargo.toml; wasm-opt is used when found.
set -eu
cd "$(dirname "$0")"
out="${1:-dist}"

cargo build --release
target_dir=$(cargo metadata --format-version 1 --no-deps \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

rm -rf "$out"
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "$target_dir/wasm32-unknown-unknown/release/flipflop_web.wasm"
if command -v wasm-opt >/dev/null; then
  wasm-opt -Os --enable-bulk-memory --enable-nontrapping-float-to-int \
    -o "$out/flipflop_web_bg.wasm" "$out/flipflop_web_bg.wasm"
fi
cp ../src/guest/page.html "$out/index.html"
cp sw.js "$out/"
