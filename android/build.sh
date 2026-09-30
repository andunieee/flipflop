#!/usr/bin/env bash
# Build the Rust library for Android with cargo-ndk, then the APK with gradle.
#   ./android/build.sh [debug|release] [abi ...]      (default: debug arm64-v8a)
set -euo pipefail
cd "$(dirname "$0")"
profile=${1:-debug}; shift || true
abis=("${@:-arm64-v8a}")
export ANDROID_HOME=${ANDROID_HOME:-/opt/android-sdk}
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/27.2.12479018}
export ANDROID_JAR=${ANDROID_JAR:-$ANDROID_HOME/platforms/android-35/android.jar}  # slint compiles a Java helper against it

targets=(); for abi in "${abis[@]}"; do targets+=(-t "$abi"); done
rm -rf app/src/main/jniLibs
flags=(); [ "$profile" = release ] && flags+=(--release)
(cd .. && cargo ndk "${targets[@]}" -o android/app/src/main/jniLibs build --lib "${flags[@]}")

task=assembleDebug; [ "$profile" = release ] && task=assembleRelease
gradle --no-daemon -q "$task"
ls -1 app/build/outputs/apk/*/*.apk
