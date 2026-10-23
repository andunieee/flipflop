# flipflop

Rust + Slint peer-to-peer app (desktop and Android). Peers connect with [iroh](https://iroh.computer).

- **LAN**: devices announce over mDNS (service `_flipflop._udp`) with their name; they appear under *Nearby*.
- **QR**: *Add by QR* shows your ticket as a QR code and scans others' (webcam on desktop, system scanner on Android). Tickets can also be pasted.
- **BLE**: every device advertises a GATT service whose characteristic holds `{name, ticket}` and scans for it. BLE is discovery only; the connection itself is iroh over IP.
- **Name**: generated as `<hostname or model> (<os>)`, editable in the UI, persisted.

## Run

    cargo run                      # desktop
    cargo test                     # unit + two-node integration tests
    cargo run --example headless   # CLI node for debugging (optionally pass a ticket or endpoint id)
    ./android/build.sh [debug|release] [abi...]   # needs cargo-ndk, NDK 27, Android SDK, gradle
    adb install -r android/app/build/outputs/apk/debug/app-debug.apk

Linux BLE needs BlueZ (`bluetoothd`) and libdbus.
