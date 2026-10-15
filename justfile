# flipflop recipes.

# Run the Slint GUI locally on the desktop.
gui:
    cargo run --release

# Everything is one crate now, so one command covers unit + integration tests.
test:
    cargo test

# Build a debug APK and install it on the connected device/emulator.
# It installs as `com.flipflop.app.debug` ("flipflop debug"), next to a
# release install rather than clashing with its signature and data.
# Prereqs: cargo-apk (`cargo install cargo-apk`), Android SDK + NDK.
# ANDROID_HOME defaults to /opt/android-sdk; ANDROID_NDK_ROOT defaults to
# the newest NDK under $ANDROID_HOME/ndk (see docs/development.md).
android-install:
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_HOME="${ANDROID_HOME:-/opt/android-sdk}"
    if [ -z "${ANDROID_NDK_ROOT:-}" ]; then
        export ANDROID_NDK_ROOT="$(ls -d "$ANDROID_HOME"/ndk/*/ | sort -V | tail -1)"
    fi
    rustup target add aarch64-linux-android x86_64-linux-android
    # cargo-apk has no per-profile package id, so patch Cargo.toml for the
    # duration of the build (mv restores the original, mtime included).
    cp -p Cargo.toml Cargo.toml.orig
    trap 'mv Cargo.toml.orig Cargo.toml' EXIT
    sed -i \
        -e 's/^package = "com\.flipflop\.app"$/package = "com.flipflop.app.debug"/' \
        -e 's/^label = "flipflop"$/label = "flipflop debug"/' \
        Cargo.toml
    grep -q '^package = "com\.flipflop\.app\.debug"$' Cargo.toml \
        || { echo "android-install: package id patch did not apply" >&2; exit 1; }
    cargo apk build --lib -p flipflop
    adb install -r ~/.cache/cargo-target/debug/apk/flipflop.apk
