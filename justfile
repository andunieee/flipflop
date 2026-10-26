# flipflop: desktop + Android tasks

set positional-arguments

default:
    @just --list

# --- Desktop ---

# run the desktop app (Compose UI over the Rust core; builds the Rust library first)
desktop: build
    cd kmp && ./gradlew :desktopApp:run

# build the Rust library (debug)
build:
    cargo build

# unit + two-node integration tests
test:
    cargo test

# command line client
cli *cargs:
    cargo run --bin flipflop-cli -- {{cargs}}

# --- Android ---
# gradle builds the Rust library with cargo-ndk (see kmp/build-native.sh). Needs NDK 27, Android SDK 35.

# install APK on connected device (build first). `just install android` = debug, default ABIs.
android *args:
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
    task=Debug; [ "$profile" = release ] && task=Release
    (cd kmp && ./gradlew ":androidApp:assemble$task" -Pprofile="$profile" ${abis[@]:+-Pabis="$(IFS=,; echo "${abis[*]}")"})
    for serial in $(adb devices | awk 'NR>1 && $2 == "device" {print $1}'); do
        adb -s "$serial" install -r "kmp/androidApp/build/outputs/apk/$profile/androidApp-$profile.apk"
    done

# follow logcat for the app
logs:
    adb logcat -v color --pid=`adb shell pidof -s dev.flipflop.app`
