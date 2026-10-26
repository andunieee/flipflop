#!/bin/sh
# Builds the guest page's engine (see src/lib.rs) into two places:
#   web/dist             the full build (iroh included) with the page and the download service worker: what
#                        GitHub Pages serves for links from anywhere (.github/workflows/pages.yml)
#   src/core/guest/assets  the build without iroh, which the app embeds and serves to guests on the LAN
# Needs the wasm32-unknown-unknown target and a wasm-bindgen CLI matching the version pinned in Cargo.toml;
# wasm-opt is used when found. `build.sh lan` only refreshes the app's assets (commit them), `build.sh
# <dir>` builds the full page into <dir>.
set -eu
cd "$(dirname "$0")"
what="${1:-all}"

target_dir=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
wasm="$target_dir/wasm32-unknown-unknown/release/flipflop_web.wasm"

bindgen() { # <out dir>
  rm -rf "$1"
  mkdir -p "$1"
  wasm-bindgen --target web --no-typescript --out-dir "$1" "$wasm"
  if command -v wasm-opt >/dev/null; then
    wasm-opt -Os --enable-bulk-memory --enable-nontrapping-float-to-int \
      -o "$1/flipflop_web_bg.wasm" "$1/flipflop_web_bg.wasm"
  fi
}

if [ "$what" = all ] || [ "$what" = lan ]; then
  cargo build --release --no-default-features
  bindgen ../src/core/guest/assets
fi
if [ "$what" != lan ]; then
  out=dist
  [ "$what" = all ] || out="$what"
  cargo build --release
  bindgen "$out"
  cp ../src/core/guest/page.html "$out/index.html"
  cp sw.js "$out/"
fi
