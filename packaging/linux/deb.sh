#!/usr/bin/env bash
# Build dist/flipflop_<version>_amd64.deb from dist/stage/root (run packaging/linux/stage.sh first).
# Needs dpkg-deb. Usage: packaging/linux/deb.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
dir=$(mktemp -d)/flipflop
cp -a dist/stage/root "$dir"
mkdir -p "$dir/DEBIAN"
cat > "$dir/DEBIAN/control" <<CONTROL
Package: flipflop
Version: $version
Architecture: amd64
Maintainer: andunie <andunie@cassis.cash>
Installed-Size: $(du -sk "$dir" | cut -f1)
Depends: libc6 (>= 2.35), libstdc++6, libgcc-s1, libdbus-1-3, libfontconfig1, libfreetype6, libegl1, libgl1, libxkbcommon0
Recommends: libwayland-client0, libwayland-egl1, libx11-xcb1, libxcursor1, libxi6, libxrandr2, bluez
Section: net
Priority: optional
Homepage: https://github.com/andunieee/flipflop
Description: Share files, texts and a scratchpad with your devices, peer to peer
CONTROL
cat > "$dir/DEBIAN/postinst" <<'SCRIPT'
#!/bin/sh
set -e
command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
SCRIPT
chmod 755 "$dir/DEBIAN/postinst"
cp "$dir/DEBIAN/postinst" "$dir/DEBIAN/postrm"
mkdir -p dist
dpkg-deb --root-owner-group --build "$dir" "dist/flipflop_${version}_amd64.deb"
