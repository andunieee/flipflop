# flipflop

Rust + Slint peer-to-peer app (desktop and Android). Peers connect with [iroh](https://iroh.computer).

- **LAN**: devices announce over mDNS (service `_flipflop._udp`) with their name; they appear under *Nearby*.
- **QR**: *Add by QR* shows your ticket as a QR code and scans others' (webcam on desktop, system scanner on Android). Tickets can also be pasted.
- **BLE discovery**: every device advertises a GATT service whose characteristic holds `{name, ticket}` and scans for it, so peers learn each other's full endpoint id and name.
- **BLE transport**: [`iroh-ble-transport`](https://crates.io/crates/iroh-ble-transport) adds Bluetooth LE as an iroh network path next to IP and relay, so connections and chat can run over Bluetooth when there is no network. It only advertises a 12-byte key prefix, which is why discovery above is kept. It is experimental. On Android the transport is added when the endpoint is bound, so the Bluetooth permission is requested at start-up; if it is denied the transport stays off until the next launch.
- **Name**: generated as `<hostname or model> (<os>)`, editable in the UI, persisted.

## Contacts and groups

- **Contacts**: every peer you connect to (or that connects to you) is saved with its public key and name. Saved contacts reconnect automatically, with backoff, and immediately when they show up on the LAN or over BLE. You can rename them locally (the peer's own name stays visible, dimmed) or remove them. A removed contact that keeps dialing you is still accepted but is not saved again; connecting to them yourself saves them again.
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

## CLI

`flipflop-cli` (clap) does everything the GUI does. Each command starts a node with the identity in `--dir` (default `~/.local/share/flipflop`, the same as the GUI, so both are the same device), waits for the peers it needs, does its job and gives the change a moment to sync. Only one process runs a node on a `--dir`. If the GUI (or a `serve`) already has it, the CLI checks the lock, finds the Unix socket (`ipc.sock` in the dir; a named pipe on Windows, untested) and sends the command there; the GUI runs it and the output comes back. Relative file paths are resolved in the CLI's working directory. `--downloads` is ignored in that case. Give one of them its own `FLIPFLOP_DIR` to keep two identities. `serve` keeps a node running (downloading, serving files) so offers stay available.

    flipflop-cli id                                  # id, name, ticket
    flipflop-cli peers                               # nearby peers and contacts
    flipflop-cli connect <ticket>
    flipflop-cli group new | list | show G | invite G PEER | accept G | decline G | rename G NAME | leave G
    flipflop-cli group settings G --auto-download false --auto-stop true
    flipflop-cli file offer G ./photo.jpg            # stays online until everyone has it (or --wait)
    flipflop-cli file list G | get G FILE | stop G FILE
    flipflop-cli text send G "https://example.com" | list G | delete G ID
    flipflop-cli pad show G | set G TEXT | append G TEXT
    flipflop-cli serve                               # run until Ctrl-C

`G`, `PEER` and `FILE` can be an id prefix or part of a name. `--wait SECS` bounds how long a command waits for peers. `FLIPFLOP_DIR` and `FLIPFLOP_DOWNLOADS` set the directories.

## Run

    cargo run                      # desktop GUI (FLIPFLOP_DIR=... for a separate identity)
    cargo run --bin flipflop-cli -- --help
    cargo test                     # unit tests + multi-node integration tests
    ./android/build.sh [debug|release] [abi...]   # needs cargo-ndk, NDK 27, Android SDK, gradle
    adb install -r android/app/build/outputs/apk/debug/app-debug.apk

Linux BLE needs BlueZ (`bluetoothd`) and libdbus.

## License

AGPL-3.0-or-later, inherited from `iroh-ble-transport` and `blew`.
