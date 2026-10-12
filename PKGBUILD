# Maintainer: SrDicov
pkgname=z-dinit-generator
pkgver=0.1.0
pkgrel=1
pkgdesc="Perfect dinit replacement frontend for zinit (convert + watch + dinitctl + compatd)"
arch=('x86_64' 'aarch64')
url="https://github.com/SrDicov/z-dinit-generator"
license=('GPL-3.0-or-later')
depends=('zinit')
makedepends=('rust')
source=("z-dinit-generator.rs")
sha256sums=('SKIP')

build() {
  rustc -O -D warnings --edition=2021 "$srcdir/z-dinit-generator.rs" -o "$srcdir/z-dinit-generator"
}

package() {
  install -Dm755 "$srcdir/z-dinit-generator" "$pkgdir/usr/bin/z-dinit-generator"
  ln -sf z-dinit-generator "$pkgdir/usr/bin/dinitctl"
}
