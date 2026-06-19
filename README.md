# OdyTTY

**Website:** [odytty.unfinished-works.com](https://odytty.unfinished-works.com)

OdyTTY is a standalone, from-scratch, GPU-rendered Rust terminal emulator for
Linux. It owns the terminal byte path from PTY allocation through escape
parsing, terminal state, render geometry, and shaders, while relying on focused
external crates for lower-level infrastructure such as `wgpu`, `winit`,
`ab_glyph`, `swash`, `arboard`, and Unicode width tables.

The name and visual direction come from OdysseyOS, the maintainer's private
Linux From Scratch system. That system is inspiration, not a platform
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
renders text and inline graphics on the GPU, and has a substantial compatibility
and smoke-test suite. It is still Linux-first and pre-release; official release
binary artifacts, cross-platform support, panes, and profiles are not done.

## Highlights

- **Owned terminal core:** Linux PTY layer via `rustix`, clean-room DEC/xterm
  parser, OdyTTY terminal model, scrollback, alternate screen, mouse, keyboard,
  OSC, DCS, APC, and render geometry.
- **GPU renderer:** `wgpu`/Vulkan path with dynamic glyph atlas, bundled
  Victor Mono (default) plus JetBrains Mono, bold/italic/weight faces, optional
  synthetic styles, subpixel AA, HiDPI-aware atlas rebuilds, color emoji atlas,
  and GPU image layer.
- **Inline media:** Kitty graphics protocol and Sixel, including direct,
  file/temp-file, and shared-memory Kitty transports with conservative local
  file-safety restrictions.
- **Modern input:** Kitty keyboard protocol, SGR pixel mouse mode 1016, focus
  reporting, configurable local keybindings, keyboard hints, keyboard copy
  mode, and OSC 8 hyperlink hover/open.
- **Daily workflow:** search, refined selection, PRIMARY selection,
  bracketed-paste hardening, chunked large paste, right-click context menu,
  command-aware prompt navigation from OSC 133, close confirmation, and tabs.
- **Visual experience layer:** 100 built-in themes, user `.theme` files, live theme
  picker, theme builder, semantic cursor/selection/search roles, optional
  bloom/CRT/retro effects, background treatments, cursor motion, focus dimming,
  new-output fade, window padding, and window border.
- **Local configuration UX:** `odytty.conf`, live reload, in-app settings panel,
  atomic preservation-first writeback, mouse-friendly controls, font picker,
  keybinding editor, and first-run onboarding. Environment variables always win.
- **Privacy posture:** no telemetry, analytics, crash reporting, account,
  cloud sync, or update ping. The only network-capable action is explicit
  Ctrl-click link opening through `xdg-open` with a scheme allowlist.

## Install And Run

Requires Linux and a Vulkan-capable GPU. Wayland is the primary target; X11
works through the current `winit`/GPU stack with some window-manager-dependent
behavior for borderless windows and OS theme detection.

For the current source release, install OdyTTY for the current user.

Download and verify the release archive:

```sh
version=0.1.5
workdir=$(mktemp -d /tmp/odytty-install.XXXXXX)
cd "$workdir"
curl -LO "https://github.com/ghreprimand/odytty/releases/download/v${version}/odytty-${version}.tar.gz"
curl -LO "https://github.com/ghreprimand/odytty/releases/download/v${version}/SHA256SUMS"
sha256sum -c SHA256SUMS
tar -xf "odytty-${version}.tar.gz"
cd "odytty-${version}"
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

Run a command directly inside OdyTTY:

```sh
odytty -e btop
odytty --working-directory /tmp -e sh -lc 'pwd; exec "$SHELL"'
odytty --title Monitor -e btop
```

Useful launch examples:

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

CLI introspection commands print and exit without opening a window:

```sh
odytty --list-themes
odytty --list-fonts
odytty --show-config
```

From the source tree without installing:

```sh
./target/release/odytty --list-themes
./target/release/odytty --list-fonts
./target/release/odytty --show-config
```

`--list-themes` prints the 100 built-in themes as stable
`name`/`appearance`/`family` rows. `--list-fonts` prints discoverable system
font files. `--show-config` prints the current stable config-dump subset; the
full settings authority is [`docs/runtime-knobs.md`](docs/runtime-knobs.md).

## Current Feature Surface

### Terminal Compatibility

The owned parser and terminal core cover common shell and TUI behavior:
printing, UTF-8 chunking, SGR attributes including 256-color and truecolor,
cursor movement, erase, insert/delete character and line, repeat, reverse index,
scroll regions, origin mode, tab stops, bracketed paste, focus reporting,
alternate screen modes 47/1047/1048/1049, OSC 0/2 titles, OSC 7 working
directory tracking, OSC 8 hyperlinks, OSC 52 clipboard write plus opt-in read,
OSC 133 prompt marks, OSC 4/10/11/12 dynamic colors, DECRQM/DECRPM, XTWINOPS,
XTGETTCAP, DECRQSS, rectangle operations, selective erase, synchronized output
mode 2026, and broad mouse reporting.

Mouse support includes X10/normal/button-event/any-event tracking, focus
events, UTF-8, SGR, urxvt, legacy encodings, and SGR-pixel mode 1016 with true
physical pixel coordinates from the native window.

Keyboard support includes mode-aware legacy encoding and the Kitty keyboard
protocol as a negotiated overlay. With no Kitty flags active, legacy bytes are
preserved.

### Text, Emoji, And Graphics

Text rendering uses bundled Victor Mono by default at 22 logical pixels with
line height `1.0`. JetBrains Mono is also bundled and remains selectable via
`font_family`. System font families, direct font files, font-weight
variants, symbol/Nerd-font fallback, per-range symbol maps, synthetic styles,
subpixel AA, glyph coverage gamma, stem darkening, and minimum-contrast
enforcement are configurable.

Color emoji uses `swash` and a dedicated premultiplied-RGBA atlas. Noto Color
Emoji CBDT/CBLC is supported on Linux, including variation selectors, flags,
keycaps, skin tones, and common ZWJ clusters. Emoji pixels are not SGR-tinted.
COLR/CPAL and SVG-in-OpenType expansion remain future work.

Kitty graphics support includes actions `t`, `T`, `p`, `d`, and `q`; raw RGB,
raw RGBA, and PNG still images; direct, file, temp-file, and POSIX shared-memory
transports; chunking; image and placement ids; z-index; crop; cell scaling; and
pixel offsets. Sixel supports the DEC/xterm data language, RGB/HLS color
introducers, repeat, raster attributes, transparency, VT340 palette, and DECSDM.
Animation and Kitty Unicode placeholders are not supported.

### Native App Workflow

The native app runs multiple sessions. `Ctrl+Shift+T` opens a new tab,
`Ctrl+Shift+W` closes the active tab, and `Ctrl+PageDown` /
`Ctrl+PageUp` switch tabs. The tab bar appears when two or more sessions exist;
a single shell keeps the original full-grid view. Current limitation: in-band
image placements can sit one row high while the tab bar is visible.

Core local shortcuts:

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+F` | Search scrollback |
| `Ctrl+Shift+,` | Settings panel |
| `Ctrl+Shift+H` | Theme picker |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / paste |
| `Shift+PageUp` / `Shift+PageDown` | Scroll local viewport |
| `Ctrl+Shift+L` | Keyboard quick-select hints |
| `Ctrl+Shift+Space` | Keyboard copy mode |
| `Ctrl+Shift+Up` / `Ctrl+Shift+Down` | Jump to previous / next prompt mark |
| `Ctrl+Shift+K` | Clear editable prompt input when shell integration allows it |

`ODYTTY_KEYBINDS` can rebind local actions: `search`, `settings`,
`theme-picker`, `copy`, `paste`, `scroll-up`, `scroll-down`,
`jump-prompt-prev`, `jump-prompt-next`, `copy-mode`, `hints`, `clear-input`,
`new-tab`, `next-tab`, `prev-tab`, and `close-tab`.

### Settings And Themes

Settings load in this order:

1. Built-in defaults.
2. `$XDG_CONFIG_HOME/odytty/odytty.conf`, or
   `~/.config/odytty/odytty.conf`.
3. `ODYTTY_*` environment variables.

The config file format is `key = value` with `#` comments. The native app polls
the resolved file about once per second; env-pinned keys stay pinned for the
session. The settings panel live-applies changes and writes only changed keys
back to `odytty.conf`, preserving comments, blank lines, unknown keys, and
ordering via same-directory atomic rename.

`theme = system` or `ODYTTY_THEME=system` follows the desktop dark/light
preference using OdyTTY defaults (`odyssey` dark, `odyssey-light` light).
Explicit `follow_os_theme`, `os_theme_dark`, and `os_theme_light` settings allow
custom mappings.

See:

- [`docs/runtime-knobs.md`](docs/runtime-knobs.md) for every config key,
  environment variable, range, default, and reload behavior.
- [`docs/odytty.conf.example`](docs/odytty.conf.example) for an annotated config.
- [`docs/themes.md`](docs/themes.md) for the theme format and built-in roster.
- [`docs/effects.md`](docs/effects.md) for bloom, CRT, retro, background, and
  motion effects.

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
| Native app and GPU | `src/native/` |

External crates do not own terminal semantics. `vte`, `portable-pty`, and
`crossterm` are not in the dependency tree.

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

Recent library-only checks in the devlog show `cargo test --lib` at 1778
passing tests, with the full tree carrying additional integration and smoke
suites. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the commit gate.

## Status

**Works today:** real shells, multi-session tabs, scrollback, search, selection,
copy/paste, font/theme/settings overlays, theme builder, 100 themes, color
emoji, Kitty graphics, Sixel, Kitty keyboard protocol, SGR-pixel mouse,
OSC 8/52/133, dynamic colors, prompt navigation, command status gutter,
readability and accessibility settings, bloom/CRT/retro effects, background
treatments, and a large compatibility test surface.

**Known gaps:** official release artifacts, macOS/Windows support, panes,
profiles, session persistence, Kitty animation, Kitty Unicode placeholders,
iTerm2 graphics, COLR/CPAL color fonts, broader ligature/stylistic-set shaping,
custom tab-bar polish, and the current tab-bar image-placement offset issue.

The running history lives in [`DEVLOG.md`](DEVLOG.md). The current public
roadmap lives in [`TODO.md`](TODO.md) and
[`docs/full-build-roadmap.md`](docs/full-build-roadmap.md).

## Public Repository Safety

This repository is public. Do not commit secrets, credentials, API keys, tokens,
private hostnames or URLs, personal data, `.env` files, local-only config, or
machine-specific notes. Before any commit or push, inspect staged changes for
sensitive content.

OdyTTY itself is local-first: no telemetry, no account, no cloud sync, no
analytics, no crash reporting, and no update pings.

## Project Docs

- [`SPEC.md`](SPEC.md) — product charter and architecture decisions.
- [`TODO.md`](TODO.md) — current milestone checklist and remaining work.
- [`DEVLOG.md`](DEVLOG.md) — reverse-chronological development record.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — contribution, testing, and public-repo
  safety rules.
- [`PACKAGING.md`](PACKAGING.md) — downstream package install surface and
  release packaging notes.
- [`docs/install.md`](docs/install.md) — source builds, desktop launcher
  registration, AppStream metadata, Odyssey/LFS packaging, and default-terminal
  notes.
- [`docs/release.md`](docs/release.md) — release artifact checklist and
  Odyssey-Mon upstream tracking notes.
- [`docs/runtime-knobs.md`](docs/runtime-knobs.md) — settings reference.
- [`docs/themes.md`](docs/themes.md) — theme format and built-in library.
- [`docs/graphics.md`](docs/graphics.md) — Kitty graphics and Sixel support.
- [`docs/visual-architecture.md`](docs/visual-architecture.md) — renderer and
  visual-layer architecture.
- [`docs/hidpi-validation.md`](docs/hidpi-validation.md) — manual HiDPI checks.
- [`docs/full-build-roadmap.md`](docs/full-build-roadmap.md) — long-range map.

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
