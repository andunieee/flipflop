# TunnelManager — Slint GUI

Native Rust GUI built with [Slint](https://slint.dev), replacing the Tauri + React
frontend. It drives the same P2P engine crate (`engine/`) directly — no Tauri,
no webview, no JavaScript.

## Scope

Implemented:

- **Peers** — sidebar lists known peers from the paired-device store with
  presence dots; rename/forget a peer; rename your own device in Settings
  (set via `set_device_display_name`); per-peer pages show history and a
  "Send files…" button.
- **Add peer** — paste the peer's iroh address/ticket (`join_pairing`), or add
  one of the suggested peers: LAN mDNS neighbours and inbound pair requests
  (`request_nearby_pair` / `accept_nearby_invite`, decline supported).
- **Send** — pick files/folders, share them, deliver directly to the peer with
  `invite_paired_device`; live progress, stop sharing.
- **Receive** — automatic: paired peers' file invites are accepted and
  downloaded into `downloads/<peer-name>` without prompts; progress + cancel;
  conflict renaming recorded.
- **Settings** — downloads folder, own device name, relay mode (default /
  disabled / custom URLs + auth token), local discovery (everyone / paired
  only / off), history toggle; persisted to `settings.json`.

Not implemented (v1): tray, autostart, updater. Engine-level transfer behavior
(iroh, BLAKE3 verification, resume, relay fallback, history partial stores) is
identical to the Tauri app because it is the same code path.

## Build & run

```sh
cd gui-slint
cargo run --release
```

Data dir: `$XDG_DATA_HOME/tunnelmanager-slint` (override with
`TUNNELMANAGER_SLINT_DATA_DIR`). Own per-peer history entries (rows carrying
peer info) are not shared with the Tauri app's ticket-era history file; by
default this GUI keeps its own history.

## Android

The same UI runs on Android through Slint's `android-activity` backend. The
crate builds as a `cdylib` (`src/lib.rs`) plus the desktop bin; the window
switches to a single-pane layout with a bottom nav bar when there is no room
for the sidebar (`State.compact`), and uses larger touch-sized rows and
buttons on Android (`State.touch`).

Platform integration (see `src/android.rs`):

- **Clipboard / toasts** go through JNI (`ClipboardManager`, `Toast`) on the
  Java main thread.
- **Sending**: pick files through the system share sheet — "Share →
  TunnelManager" from any app stages the content into an app-private outbox;
  the app immediately opens the peer picker and a toast shows the count, and
  "Send files…" on a peer sends the staged content. A native SAF picker is not
  possible because `android-activity` does not forward `onActivityResult`.
  Sharing while the app is only in the background cannot be observed either
  (no `onNewIntent` forwarding); that share restarts the activity.
- **Receiving** land in the app-private downloads folder; opening rows is a
  no-op on Android (use a file manager). The Settings page hides the folder
  picker accordingly.
- **Data dir** is `/data/data/dev.tunnelmanager.slint/files`.

Prerequisites: Android SDK + NDK, `ANDROID_HOME`/`ANDROID_NDK_ROOT` set, and
the rust targets `rustup target add aarch64-linux-android x86_64-linux-android`.

Build & run on a device/emulator with [cargo-apk](https://crates.io/crates/cargo-apk):

```sh
cargo install cargo-apk
cd gui-slint
cargo apk run -p tunnelmanager-slint
```

Logs: `adb logcat -s slint RustStdoutStderr` (tracing output is not wired to
logcat yet; `RustStdoutStderr` shows panics).

The manifest (package `dev.tunnelmanager.slint`, min SDK 26, share-sheet
intent filters, INTERNET permission) is generated from
`[package.metadata.android*]` in `Cargo.toml`.

## Toolchain note

The repo pins rustc 1.91 for the engine, but Slint 1.17 needs 1.92, so this
crate carries its own `rust-toolchain.toml` (1.92). `tinyvec` is pinned to
1.10.0 in `Cargo.lock` because 1.13 fails to compile on 1.92.

## Layout

- `ui/` — Slint markup (`globals.slint` holds shared state + logic callbacks,
  one file per page; `State.compact`/`State.touch` drive the responsive
  single-pane/touch layout).
- `src/lib.rs` — crate root: shared by the desktop bin and the Android
  `cdylib`.
- `src/main.rs` — desktop entry point.
- `src/app.rs` — UI wiring, share/receive flows, per-platform dialog/
  clipboard/open hooks.
- `src/android.rs` — Android platform services (JNI clipboard + toast,
  share-sheet outbox, `android_main`).
- `src/emitter.rs` — implements the engine's `EventEmitter` on a Slint window
  handle: engine events update the UI.
- `src/recorder.rs` — slim port of the Tauri shell's history recorder.
- `src/settings.rs` — settings persistence.
