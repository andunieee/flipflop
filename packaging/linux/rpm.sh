#!/usr/bin/env bash
# Build dist/flipflop-<version>-1.x86_64.rpm from dist/stage/root (run packaging/linux/stage.sh first).
# Needs rpmbuild. Usage: packaging/linux/rpm.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
top=$(mktemp -d)
root=$(realpath dist/stage/root)
cat > "$top/flipflop.spec" <<SPEC
Name: flipflop
Version: $version
Release: 1
Summary: Share files, texts and a scratchpad with your devices, peer to peer
License: AGPL-3.0-or-later
URL: https://github.com/andunieee/flipflop
Requires: libxkbcommon
Recommends: bluez
%define _build_id_links none
%define __strip /bin/true
%description
Share files, texts and a scratchpad with your devices, peer to peer.
%install
cp -a $root/. %{buildroot}/
%files
/opt/flipflop
/usr/bin/flipflop-gui
/usr/share/applications/dev.flipflop.app.desktop
/usr/share/metainfo/dev.flipflop.app.metainfo.xml
/usr/share/icons/hicolor/*/apps/dev.flipflop.app.png
SPEC
rpmbuild --define "_topdir $top" -bb "$top/flipflop.spec"
mkdir -p dist
cp "$top"/RPMS/x86_64/*.rpm dist/
