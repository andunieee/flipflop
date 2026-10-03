# flipflop: desktop + Android tasks

set positional-arguments

default:
    @just --list

# --- Desktop ---

# run the desktop app (Compose UI over the Rust core; builds the Rust library first)
desktop:
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

# build APK: just android [debug|release] [abi...] (default: debug arm64-v8a)
android profile='debug' *abis:
    cd kmp && ./gradlew :androidApp:assemble{{ if profile == "release" { "Release" } else { "Debug" } }} -Pprofile={{profile}} {{ if abis != "" { "-Pabis=" + replace(abis, " ", ",") } else { "" } }}

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
    task=Debug; [ "$profile" = release ] && task=Release
    (cd kmp && ./gradlew ":androidApp:assemble$task" -Pprofile="$profile" ${abis[@]:+-Pabis="$(IFS=,; echo "${abis[*]}")"})
    adb install -r "kmp/androidApp/build/outputs/apk/$profile/androidApp-$profile.apk"

# install + launch app on connected device
run profile='debug' package='dev.flipflop.app' activity='.MainActivity':
    just install {{profile}}
    adb shell am start -n {{package}}/{{activity}}

# list connected devices
devices:
    adb devices -l

# follow logcat for the app
logs:
    adb logcat -v color --pid=`adb shell pidof -s dev.flipflop.app`
