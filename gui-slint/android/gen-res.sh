#!/bin/sh
# Regenerates the launcher icon PNGs in android/res from the SVG sources in
# android/icon (needs rsvg-convert). The generated PNGs are committed, so
# this only needs to run after editing the artwork.
set -eu
cd "$(dirname "$0")"

render() { # svg, size, out
    mkdir -p "$(dirname "$3")"
    rsvg-convert -w "$2" -h "$2" "$1" > "$3"
}

for d in mdpi:1 hdpi:1.5 xhdpi:2 xxhdpi:3 xxxhdpi:4; do
    name=${d%%:*}
    scale=${d#*:}
    adaptive=$(awk "BEGIN { print int(108 * $scale) }")
    legacy=$(awk "BEGIN { print int(48 * $scale) }")
    splash=$(awk "BEGIN { print int(112 * $scale) }")
    render icon/background.svg "$adaptive" "res/mipmap-$name/ic_launcher_background.png"
    render icon/foreground.svg "$adaptive" "res/mipmap-$name/ic_launcher_foreground.png"
    render icon/mark.svg "$legacy" "res/mipmap-$name/ic_launcher.png"
    render icon/mark.svg "$splash" "res/drawable-$name/launch_mark.png"
done
