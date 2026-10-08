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
depends=('dbus' 'wayland' 'libxtst' 'libxrender' 'libxi' 'fontconfig' 'freetype2')
optdepends=('bluez: Bluetooth discovery and transport')
# the bundled Java runtime is shipped as it is
options=('!strip' '!debug')
install=flipflop-bin.install
source=("flipflop-\${pkgver}.deb::https://github.com/andunieee/flipflop/releases/download/v\${pkgver}/flipflop-linux-amd64-v\${pkgver}.deb")
sha256sums=('${sha}')

package() {
  bsdtar -xOf "flipflop-\${pkgver}.deb" 'data.tar.*' | bsdtar -xf - -C "\${pkgdir}"

  # jpackage's .deb installs its menu entry from a postinst script; ship it as a file instead
  local entry
  for entry in "\${pkgdir}"/opt/flipflop/lib/*.desktop; do
    install -Dm644 "\${entry}" "\${pkgdir}/usr/share/applications/flipflop.desktop"
  done
  grep -q '^StartupWMClass=' "\${pkgdir}/usr/share/applications/flipflop.desktop" \\
    || echo 'StartupWMClass=dev-flipflop-desktop-MainKt' >> "\${pkgdir}/usr/share/applications/flipflop.desktop"

  # \`flipflop\` is the command line client's name, so the app goes in PATH as flipflop-gui (as \`flipflop gui\` expects)
  install -d "\${pkgdir}/usr/bin"
  ln -s /opt/flipflop/bin/flipflop "\${pkgdir}/usr/bin/flipflop-gui"
}
PKGBUILD
