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
- **Home**: per group, what is new since you last looked (files and texts from others, scratchpad changes, new members). Opening a group marks it as seen; `flipflop home` shows the same.
- **Groups**: create a group, then invite contacts. Each gets an invite and must accept. If they are offline, the invite is sent when they next connect. Declining is remembered per `(inviter, group)`: repeat invites for that pair are not shown, invites from other peers or to other groups still are. Default group names are two-word verses (e.g. "pale moon") generated from the group's random id; you can set a local alias.
- Members learn about each other through the group's document and connect directly, so a group works even if two members were never introduced.

## What a group does

Each group has one [Automerge](https://automerge.org) document (`src/groupdoc.rs`) that members sync over their iroh connections (`src/core/sharing.rs`). It holds the member list, the file offers, the texts, the scratchpad, the group's tabs and the apps' data. Documents live in `groups/<id>.am` next to `state.json`.

- **Files**: an offer is an entry in the document (name, size, blake3 hash, who offered it, who has downloaded it). The bytes stay on the offerer's device and are sent straight to whoever downloads them, verified against the hash; the offerer sees a progress bar per receiver. A file with the same name and bytes as one on offer in the group is refused, unless the only ones who have it are offline. Per group, on each device:
  - *Download every file automatically* (default **on**): new offers are fetched as soon as the offerer is reachable, one at a time. Turn it off to get a Download button instead.
  - Whatever that setting says, a download can be cancelled, which puts the file on hold on this device: what arrived is thrown away, and it is not fetched again until you press Download (it then starts over).
  - *Stop sharing a file once everybody has it* (default **off**, **on** in an implied group): applies to the files you offer. You can always stop sharing a file by hand.
  - On Android there are three pickers: any file, photos and videos, and installed apps (the app's APK). Downloads end up in `Download/flipflop`. While files are moving the app stays alive out of sight, with a progress notification, until they are done. Android 12+ only allows that to start while the app is on screen: a transfer that begins in the background gets it the next time the app is opened.
- **Texts**: not a chat, a feed of pasted links, codes and snippets, each labelled with the name of the device that sent it. Links get an Open button; everything has Copy; you can delete your own.
- **Scratchpad**: one big plain-text document everybody can edit; concurrent edits merge, and your cursor stays put when someone else edits.
- **Tabs**: Files, Texts and Scratchpad, plus one per app. Which ones show is part of the document, so it is the same for every member, and anyone can switch them in the group's settings.
- **Apps**: a WebAssembly file added from the group's settings becomes a tab. Every member's device fetches it while its tab is on (whatever the download settings; it is not listed with the files), keeps it in `apps/<hash>.wasm` and serves it to the others. The runtime (`src/apps.rs`) gives each app its own section of the document, read and written by path in CBOR, and tells it what changed as patches. Not yet: running apps in the app and showing their hypereffect.dev interface.

## Guest links

A guest link is a group that anyone with a browser joins, no app needed (*Groups → Guest link*, or `flipflop guest new`). It is an ordinary group with its own document, plus a secret token. Each link comes as two addresses:

- **Same network**: `http://<lan ip>:53530/<token>/`, served by the app itself (a small HTTP server that only runs while a guest link exists). Needs no internet. The page syncs the document over a WebSocket to that server.
- **Anywhere**: `https://andunieee.github.io/flipflop/guest/v<app version>/#t=<token>&e=<our endpoint id>&r=<our home relay>`. The same page, with iroh running in the browser (wasm): it dials us through our home relay and syncs the document over an iroh stream. The fragment never leaves the browser.

Whoever opens a link is a member of the group, named after where they come from, such as `beautiful-iguana` (a hash of their iroh key, kept in the browser, or of their LAN address). The page has a copy of the group's Automerge document, so it does what the apps do: download the files on offer, upload a file (only if you allowed it when making the link, switchable any time), write and read texts, edit the scratchpad. Uploads are saved under `<downloads>/Guests` and offered to the group in the guest's name. You can make as many links as you like; closing one disconnects its guests and deletes its group.

Anyone with a link can read and change everything in the group's document, so keep guest groups for guests, and share a link as you would the files in it.

The page is `src/core/guest/page.html`; its engine is `web/`, a wasm crate that compiles the very `src/groupdoc.rs` the app uses (plus iroh, for the full build). `just web` rebuilds it: the full page (`web/dist`; `.github/workflows/pages.yml` publishes it to GitHub Pages once per release at `/guest/v<version>/`, so a link always opens the page that matches the app that made it, and old links keep working) and the build without iroh that the app embeds for the LAN (`src/core/guest/assets`, committed, so rebuild and commit it when `groupdoc.rs` or `web/` change).

## CLI

`flipflop` (clap) does everything the GUI does. Each command starts a node with the identity in `--dir` (default `~/.local/share/flipflop`, the same as the GUI, so both are the same device), waits for the peers it needs, does its job and gives the change a moment to sync. Only one process runs a node on a `--dir`. If the GUI (or a `serve`) already has it, the CLI checks the lock, finds the Unix socket (`ipc.sock` in the dir; a named pipe on Windows, untested) and sends the command there; the GUI runs it and the output comes back. Relative file paths are resolved in the CLI's working directory. `--downloads` is ignored in that case. Give one of them its own `FLIPFLOP_DIR` to keep two identities. `serve` keeps a node running (downloading, serving files) so offers stay available. `flipflop gui` (or `flipflop open`) starts the desktop app on the same `--dir`: `flipflop-gui` from `PATH` or next to `flipflop`, else the installed app (`/opt/flipflop`, `/Applications/flipflop.app`, `%LOCALAPPDATA%\flipflop`, then the Flatpak). On Linux and macOS the app ships a `bin/flipflop-gui` link to its launcher (the app and its shortcuts are still called flipflop), so `ln -s /opt/flipflop/bin/flipflop-gui ~/.local/bin/` puts it in `PATH` without taking the `flipflop` name.

    flipflop id                                  # id, name, ticket
    flipflop peers                               # nearby peers and contacts
    flipflop connect <ticket>
    flipflop group new | list | show G | invite G PEER | accept G | decline G | rename G NAME | leave G
    flipflop group settings G --auto-download false --auto-stop true
    flipflop guest new [--uploads] [--name N]    # prints both addresses (while `serve` runs)
    flipflop guest list | show G | uploads G true|false | close G
    flipflop file offer G ./photo.jpg            # stays online until everyone has it (or --wait)
    flipflop file list G | get G FILE | hold G FILE | stop G FILE
    flipflop text send G "https://example.com" | list G | delete G ID
    flipflop pad show G | set G TEXT | append G TEXT
    flipflop serve                               # run until Ctrl-C
    flipflop gui                                 # open the desktop app

`G`, `PEER` and `FILE` can be an id prefix or part of a name. `--wait SECS` bounds how long a command waits for peers. `FLIPFLOP_DIR` and `FLIPFLOP_DOWNLOADS` set the directories.

## Layout

The core, networking and CLI are Rust (`src/`). The UI is Compose Multiplatform in `kmp/`, a pure view over
a [UniFFI](https://mozilla.github.io/uniffi-rs/) bridge (`src/ffi.rs`): Rust builds a whole `UiState` and hands
it to the UI on every change; the UI calls commands on `App`. `kmp/shared` is the UI (and the generated
bindings), `kmp/desktopApp` the JVM desktop app, `kmp/androidApp` the Android app. Gradle builds the Rust
library itself (host for desktop, `cargo-ndk` for Android).

## Install

Grab the file for your system from the [latest release](https://github.com/andunieee/flipflop/releases/latest), or use the commands below, which all download the latest one. Each desktop package carries its own Java runtime.

<details>
<summary>Linux</summary>

**Flatpak** (any distro), `.flatpak`, installs it for your user. It keeps its identity in `~/.local/share/flipflop`, like the command line client, and `flipflop gui` finds it:
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*\.flatpak')" -o /tmp/flipflop.flatpak && flatpak install --user -y /tmp/flipflop.flatpak
```

**Debian / Ubuntu**, `.deb`:
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*\.deb')" -o /tmp/flipflop.deb && sudo apt install /tmp/flipflop.deb
```

**Fedora / openSUSE**, `.rpm`:
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*\.rpm')" -o /tmp/flipflop.rpm && sudo dnf install /tmp/flipflop.rpm
```

**Arch**, from the [AUR](https://aur.archlinux.org/packages/flipflop-bin) with your AUR helper:
```
yay -S flipflop-bin
```
or the `.pkg.tar.zst` from the release:
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*\.pkg\.tar\.zst"' | tr -d '"')" -o /tmp/flipflop.pkg.tar.zst && sudo pacman -U /tmp/flipflop.pkg.tar.zst
```

**AppImage**, saves it to `~/.local/bin` and runs it with `--install-desktop-entry`, which adds flipflop to the application menu for your user:
```
mkdir -p ~/.local/bin && curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*\.AppImage')" -o ~/.local/bin/flipflop.AppImage && chmod +x ~/.local/bin/flipflop.AppImage && ~/.local/bin/flipflop.AppImage --install-desktop-entry
```
It needs FUSE (otherwise run it with `--appimage-extract-and-run`).

**Portable** (`.tar.gz`), unpacks it to `~/.local/lib/flipflop`, puts `flipflop-gui` in `~/.local/bin` and adds it to the menu with the same flag:
```
mkdir -p ~/.local/lib ~/.local/bin && curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*linux-x86_64[^"]*\.tar\.gz')" | tar -xz -C ~/.local/lib && ln -sf ~/.local/lib/flipflop/bin/flipflop-gui ~/.local/bin/ && ~/.local/bin/flipflop-gui --install-desktop-entry
```

Bluetooth needs BlueZ (`bluetoothd`) and libdbus.
</details>

<details>
<summary>Windows</summary>

**Manually**: download the `windows-x64-setup-` `.msi` from the [releases page](https://github.com/andunieee/flipflop/releases) and run it. It installs for your user (no admin rights) and adds a Start menu entry and an uninstaller.

**One-liner** (PowerShell), does the same: downloads the installer and runs it.
```
$u=(irm https://api.github.com/repos/andunieee/flipflop/releases/latest).assets.browser_download_url -match 'windows-x64-setup-v'; iwr $u -OutFile $env:TEMP\flipflop-setup.msi; Start-Process $env:TEMP\flipflop-setup.msi
```

**No installer** (PowerShell): downloads the `.zip` and unzips it to `%LOCALAPPDATA%\flipflop` (where `flipflop gui` looks for it). No Start menu entry.
```
$u=((irm https://api.github.com/repos/andunieee/flipflop/releases/latest).assets.browser_download_url -match 'windows-x64-v.*\.zip$')[0]; iwr $u -OutFile $env:TEMP\flipflop.zip; Expand-Archive -Force $env:TEMP\flipflop.zip $env:LOCALAPPDATA; & $env:LOCALAPPDATA\flipflop\flipflop.exe
```

The app is unsigned, so SmartScreen shows "Windows protected your PC": More info → Run anyway.
</details>

<details>
<summary>macOS</summary>

Apple Silicon only, and the app is unsigned.

**Manually**: open the `macos-arm64` `.dmg` and drag flipflop to Applications. On first launch right-click it and choose Open, or run `xattr -dr com.apple.quarantine /Applications/flipflop.app`.

**One-liner** (Terminal), copies the app out of the `.dmg` into `/Applications` (a download made with `curl` is not quarantined, so it opens straight away):
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*\.dmg')" -o /tmp/flipflop.dmg && hdiutil attach -nobrowse -mountpoint /tmp/flipflop-dmg /tmp/flipflop.dmg && ditto /tmp/flipflop-dmg/flipflop.app /Applications/flipflop.app; hdiutil detach /tmp/flipflop-dmg
```
</details>

<details>
<summary>Android</summary>

Android 7+.

**Zapstore**: install [Zapstore](https://zapstore.dev) and search for flipflop. It keeps the app up to date.

**Manually**: download the `.apk`, open it and allow installing from this source.

**With adb**, on a phone connected over `adb`:
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/flipflop/releases/latest | grep -o 'https://[^"]*\.apk')" -o /tmp/flipflop.apk && adb install /tmp/flipflop.apk
```
</details>

## Building the packages

`.github/workflows/release.yml` builds all of these when a `v*` tag is pushed (the tag has to match `version` in `Cargo.toml`, which is also the version of every package). To build one yourself, you must be on the target OS (jpackage cannot cross-build desktop packages); the release build of the Rust library is made and bundled for you. Everything needs Rust, a **full JDK 17+** (with `jpackage`; a bare JRE will not do) and the usual [Run](#run) requirements. Packages end up in `kmp/desktopApp/build/compose/binaries/main/<format>/`.

| Package | Run on | Command | Also needs |
| --- | --- | --- | --- |
| `.deb` | Linux | `just deb` | `dpkg`, `fakeroot` |
| `.rpm` | Linux | `just rpm` | `rpmbuild` (`rpm` / `rpm-build`) |
| `.AppImage` | Linux | `just appimage` (→ `dist/`) | `curl` (fetches appimagetool), FUSE or `APPIMAGE_EXTRACT_AND_RUN=1` |
| `.flatpak` | Linux | `just flatpak` (→ `dist/`) | `flatpak-builder` (fetches the freedesktop 25.08 runtime from flathub) |
| `.pkg.tar.zst` | Arch | `just arch` (→ `dist/`) | what `.deb` needs, `makepkg` |
| portable folder / `.tar.gz` | any desktop | `just portable` | |
| `.msi` | Windows | `just msi` | [WiX 3](https://wixtoolset.org/docs/wix3/) in `PATH` |
| `.dmg` | macOS | `just dmg` | Xcode command line tools |
| `.apk` | any | `just apk` | cargo-ndk, NDK 27, Android SDK 35 (see [Run](#run)) |

Linux also needs libdbus, libv4l, libclang and libwayland development headers to compile the Rust library (`libdbus-1-dev libv4l-dev libclang-dev libwayland-dev` on Debian). The Android build needs them as well, even though it targets Android: its Kotlin bindings are generated from a host build of the library.

Android release APKs are signed with the debug key unless you set `FLIPFLOP_KEYSTORE` (path to a `.jks`), `FLIPFLOP_KEYSTORE_PASSWORD` and optionally `FLIPFLOP_KEY_PASSWORD`, as environment variables or gradle properties. In CI, store the base64 of the keystore in the `FLIPFLOP_KEYSTORE_B64` secret and the password as the `FLIPFLOP_KEYSTORE_PASSWORD` secret. Only set `FLIPFLOP_KEY_PASSWORD` if the keystore really has a separate key password: modern `keytool` creates PKCS12 keystores, which use the store password for the key too, so a differing (or blank) `FLIPFLOP_KEY_PASSWORD` makes the build fail with `Given final block not properly padded`. The key alias is always `flipflop`; create the keystore with `keytool -genkeypair -v -keystore flipflop.jks -alias flipflop -keyalg RSA -keysize 2048 -validity 10000`. Use one key for good: Android refuses to update an app signed with a different one.

The Flatpak is not built from source: it wraps the portable app (with its Java runtime) in the freedesktop runtime (`packaging/flatpak`), since Gradle and cargo would need every dependency vendored to build offline in flatpak-builder. The Arch package is the `.deb` repackaged by `packaging/aur/pkgbuild.sh`, which is also the PKGBUILD the AUR gets. The AppImage and the portable app take `--install-desktop-entry`, which writes `dev.flipflop.app.desktop` and the icons to `~/.local/share` with `Exec` pointing at the AppImage (or the launcher).

### Publishing

After the GitHub release the workflow publishes to two stores. Each job only warns and skips while its secret is missing:

- **AUR** (`flipflop-bin`): `AUR_SSH_PRIVATE_KEY`, the private half of an SSH key added to your [AUR account](https://aur.archlinux.org/account). The first push creates the package.
- **Zapstore** (`zapstore.yaml`): `ZAPSTORE_SIGN_WITH`, the `nsec` (or a `bunker://` URL) of the Nostr key that publishes it. Zapstore ties the app to its signing certificate, so set the Android keystore secrets above before the first one.
- The Arch `.pkg.tar.zst` on the release is signed when `GPG_PRIVATE_KEY` (base64 of `gpg --export-secret-keys`) and `GPG_KEY_ID` are set.

On macOS and Windows the packages are not code-signed or notarized. To do so, add `signing { }` / `notarization { }` to `nativeDistributions.macOS` in `kmp/desktopApp/build.gradle.kts` ([docs](https://github.com/JetBrains/compose-multiplatform/blob/master/tutorials/Signing_and_notarization_on_macOS/README.md)) and sign the `.msi` with `signtool`.

### Icons

Every icon comes from `assets/top.png` and is committed: the Linux hicolor PNGs (`packaging/icons`), `packaging/windows/flipflop.ico`, `packaging/macos/flipflop.icns`, the Android launcher icons (`kmp/androidApp/src/main/res`, adaptive) and the logo in the app and on the guest page. After changing the artwork run `packaging/icons.sh` (needs ImageMagick 7). `assets/side.png` is the banner above. The `.desktop` file and AppStream metadata for Linux are in `packaging/linux/`.

## Run

    just desktop                   # desktop GUI (FLIPFLOP_DIR=... for a separate identity)
    just android                   # APK; `just install` to put it on a device
    cargo run --bin flipflop -- --help
    just web                       # rebuild the guest page's wasm (see Guest links)
    cargo test                     # unit tests + multi-node integration tests

Android needs cargo-ndk (`cargo install cargo-ndk`), NDK 27 and an Android SDK with platform 35 (`kmp/local.properties`: `sdk.dir=...`).
The desktop app needs a JDK 17+ and loads `libflipflop` from the cargo target directory.

Linux BLE needs BlueZ (`bluetoothd`) and libdbus.

## License

[AGPL-3.0-or-later](LICENSE), inherited from `iroh-ble-transport` and `blew`.
