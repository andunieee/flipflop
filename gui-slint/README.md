# DashBeam — Slint GUI (alternative)

Native Rust GUI built with [Slint](https://slint.dev), replacing the Tauri + React
frontend. It drives the same P2P engine crate (`engine/`) directly — no Tauri,
no webview, no JavaScript.

## Scope

Implemented:

- **Send** — pick files/folders, create a share ticket, copy it, live progress
  per peer, stop sharing.
- **Receive** — paste a ticket, fetch sender metadata (name/size/item count),
  pick save folder, download with progress + speed, cancel (partial store kept
  for resume), open destination.
- **History** — same `transfer-history.json` as the Tauri app: list, open,
  delete, clear; rows are recorded from the engine events (send and receive).
- **Settings** — downloads folder, relay mode (default / disabled / custom URLs
  + auth token), history toggle; persisted to `settings.json`.

Not implemented (v1): device pairing, Nearby discovery, tray, autostart,
updater. Engine-level transfer behavior (iroh, BLAKE3 verification, resume,
conflict renaming, relay fallback of history partial stores) is identical to
the Tauri app because it is the same code path.

## Build & run

```sh
cd gui-slint
cargo run --release
```

Data dir: `$XDG_DATA_HOME/dashbeam-slint` (override with
`DASHBEAM_SLINT_DATA_DIR`). `transfer-history.json` is shared with the Tauri
app only if you point both at the same directory; by default this GUI keeps
its own.

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
