# Development notes

Native Rust GUI built with [Slint](https://slint.dev), replacing the Tauri + React
frontend. It drives the same P2P engine module (`src/engine.rs`, re-exporting
`src/native` + `src/protocol`) directly, no Tauri,
no webview, no JavaScript.

## Scope

Implemented:

- **Peers**, sidebar lists known peers from the paired-device store with
  presence dots; rename/forget a peer; rename your own device in Settings
  (set via `set_device_display_name`); per-peer pages show history and a
  "Send files…" button.
- **Add peer**, paste the peer's iroh address/ticket (`join_pairing`), or add
  one of the suggested peers: LAN mDNS neighbours and inbound pair requests
  (`request_nearby_pair` / `accept_nearby_invite`, decline supported).
- **Send**, one card per peer: type or paste text, attach pictures, an
  installed app (Android) or files/folders, then Send; shared and delivered
  directly to the peer with `invite_paired_device`; live progress and speed,
  stop sharing. Desktop has only text, Paste and Files (sent right away).
  A peer that can't be reached keeps the send queued ("Waiting for … to come
  online"): its share stays open and the invite is retried when the peer's
  presence comes back, when the network is ready, and every 60 s. Queued
  sends are saved to `queued-sends.json` in the data dir and restart with
  the app. The share
  closes by itself once the peer has everything.
- **Receive**, automatic: paired peers' file invites are accepted and
  downloaded into `<downloads folder>/flipflop/<peer-name>` without
  prompts; progress + cancel; conflict renaming recorded. Cancelled/failed
  receives keep their partial store for resume; deleting the history row
  frees it.
- **Transfers**, any number of sends and receives run at once (different
  peers); each peer's page shows its own transfer cards, and the sidebar marks
  peers with a transfer in flight (↑/↓).
- **Settings**, downloads folder, own device name, relay mode (default /
  disabled / custom URLs + auth token), local discovery (everyone / paired
  only / off), history toggle; persisted to `settings.json`.
- **Browser link**, for someone without flipflop: "Add peer → Open browser
  link" starts a small HTTP/1.1 server (`src/guest`) on the LAN and adds a
  temporary "Browser" peer with a QR code and URL
  (`http://<lan-ip>:53530/<random token>`; any free port if 53530 is taken).
  The token is the only key: no prompts, and every other path is a 404.
  Sends to it become offers on the web page (one file as is, a folder or
  several files as a streamed, stored zip with an exact length; text shown
  inline); uploads from the page are saved to `<downloads>/Browser`. Both
  use ordinary transfer rows and history under the peer id `browser`.
  Closing the link stops the server and withdraws whatever wasn't
  downloaded. Plain HTTP (no secure context in the page, so its Copy button
  falls back to `execCommand`), IPv4 only, and only while the app runs (no
  Android foreground service).
  "Anywhere" on the link card swaps the URL for one to the same page on
  GitHub Pages (`https://andunieee.github.io/flipflop/#t=<token>&e=<endpoint
  id>&r=<home relay>`, all in the fragment, which never leaves the browser).
  There the page loads iroh as wasm (`web/`), dials us through our home relay
  on the `flipflop/guest/0` ALPN, and sends the same requests as on the LAN,
  one HTTP/1.1 exchange per QUIC stream; the app serves them with the same
  routes and token check (`guest::http::serve_tunnel`). Downloads stream
  to disk through a service worker (`web/sw.js`), or are buffered in memory
  where there is none. Needs the relay (not "disabled"); a custom relay
  works only if browsers may connect to it without an auth token.
- **Notifications**, toasts: an in-window overlay on desktop, native
  `Toast`s on Android.

Not implemented (v1): tray, autostart, updater. Engine-level transfer behavior
(iroh, BLAKE3 verification, resume, relay fallback, history partial stores) is
identical to the Tauri app because it is the same code path.

## Build & run

```sh
cargo run --release
```

Data dir: `$XDG_DATA_HOME/flipflop` (override with
`FLIPFLOP_DATA_DIR`); an existing `tunnelmanager-slint` dir from before the
rename is moved there on first launch. Own per-peer history entries (rows carrying
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
- **Sending**: the peer's send card stages attachments into an
  app-private outbox and lists them above the text box; "Send" sends the
  text and the staged files. Files come from the SAF picker, pictures from
  the photo picker (`FilePicker.java`, a headless fragment, since
  `NativeActivity` drops `onActivityResult`), and "App" copies an installed
  app's base APK (`AppList.java`; split APKs are not bundled). "Share →
  flipflop" from any app stages its content (files, or shared text/links as
  a `.txt`) the same way and opens the peer picker.
- **Shares while running**: the share sheet starts a second activity instance
  in the sharing app's task, and android-activity runs `android_main` again
  for it in the same process. That instance does not start Slint; it stages
  its intent's content, hands it to the running UI, moves the UI's task to the
  front (`REORDER_TASKS`) and finishes itself. The process exits when the main
  activity is destroyed, so a later launch always starts a fresh UI.
- **Look**: launcher icon (adaptive), a branded launch screen and the light
  system-bar colors come from `android/res`; the PNGs there are generated
  from the SVGs in `android/icon` by `android/gen-res.sh`. The UI pads itself
  by the window's safe-area insets, since it is drawn edge-to-edge.
- **Received files** land in `Download/flipflop/<peer>/` (public storage;
  Android 8/9 ask for the storage permission first, Android 10 falls back to
  the app-private downloads folder). The Settings page hides the folder
  picker accordingly.
- **Data dir** is `/data/data/com.flipflop.app/files`. The package id changed
  with the rename (was `dev.tunnelmanager.slint`), so Android installs it as a
  new app; pairings from the old one do not carry over.

Prerequisites: Android SDK + NDK, `ANDROID_HOME`/`ANDROID_NDK_ROOT` set, and
the rust targets `rustup target add aarch64-linux-android x86_64-linux-android`.

Build & run on a device/emulator with [cargo-apk](https://crates.io/crates/cargo-apk):

```sh
cargo install cargo-apk
cargo apk run -p flipflop
```

Logs: `adb logcat -s flipflop RustStdoutStderr` (`tracing` output goes
to logcat under the `flipflop` tag; `RustStdoutStderr` shows panics).

The manifest (package `com.flipflop.app`, min SDK 26, share-sheet
intent filters, permissions, icon and theme) is generated from
`[package.metadata.android*]` in `Cargo.toml`.

## Toolchain note

The repo pins rustc 1.91 for the engine, but Slint (pinned to `=1.16.1`; 1.17.x
depends on an unpublished android backend crate) needs 1.92, so this crate
carries its own `rust-toolchain.toml` (1.92). `tinyvec` is pinned to
1.10.0 in `Cargo.lock` because 1.13 fails to compile on 1.92.

## Layout

- `ui/`, Slint markup (`globals.slint` holds shared state + logic callbacks,
  one file per page; `State.compact`/`State.touch` drive the responsive
  single-pane/touch layout). `ui/icons/` holds the line icons, exposed through
  the `Icons` global in `components.slint`.
- `android/`, Android resources (launcher icon, launch theme) and their
  sources.
- `src/lib.rs`, crate root: shared by the desktop bin and the Android
  `cdylib`.
- `src/main.rs`, desktop entry point.
- `src/app.rs`, startup, node events, peers/pairing/history/settings wiring.
- `src/transfers.rs`, send and receive flows; one `TransferRow` per transfer.
- `src/guest.rs`, the browser link: session, offers, uploads; `guest/http.rs`
  is the server and routes, `guest/zip.rs` the streaming zip writer,
  `guest/page.html` the page itself.
- `web/`, the browser link's page on the web: a separate wasm crate (the
  iroh tunnel), the download service worker and `build.sh`, which bundles
  them with `src/guest/page.html`. `.github/workflows/pages.yml` publishes
  it to GitHub Pages on every push to master touching them. Locally:
  `rustup target add wasm32-unknown-unknown`, a `wasm-bindgen` CLI of the
  version pinned in `web/Cargo.toml`, then `web/build.sh` and serve
  `web/dist` from `http://localhost` (a secure context, for the worker).
- `src/platform.rs`, per-platform clipboard, toasts, dialogs, "open", data
  dir and logging.
- `src/android.rs`, Android platform services (JNI clipboard + toast,
  share-sheet outbox, `android_main`).
- `src/emitter.rs`, the node service's `EventEmitter` (an event queue the UI
  drains on a timer) and `apply_transfer_event`, the engine event → transfer
  row mapping.
- `src/recorder.rs`, slim port of the Tauri shell's history recorder.
- `src/settings.rs`, settings persistence.
- `examples/screenshots.rs`, renders every page with sample data, headless.

## Development

```sh
cargo test                       # pure logic: formatting, event mapping, recorder, settings
cargo run --example screenshots -- /tmp/shots   # PPM renders of each page (desktop + phone)
```

The screenshot example uses Slint's software renderer, so it needs no display
and starts no node, handy for checking layout changes. The std-widgets are
pinned to the `fluent-light` style in `build.rs`.
