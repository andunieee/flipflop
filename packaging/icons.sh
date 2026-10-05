#!/usr/bin/env bash
# Regenerate every icon from assets/top.png (the app icon). The outputs are committed, so this only
# needs to run when the artwork changes. Needs ImageMagick 7.  Usage: packaging/icons.sh
set -euo pipefail
cd "$(dirname "$0")/.."
src=assets/top.png
bg='#FCF9F6'
out=packaging/icons
mkdir -p "$out" packaging/windows packaging/macos

# rounded square of the artwork, $1 px (about the radius of a macOS / Android launcher icon)
rounded() {
    local s=$1 r=$(( $1 * 22 / 100 ))
    magick "$src" -resize "${s}x${s}" \( +clone -alpha extract -fill black -colorize 100 -fill white \
        -draw "roundrectangle 0,0,$((s-1)),$((s-1)),$r,$r" \) -alpha off -compose CopyOpacity -composite "$2"
}

# Linux hicolor theme, and the 512 px one the Compose window and .deb/.rpm use
for s in 16 24 32 48 64 128 256 512; do rounded $s "$out/$s.png"; done

# Windows .ico (installer, exe, shortcuts)
magick "$out/16.png" "$out/24.png" "$out/32.png" "$out/48.png" "$out/64.png" "$out/128.png" "$out/256.png" packaging/windows/flipflop.ico

# macOS .icns (ImageMagick writes it; iconutil is not needed)
magick "$out/16.png" "$out/32.png" "$out/64.png" "$out/128.png" "$out/256.png" "$out/512.png" packaging/macos/flipflop.icns

# the logo inside the app
rounded 256 kmp/shared/src/commonMain/composeResources/drawable/logo.png

# Android: legacy launcher icons, plus an adaptive icon (cream background colour + the shoes on transparent)
res=kmp/androidApp/src/main/res
mkdir -p "$res/mipmap-anydpi-v26" "$res/values"
for d in mdpi:48:108 hdpi:72:162 xhdpi:96:216 xxhdpi:144:324 xxxhdpi:192:432; do
    IFS=: read -r n legacy fg <<<"$d"
    mkdir -p "$res/mipmap-$n"
    rounded "$legacy" "$res/mipmap-$n/ic_launcher.png"
    magick "$src" -resize "${fg}x${fg}" -fuzz 7% -transparent "$bg" "$res/mipmap-$n/ic_launcher_foreground.png"
done
cat > "$res/values/ic_launcher_background.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<resources>
    <color name="ic_launcher_background">$bg</color>
</resources>
XML
cat > "$res/mipmap-anydpi-v26/ic_launcher.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/ic_launcher_background" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
</adaptive-icon>
XML
