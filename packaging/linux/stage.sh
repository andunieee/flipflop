#!/usr/bin/env bash
# Build the release desktop app and lay it out for the Linux packages, in dist/stage:
#   portable/flipflop  the portable folder: bin/flipflop-gui, and bin/flipflop, a link to it (where older
#                      `flipflop gui`s and shortcuts look)
#   root               what the .deb and .rpm install: that folder as /opt/flipflop, flipflop-gui in /usr/bin,
#                      the menu entry, icons and AppStream metadata in /usr/share
# Usage: packaging/linux/stage.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build --release --locked -p flipflop-app --bin flipflop-gui
target=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')

stage=dist/stage
rm -rf "$stage"
portable=$stage/portable/flipflop
install -Dm755 "$target/release/flipflop-gui" "$portable/bin/flipflop-gui"
ln -s flipflop-gui "$portable/bin/flipflop"

root=$stage/root
mkdir -p "$root/opt" "$root/usr/bin"
cp -a "$portable" "$root/opt/flipflop"
ln -s /opt/flipflop/bin/flipflop-gui "$root/usr/bin/flipflop-gui"
install -d "$root/usr/share/applications"
sed 's|^Exec=flipflop$|Exec=/opt/flipflop/bin/flipflop-gui|' packaging/linux/dev.flipflop.app.desktop \
    > "$root/usr/share/applications/dev.flipflop.app.desktop"
install -Dm644 packaging/linux/dev.flipflop.app.metainfo.xml -t "$root/usr/share/metainfo"
for s in 16 24 32 48 64 128 256 512; do
    install -Dm644 "packaging/icons/$s.png" "$root/usr/share/icons/hicolor/${s}x${s}/apps/dev.flipflop.app.png"
done
