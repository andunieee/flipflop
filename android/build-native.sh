#!/usr/bin/env bash
# Build the app's Rust library (the Freya UI and the node, crate app/) for Android with cargo-ndk into
# app/src/main/jniLibs, and copy blew's Kotlin BLE layer (it ships inside the crate) next to the app.
# Run by Gradle; usable by hand too:
#   ./build-native.sh [debug|release] [abi ...]      (default: debug arm64-v8a)
set -euo pipefail
cd "$(dirname "$0")"
profile=${1:-debug}; shift || true
abis=("${@:-arm64-v8a}")
export ANDROID_HOME=${ANDROID_HOME:-/opt/android-sdk}
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/27.2.12479018}
# Freya's mundy (the system's light/dark preference) compiles a little Java against it.
export ANDROID_JAR=${ANDROID_JAR:-$ANDROID_HOME/platforms/android-35/android.jar}

# cargo's own "no such command: ndk" gives no hint about how to get it.
if ! command -v cargo-ndk >/dev/null; then
    echo "cargo-ndk is not installed; install it with: cargo install cargo-ndk" >&2
    exit 1
fi

targets=(); for abi in "${abis[@]}"; do targets+=(-t "$abi"); done
rm -rf app/src/main/jniLibs
flags=(); [ "$profile" = release ] && flags+=(--release)
# Freya's skia compiles against API 26 (freya-skia-bindings hardcodes -D__ANDROID_API__=26). cargo-ndk
# defaults to 21, and the NDK headers then reject API-26 calls such as strtof_l in the C++ standard library.
# Keep this in step with minSdk in app/build.gradle.kts.
(cd .. && cargo ndk "${targets[@]}" -P 26 -o android/app/src/main/jniLibs build -p flipflop-app --lib "${flags[@]}")
# cargo-ndk copies every cdylib of the build (iroh's own, too); the app only loads its own.
find app/src/main/jniLibs -name '*.so' ! -name libflipflop_app.so -delete

# blew's Android BLE layer is Kotlin shipped inside the crate; compile it into the app (minus the
# Tauri plugin class, which nothing else references).
blew_dir=$(cd .. && cargo metadata --format-version 1 | python3 -c 'import json,sys; print(next(p["manifest_path"] for p in json.load(sys.stdin)["packages"] if p["name"]=="blew").rsplit("/",1)[0])')
rm -rf app/blew-kt && mkdir -p app/blew-kt
cp -r "$blew_dir/android/src/main/java/org" app/blew-kt/
rm -f app/blew-kt/org/jakebot/blew/BlewPlugin.kt
./patch-blew.py app/blew-kt/org/jakebot/blew   # backport to Android < 13
