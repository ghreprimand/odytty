# Packaging OdyTTY

OdyTTY is pre-release. The Linux release shape is a versioned source release
(`v0.2.1`) plus desktop integration files that downstream packages can install
in normal XDG locations.

This file describes the packaging surface for the source tree it ships with.
For a tagged release, read the `PACKAGING.md` from that same tag.

## Install Surface

Packages should install:

```text
/usr/bin/odytty
/usr/share/applications/io.unfinished_works.odytty.desktop
/usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml
/usr/share/icons/hicolor/scalable/apps/io.unfinished_works.odytty.svg
/usr/share/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png
/usr/share/doc/odytty/
/usr/share/licenses/odytty/LICENSE
```

The desktop entry uses `Icon=io.unfinished_works.odytty`, so the hicolor icon
theme assets need to be installed with that basename.

The AppStream metadata is intentionally small for `v0.2.0`: it gives software
centers and inventory tools a stable component id, homepage, bug tracker,
license, summary, and release version.

OdyTTY currently launches child shells with `TERM=xterm-256color`. Packages do
not need to install a custom terminfo entry yet. If a future release switches to
an OdyTTY-specific `TERM`, that release must ship and document the matching
terminfo entry before packagers set it by default.

## Build

```sh
cargo build --release --locked
```

The installed binary should be launched as:

```sh
odytty
```

`odytty --native` remains accepted as a compatibility alias. Command launchers
can run a program directly in the initial PTY:

```sh
odytty -e btop
odytty --working-directory /tmp -e sh -lc 'pwd; exec "$SHELL"'
odytty --title Monitor -e btop
```

Introspection commands that print and exit without opening a window are:

```sh
odytty --list-themes
odytty --list-fonts
odytty --show-config
odytty --version
```

Build requirements:

```text
rust
cargo
pkg-config or pkgconf
fontconfig
freetype2
vulkan-loader
```

Distribution build systems that forbid network access during the build should
vendor Rust crates before the build step, for example with `cargo vendor`, and
configure Cargo to use the vendored source.

## Upstream Release Tracking

Use GitHub releases/tags as the upstream version source:

```text
type: github
owner: ghreprimand
repo: odytty
tag_prefix: v
```

See [`docs/release.md`](docs/release.md) for the release artifact checklist.

## Default-Terminal Integration

OdyTTY's desktop entry advertises the relevant terminal execution keys for
`xdg-terminal-exec`-style integrations:

```ini
X-TerminalArgExec=-e
X-TerminalArgDir=--working-directory=
X-TerminalArgTitle=--title=
```

Do not silently set OdyTTY as the user's default terminal in package install
scripts. Register it as an available terminal where the target distribution has
a standard mechanism, then let the user choose it.

## Odyssey/LFS

On Odyssey, package OdyTTY as a normal source-build PKGBUILD in `~/pkgbuilds`
and build it with `odyssey-build`. That makes pacman own `/usr/bin/odytty` and
the desktop entry, giving a versioned install such as `odytty 0.2.0-1`.

See [`docs/install.md`](docs/install.md) for a concrete Odyssey PKGBUILD
example and default-terminal notes.
