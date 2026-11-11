#!/usr/bin/env bash
# Print the PKGBUILD of flipflop-bin: the release .deb, repackaged for Arch. The release workflow builds a
# .pkg.tar.zst from it (with the .deb next to it) and pushes it to the AUR.
# Usage: packaging/aur/pkgbuild.sh VERSION SHA256_OF_THE_DEB > PKGBUILD
set -euo pipefail
version=$1 sha=$2
cat <<PKGBUILD
# Maintainer: andunie <andunie@cassis.cash>
pkgname=flipflop-bin
pkgver=${version}
pkgrel=1
pkgdesc="Share files, texts and a scratchpad with your devices, peer to peer"
arch=('x86_64')
url="https://github.com/andunieee/flipflop"
license=('AGPL-3.0-or-later')
depends=('gcc-libs' 'dbus' 'fontconfig' 'freetype2' 'libglvnd' 'libxkbcommon')
optdepends=('bluez: Bluetooth discovery and transport'
            'wayland: running on Wayland'
            'libxcursor: running on X11'
            'libxrandr: running on X11'
            'libxi: running on X11')
# already stripped by the release profile
options=('!strip' '!debug')
install=flipflop-bin.install
source=("flipflop-\${pkgver}.deb::https://github.com/andunieee/flipflop/releases/download/v\${pkgver}/flipflop-linux-amd64-v\${pkgver}.deb")
sha256sums=('${sha}')

package() {
  # /opt/flipflop, flipflop-gui in /usr/bin (\`flipflop\` is the command line client's name), the menu entry and icons
  bsdtar -xOf "flipflop-\${pkgver}.deb" 'data.tar.*' | bsdtar -xf - -C "\${pkgdir}"
}
PKGBUILD
