# OdyTTY

**Website:** [odytty.unfinished-works.com](https://odytty.unfinished-works.com)

![OdyTTY rendering a colorized git graph, project tree, and truecolor gradients under the default Odyssey theme with bloom](assets/demo.png)

OdyTTY is a standalone, from-scratch, GPU-rendered Rust terminal emulator for
Linux, macOS, and Windows. It owns the terminal byte path from PTY allocation through escape
parsing, terminal state, render geometry, and shaders, while relying on focused
external crates for lower-level infrastructure such as `wgpu`, `winit`,
`ab_glyph`, `swash`, `arboard`, and Unicode width tables.

The name and visual direction come from OdysseyOS, a companion Linux From
Scratch system. That system is inspiration, not a platform
requirement: OdyTTY is a public Linux application and does not require
OdysseyOS or any custom distribution.

The project goal is not to skin an existing terminal. OdyTTY is testing whether
a terminal can carry a distinctive OdyTTY visual identity, richer in-app
configuration, inline media, motion, and accessibility features while remaining
practical for real command-line work. Terminal correctness, readable text,
input behavior, stable rendering, local privacy, and performance are the hard
floor.

OdyTTY is in active development. It is already a broad prototype: a native
window opens real local shells, supports multiple sessions with a tab bar,
splits each tab into panes, renders text and inline graphics on the GPU, and has
a substantial compatibility and smoke-test suite. It is still Linux-first and
pre-release; macOS is newly supported and in active development, and
Windows ships as an unsigned portable zip.

## Contents

- [Highlights](#highlights)
- [Install And Run](#install-and-run)
- [Features](#features)
- [Architecture](#architecture)
- [Testing](#testing)
- [Status](#status)
- [Public Repository Safety](#public-repository-safety)
- [Project Docs](#project-docs)
- [License](#license)

---

## Highlights

- **Owned terminal core:** clean-room DEC/xterm parser, OdyTTY terminal model,
  scrollback, alternate screen, and Linux PTY layer via `rustix`.
- **GPU renderer:** `wgpu`/Vulkan glyph atlas, bundled Victor Mono and JetBrains
  Mono with weight/synthetic faces, subpixel AA, HiDPI rebuilds, and a color
  emoji atlas.
- **Inline media:** Kitty graphics protocol and Sixel with direct, file, and
  shared-memory transports under conservative file-safety limits.
- **Modern input:** Kitty keyboard protocol, SGR-pixel mouse, focus reporting,
  IME composition, keyboard hints and copy mode, and OSC 8 hyperlinks.
- **Daily workflow:** search, refined and PRIMARY selection, bracketed-paste
  hardening, prompt navigation, click-to-place cursor, and a configurable bell.
- **Splits and panes:** side-by-side or stacked panes with a configurable
  tmux-style prefix (default `Ctrl+b`); single-pane input stays byte-identical.
- **Clickable paths:** opt-in Ctrl+click opens files at line/column, an image
  lightbox, an "Open With…" picker, and keyboard quick-select hints.
- **Detached and managed sessions:** sessions that outlive the window, driven
  from a local CLI and an in-window Manage Sessions overlay.
- **Visual layer:** 112 built-in themes, user `.theme` files, theme builder,
  bloom/CRT/retro effects, background treatments, and window transparency.
- **Local configuration:** `odytty.conf`, live reload, in-app settings panel
  with preservation-first writeback, font picker, and keybinding editor.
- **SSH substrate:** an OdyTTY-owned hosts list, opt-in OpenSSH host-name import
  (read-only, name-only), and connect-in-a-tab with optional remote integration,
  connection reuse, and `tmux` persistence, each degrading to a plain `ssh`.
- **Privacy posture:** no telemetry, analytics, crash reporting, account, cloud
  sync, or update ping; network actions are explicit and user-initiated.

## Install And Run

Linux is the primary, battle-tested target. macOS is newly supported and in
active development, with Homebrew as its recommended install path. Windows ships
as an unsigned portable zip, with Scoop as its recommended install path.

**Jump to:** [Linux](#linux) · [macOS](#macos) · [Windows](#windows)

---

### Linux

Requires Linux and a Vulkan-capable GPU. Wayland is the primary target; X11
works through the current `winit`/GPU stack with some window-manager-dependent
behavior for borderless windows and OS theme detection.

#### AppImage (recommended)

Download the AppImage and `SHA256SUMS` from the latest release, verify it, make
it executable, and run it:

```sh
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty-x86_64.AppImage
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
chmod +x odytty-x86_64.AppImage
./odytty-x86_64.AppImage
```

The `chmod +x` is required: browsers download the AppImage without the
executable bit, so without it the file opens in an archive viewer or fails with
"permission denied". The AppImage needs a working host Vulkan driver (Mesa or a
vendor driver); it deliberately does not bundle the GPU stack. This is a
best-effort x86_64 artifact; if it fails to start, build from source below.

Each release attaches both version-less and version-pinned downloads:
`odytty-x86_64.AppImage` / `odytty-windows-x86_64.zip` / `odytty.tar.gz` are
the always-latest names used above (resolved by the `releases/latest/download/`
URLs), while `odytty-<version>-x86_64.AppImage` /
`odytty-<version>-windows-x86_64.zip` / `odytty-<version>.tar.gz` are the
**identical** copies for pinning a specific version. Each alias and its
version-pinned twin carry matching checksums in `SHA256SUMS`.

<details>
<summary><strong>Arch Linux (AUR)</strong></summary>

A community-maintained AUR package,
[`odytty`](https://aur.archlinux.org/packages/odytty), is available for
Arch-family systems. It builds from the tagged GitHub source and compiles
locally:

```sh
paru -S odytty      # or: yay -S odytty
```

…or manually:

```sh
git clone https://aur.archlinux.org/odytty.git
cd odytty
makepkg -si
```

This package is maintained by a community contributor and is **not published by
the project**. It is not an official release channel. As with every AUR
package, the PKGBUILD is a build script that runs on your machine and AUR
packages are not vetted by Arch, so review the PKGBUILD before installing (your
AUR helper shows it by default). It tracks the published GitHub releases and
usually updates shortly after one, though timing depends on its maintainer. The
channels the project publishes and controls directly are the always-latest
release artifacts above (AppImage, tarball) and the from-source build below.

</details>

<details>
<summary><strong>Build from source</strong></summary>

For the current source release, install OdyTTY for the current user.

Download and verify the release archive:

```sh
workdir=$(mktemp -d /tmp/odytty-install.XXXXXX)
cd "$workdir"
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty.tar.gz
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
grep " odytty.tar.gz$" SHA256SUMS | sha256sum -c -
tar -xf odytty.tar.gz
cd odytty-*/
```

Build the release:

```sh
cargo build --release --locked
```

Install a versioned binary and point `~/.local/bin/odytty` at it:

```sh
install -Dm755 target/release/odytty "$HOME/.local/opt/odytty/$version/bin/odytty"
mkdir -p "$HOME/.local/bin"
ln -sfn "$HOME/.local/opt/odytty/$version/bin/odytty" "$HOME/.local/bin/odytty"
```

Register the app launcher, metadata, and icon:

```sh
install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  "$HOME/.local/share/applications/io.unfinished_works.odytty.desktop"
install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  "$HOME/.local/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
install -d "$HOME/.local/share/icons/hicolor"
cp -a dist/icons/hicolor/* "$HOME/.local/share/icons/hicolor/"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
```

Make sure `$HOME/.local/bin` is on `PATH`, then launch OdyTTY as a normal
application:

```sh
odytty
```

</details>

---

### macOS

macOS support is newer and in active development, not yet as battle-tested as
Linux. The prebuilt app is an Apple Silicon (arm64) build; Intel Macs use the
source build below.

#### Homebrew (recommended)

The Homebrew tap is the least-friction path. Add the tap and install the cask:

```sh
brew tap ghreprimand/odytty
brew install --cask odytty
```

The cask installs the prebuilt, ad-hoc-signed `OdyTTY.app` into `/Applications`,
so it appears in Launchpad and Spotlight and can be dragged to the Dock to pin
it. Homebrew is Apple-independent and strips the quarantine attribute on
install, so the app launches with no Gatekeeper warning even though it is not
notarized; no Apple Developer account is involved. `brew upgrade` picks up new
releases automatically.

To compile locally instead (Intel Macs, or to build from source), use the
source formula, which installs the `odytty` CLI on your `PATH` with no `.app`
bundle:

```sh
brew install ghreprimand/odytty/odytty
```

<details>
<summary><strong>Build from source</strong></summary>

A source build works with the standard Rust toolchain
([rustup](https://rustup.rs)) and the Xcode Command Line Tools. Both Apple
Silicon and Intel are supported (whatever `cargo` targets natively on your
machine). A binary you compile locally is never quarantined, so it launches with
no Gatekeeper prompt.

```sh
xcode-select --install   # once, if you don't already have the Command Line Tools
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty.tar.gz
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
grep " odytty.tar.gz$" SHA256SUMS | shasum -a 256 -c -
tar -xf odytty.tar.gz
cd odytty-*/
cargo build --release --locked
./target/release/odytty
```

To get a double-clickable **OdyTTY.app** in Applications (a locally built bundle
is also not quarantined, so it opens with no Gatekeeper prompt):

```sh
mkdir -p dist/build
cp target/release/odytty dist/build/odytty
bash dist/macos/make-app.sh "$version"
cp -R dist/build/OdyTTY.app /Applications/
```

To run it as just `odytty` from any shell, symlink the built binary onto your
`PATH`:

```sh
mkdir -p "$HOME/.local/bin"
ln -sfn "$PWD/target/release/odytty" "$HOME/.local/bin/odytty"
# Make sure ~/.local/bin is on PATH (add to ~/.zshrc if needed):
#   export PATH="$HOME/.local/bin:$PATH"
```

</details>

---

### Windows

Windows support is new and still maturing; treat it as an early, actively
supported target rather than a settled one. It builds and runs the full
terminal, and every push is exercised on a Windows CI leg, but the polish bar
is behind Linux. **Bug reports for the Windows build are especially welcome.**
Please [open an issue](https://github.com/ghreprimand/odytty/issues) with your
Windows version and a short repro if something misbehaves.

The Windows build is a single unsigned, portable `odytty.exe` packaged as
`odytty-windows-x86_64.zip`. There is no installer and nothing is written
outside your profile: configuration lives under `%APPDATA%\odytty\`.

**Scoop (recommended).** [Scoop](https://scoop.sh) is a per-user package
manager for Windows (no admin rights, everything under your profile). If you
don't already have it, install it once in PowerShell:

```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
Invoke-RestMethod -Uri https://get.scoop.sh | Invoke-Expression
```

Then add the OdyTTY bucket and install:

```powershell
scoop bucket add odytty https://github.com/ghreprimand/odytty
scoop install odytty
```

Scoop puts `odytty` on your PATH (a shim under `~\scoop\shims`), so you can
launch it by typing `odytty` in any shell, and it adds an **OdyTTY** entry to
the Start menu. It also verifies the download against the release checksum and
unpacks the zip for you, so the browser "downloaded from the internet" file
warning never comes up. Update later with `scoop update odytty`.

<details>
<summary><strong>Windows: portable zip, SmartScreen, scope details</strong></summary>

**Portable zip (direct download).** Download the always-latest zip and checksum
file, verify the hash, unpack, and run:

```powershell
Invoke-WebRequest https://github.com/ghreprimand/odytty/releases/latest/download/odytty-windows-x86_64.zip -OutFile odytty-windows-x86_64.zip
Invoke-WebRequest https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
Get-FileHash odytty-windows-x86_64.zip -Algorithm SHA256
Expand-Archive odytty-windows-x86_64.zip -DestinationPath .\odytty
.\odytty\odytty.exe
```

Compare the printed hash against the `odytty-windows-x86_64.zip` row in
`SHA256SUMS`; they must match before you run the binary. Put `odytty.exe`
anywhere on your `PATH` (for example `%LOCALAPPDATA%\Microsoft\WindowsApps`) to
launch it as `odytty` from any shell; right-click it in Explorer to pin it to
Start or the taskbar for an easy launcher.

**About the SmartScreen prompt.** OdyTTY is not code-signed yet, so the first
time you launch it Windows may show a blue "Windows protected your PC"
SmartScreen dialog naming an unknown publisher. This is expected for unsigned
open-source software and can appear however you install it: a package manager
removes the browser-download friction but does not, on its own, guarantee the
first-run prompt won't show. To run OdyTTY: click **More info**, then **Run
anyway**. To clear the "downloaded from the internet" mark up front instead,
unblock the unpacked exe in PowerShell before launching:

```powershell
Unblock-File .\odytty\odytty.exe
```

Code-signed Windows binaries are a planned improvement; until then the
SmartScreen click-through above is the intended one-time step.

**Not yet a Windows "default terminal."** You launch OdyTTY directly (from the
Start menu, by typing `odytty`, or from a pinned shortcut), and it hosts your
shells in its own tabs and panes. It can't yet be set as the system *default
terminal* (the app Windows hands console programs to when they're launched from
Explorer or another program): that requires implementing the Windows
default-terminal handoff protocol, which OdyTTY doesn't do yet. It's tracked as
future work.

**Windows scope (v1).** The Windows build opens local ConPTY-backed tabs and
panes with the full rendering, theme, effect, and inline-graphics stack, plus
persistent configuration under `%APPDATA%`, host-font discovery from
`C:\Windows\Fonts`, Windows clickable-path detection (drive-letter and UNC),
default-open/reveal via `cmd`/`explorer`, and SSH-in-a-tab over the local
pseudoconsole. Not in this release: detachable/resumable session hosting and
detached SSH (Unix-only), the headless `--interactive` mode, and the full
"Open With" application list; the hostname field and command-palette shell
history degrade to empty on Windows. Interactive Windows behaviour is verified
manually on-device; CI proves the build compiles and its unit tests pass.

</details>

---

### Running and inspecting

These commands apply on every platform once `odytty` is on your `PATH`. Run a
command directly inside OdyTTY:

```sh
odytty
odytty -e btop
odytty --working-directory /tmp -e sh -lc 'pwd; exec "$SHELL"'
odytty --title Monitor -e btop
```

OdyTTY is driven primarily by its in-app menus: the settings panel, the theme
and font pickers, and the `Ctrl+Shift+P` command palette all run live inside the
terminal, so browsing themes, choosing fonts, and changing configuration happen
visually with an immediate preview, with no command-line required.

<details>
<summary><strong>Introspection and detached-session CLI</strong></summary>

More launch examples:

```sh
# Use the hard plain renderer profile.
ODYTTY_RENDER_QUALITY=plain odytty

# Follow the desktop dark/light preference with OdyTTY defaults.
ODYTTY_THEME=system odytty

# Larger text with a named system font.
ODYTTY_FONT_SIZE=24 ODYTTY_FONT_FAMILY="DejaVu Sans Mono" odytty

# RGB subpixel antialiasing when supported by the GPU.
ODYTTY_SUBPIXEL=rgb odytty

# Stronger phosphor reference look.
ODYTTY_RETRO=on odytty
```

For system installs, Odyssey/LFS packaging, rollback, and default-terminal
notes, see [`docs/install.md`](docs/install.md). A quick source-tree smoke run
is:

```sh
cargo build --release --locked
./target/release/odytty
```

The introspection commands are a **scriptable alternative** to the in-app menus
for quick checks, automation, and headless inspection; they print and exit
without opening a window:

```sh
odytty --list-themes
odytty --list-fonts
odytty --show-config
```

Detached-session CLI commands are additive and local-only:

```sh
odytty new --detached
odytty new --detached --title work -e bash
odytty list
odytty attach
odytty attach <id>
odytty attach --diagnostic <id>
```

The same introspection works from the source tree before installing:

```sh
./target/release/odytty --list-themes
./target/release/odytty --list-fonts
./target/release/odytty --show-config
```

`--list-themes` prints the 112 built-in themes as stable
`name`/`appearance`/`family` rows. `--list-fonts` prints discoverable system
font files. `--show-config` prints the current stable config-dump subset (including
`symbol_fallback` and `symbol_font_source`, which reports the resolved
symbol/Nerd-font fallback **chain**, joined with ` > `, for example
`bundled > bundled > host:<path>`, or `disabled`); the full settings authority is
[`docs/runtime-knobs.md`](docs/runtime-knobs.md).

`odytty new --detached` starts a local session-host process and prints a stable
`id=...` row. `odytty list` prints live detached sessions as metadata-only rows
with the session name first, pane count, age, and id when the name is distinct;
it never dumps scrollback or command output. `odytty attach` with no id attaches
the only live session; if several are live, it prints the same readable list and
asks you to choose `odytty attach <id>`; if none are live, it reports that there
is nothing to attach. `odytty attach <id>` reattaches a detached session in a
live native window: the window opens its normal local shell, adds the hosted
session as a focused tab, repaints from the host snapshot, then streams live
output. If the id is missing or dead, the local shell still opens and stderr
reports `odytty: attach session <id> failed: <err>`. `odytty attach
--diagnostic <id>` is the script/CI form: it prints a one-line status dump and
exits without opening a window. The host keeps the PTY and bounded terminal
model alive across attach/detach cycles until the child exits or the detached
idle timeout reaps it.

</details>

---

## Features

OdyTTY is a broad terminal. This is the scannable summary; see
[`docs/features.md`](docs/features.md) for the complete feature and workflow
reference.

- **Terminal core:** owned clean-room DEC/xterm parser with broad escape
  coverage, X10-through-SGR-pixel mouse reporting, the Kitty keyboard protocol,
  bounded scrollback, and alternate-screen handling.
  → [reference](docs/features.md#terminal-compatibility)
- **Text and graphics:** bundled Victor Mono (default, 20px) plus JetBrains
  Mono, a Nerd-font fallback chain, color emoji, Kitty graphics, and Sixel.
  → [reference](docs/features.md#text-emoji-and-graphics),
  [`docs/graphics.md`](docs/graphics.md)
- **Workflow:** tabs, workspaces, splits and panes with a tmux-style prefix,
  detached and in-window managed sessions, and named layouts with session
  restore. → [reference](docs/features.md#native-app-workflow)
- **Shell integration and SSH:** OSC 133 prompt marks, click-to-place cursor,
  command palette, connection manager, and opt-in remote shell integration.
  → [reference](docs/features.md#shell-integration)
- **Visual and config:** 112 built-in themes, theme builder, bloom/CRT/retro
  effects, a bundled background image, window transparency, and a live settings
  panel. → [reference](docs/features.md#settings-and-themes),
  [`docs/themes.md`](docs/themes.md), [`docs/effects.md`](docs/effects.md),
  [`docs/runtime-knobs.md`](docs/runtime-knobs.md)
- **Privacy:** no telemetry, analytics, crash reporting, account, cloud sync, or
  update ping; network actions are explicit and user-initiated.

Local shortcuts, the full rebindable-action list, the pane prefix table, and
every configuration knob live in [`docs/features.md`](docs/features.md),
[`docs/keybindings.md`](docs/keybindings.md), and
[`docs/runtime-knobs.md`](docs/runtime-knobs.md).

---
## Architecture

The terminal core and visual layer are deliberately separate:

| Area | Path |
| --- | --- |
| PTY | `src/pty.rs` |
| Parser | `src/parser/` |
| Terminal model | `src/core/` |
| Render geometry | `src/grid.rs`, `src/render.rs`, `src/boxdraw.rs` |
| Text atlas and font resolution | `src/atlas/`, `src/text.rs`, `src/emoji/` |
| Graphics protocols | `src/graphics/`, `src/core/graphics_routing.rs` |
| Settings | `src/settings.rs`, `src/settings/` |
| Theme system | `src/theme/`, `src/theme_author.rs`, `src/palette_gen.rs` |
| Command palette | `src/fuzzy.rs`, `src/palette.rs`, `src/palette_catalog.rs`, `src/palette_sources.rs`, `src/native/palette_overlay.rs` |
| Native app and GPU | `src/native/` |

External crates do not own terminal semantics. `vte`, `portable-pty`, and
`crossterm` are not in the dependency tree.

---
## Testing

The repository carries unit, integration, fuzz-smoke, pixel-smoke, PTY-smoke,
GPU-composite, and CLI tests. The default suite is intended to be deterministic
and host-independent; PTY smoke and deep fuzz tiers are ignored by default.

```sh
cargo test
cargo fmt --check

# Parser/protocol deep tier when touching those paths:
ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture

# Evidence-only performance harness:
cargo bench --bench perf
```

The library test suite alone runs in the thousands of cases and is exercised on
every CI run; the current counts live in [`DEVLOG.md`](DEVLOG.md), with the full
tree carrying additional integration and smoke suites. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for the commit gate.

---
## Status

**Works today:** real shells, multi-session tabs, splits/panes with a
configurable tmux-style prefix, scrollback, search, selection,
copy/paste, font/theme/settings overlays, theme builder, 112 themes, color
emoji, Kitty graphics, Sixel, Kitty keyboard protocol, SGR-pixel mouse,
clickable paths with an inline image viewer, detached and in-window managed
sessions, a connection manager, command palette, session replay,
OSC 8/52/133, dynamic colors, prompt navigation, command status gutter,
readability and accessibility settings, bloom/CRT/retro effects, background
treatments, and a large compatibility test surface.

**Platforms:** Linux is the primary, battle-tested target. macOS is newly
supported and in active development (see Install And Run). Windows is
supported through ConPTY and ships as an unsigned portable zip. Linux, macOS,
and Windows are all blocking CI legs.

**Known gaps:** Windows detached/resumable session hosting, profiles, per-pane
inline graphics, Kitty animation, Kitty Unicode placeholders, iTerm2 graphics,
COLR/CPAL color fonts, and broader ligature/stylistic-set shaping.

The running history lives in [`DEVLOG.md`](DEVLOG.md). The current public
roadmap lives in [`TODO.md`](TODO.md) and
[`docs/full-build-roadmap.md`](docs/full-build-roadmap.md).

---
## Public Repository Safety

This repository is public. Do not commit secrets, credentials, API keys, tokens,
private hostnames or URLs, personal data, `.env` files, local-only config, or
machine-specific notes. Before any commit or push, inspect staged changes for
sensitive content.

OdyTTY itself is local-first: no telemetry, no account, no cloud sync, no
analytics, no crash reporting, and no update pings.

---
## Project Docs

- [`SPEC.md`](SPEC.md): product charter and architecture decisions.
- [`TODO.md`](TODO.md): current milestone checklist and remaining work.
- [`DEVLOG.md`](DEVLOG.md): reverse-chronological development record.
- [`CONTRIBUTING.md`](CONTRIBUTING.md): project stance on contributions, plus
  testing and public-repo safety rules.
- [`SECURITY.md`](SECURITY.md): supported versions and private vulnerability
  reporting.
- [`PACKAGING.md`](PACKAGING.md): downstream package install surface and
  release packaging notes.
- [`docs/install.md`](docs/install.md): source builds, desktop launcher
  registration, AppStream metadata, Odyssey/LFS packaging, and default-terminal
  notes.
- [`docs/release.md`](docs/release.md): release artifact checklist and
  Odyssey-Mon upstream tracking notes.
- [`docs/features.md`](docs/features.md): complete feature and workflow
  reference.
- [`docs/runtime-knobs.md`](docs/runtime-knobs.md): settings reference.
- [`docs/keybindings.md`](docs/keybindings.md): complete keyboard reference and
  rebinding.
- [`docs/accessibility.md`](docs/accessibility.md): contrast floor, color-vision
  modes, dimming, motion, and bell.
- [`docs/themes.md`](docs/themes.md): theme format and built-in library.
- [`docs/graphics.md`](docs/graphics.md): Kitty graphics and Sixel support.
- [`docs/visual-architecture.md`](docs/visual-architecture.md): renderer and
  visual-layer architecture.
- [`docs/hidpi-validation.md`](docs/hidpi-validation.md): manual HiDPI checks.
- [`docs/diagnostics.md`](docs/diagnostics.md): logging, crash reporting, and
  the privacy floor.
- [`docs/full-build-roadmap.md`](docs/full-build-roadmap.md): long-range map.

---
## License

OdyTTY is licensed under **GPL-3.0-only**. See [`LICENSE`](LICENSE).

You may use, study, share, and modify the source under that license. If you
distribute a modified version, you must release your changes under the same
license.

Copyright (C) 2026 Unfinished Works and the OdyTTY contributors.

The OdyTTY name and branding are separate from the source license. Forks and
modified builds should use their own name and must not imply endorsement by
Unfinished Works. See [`NOTICE`](NOTICE).

Contributions are accepted under the Developer Certificate of Origin. See
[`CONTRIBUTING.md`](CONTRIBUTING.md).
