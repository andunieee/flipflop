#!/usr/bin/env bash
# Build the Rust library for Android (cargo-ndk) into androidApp/src/main/jniLibs, and copy blew's Kotlin
# BLE layer (it ships inside the crate) next to the app.  Run by Gradle; usable by hand too:
#   ./build-native.sh [debug|release] [abi ...]      (default: debug arm64-v8a)
set -euo pipefail
cd "$(dirname "$0")"
profile=${1:-debug}; shift || true
abis=("${@:-arm64-v8a}")
export ANDROID_HOME=${ANDROID_HOME:-/opt/android-sdk}
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/27.2.12479018}

targets=(); for abi in "${abis[@]}"; do targets+=(-t "$abi"); done
rm -rf androidApp/src/main/jniLibs
flags=(); [ "$profile" = release ] && flags+=(--release)
(cd .. && cargo ndk "${targets[@]}" -o kmp/androidApp/src/main/jniLibs build --lib "${flags[@]}")

# blew's Android BLE layer is Kotlin shipped inside the crate; compile it into the app (minus the
# Tauri plugin class, which nothing else references).
blew_dir=$(cd .. && cargo metadata --format-version 1 | python3 -c 'import json,sys; print(next(p["manifest_path"] for p in json.load(sys.stdin)["packages"] if p["name"]=="blew").rsplit("/",1)[0])')
rm -rf androidApp/blew-kt && mkdir -p androidApp/blew-kt
cp -r "$blew_dir/android/src/main/java/org" androidApp/blew-kt/
rm -f androidApp/blew-kt/org/jakebot/blew/BlewPlugin.kt
./patch-blew.py androidApp/blew-kt/org/jakebot/blew   # backport to Android < 13
