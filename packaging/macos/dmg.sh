#!/usr/bin/env bash
# Build the release app as dist/flipflop.app, and dist/flipflop-<version>.dmg with it (and a link to /Applications
# to drag it to). On macOS, with the Xcode command line tools. Usage: packaging/macos/dmg.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
# macOS versions need a major of at least 1
IFS=. read -r major minor patch <<<"$version"
bundle_version="$(( major < 1 ? 1 : major )).$minor.$patch"
cargo build --release --locked -p flipflop-app --bin flipflop-gui
target=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')

app=dist/flipflop.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/release/flipflop-gui" "$app/Contents/MacOS/flipflop"
# `flipflop gui` and PATH: `ln -s /Applications/flipflop.app/Contents/MacOS/flipflop-gui ~/.local/bin/`
ln -s flipflop "$app/Contents/MacOS/flipflop-gui"
cp packaging/macos/flipflop.icns "$app/Contents/Resources/"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>FlipFlop</string>
    <key>CFBundleDisplayName</key><string>FlipFlop</string>
    <key>CFBundleIdentifier</key><string>dev.flipflop.app</string>
    <key>CFBundleExecutable</key><string>flipflop</string>
    <key>CFBundleIconFile</key><string>flipflop.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$bundle_version</string>
    <key>CFBundleShortVersionString</key><string>$bundle_version</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSCameraUsageDescription</key><string>To scan another device's FlipFlop code.</string>
    <key>NSBluetoothAlwaysUsageDescription</key><string>To find and reach your devices nearby over Bluetooth.</string>
    <key>NSLocalNetworkUsageDescription</key><string>To find and reach your devices on this network.</string>
    <key>NSBonjourServices</key><array><string>_flipflop._udp</string></array>
</dict>
</plist>
PLIST

dir=$(mktemp -d)
cp -a "$app" "$dir/"
ln -s /Applications "$dir/Applications"
rm -f "dist/flipflop-$version.dmg"
hdiutil create -volname flipflop -srcfolder "$dir" -ov -format UDZO "dist/flipflop-$version.dmg"
