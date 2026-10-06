pkgname=bookos-lumina
pkgver=0.1.0
pkgrel=1
pkgdesc="Cliente de WhatsApp para BookOS"
arch=('x86_64')
license=('MIT')
depends=('webkit2gtk-4.1' 'gtk3' 'libsoup3' 'xdg-utils' 'glib2')
makedepends=('rust' 'cargo' 'pkgconf' 'base-devel')
source=()
options=('!strip' '!debug')

build() {
  cd "$startdir/src-tauri"
  cargo build --release --locked
}

package() {
  install -Dm755 "$startdir/src-tauri/target/release/bookos-lumina" \
    "$pkgdir/usr/bin/bookos-lumina"

  install -Dm644 /dev/stdin "$pkgdir/usr/share/applications/bookos-lumina.desktop" <<DESKTOP
[Desktop Entry]
Categories=Network;InstantMessaging;
Comment=WhatsApp client
Comment[es]=Cliente de WhatsApp
Exec=bookos-lumina
StartupWMClass=bookos-lumina
Icon=bookos-lumina
Name=Lumina
Terminal=false
Type=Application
DESKTOP

  install -Dm644 "$startdir/src-tauri/icons/icon.png" \
    "$pkgdir/usr/share/icons/hicolor/512x512/apps/bookos-lumina.png"
  install -Dm644 "$startdir/src-tauri/icons/icon.svg" \
    "$pkgdir/usr/share/icons/hicolor/scalable/apps/bookos-lumina.svg"
}
