# TunnelManager — Slint GUI recipes.

# Run the Slint GUI locally on the desktop.
gui:
    cargo run --release

# Everything is one crate now, so one command covers unit + integration tests.
test:
    cargo test

# Build a debug APK and install it on the connected device/emulator.
# Prereqs: cargo-apk (`cargo install cargo-apk`), Android SDK + NDK.
# ANDROID_HOME defaults to /opt/android-sdk; ANDROID_NDK_ROOT defaults to
# the newest NDK under $ANDROID_HOME/ndk (see README.md).
android-install:
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_HOME="${ANDROID_HOME:-/opt/android-sdk}"
    if [ -z "${ANDROID_NDK_ROOT:-}" ]; then
        export ANDROID_NDK_ROOT="$(ls -d "$ANDROID_HOME"/ndk/*/ | sort -V | tail -1)"
    fi
    rustup target add aarch64-linux-android x86_64-linux-android
    cargo apk build --lib -p tunnelmanager-slint
    adb install -r target/debug/apk/tunnelmanager-slint.apk
