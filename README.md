# OdyTTY

**Website:** [odytty.unfinished-works.com](https://odytty.unfinished-works.com)

![OdyTTY rendering a colorized git graph, project tree, and truecolor gradients under the default Odyssey theme with bloom](assets/demo.png)

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
splits each tab into panes, renders text and inline graphics on the GPU, and has
a substantial compatibility and smoke-test suite. It is still Linux-first and
pre-release; macOS is supported as an experimental build-from-source target (see
the macOS install notes below), while profiles remain a follow-up.

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
  reporting, IME composition input (CJK / compose-key accents), configurable
  local keybindings, keyboard hints, keyboard copy mode, and OSC 8 hyperlink
  hover/open.
- **Daily workflow:** search, refined selection, PRIMARY selection,
  bracketed-paste hardening, chunked large paste, bounded scrollback history,
  right-click context menu,
  command-aware prompt navigation from OSC 133, configurable bell (visual flash
  / window urgency), close confirmation, and tabs.
- **Splits / panes:** split any tab into side-by-side or stacked panes, each
  with its own shell, scrollback, selection, search, and cursor. A configurable
  tmux-style prefix (default `Ctrl+b`) drives split, focus-move, close, zoom
  (full-bleed the focused pane), and equalize once a tab has multiple panes;
  dividers are drag-resizable. On a single-pane tab, `Ctrl+b` passes through to
  the shell unchanged, and a single-pane tab renders exactly as before.
- **Visual experience layer:** 100 built-in themes, user `.theme` files, live theme
  picker, theme builder, semantic cursor/selection/search roles, optional
  bloom/CRT/retro effects, background treatments, cursor motion, focus dimming,
  new-output fade, window padding, and window border.
- **Local configuration UX:** `odytty.conf`, live reload, in-app settings panel,
  atomic preservation-first writeback, mouse-friendly controls, font picker,
  keybinding editor, and first-run onboarding. Environment variables always win.
- **SSH connection substrate:** an OdyTTY-owned local hosts list for the SSH
  manager, default-off opt-in OpenSSH config host-name import, and a connect
  action that opens `ssh` in a new tab/session. Imports are read-only,
  name-only, bounded, and never surface key material; authentication remains
  with the system `ssh` binary and agent.
- **Privacy posture:** no telemetry, analytics, crash reporting, account,
  cloud sync, or update ping. Network-capable actions are explicit and
  user-initiated: Ctrl-click link opening through `xdg-open` with a scheme
  allowlist, and SSH connect entries that exec the system `ssh` binary.

## Install And Run

OdyTTY ships as source. Linux is the primary, battle-tested target; macOS is an
experimental build-from-source target (see [macOS](#macos-experimental) below).
Both build from the same source archive with `cargo build --release`. Windows is
not yet supported.

### Linux

Requires Linux and a Vulkan-capable GPU. Wayland is the primary target; X11
works through the current `winit`/GPU stack with some window-manager-dependent
behavior for borderless windows and OS theme detection.

For the current source release, install OdyTTY for the current user.

Download and verify the release archive:

```sh
version=0.2.1
workdir=$(mktemp -d /tmp/odytty-install.XXXXXX)
cd "$workdir"
curl -LO "https://github.com/ghreprimand/odytty/releases/download/v${version}/odytty-${version}.tar.gz"
curl -LO "https://github.com/ghreprimand/odytty/releases/download/v${version}/SHA256SUMS"
grep "odytty-${version}.tar.gz" SHA256SUMS | sha256sum -c -
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

### macOS (experimental)

macOS is experimental and not yet as battle-tested as Linux. OdyTTY is built
from source: a binary you compile locally is never quarantined, so it launches
with no Gatekeeper warning and needs no signing, notarization, or workaround.
There is no prebuilt download — and that is deliberate, since an unsigned
prebuilt would otherwise be blocked by Gatekeeper on first launch.

Requires the Rust toolchain ([rustup](https://rustup.rs)) and the Xcode Command
Line Tools. Both Apple Silicon and Intel are supported (whatever `cargo` targets
natively on your machine).

```sh
xcode-select --install   # once, if you don't already have the Command Line Tools
version=0.2.1
curl -LO "https://github.com/ghreprimand/odytty/releases/download/v${version}/odytty-${version}.tar.gz"
curl -LO "https://github.com/ghreprimand/odytty/releases/download/v${version}/SHA256SUMS"
grep "odytty-${version}.tar.gz" SHA256SUMS | shasum -a 256 -c -
tar -xf "odytty-${version}.tar.gz"
cd "odytty-${version}"
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

To run it as just `odytty` from any shell (as the examples below show), symlink
the built binary onto your `PATH`:

```sh
mkdir -p "$HOME/.local/bin"
ln -sfn "$PWD/target/release/odytty" "$HOME/.local/bin/odytty"
# Make sure ~/.local/bin is on PATH (add to ~/.zshrc if needed):
#   export PATH="$HOME/.local/bin:$PATH"
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

Detached-session CLI commands are additive and local-only:

```sh
odytty new --detached
odytty new --detached --title work -e bash
odytty list
odytty attach <id>
odytty attach --diagnostic <id>
```

From the source tree without installing:

```sh
./target/release/odytty --list-themes
./target/release/odytty --list-fonts
./target/release/odytty --show-config
```

`--list-themes` prints the 100 built-in themes as stable
`name`/`appearance`/`family` rows. `--list-fonts` prints discoverable system
font files. `--show-config` prints the current stable config-dump subset (including
`symbol_fallback` and `symbol_font_source`, which reports the resolved
symbol/Nerd-font fallback **chain**, joined with ` > ` — e.g.
`bundled > bundled > host:<path>`, or `disabled`); the full settings authority is
[`docs/runtime-knobs.md`](docs/runtime-knobs.md).

`odytty new --detached` starts a local session-host process and prints a stable
`id=...` row. `odytty list` prints live detached sessions as metadata-only rows
(`id`, `name`, `state`, `age_ms`, `panes`) and never dumps scrollback or command
output. `odytty attach <id>` reattaches a detached session in a live native
window: the window opens its normal local shell, adds the hosted session as a
focused tab, repaints from the host snapshot, then streams live output. If the
id is missing or dead, the local shell still opens and stderr reports
`odytty: attach session <id> failed: <err>`. `odytty attach --diagnostic <id>`
is the script/CI form: it prints a one-line status dump and exits without
opening a window. The host keeps the PTY and bounded terminal model alive across
attach/detach cycles until the child exits or the detached idle timeout reaps
it.

## Current Feature Surface

### Terminal Compatibility

The owned parser and terminal core cover common shell and TUI behavior:
printing, UTF-8 chunking, SGR attributes including 256-color and truecolor,
cursor movement, erase, insert/delete character and line, insert/replace mode
(IRM), repeat, reverse index, scroll regions, origin mode, tab stops, bracketed
paste, focus reporting,
alternate screen modes 47/1047/1048/1049, OSC 0/2 titles, OSC 7 working
directory tracking, OSC 8 hyperlinks, OSC 52 clipboard write plus opt-in read,
OSC 133 prompt marks, OSC 4/10/11/12 dynamic colors, DECRQM/DECRPM, XTWINOPS,
XTGETTCAP, DECRQSS, rectangle operations, selective erase, synchronized output
mode 2026, and broad mouse reporting.

Mouse support includes X10/normal/button-event/any-event tracking, focus
events, UTF-8, SGR, urxvt, legacy encodings, and SGR-pixel mode 1016 with true
physical pixel coordinates from the native window. Alternate scroll mode (1007,
default on) translates the wheel into cursor-key presses on the alternate
screen, so full-screen TUIs that do not track the mouse — pagers and similar —
scroll with the wheel.

Keyboard support includes mode-aware legacy encoding and the Kitty keyboard
protocol as a negotiated overlay. With no Kitty flags active, legacy bytes are
preserved. IME composition is enabled: input-method pre-edit is rendered inline
at the cursor and committed text is sent to the shell, so CJK input methods and
compose-key/dead-key accents work.

The bell (BEL) is configurable via the `bell` setting: `urgent` (default,
requests window attention when unfocused), `visual` (a brief readability-safe
screen flash), `all` (both), or `off`. There is no audible bell.

### Text, Emoji, And Graphics

Text rendering uses bundled Victor Mono by default at 20 logical pixels with
line height `1.0`. JetBrains Mono is also bundled and remains selectable via
`font_family`. The font picker groups families into **Bundled Fonts** (Victor
Mono, JetBrains Mono — always available) and **System Fonts** (host monospace
families), and either resolves with zero config. The symbol/Nerd-font fallback
is a **chain** of bundled faces — Nerd Fonts **v3** and **v2** — so PUA prompt
icons render out of the box regardless of which Nerd Font era a config emits or
whether the host has any Nerd font installed. System font families, direct font
files, font-weight variants, per-range symbol maps, synthetic styles, subpixel
AA, glyph coverage gamma, stem darkening, and minimum-contrast enforcement are
configurable.

Color emoji uses `swash` and a dedicated premultiplied-RGBA atlas. Bitmap-strike
color fonts are supported — Noto Color Emoji (CBDT/CBLC) on Linux and Apple Color
Emoji (sbix) on macOS — including variation selectors, flags, keycaps, skin
tones, and common ZWJ clusters. Text-default symbols stay on the monochrome
fallback path, and missing color glyph coverage falls back there instead of
tofu. Emoji pixels are not SGR-tinted. COLR/CPAL and SVG-in-OpenType expansion
remain future work.

Kitty graphics support includes actions `t`, `T`, `p`, `d`, and `q`; raw RGB,
raw RGBA, and PNG still images; direct, file, temp-file, and POSIX shared-memory
transports; chunking; image and placement ids; z-index; crop; cell scaling; and
pixel offsets. Sixel supports the DEC/xterm data language, RGB/HLS color
introducers, repeat, raster attributes, transparency, VT340 palette, and DECSDM.
Animation and Kitty Unicode placeholders are not supported.

### Native App Workflow

The native app runs multiple sessions. `Ctrl+Shift+T` opens a new tab,
`Ctrl+Shift+W` closes the active tab, and `Ctrl+PageDown` /
`Ctrl+PageUp` switch tabs. Closing a tab closes the **whole** tab — every pane
it holds — which is distinct from closing a single pane (see "Close Pane"
below); closing the last remaining tab quits the app. The tab bar appears when
two or more sessions exist; a single shell keeps the original full-grid view.
The active tab is framed with a thin themed outline so it stays legible over
background images and treatments. Inline graphics are offset by the same
reserved tab-bar row as text, so Kitty/Sixel placements stay aligned with the
visible grid while the bar is shown. Right-click a tab to rename it for the
session; the custom name overrides shell title updates until cleared.

Any tab can be split into panes. The direct chords `Ctrl+Shift+E` (split into
columns, new pane on the right) and `Ctrl+Shift+O` (split into rows, new pane
below) create a split on a single-pane tab — they match Ghostty's Linux
defaults and work at both single-pane and multi-pane. You can also split from
the right-click menu's "Split Right" / "Split Down" items. When the active tab
is already multi-pane, the right-click menu also offers a "Close Pane" item
(labelled with the effective `Ctrl+b x` prefix chord) to close just the focused
pane; it is hidden in a single-pane tab, where closing the tab is the only
close. Once the active tab has multiple panes, a tmux-style prefix (default
`Ctrl+b`, configurable via
`pane_prefix`) opens a transient pane-command mode; press the prefix then a pane
key:

| After the prefix | Action |
| --- | --- |
| `%` | Split the focused pane into columns (side-by-side) |
| `"` | Split the focused pane into rows (stacked) |
| `←` / `→` / `↑` / `↓` | Move focus to the neighbor pane |
| `o` | Cycle focus to the next pane |
| `x` | Close the focused pane |
| `z` | Zoom / un-zoom the focused pane (full-bleed; layout is preserved) |
| `Space` / `=` | Equalize split sizes |
| `Ctrl+b` (prefix again) | Send a literal prefix to the focused pane (nested multiplexer) |

Each pane owns an independent PTY, terminal model, scrollback, viewport,
selection, search, and cursor. Drag a divider to resize the panes on either
side. The prefix is captured only when the active tab has more than one pane; a
single-pane shell receives `Ctrl+b` unchanged, preserving the byte-identical
default input path. Set `pane_prefix=off` to disable the pane prefix entirely
and free the chord in multi-pane tabs too. v1 cuts: inline
graphics render in single-pane tabs only, interactive overlays (selection /
search) are painted for the focused pane only. Optional inactive-pane dimming is
implemented via `inactive_pane_dim`; it defaults to `0.0`, is disabled on
`render_quality=plain`, and leaves the no-dim pane frame byte-identical.

Core local shortcuts:

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+E` / `Ctrl+Shift+O` | Split the focused pane into columns / rows |
| `Ctrl+Shift+F` | Search scrollback |
| `Ctrl+Shift+,` | Settings panel |
| `Ctrl+Shift+H` | Theme picker |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / paste |
| `Shift+PageUp` / `Shift+PageDown` | Scroll local viewport |
| `Ctrl+Shift+L` | Keyboard quick-select hints |
| `Ctrl+Shift+Space` | Keyboard copy mode |
| `Ctrl+Shift+Up` / `Ctrl+Shift+Down` | Jump to previous / next prompt mark |
| `Ctrl+Shift+K` | Clear editable prompt input when shell integration allows it |

The command palette is available as the `command-palette` action, but it is
unbound by default so existing PTY input stays unchanged. Opt in with a free
chord, for example:

```sh
ODYTTY_KEYBINDS="ctrl+alt+p=command-palette" odytty
```

The palette fuzzy-filters local actions, bounded read-only shell history, and
recent OSC 7 directories. Selecting a history or directory row types that text
into the active pane without pressing Enter; selecting an action runs the local
action after the overlay closes.

Output replay is available as the `session-replay` action (also unbound by
default). Turn on recording with `session_replay = on` (or
`ODYTTY_SESSION_REPLAY=on`) — off by default, so the plain path is unchanged —
then bind a chord to scrub a bounded in-memory ring of recent screen frames:

```sh
ODYTTY_SESSION_REPLAY=on ODYTTY_KEYBINDS="ctrl+alt+r=session-replay" odytty
```

The ring is capped (600 frames and 24 MiB, whichever binds first) and
local-only — frames never touch disk or the network. The overlay is
presentation-only: `←`/`→` step, `PgUp`/`PgDn` jump ten, `Home`/`End` jump to
the ends, and the live session keeps running underneath untouched while you
scrub.

The connection manager is available as the `connection-manager` action (also
unbound by default). Bind a chord to open a type-to-filter list of saved hosts
and quick-connect with Enter:

```sh
ODYTTY_KEYBINDS="ctrl+alt+h=connection-manager" odytty
```

Hosts come from the OdyTTY-owned `hosts.conf` and, only when
`ssh_config_hosts = on`, name-only entries from your OpenSSH config. With the
opt-in off, the overlay lists OdyTTY-owned hosts only and never references
`~/.ssh`. The overlay is presentation-only; selecting a host spawns the system
`ssh` in a new session.

`ODYTTY_KEYBINDS` can rebind local actions: `search`, `settings`,
`theme-picker`, `copy`, `paste`, `scroll-up`, `scroll-down`,
`jump-prompt-prev`, `jump-prompt-next`, `copy-mode`, `hints`, `clear-input`,
`command-palette`, `session-replay`, `connection-manager`, `new-tab`,
`next-tab`, `prev-tab`, and `close-tab`. The pane actions
(`split-columns`, `split-rows`, `focus-pane-left` / `-right` / `-up` / `-down`,
`focus-pane-next`, `close-pane`, `zoom-pane`, `equalize-panes`) are rebindable
too — the chord is the key pressed *after* the prefix, e.g.
`ODYTTY_KEYBINDS="ctrl+f=zoom-pane"`.

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
| Command palette | `src/fuzzy.rs`, `src/palette.rs`, `src/palette_catalog.rs`, `src/palette_sources.rs`, `src/native/palette_overlay.rs` |
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

Recent library-only checks in the devlog show `cargo test --lib` at 2009
passing tests, with the full tree carrying additional integration and smoke
suites. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the commit gate.

## Status

**Works today:** real shells, multi-session tabs, splits/panes with a
configurable tmux-style prefix, scrollback, search, selection,
copy/paste, font/theme/settings overlays, theme builder, 100 themes, color
emoji, Kitty graphics, Sixel, Kitty keyboard protocol, SGR-pixel mouse,
OSC 8/52/133, dynamic colors, prompt navigation, command status gutter,
readability and accessibility settings, bloom/CRT/retro effects, background
treatments, and a large compatibility test surface.

**Platforms:** Linux is the primary, battle-tested target. macOS is an
experimental build-from-source target (see Install And Run). Both are exercised
in CI. Windows is not yet supported.

**Known gaps:** Windows support, profiles, per-pane inline graphics, Kitty
animation, Kitty Unicode placeholders, iTerm2 graphics, COLR/CPAL color fonts,
and broader ligature/stylistic-set shaping.

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
