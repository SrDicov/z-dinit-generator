# cports template for z-dinit-generator (Chimera Linux).
# Single std-only Rust file: no cargo, no dependencies.
pkgname = "z-dinit-generator"
pkgver = "0.1.0"
pkgrel = 0
build_style = "meta"
hostmakedepends = ["rust"]
depends = ["zinit"]
pkgdesc = "Perfect dinit replacement frontend for zinit"
license = "GPL-3.0-or-later"
url = "https://github.com/SrDicov/z-dinit-generator"
source = f"https://github.com/SrDicov/z-dinit-generator/archive/v{pkgver}.tar.gz"
# FIXME: fill in on release (bunshake -a <file>)
sha256 = "0" * 64


def do_build(self):
    self.do(
        "rustc",
        "-O",
        "-D",
        "warnings",
        "--edition=2021",
        "z-dinit-generator.rs",
        "-o",
        "z-dinit-generator",
    )


def do_install(self):
    self.install_bin("z-dinit-generator")
    self.install_link("usr/bin/dinitctl", "z-dinit-generator")
