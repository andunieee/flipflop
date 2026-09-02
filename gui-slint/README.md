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

## Toolchain note

The repo pins rustc 1.91 for the engine, but Slint 1.17 needs 1.92, so this
crate carries its own `rust-toolchain.toml` (1.92). `tinyvec` is pinned to
1.10.0 in `Cargo.lock` because 1.13 fails to compile on 1.92.

## Layout

- `ui/` — Slint markup (`globals.slint` holds shared state + logic callbacks,
  one file per page).
- `src/main.rs` — UI wiring, share/receive flows, dialogs (rfd), clipboard
  (arboard).
- `src/emitter.rs` — implements the engine's `EventEmitter` on a Slint window
  handle: engine events update the UI.
- `src/recorder.rs` — slim port of the Tauri shell's history recorder.
- `src/settings.rs` — settings persistence.
