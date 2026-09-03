# TunnelManager — Slint GUI recipes.
# The gui-slint crate carries its own rust-toolchain.toml (1.92); running
# `cargo` inside that directory picks it up automatically.

# Run the Slint GUI locally on the desktop.
gui:
    cd gui-slint && cargo run --release

# Build a debug APK and install it on the connected device/emulator.
# Prereqs: cargo-apk (`cargo install cargo-apk`), Android SDK + NDK.
# ANDROID_HOME defaults to /opt/android-sdk; ANDROID_NDK_ROOT defaults to
# the newest NDK under $ANDROID_HOME/ndk (see gui-slint/README.md).
android-install:
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_HOME="${ANDROID_HOME:-/opt/android-sdk}"
    if [ -z "${ANDROID_NDK_ROOT:-}" ]; then
        export ANDROID_NDK_ROOT="$(ls -d "$ANDROID_HOME"/ndk/*/ | sort -V | tail -1)"
    fi
    cd gui-slint
    rustup target add aarch64-linux-android x86_64-linux-android
    cargo apk build --lib -p tunnelmanager-slint
    adb install -r target/debug/apk/tunnelmanager-slint.apk
