# flipflop: desktop + Android tasks

set positional-arguments

default:
    @just --list

# --- Desktop ---

# run desktop app
desktop *cargs:
    cargo run --bin=flipflop -- {{cargs}}

# build desktop (debug)
build:
    cargo build

# unit + two-node integration tests
test:
    cargo test

# command line client
cli *cargs:
    cargo run --bin flipflop-cli -- {{cargs}}

# --- Android ---
# wraps ./android/build.sh (cargo-ndk + gradle). Needs NDK 27, Android SDK, gradle.

# build APK: just android [debug|release] [abi...] (default: debug arm64-v8a)
android profile='debug' *abis:
    ./android/build.sh {{profile}} {{abis}}

# install APK on connected device (build first). `just install android` = debug, default ABIs.
install *args:
    #!/usr/bin/env bash
    set -euo pipefail
    args=("$@")
    # `just install android` — Android is the target, not a profile/ABI. Drop it.
    clean=()
    for a in "${args[@]:-}"; do [ "$a" != android ] && [ -n "$a" ] && clean+=("$a"); done
    args=("${clean[@]:-}")
    profile=debug; abis=()
    if [ ${#args[@]} -eq 0 ]; then
        : # default: debug, build.sh default ABIs
    else
        case "${args[0]}" in
            debug|release) profile=${args[0]}; abis=("${args[@]:1}") ;;
            *) abis=("${args[@]}") ;;
        esac
    fi
    ./android/build.sh "$profile" "${abis[@]}"
    adb install -r "android/app/build/outputs/apk/$profile/app-$profile.apk"

# install + launch app on connected device
run profile='debug' package='dev.flipflop.app' activity='.FlipflopActivity':
    just install {{profile}}
    adb shell am start -n {{package}}/{{activity}}

# list connected devices
devices:
    adb devices -l

# follow logcat for the app
logs:
    adb logcat -v color --pid=`adb shell pidof -s dev.flipflop.app`
