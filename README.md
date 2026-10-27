<p align="center"><img src="assets/side.png" alt="flipflop" width="360"></p>

# flipflop

Rust peer-to-peer app (desktop and Android) with a Kotlin Multiplatform (Compose) UI. Peers connect with [iroh](https://iroh.computer).

- **LAN**: devices announce over mDNS (service `_flipflop._udp`) with their name; they appear under *Nearby*.
- **QR**: *Add by QR* shows your ticket as a QR code and scans others' (webcam on desktop, system scanner on Android). Tickets can also be pasted.
- **BLE discovery**: every device advertises a GATT service whose characteristic holds `{name, ticket}` and scans for it, so peers learn each other's full endpoint id and name.
- **BLE transport**: [`iroh-ble-transport`](https://crates.io/crates/iroh-ble-transport) adds Bluetooth LE as an iroh network path next to IP and relay, so connections and chat can run over Bluetooth when there is no network. It only advertises a 12-byte key prefix, which is why discovery above is kept. It is experimental. On Android the transport is added when the endpoint is bound, so the Bluetooth permission is requested at start-up; if it is denied the transport stays off until the next launch.
- **Name**: generated as `<hostname or model> (<os>)`, editable in the UI, persisted.

## Contacts and groups

- **Contacts**: every peer you connect to (or that connects to you) is saved with its public key and name. Saved contacts reconnect automatically, with backoff, and immediately when they show up on the LAN or over BLE. You can rename them locally (the peer's own name stays visible, dimmed) or remove them. A removed contact that keeps dialing you is still accepted but is not saved again; connecting to them yourself saves them again.
- **Pairing**: press Pair on a peer (scanning or pasting someone's code pairs automatically). When both of you have paired, you share an implied group, with an id derived from both device ids. It is not listed with the groups: it is that peer's page, reached from the Peers menu. From there you can invite the peer to any of your groups. Unpairing removes it on both sides.
- **Home**: per group, what is new since you last looked (files and texts from others, scratchpad changes, new members). Opening a group marks it as seen; `flipflop-cli home` shows the same.
- **Groups**: create a group, then invite contacts. Each gets an invite and must accept. If they are offline, the invite is sent when they next connect. Declining is remembered per `(inviter, group)`: repeat invites for that pair are not shown, invites from other peers or to other groups still are. Default group names are two-word verses (e.g. "pale moon") generated from the group's random id; you can set a local alias.
- Members learn about each other through the group's document and connect directly, so a group works even if two members were never introduced.

## What a group does

Each group has one [Automerge](https://automerge.org) document (`src/groupdoc.rs`) that members sync over their iroh connections (`src/core/sharing.rs`). It holds the member list, the file offers, the texts and the scratchpad. Documents live in `groups/<id>.am` next to `state.json`.

- **Files**: an offer is an entry in the document (name, size, blake3 hash, who offered it, who has downloaded it). The bytes stay on the offerer's device and are sent straight to whoever downloads them, verified against the hash. Per group, on each device:
  - *Download every file automatically* (default **on**): new offers are fetched as soon as the offerer is reachable. Turn it off to get a Download button instead.
  - *Stop sharing a file once everybody has it* (default **off**): applies to the files you offer. You can always stop sharing a file by hand.
  - On Android there are three pickers: any file, photos and videos, and installed apps (the app's APK). Downloads end up in `Download/flipflop`.
- **Texts**: not a chat, a feed of pasted links, codes and snippets, each labelled with the name of the device that sent it. Links get an Open button; everything has Copy; you can delete your own.
- **Scratchpad**: one big plain-text document everybody can edit; concurrent edits merge, and your cursor stays put when someone else edits.
- **Apps**: placeholder. Planned: WebAssembly apps shared in the group, run on every device and rendered with a hypereffect.dev interface.

## Guest links

A guest link is a group that anyone with a browser joins, no app needed (*Groups → Guest link*, or `flipflop-cli guest new`). It is an ordinary group with its own document, plus a secret token. Each link comes as two addresses:

- **Same network**: `http://<lan ip>:53530/<token>/`, served by the app itself (a small HTTP server that only runs while a guest link exists). Needs no internet. The page syncs the document over a WebSocket to that server.
- **Anywhere**: `https://andunieee.github.io/flipflop/guest/v<app version>/#t=<token>&e=<our endpoint id>&r=<our home relay>`. The same page, with iroh running in the browser (wasm): it dials us through our home relay and syncs the document over an iroh stream. The fragment never leaves the browser.

Whoever opens a link is a member of the group, named after where they come from, such as `beautiful-iguana` (a hash of their iroh key, kept in the browser, or of their LAN address). The page has a copy of the group's Automerge document, so it does what the apps do: download the files on offer, upload a file (only if you allowed it when making the link, switchable any time), write and read texts, edit the scratchpad. Uploads are saved under `<downloads>/Guests` and offered to the group in the guest's name. You can make as many links as you like; closing one disconnects its guests and deletes its group.

Anyone with a link can read and change everything in the group's document, so keep guest groups for guests, and share a link as you would the files in it.

The page is `src/core/guest/page.html`; its engine is `web/`, a wasm crate that compiles the very `src/groupdoc.rs` the app uses (plus iroh, for the full build). `just web` rebuilds it: the full page (`web/dist`; `.github/workflows/pages.yml` publishes it to GitHub Pages once per release at `/guest/v<version>/`, so a link always opens the page that matches the app that made it, and old links keep working) and the build without iroh that the app embeds for the LAN (`src/core/guest/assets`, committed, so rebuild and commit it when `groupdoc.rs` or `web/` change).

## CLI

`flipflop-cli` (clap) does everything the GUI does. Each command starts a node with the identity in `--dir` (default `~/.local/share/flipflop`, the same as the GUI, so both are the same device), waits for the peers it needs, does its job and gives the change a moment to sync. Only one process runs a node on a `--dir`. If the GUI (or a `serve`) already has it, the CLI checks the lock, finds the Unix socket (`ipc.sock` in the dir; a named pipe on Windows, untested) and sends the command there; the GUI runs it and the output comes back. Relative file paths are resolved in the CLI's working directory. `--downloads` is ignored in that case. Give one of them its own `FLIPFLOP_DIR` to keep two identities. `serve` keeps a node running (downloading, serving files) so offers stay available.

    flipflop-cli id                                  # id, name, ticket
    flipflop-cli peers                               # nearby peers and contacts
    flipflop-cli connect <ticket>
    flipflop-cli group new | list | show G | invite G PEER | accept G | decline G | rename G NAME | leave G
    flipflop-cli group settings G --auto-download false --auto-stop true
    flipflop-cli guest new [--uploads] [--name N]    # prints both addresses (while `serve` runs)
    flipflop-cli guest list | show G | uploads G true|false | close G
    flipflop-cli file offer G ./photo.jpg            # stays online until everyone has it (or --wait)
    flipflop-cli file list G | get G FILE | stop G FILE
    flipflop-cli text send G "https://example.com" | list G | delete G ID
    flipflop-cli pad show G | set G TEXT | append G TEXT
    flipflop-cli serve                               # run until Ctrl-C

`G`, `PEER` and `FILE` can be an id prefix or part of a name. `--wait SECS` bounds how long a command waits for peers. `FLIPFLOP_DIR` and `FLIPFLOP_DOWNLOADS` set the directories.

## Layout

The core, networking and CLI are Rust (`src/`). The UI is Compose Multiplatform in `kmp/`, a pure view over
a [UniFFI](https://mozilla.github.io/uniffi-rs/) bridge (`src/ffi.rs`): Rust builds a whole `UiState` and hands
it to the UI on every change; the UI calls commands on `App`. `kmp/shared` is the UI (and the generated
bindings), `kmp/desktopApp` the JVM desktop app, `kmp/androidApp` the Android app. Gradle builds the Rust
library itself (host for desktop, `cargo-ndk` for Android).

## Install

Grab the file for your system from the [latest release](https://github.com/andunieee/flipflop/releases/latest). Each desktop package carries its own Java runtime.

<details>
<summary>Linux</summary>

- **Debian / Ubuntu**: `sudo apt install ./flipflop-linux-amd64-v*.deb`
- **Fedora / openSUSE**: `sudo dnf install ./flipflop-linux-x86_64-v*.rpm`
- **AppImage** (any distro): `chmod +x flipflop-linux-x86_64-v*.AppImage && ./flipflop-linux-x86_64-v*.AppImage`. Needs FUSE, or run it with `--appimage-extract-and-run`.
- **Portable** `.tar.gz`: `tar xzf flipflop-linux-x86_64-v*.tar.gz && ./flipflop/bin/flipflop`

Bluetooth needs BlueZ (`bluetoothd`) and libdbus.
</details>

<details>
<summary>Windows</summary>

Run the `flipflop-windows-x64-setup-v*.msi`: it installs for your user (no admin rights), adds a Start menu entry and an uninstaller. Or extract the `.zip` and run `flipflop\flipflop.exe`. The app is unsigned, so SmartScreen shows "Windows protected your PC": More info → Run anyway.
</details>

<details>
<summary>macOS</summary>

Apple Silicon only. Open `flipflop-macos-arm64-v*.dmg` and drag flipflop to Applications. The app is unsigned: on first launch right-click it and choose Open, or run `xattr -dr com.apple.quarantine /Applications/flipflop.app`.
</details>

<details>
<summary>Android</summary>

Android 7+. Download `flipflop-android-v*.apk`, open it and allow installing from this source. Or, with a phone connected over `adb`: `adb install flipflop-android-v*.apk`.
</details>

## Building the packages

`.github/workflows/release.yml` builds all of these when a `v*` tag is pushed (the tag has to match `version` in `Cargo.toml`, which is also the version of every package). To build one yourself, you must be on the target OS (jpackage cannot cross-build desktop packages); the release build of the Rust library is made and bundled for you. Everything needs Rust, a **full JDK 17+** (with `jpackage`; a bare JRE will not do) and the usual [Run](#run) requirements. Packages end up in `kmp/desktopApp/build/compose/binaries/main/<format>/`.

| Package | Run on | Command | Also needs |
| --- | --- | --- | --- |
| `.deb` | Linux | `just deb` | `dpkg`, `fakeroot` |
| `.rpm` | Linux | `just rpm` | `rpmbuild` (`rpm` / `rpm-build`) |
| `.AppImage` | Linux | `just appimage` (→ `dist/`) | `curl` (fetches appimagetool), FUSE or `APPIMAGE_EXTRACT_AND_RUN=1` |
| portable folder / `.tar.gz` | any desktop | `just portable` | |
| `.msi` | Windows | `just msi` | [WiX 3](https://wixtoolset.org/docs/wix3/) in `PATH` |
| `.dmg` | macOS | `just dmg` | Xcode command line tools |
| `.apk` | any | `just apk` | cargo-ndk, NDK 27, Android SDK 35 (see [Run](#run)) |

Linux also needs libdbus, libv4l and libclang development headers to compile the Rust library (`libdbus-1-dev libv4l-dev libclang-dev` on Debian).

Android release APKs are signed with the debug key unless you set `FLIPFLOP_KEYSTORE` (path to a `.jks`), `FLIPFLOP_KEYSTORE_PASSWORD` and optionally `FLIPFLOP_KEY_PASSWORD`, as environment variables or gradle properties. In CI, store the base64 of the keystore in the `FLIPFLOP_KEYSTORE_B64` secret and the password as the `FLIPFLOP_KEYSTORE_PASSWORD` secret (plus `FLIPFLOP_KEY_PASSWORD` if the key has its own). The key alias is always `flipflop`; create the keystore with `keytool -genkeypair -v -keystore flipflop.jks -alias flipflop -keyalg RSA -keysize 2048 -validity 10000`. Use one key for good: Android refuses to update an app signed with a different one.

On macOS and Windows the packages are not code-signed or notarized. To do so, add `signing { }` / `notarization { }` to `nativeDistributions.macOS` in `kmp/desktopApp/build.gradle.kts` ([docs](https://github.com/JetBrains/compose-multiplatform/blob/master/tutorials/Signing_and_notarization_on_macOS/README.md)) and sign the `.msi` with `signtool`.

### Icons

Every icon comes from `assets/top.png` and is committed: the Linux hicolor PNGs (`packaging/icons`), `packaging/windows/flipflop.ico`, `packaging/macos/flipflop.icns`, the Android launcher icons (`kmp/androidApp/src/main/res`, adaptive) and the logo in the app and on the guest page. After changing the artwork run `packaging/icons.sh` (needs ImageMagick 7). `assets/side.png` is the banner above. The `.desktop` file and AppStream metadata for Linux are in `packaging/linux/`.

## Run

    just desktop                   # desktop GUI (FLIPFLOP_DIR=... for a separate identity)
    just android                   # APK; `just install` to put it on a device
    cargo run --bin flipflop-cli -- --help
    just web                       # rebuild the guest page's wasm (see Guest links)
    cargo test                     # unit tests + multi-node integration tests

Android needs cargo-ndk, NDK 27 and an Android SDK with platform 35 (`kmp/local.properties`: `sdk.dir=...`).
The desktop app needs a JDK 17+ and loads `libflipflop` from the cargo target directory.

Linux BLE needs BlueZ (`bluetoothd`) and libdbus.

## License

[AGPL-3.0-or-later](LICENSE), inherited from `iroh-ble-transport` and `blew`.
