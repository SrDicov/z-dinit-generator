# Maintainer: SrDicov
pkgname=z-dinit-generator
pkgver=0.1.0
pkgrel=1
pkgdesc="Perfect dinit replacement frontend for zinit (convert + watch + dinitctl + compatd)"
arch=('x86_64' 'aarch64')
url="https://github.com/SrDicov/z-dinit-generator"
license=('GPL-3.0-or-later')
# Co-installable with dinit by design: the P1 transition runs both
# supervisors side by side, so this package must NOT provide/conflict
# `dinit` and must NOT ship /usr/bin/dinitctl (dinit owns it until
# `pacman -R dinit`; the shim arrives as an untracked symlink, see
# z-dinit-generator.install). A provides/conflicts pair would forbid
# the coexistence the migration documents.
depends=('zinit')
makedepends=('rust')
install=z-dinit-generator.install
source=("z-dinit-generator.rs")
sha256sums=('SKIP')

build() {
  rustc -O -D warnings --edition=2021 "$srcdir/z-dinit-generator.rs" -o "$srcdir/z-dinit-generator"
}

package() {
  install -Dm755 "$srcdir/z-dinit-generator" "$pkgdir/usr/bin/z-dinit-generator"
}
