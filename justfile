# flipflop: desktop + Android tasks

set positional-arguments

# run the desktop app (Freya UI over the Rust core). FLIPFLOP_PHONE_LAYOUT=1 shows the phone layout.
desktop *args:
    cargo run -p flipflop-app --bin flipflop-gui -- {{args}}

# install the flipflop CLI and flipflop-gui with cargo install, and add the menu entry in
# /usr/share/applications, for every user (Linux)
desktop-install:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo install --force --locked --path .
    cargo install --force --locked --path app
    # The menu entry launches the flipflop-gui cargo just installed.
    root=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
    entry=$(mktemp)
    trap 'rm -f "$entry"' EXIT
    sed "s|^Exec=.*|Exec=$root/bin/flipflop-gui|" packaging/linux/dev.flipflop.app.desktop > "$entry"
    sudo install -Dm644 "$entry" /usr/share/applications/dev.flipflop.app.desktop

# build the Rust code (debug)
build:
    cargo build --workspace

# unit + two-node integration tests
test:
    cargo test

# build the experimental wasm miniapps into examples/miniapps/dist
miniapps:
    examples/miniapps/build.sh

# run one miniapp over a throwaway document: just miniapp checklist actions.json
miniapp app *actions:
    cargo run --example miniapp -- examples/miniapps/dist/{{app}}.wasm {{actions}}

# run one miniapp on two peers and check they converge: just miniapp-sync checklist a.json b.json
miniapp-sync app a b:
    cargo run --example miniapp_sync -- examples/miniapps/dist/{{app}}.wasm {{a}} {{b}}

# command line client
cli *cargs:
    cargo run --bin flipflop -- {{cargs}}

# render the app's pages to PNGs without a window, over a throwaway node (FLIPFLOP_PHONE_LAYOUT=1 for the phone's)
snapshots dir="dist/snapshots":
    cargo run -p flipflop-app --example snapshot -- {{dir}}

# --- Packages (each on its own OS; see the README) ---

# portable app folder in dist/stage/portable/flipflop (Linux; also the root the .deb and .rpm install)
portable:
    packaging/linux/stage.sh

# .deb in dist/
deb: portable
    packaging/linux/deb.sh

# .rpm in dist/
rpm: portable
    packaging/linux/rpm.sh

# .AppImage in dist/
appimage: portable
    packaging/linux/appimage.sh

# .flatpak in dist/, made from the portable app (needs flatpak-builder; the freedesktop 25.08 runtime comes from flathub)
flatpak: portable
    #!/usr/bin/env bash
    set -euo pipefail
    version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
    tar -czf packaging/flatpak/flipflop.tar.gz -C dist/stage/portable flipflop
    flatpak-builder --user --force-clean --install-deps-from=flathub --state-dir=dist/flatpak-state \
        --repo=dist/flatpak-repo dist/flatpak-build packaging/flatpak/dev.flipflop.app.yml
    flatpak build-bundle dist/flatpak-repo "dist/flipflop-linux-x86_64-v${version}.flatpak" dev.flipflop.app

# Arch .pkg.tar.zst in dist/, from the .deb (needs makepkg)
arch: deb
    #!/usr/bin/env bash
    set -euo pipefail
    version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
    dir=$(mktemp -d)
    cp "dist/flipflop_${version}_amd64.deb" "$dir/flipflop-${version}.deb"
    packaging/aur/pkgbuild.sh "$version" "$(sha256sum "$dir"/*.deb | cut -d' ' -f1)" > "$dir/PKGBUILD"
    cp packaging/aur/flipflop-bin.install "$dir/"
    (cd "$dir" && makepkg --nodeps --noconfirm)
    mkdir -p dist
    mv "$dir"/flipflop-bin-*.pkg.tar.zst dist/

# .msi and the portable dist\flipflop folder (Windows, needs WiX 3)
msi:
    powershell -NoProfile -File packaging/windows/msi.ps1

# .dmg and dist/flipflop.app (macOS)
dmg:
    packaging/macos/dmg.sh

# release APK in android/app/build/outputs/apk/release (all ABIs; see README for signing)
apk:
    cd android && ./gradlew :app:assembleRelease -Pprofile=release -Pabis=arm64-v8a,armeabi-v7a,x86_64

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
# gradle builds the Rust library with cargo-ndk (see android/build-native.sh). Needs NDK 27, Android SDK 35.

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
    (cd android && ./gradlew ":app:assemble$task" -Pprofile="$profile" ${abis[@]:+-Pabis="$(IFS=,; echo "${abis[*]}")"})
    for serial in $(adb devices | awk 'NR>1 && $2 == "device" {print $1}'); do
        adb -s "$serial" install -r "android/app/build/outputs/apk/$profile/app-$profile.apk"
    done

# follow logcat for the app
logs:
    adb logcat -v color --pid=`adb shell pidof -s dev.flipflop.app`
