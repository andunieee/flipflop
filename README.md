# flipflop

Rust + Slint peer-to-peer app (desktop and Android). Peers connect with [iroh](https://iroh.computer).

- **LAN**: devices announce over mDNS (service `_flipflop._udp`) with their name; they appear under *Nearby*.
- **QR**: *Add by QR* shows your ticket as a QR code and scans others' (webcam on desktop, system scanner on Android). Tickets can also be pasted.
- **BLE discovery**: every device advertises a GATT service whose characteristic holds `{name, ticket}` and scans for it, so peers learn each other's full endpoint id and name.
- **BLE transport**: [`iroh-ble-transport`](https://crates.io/crates/iroh-ble-transport) adds Bluetooth LE as an iroh network path next to IP and relay, so connections and chat can run over Bluetooth when there is no network. It only advertises a 12-byte key prefix, which is why discovery above is kept. It is experimental. On Android the transport is added when the endpoint is bound, so the Bluetooth permission is requested at start-up; if it is denied the transport stays off until the next launch.
- **Name**: generated as `<hostname or model> (<os>)`, editable in the UI, persisted.

## Contacts and groups

- **Contacts**: every peer you connect to (or that connects to you) is saved with its public key and name. Saved contacts reconnect automatically, with backoff, and immediately when they show up on the LAN or over BLE. You can rename them locally (the peer's own name stays visible, dimmed) or remove them. A removed contact that keeps dialing you is still accepted but is not saved again; connecting to them yourself saves them again.
- **Groups**: create a group, then add contacts. Each gets an invite and must accept. If they are offline, the invite is sent when they next connect. Declining is remembered per `(inviter, group)`: repeat invites for that pair are not shown (the inviter is quietly told no again), invites from other peers or to other groups still are.
- **Group names** are haikus generated from the group's random id; you can set a local alias, and the haiku stays shown dimmed beneath it.
- State lives in `state.json` next to the secret key.

## Run

    cargo run                      # desktop
    cargo test                     # unit + two-node integration tests
    cargo run --example headless   # CLI node for debugging (optionally pass a ticket or endpoint id)
    ./android/build.sh [debug|release] [abi...]   # needs cargo-ndk, NDK 27, Android SDK, gradle
    adb install -r android/app/build/outputs/apk/debug/app-debug.apk

Linux BLE needs BlueZ (`bluetoothd`) and libdbus.

## License

AGPL-3.0-or-later, inherited from `iroh-ble-transport` and `blew`.
