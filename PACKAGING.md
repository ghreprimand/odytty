# Packaging OdyTTY

The release shape is a versioned source release, a best-effort Linux x86_64
AppImage, and an unsigned Windows x86_64 portable zip. Linux desktop
integration files are included so downstream packages can install them in
normal XDG locations.

This file describes the packaging surface for the source tree it ships with.
For a tagged release, read the `PACKAGING.md` from that same tag.

## Install Surface

Packages should install:

```text
/usr/bin/odytty
/usr/share/applications/io.unfinished_works.odytty.desktop
/usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml
/usr/share/icons/hicolor/scalable/apps/io.unfinished_works.odytty.svg
/usr/share/icons/hicolor/48x48/apps/io.unfinished_works.odytty.png
/usr/share/icons/hicolor/64x64/apps/io.unfinished_works.odytty.png
/usr/share/icons/hicolor/128x128/apps/io.unfinished_works.odytty.png
/usr/share/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png
/usr/share/doc/odytty/
/usr/share/licenses/odytty/LICENSE
```

`dist/icons/hicolor/` ships the scalable SVG plus four raster sizes
(48x48, 64x64, 128x128, 256x256). Installing the full set is recommended;
at minimum the scalable SVG plus the 256x256 PNG cover the common cases.

The desktop entry uses `Icon=io.unfinished_works.odytty`, so the hicolor icon
theme assets need to be installed with that basename.

The AppStream metadata is intentionally small: it gives software
centers and inventory tools a stable component id, homepage, bug tracker,
license, summary, and release version.

On macOS the app bundle uses the Apple-valid reverse-DNS identifier
`io.unfinished-works.odytty` (hyphen), which intentionally differs from the
Linux component/icon basename `io.unfinished_works.odytty` (underscore). Both
forms are correct for their platform; do not "normalize" one to the other.

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

The binary also exposes detached-session subcommands that packagers and
launcher integrations should be aware of:

```sh
odytty new          # start a detached session, prints id=<id>
odytty list         # list live detached sessions
odytty attach [ID]  # reattach a session in a native window
```

Detached sessions require `XDG_RUNTIME_DIR` to be set on non-macOS platforms;
their control sockets live under `$XDG_RUNTIME_DIR/odytty` (mode `0700`). The
session host hard-errors if `XDG_RUNTIME_DIR` is unset, so packaging and
launcher environments that use these subcommands must provide it. macOS falls
back to the per-user Darwin temp dir and does not require it.

Detached-session hosting is Unix-only in the current Windows port. Windows
installers should treat `odytty.exe` as a normal local terminal binary and leave
detached/resumable session integration out until a Windows host transport lands.

Build requirements:

```text
rust
cargo
pkg-config or pkgconf
vulkan-loader
```

The font stack is pure-Rust (`ab_glyph`, `swash`, `ttf-parser` for
metadata-only reads), so there is **no** build/link dependency on `freetype2`.
`fontconfig` is a **runtime-only** dependency: OdyTTY shells out to `fc-match`
to backfill symbol glyphs from the host font set. It is not needed to build or
link the binary, only at run time on systems that rely on that backfill.

Distribution build systems that forbid network access during the build should
vendor Rust crates before the build step, for example with `cargo vendor`, and
configure Cargo to use the vendored source.

## Testing during packaging

If your `check()` runs the test suite, use the full `cargo test` (or run
`cargo build` first). The attach/detach end-to-end tests locate the compiled
`odytty` binary in `target/`, and `cargo test --lib` alone does not build the
binary target — those tests then fail with an "odytty binary not found" error.
`cargo test` (what upstream CI runs) builds the binary and passes.

## Upstream Release Tracking

Use GitHub releases/tags as the upstream version source:

```text
type: github
owner: ghreprimand
repo: odytty
tag_prefix: v
```

See [`docs/release.md`](docs/release.md) for the release artifact checklist.

## Windows Artifacts

The release workflow publishes both Windows zip names:

```text
odytty-windows-x86_64.zip
odytty-<version>-windows-x86_64.zip
```

They are byte-identical copies and both are listed in `SHA256SUMS`. The zip
contains:

```text
odytty.exe
README.md
LICENSE
NOTICE
```

It must not contain `.pdb` files, build directories, or installer work
directories.

### Windows executable icon

`odytty.exe` embeds its application icon as a PE resource at build time, so
Explorer, the taskbar, and Alt-Tab show OdyTTY's icon on the executable itself —
no separate icon file ships in the zip. The embed is performed by `build.rs`
via the `winresource` build-dependency (a `cfg(windows)` build-dep: present in
`Cargo.lock` but never compiled on Linux/macOS), gated on
`CARGO_CFG_TARGET_OS == "windows"` so it fires for Windows *targets* regardless
of build host. The source art is `dist/windows/odytty.ico` (committed, so it is
present in the `git archive` tarball too); the `windows-latest` MSVC runner's
bundled `rc.exe`/`llvm-rc` performs the embed. A failed embed is non-fatal — the
build warns and produces a fully functional, icon-less exe. No `release.yml`
change is needed: the icon rides inside `odytty.exe`.

The *runtime* window/title-bar icon (and Alt-Tab/taskbar entry on X11) is set
separately via winit from the 256×256 hicolor PNG embedded in the binary; see
CONTRIBUTING.

The in-repo Scoop bucket manifest is `bucket/odytty.json`. A user can install
the repo as a Scoop bucket after the first Windows release:

```powershell
scoop bucket add odytty https://github.com/ghreprimand/odytty
scoop install odytty
```

Scoop puts `odytty` on the user's PATH (a shim) and creates an **OdyTTY**
Start-menu entry (via the manifest's `shortcuts` field), and verifies the
download against the pinned release checksum.

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
the desktop entry, giving a versioned install such as `odytty 0.6.2-1`.

See [`docs/install.md`](docs/install.md) for a concrete Odyssey PKGBUILD
example and default-terminal notes.
