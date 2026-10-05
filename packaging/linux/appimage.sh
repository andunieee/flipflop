#!/usr/bin/env bash
# Build flipflop-<version>-x86_64.AppImage from the Compose distributable (which already carries its own JRE
# and the release libflipflop.so). Needs a JDK 17+ with jpackage, cargo, and appimagetool in PATH (or it is
# downloaded). Usage: packaging/linux/appimage.sh   -> dist/
set -euo pipefail
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
(cd kmp && ./gradlew :desktopApp:createDistributable)

app=kmp/desktopApp/build/compose/binaries/main/app/flipflop
dir=$(mktemp -d)/flipflop.AppDir
mkdir -p "$dir/usr/lib" "$dir/usr/share/applications" "$dir/usr/share/metainfo"
cp -r "$app" "$dir/usr/lib/flipflop"
cp packaging/linux/dev.flipflop.app.desktop "$dir/usr/share/applications/"
cp packaging/linux/dev.flipflop.app.desktop "$dir/"
cp packaging/linux/dev.flipflop.app.metainfo.xml "$dir/usr/share/metainfo/"
cp packaging/icons/512.png "$dir/dev.flipflop.app.png"
for s in 16 24 32 48 64 128 256 512; do
    mkdir -p "$dir/usr/share/icons/hicolor/${s}x${s}/apps"
    cp "packaging/icons/$s.png" "$dir/usr/share/icons/hicolor/${s}x${s}/apps/dev.flipflop.app.png"
done
cat > "$dir/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/lib/flipflop/bin/flipflop" "$@"
RUN
chmod +x "$dir/AppRun"

if ! command -v appimagetool >/dev/null; then
    curl -fsSL -o "${TMPDIR:-/tmp}/appimagetool" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "${TMPDIR:-/tmp}/appimagetool"
    export PATH="${TMPDIR:-/tmp}:$PATH"
fi
mkdir -p dist
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 appimagetool "$dir" "dist/flipflop-$version-x86_64.AppImage"
