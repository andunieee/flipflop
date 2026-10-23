# flipflop

Rust + Slint peer-to-peer app (desktop and Android). Peers connect with [iroh](https://iroh.computer).

- **LAN**: devices announce over mDNS (service `_flipflop._udp`) with their name; they appear under *Nearby*.
- **QR**: *Add by QR* shows your ticket as a QR code and scans others' (webcam on desktop, system scanner on Android). Tickets can also be pasted.
- **BLE discovery**: every device advertises a GATT service whose characteristic holds `{name, ticket}` and scans for it, so peers learn each other's full endpoint id and name.
- **BLE transport**: [`iroh-ble-transport`](https://crates.io/crates/iroh-ble-transport) adds Bluetooth LE as an iroh network path next to IP and relay, so connections and chat can run over Bluetooth when there is no network. It only advertises a 12-byte key prefix, which is why discovery above is kept. It is experimental. On Android the transport is added when the endpoint is bound, so the Bluetooth permission is requested at start-up; if it is denied the transport stays off until the next launch.
- **Name**: generated as `<hostname or model> (<os>)`, editable in the UI, persisted.

## Run

    cargo run                      # desktop
    cargo test                     # unit + two-node integration tests
    cargo run --example headless   # CLI node for debugging (optionally pass a ticket or endpoint id)
    ./android/build.sh [debug|release] [abi...]   # needs cargo-ndk, NDK 27, Android SDK, gradle
    adb install -r android/app/build/outputs/apk/debug/app-debug.apk

Linux BLE needs BlueZ (`bluetoothd`) and libdbus.

## License

AGPL-3.0-or-later, inherited from `iroh-ble-transport` and `blew`.
