# Installing OdyTTY On Linux

OdyTTY is pre-release. The current recommended release shape is:

- `v0.2.0` git tag and source tarball;
- GitHub Release entry with checksums for release artifacts;
- source-build instructions for Odyssey/LFS and other developer systems;
- a desktop entry, AppStream metadata, and icon installed into Freedesktop
  locations.

An AppImage is a good later public artifact for people who want one download on
many Linux distributions, but Odyssey itself should use a pacman-tracked source
package so the install is versioned, owned, removable, and visible to
Odyssey-Mon.

## Build From Source

Build the release binary:

```sh
cargo build --release --locked
```

Run from the source tree:

```sh
cargo run --release
```

After installation, plain `odytty` opens the native terminal. The legacy parser
smoke path is still available as:

```sh
odytty --core-smoke
```

Check the installed version:

```sh
odytty --version
```

Run a command directly inside OdyTTY:

```sh
odytty -e btop
odytty --working-directory /tmp -e sh -lc 'pwd; exec "$SHELL"'
odytty --title Monitor -e btop
```

## User-Local Install

This is useful for quick testing on any Linux system:

```sh
cargo build --release --locked
install -Dm755 target/release/odytty "$HOME/.local/bin/odytty"
install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  "$HOME/.local/share/applications/io.unfinished_works.odytty.desktop"
install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  "$HOME/.local/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
install -d "$HOME/.local/share/icons/hicolor"
cp -a dist/icons/hicolor/* "$HOME/.local/share/icons/hicolor/"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
```

After this, `odytty` should work from a shell and OdyTTY should appear in app
launchers that read Freedesktop desktop entries.

## System Install

For a non-packaged system-wide install:

```sh
cargo build --release --locked
sudo install -Dm755 target/release/odytty /usr/bin/odytty
sudo install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  /usr/share/applications/io.unfinished_works.odytty.desktop
sudo install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  /usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml
sudo install -d /usr/share/icons/hicolor
sudo cp -a dist/icons/hicolor/* /usr/share/icons/hicolor/
sudo update-desktop-database /usr/share/applications 2>/dev/null || true
sudo gtk-update-icon-cache /usr/share/icons/hicolor 2>/dev/null || true
```

Prefer a real package for machines where installed-file ownership matters.

## Generic Versioned User Install

On any Linux system, a user can keep versioned OdyTTY builds without root by
installing each release under a versioned directory and pointing
`~/.local/bin/odytty` at the selected version:

```sh
version=0.2.0
cargo build --release --locked
install -Dm755 target/release/odytty \
  "$HOME/.local/opt/odytty/$version/bin/odytty"
ln -sfn "$HOME/.local/opt/odytty/$version/bin/odytty" \
  "$HOME/.local/bin/odytty"
install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  "$HOME/.local/share/applications/io.unfinished_works.odytty.desktop"
install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  "$HOME/.local/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
install -d "$HOME/.local/share/icons/hicolor"
cp -a dist/icons/hicolor/* "$HOME/.local/share/icons/hicolor/"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
odytty --version
```

To roll back, repoint the symlink to an older directory under
`~/.local/opt/odytty/`.

## Odyssey/LFS Versioned Install

Odyssey source builds are versioned by pacman, not by leaving build products in
the source tree. A release tag such as `v0.2.0` should be archived into
`/sources/odytty-0.2.0.tar.gz`, then built from `~/pkgbuilds/odytty/PKGBUILD`
with `odyssey-build`.

Example PKGBUILD:

```bash
# Maintainer: Joel <joel@odyssey>
pkgname=odytty
pkgver=0.2.0
pkgrel=1
pkgdesc="GPU-rendered Rust terminal emulator with an Odyssey visual identity"
arch=('x86_64')
url="https://github.com/ghreprimand/odytty"
license=('GPL-3.0-only')
depends=('fontconfig' 'freetype2' 'vulkan-loader')
makedepends=('cargo' 'rust')
source=("file:///sources/${pkgname}-${pkgver}.tar.gz")
sha256sums=('SKIP')

build() {
    cd "${pkgname}-${pkgver}"
    cargo build --release --locked
}

package() {
    cd "${pkgname}-${pkgver}"

    install -Dm755 target/release/odytty "$pkgdir/usr/bin/odytty"
    install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
        "$pkgdir/usr/share/applications/io.unfinished_works.odytty.desktop"
    install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
        "$pkgdir/usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
    install -d "$pkgdir/usr/share/icons"
    cp -a dist/icons/hicolor "$pkgdir/usr/share/icons/"
    install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
    install -Dm644 docs/install.md "$pkgdir/usr/share/doc/$pkgname/install.md"
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
```

Build and install:

```sh
cd ~/pkgbuilds/odytty
odyssey-build
```

Verify pacman ownership and version tracking:

```sh
pacman -Qi odytty
pacman -Qo /usr/bin/odytty \
  /usr/share/applications/io.unfinished_works.odytty.desktop
odytty --show-config
odytty --version
```

Use `pkgrel=1` for the first packaging revision of a source release. If the
source does not change but the PKGBUILD does, keep `pkgver` at the source
version and bump `pkgrel`.

## What The Desktop Entry Does

`dist/linux/io.unfinished_works.odytty.desktop` registers OdyTTY as a GUI
application in the `System` and `TerminalEmulator` categories:

```ini
Exec=odytty
Terminal=false
Categories=System;TerminalEmulator;
X-TerminalArgExec=-e
X-TerminalArgDir=--working-directory=
X-TerminalArgTitle=--title=
```

The entry uses the OdyTTY icon name. Packages should install the SVG and PNG
assets under the hicolor icon theme, for example:

```text
/usr/share/icons/hicolor/scalable/apps/io.unfinished_works.odytty.svg
/usr/share/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png
```

`dist/linux/io.unfinished_works.odytty.metainfo.xml` gives AppStream-aware
tools a stable application id, license, summary, homepage, bug tracker, and
release version. It belongs in:

```text
/usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml
```

## Terminfo

OdyTTY currently runs child shells with:

```text
TERM=xterm-256color
```

That is conservative and avoids requiring a custom terminfo database entry on
the local machine, remote SSH hosts, or `sudo` environments. Do not package an
OdyTTY-specific terminfo entry until the binary also starts using that `TERM`
value and the release documents remote-host setup.

## Default Terminal

Linux does not have one universal default-terminal mechanism. App launchers,
file managers, desktop environments, and scripts use different conventions.

### LFS Or Other Manual Systems

On a Linux From Scratch-style system, app launcher registration usually means
installing the binary and `.desktop` file as shown above. To make shell scripts
that respect `$TERMINAL` choose OdyTTY:

```sh
export TERMINAL=odytty
```

Put that in the environment setup used by the desktop session, not only an
interactive shell startup file, if graphical applications need to see it.

For a keyboard shortcut such as `Ctrl+Alt+T`, configure the shortcut in the
desktop environment or window manager to run:

```sh
odytty
```

### `xdg-terminal-exec`

Some systems use the proposed `xdg-terminal-exec` default-terminal mechanism.
That system selects terminal emulators from installed desktop entries and a
preference file such as:

```text
$HOME/.config/xdg-terminals.list
```

The desktop entry advertises OdyTTY's command execution arguments:

```ini
X-TerminalArgExec=-e
X-TerminalArgDir=--working-directory=
X-TerminalArgTitle=--title=
```

Then a user can prefer OdyTTY with:

```sh
printf 'io.unfinished_works.odytty.desktop\n' > "$HOME/.config/xdg-terminals.list"
```

### Debian-Style `x-terminal-emulator`

Debian-family systems often use `update-alternatives` for
`x-terminal-emulator`. A package can register OdyTTY as an alternative:

```sh
sudo update-alternatives --install \
  /usr/bin/x-terminal-emulator x-terminal-emulator /usr/bin/odytty 50
sudo update-alternatives --config x-terminal-emulator
```

On systems without `update-alternatives`, this mechanism does not exist unless
the distribution or local system owner installs it.

## Public Release Direction

For an upstream release that avoids maintaining many distro-specific packages:

- publish `v0.2.0` source tarballs with checksums/signatures;
- create a GitHub Release for the tag so package monitors can track upstream
  versions;
- include `dist/linux/io.unfinished_works.odytty.desktop`;
- include `dist/linux/io.unfinished_works.odytty.metainfo.xml`;
- include `dist/icons/hicolor/`;
- include install docs for source builds, versioned user-local installs, and
  packagers;
- add an AppImage once the binary dependency and icon/metainfo story is stable;
- let distro-specific `.deb`, `.rpm`, Arch, Nix, and similar packages be
  maintained downstream using the same install surface.
