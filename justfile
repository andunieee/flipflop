# flipflop: desktop + Android tasks

set positional-arguments

default:
    @just --list

# --- Desktop ---

# run the desktop app (Compose UI over the Rust core; rebuilds the Rust library first)
desktop: build
    #!/usr/bin/env bash
    set -euo pipefail
    # A release lib left over from packaging would shadow the debug one, so drop the bundled copies.
    rm -rf kmp/desktopApp/build/native kmp/desktopApp/build/compose/tmp/prepareAppResources
    target=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')
    cd kmp
    LD_LIBRARY_PATH="$target/debug${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" DYLD_LIBRARY_PATH="$target/debug" ./gradlew :desktopApp:run

# build the Rust library (debug)
build:
    cargo build

# unit + two-node integration tests
test:
    cargo test

# command line client
cli *cargs:
    cargo run --bin flipflop-cli -- {{cargs}}

# --- Packages (each on its own OS; see the README) ---

# .deb in kmp/desktopApp/build/compose/binaries/main/deb
deb:
    cd kmp && ./gradlew :desktopApp:packageDeb

# .rpm in kmp/desktopApp/build/compose/binaries/main/rpm
rpm:
    cd kmp && ./gradlew :desktopApp:packageRpm

# .AppImage in dist/
appimage:
    packaging/linux/appimage.sh

# portable app folder (with its own Java runtime) in kmp/desktopApp/build/compose/binaries/main/app
portable:
    cd kmp && ./gradlew :desktopApp:createDistributable

# .msi (Windows, needs WiX 3)
msi:
    cd kmp && ./gradlew :desktopApp:packageMsi

# .dmg (macOS)
dmg:
    cd kmp && ./gradlew :desktopApp:packageDmg

# release APK in kmp/androidApp/build/outputs/apk/release (all ABIs; see README for signing)
apk:
    cd kmp && ./gradlew :androidApp:assembleRelease -Pprofile=release -Pabis=arm64-v8a,armeabi-v7a,x86_64

# regenerate every icon from assets/top.png
icons:
    packaging/icons.sh

# --- Guest page ---

# rebuild the guest page's wasm: refreshes the copy the app embeds for guests on the LAN (src/core/guest/assets,
# commit it after changing src/groupdoc.rs or web/) and the full page for links from anywhere in web/dist
web:
    web/build.sh

# serve the full guest page on http://localhost:8099 (a link from anywhere opens it with #t=..&e=..&r=..)
web-serve: web
    cd web/dist && python3 -m http.server 8099

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
