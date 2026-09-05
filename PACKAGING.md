# Packaging OdyTTY

Use this reference to build distribution packages, install platform metadata,
and preserve the public release contract.

## Contents

- [Release Contract](#release-contract)
- [Install The Platform Surface](#install-the-platform-surface)
- [Build The Package](#build-the-package)
- [Run Packaging Checks](#run-packaging-checks)
- [Track Upstream Releases](#track-upstream-releases)
- [Publish Arch And AUR Packages](#publish-arch-and-aur-packages)
- [Package Windows](#package-windows)
- [Register Default-Terminal Integration](#register-default-terminal-integration)
- [Package Odyssey And LFS](#package-odyssey-and-lfs)

## Release Contract

This file describes the packaging surface for the source tree it ships with.
For a tagged release, read the `PACKAGING.md` from that same tag.

Each release publishes seven artifact types:

| Platform | Published artifact |
| --- | --- |
| Linux | Debian package, RPM package, binary tarball, and AppImage |
| macOS | Apple Silicon app zip |
| Windows | x86_64 portable zip |
| Source builds | Versioned source archive |

Every artifact has an always-latest alias and a byte-identical version-pinned
copy. `SHA256SUMS` is the fifteenth release asset. Use the
[Install Guide artifact table](docs/install.md#release-artifact-names-and-checksums)
for exact filenames and the [Release Guide](docs/release.md) for publication
checks.

## Install The Platform Surface

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

The AppStream metadata gives software centers and inventory tools a stable
component id, homepage, bug tracker, license, summary, full description,
categories and keywords, a remotely hosted screenshot, content rating, and a
hand-maintained release history.

On macOS the app bundle uses the Apple-valid reverse-DNS identifier
`io.unfinished-works.odytty` (hyphen), which intentionally differs from the
Linux component/icon basename `io.unfinished_works.odytty` (underscore). Both
forms are correct for their platform; do not "normalize" one to the other.

OdyTTY launches child shells with `TERM=xterm-256color`,
`COLORTERM=truecolor`, `TERM_PROGRAM=odytty`, and
`TERM_PROGRAM_VERSION=<package version>`. Packages must not replace this
environment contract or install a custom terminfo entry. If a future release
switches to an OdyTTY-specific `TERM`, that release must ship and document the
matching terminfo entry before packagers set it by default.

## Build The Package

```sh
cargo build --release --locked
```

The About panel embeds the source commit. Its precedence is a validated
`ODYTTY_BUILD_SHA`, a live checkout's short Git commit (plus `-dirty` for a
modified tree), the `.git_archival.txt` token substituted by `git archive`, and
finally `unavailable`. The former `unknown` fallback is not used.

Packages built from OdyTTY's published source archive inherit its abbreviated
commit automatically: the release archive is made with `git archive`, and the
repository's narrow `.gitattributes` rule substitutes only
`.git_archival.txt`. Preserve that file when repacking. A package builder using
a different archive or an exported tree without `.git` should set
`ODYTTY_BUILD_SHA` to the exact 7-to-40-digit hexadecimal source commit for the
build; malformed values are ignored rather than embedded.

OdyTTY's verified minimum Rust version is 1.96. `rust-toolchain.toml` pins
`1.96.0` and `Cargo.toml` declares `rust-version = "1.96"`; packagers may use a
newer stable compiler, but upstream CI builds at this floor.

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
odytty --app-id com.example.Monitor -e btop
odytty --class=com.example.Monitor -e btop
odytty --hold -e sh -lc 'exit 7'
```

On Linux, `--app-id` and `--class` are aliases and accept both space and equals
forms. They override only that window's Wayland `app_id` and X11 `WM_CLASS`
class; the X11 instance stays `odytty`, and the installed desktop id, icon, and
`StartupWMClass` stay unchanged. Without an override, the window uses
`io.unfinished_works.odytty`.

`--hold`, `--hold=true`, and `--hold=false` apply only to the initial local
command and default to false. A held exit reports a numeric or explicit unknown
status in the pane, then closes through the normal shell-exit path on the next
keypress. Later sessions do not inherit hold, and remote reconnect handling
retains precedence.

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
odytty new [--app-id APP_ID | --class APP_ID]  # start a detached session
odytty list                                      # list live detached sessions
odytty attach [ID]                               # reattach in a native window
```

The two identity aliases also accept equals forms on `odytty new`. Detached
creation has no window, so the parsed value is not stored in host metadata and
does not affect a later `odytty attach`, which uses the packaged default
identity.

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
rust 1.96 or newer, including cargo
```

The release runner also prepares Linux development packages used by its wider
packaging and smoke environment. They are not link-time requirements for the
Rust build itself; the reference PKGBUILD therefore needs only `cargo` in
`makedepends`.

The font stack is pure-Rust (`ab_glyph`, `swash`, `ttf-parser` for
metadata-only reads), so there is **no** build/link dependency on `freetype2`.
`fontconfig` is a **runtime-only** dependency: OdyTTY shells out to `fc-match`
to backfill symbol glyphs from the host font set. It is not needed to build or
link the binary, only at run time on systems that rely on that backfill.
The packaged runtime dependency lists (`.deb`, `.rpm`, AUR) still name
`freetype2` deliberately: the shipped binary links only libc, libm, and
libgcc_s (verified with `readelf -d` on a release build), but the `fc-match`
tooling it executes is built on FreeType, so the entry records that indirect
requirement and keeps the three lists identical. It is not a link-time
dependency and its removal would change nothing about the binary.

Distribution build systems that forbid network access should vendor Rust crates
before the build step:

```sh
cargo vendor
```

Configure Cargo to use that vendored source before starting the offline build.

## Run Packaging Checks

If the package's `check()` runs the suite, use the full test target:

```sh
cargo test --locked
```

The attach and detach end-to-end tests locate the compiled `odytty` binary in
`target/`. `cargo test --lib` alone does not build that binary and fails with
an "odytty binary not found" error. A prior full build also satisfies the
binary requirement.

## Track Upstream Releases

Use GitHub releases and tags as the upstream version source:

```text
type: github
owner: ghreprimand
repo: odytty
tag_prefix: v
```

See the [Release Guide](docs/release.md) for the artifact checklist.

## Publish Arch And AUR Packages

The live `odytty` AUR package builds from the version-pinned source archive.
The templates in `dist/aur/` are stamped, checksummed, regenerated, and normally
pushed automatically after every tagged GitHub Release. A transient AUR outage
can leave the GitHub Release healthy while only this downstream channel needs a
retry.

When the AUR publishing credential is unavailable, the workflow still validates
the generated `PKGBUILD` and `.SRCINFO`, then exits without pushing. See the
[AUR publishing guide](dist/aur/README.md) for the automatic path, the dedicated
idempotent retry workflow, and the manual fallback.

End-user installation steps live in the
[Install Guide](docs/install.md#arch-linux-aur). The exact upstream release
filenames are listed in its
[artifact table](docs/install.md#release-artifact-names-and-checksums).

## Package Windows

### Keep The Portable Zip Minimal

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

### Embed The Executable Icon

`odytty.exe` embeds its application icon as a PE resource at build time.
Explorer, the taskbar, and Alt-Tab therefore show OdyTTY's icon from the
executable itself; no separate icon file ships in the zip.

`build.rs` performs the embed through the `winresource` build dependency. That
dependency is present in `Cargo.lock` but compiled only when the build host is
Windows. The embed requires both a Windows host and a Windows target: the host
controls `cfg(windows)`, while `CARGO_CFG_TARGET_OS == "windows"` checks the
target. Cross-compiling from Linux or macOS therefore produces a functional
executable without the embedded icon. Shipped zips are built on
`windows-latest`, so release executables include it.

The committed source art is `dist/windows/odytty.ico`, so it is also present in
the source archive. The `windows-latest` MSVC runner uses its bundled `rc.exe`
or `llvm-rc` to embed it.

An embed failure is non-fatal: the build warns and produces a functional
executable without an icon. The icon rides inside `odytty.exe`, so
`release.yml` needs no separate icon step.

The *runtime* window/title-bar icon (and Alt-Tab/taskbar entry on X11) is set
separately via winit from the 256×256 hicolor PNG embedded in the binary; see
the [contribution guide](CONTRIBUTING.md).

### Publish The Scoop Manifest

The in-repo Scoop bucket manifest is `bucket/odytty.json`. A user can install
the repository as a Scoop bucket:

```powershell
scoop bucket add odytty https://github.com/ghreprimand/odytty
scoop install odytty
```

Scoop puts `odytty` on the user's PATH (a shim) and creates an **OdyTTY**
Start-menu entry (via the manifest's `shortcuts` field), and verifies the
download against the pinned release checksum.

## Register Default-Terminal Integration

OdyTTY's desktop entry advertises the relevant terminal execution keys for
`xdg-terminal-exec`-style integrations:

```ini
X-TerminalArgExec=-e
X-TerminalArgDir=--working-directory=
X-TerminalArgTitle=--title=
X-TerminalArgAppId=--app-id=
X-TerminalArgHold=--hold
```

The five keys describe OdyTTY's complete launcher translation surface: command,
working directory, title, per-window application id, and keep-open behavior.
The application-id translation changes only the launched Linux window identity;
the hold translation retains only its initial local command.

Do not silently set OdyTTY as the user's default terminal in package install
scripts. Register it as an available terminal where the target distribution has
a standard mechanism, then let the user choose it.

## Package Odyssey And LFS

On Odyssey, package OdyTTY as a normal source-build PKGBUILD in `~/pkgbuilds`,
then build it with:

```sh
odyssey-build
```

Pacman then owns `/usr/bin/odytty` and the desktop entry, producing a versioned
install such as `odytty <version>-1`.

See [`docs/install.md`](docs/install.md) for a concrete Odyssey PKGBUILD
example and default-terminal notes.
