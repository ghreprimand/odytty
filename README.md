# OdyTTY

**Website:** [odytty.unfinished-works.com](https://odytty.unfinished-works.com)

OdyTTY is a custom Rust terminal emulator built from the ground up for
OdysseyOS. Every byte from the PTY to the rendered glyph passes through
OdyTTY-owned code — the PTY layer, escape-sequence parser, terminal model,
renderer geometry, and GPU shaders are all original. External crates handle
font rasterization, GPU API, windowing, and clipboard transport, which is the
same boundary mature independent terminals draw.

The terminal is GPU-backed (`wgpu`/Vulkan), Linux-first, and designed to be
reliable enough for daily use. It has a handful of features you won't find
combined in most terminals: live color emoji with ZWJ sequence / flag /
skin-tone cluster support, the Kitty graphics protocol plus Sixel, the Kitty
keyboard protocol, SGR-pixel mouse reporting (mode 1016), a fully theme-driven
ANSI palette with semantic roles, and an optional ambient visual treatment with
a planned expansion into readability-first enhancements and atmospheric effects
— all off by default and gated behind explicit settings.

---

## Features

### Full byte-path ownership

The PTY layer (`src/pty.rs`) uses `rustix` directly for
`openpt`/`grantpt`/`unlockpt` and session-leader spawn. The VT parser
(`src/parser/`) is a clean-room two-layer pipeline built from primary
specifications (vt100.net, ECMA-48, xterm `ctlseqs`) — not derived from `vte`
or any other terminal's source. `vte`, `portable-pty`, and `crossterm` are not
in the dependency tree.

### Live color emoji

Full cluster rendering via `swash`: ZWJ family sequences, flag pairs, skin-tone
modifiers, keycap sequences, and variation-selector policy (VS15 forces text
presentation, VS16 forces color, `Emoji_Presentation`-default characters choose
color automatically). Multi-codepoint clusters whose cells carry trailing ZWJ
marks are stitched into a single wide atlas slot. Noto Color Emoji CBDT/CBLC
bitmaps are rasterized and stored in a dedicated `ColorGlyphAtlas` (premultiplied
RGBA, keyed by shaped cluster identity rather than Unicode scalar). Emoji pixels
are never SGR-tinted. Falls back gracefully if Noto Color Emoji is not installed.

### Kitty graphics protocol + Sixel

The full Kitty APC-based graphics surface: actions `t/T/p/d/q`, raw RGB/RGBA
(`f=24`/`f=32`) and PNG (`f=100`) formats, direct and file/shared-memory
transports (`t=d/f/t/s`), chunked transfer, placement ids, z-index, source crop,
cell scaling, and pixel offsets. File transports apply conservative security
restrictions (temp-dir allowlist, `O_NOFOLLOW`, delete-before-decode). Sixel
(`DCS q`) covers the complete data language: RGB/HLS color introducers, repeat,
raster attributes, VT340 16-color default palette, and `DECSDM`.

### Kitty keyboard protocol

Progressive keyboard enhancement as a negotiated overlay: flags for
disambiguation, event types (press/repeat/release), alternate keys,
report-all, and associated text. The stack is bounded at 16 entries;
`RIS`/`DECSTR` resets it. At flags 0 the output is byte-identical to legacy
key encoding.

### SGR-pixel mouse reporting (mode 1016)

Full end-to-end support: `DECSET`/`DECRST`/`DECRQM` wired, native pixel-to-cell
coordinate seam closed, pixel reports emitted as `CSI < Cb ; Px ; Py M|m` with
true physical pixel coordinates.

### Theme-driven ANSI palette + semantic roles

`Theme` carries the full 16-color ANSI palette (indices 0–7 normal, 8–15
bright) plus semantic-role colors (cursor, selection, search highlight, and
reserved border/inactive). The library ships 61 contrast-validated built-in
themes: the Odyssey identity family (`plain` — the default, reproducing
historical xterm defaults byte-for-byte — plus `odyssey`, `odyssey-noir`,
`odyssey-light`, `odyssey-aurora`, and more), a set of widely-used community
palettes (`solarized-dark`/`-light`, `gruvbox-dark`, `nord`, `dracula`,
`tokyo-night`, `catppuccin-mocha`/`-latte`, `one-dark`, `monokai`, and others),
and a retro / phosphor family (`green-phosphor`, `amber-crt`, and more). See
[`docs/themes.md`](docs/themes.md) for the full roster. OSC-4 and OSC-10/11/12 dynamic
overrides layer on top with correct precedence. Select with
`ODYTTY_THEME=odyssey` or `theme = odyssey` in the config file. User theme
files are supported: write a `.theme` file and drop it in
`~/.config/odytty/themes/` or point `ODYTTY_THEME` at a path.

### Terminal compatibility

Comprehensive VT sequence coverage confirmed across the fixture suite: SGR
(all standard attributes, 256-color, truecolor; colon-form `38:2::r:g:b` and
`48:2::r:g:b` alongside semicolon form), cursor movement and position reporting,
scroll regions (DECSTBM, DECOM), alternate screen (modes 47/1047/1048/1049 with
correct per-mode cursor save/restore), mouse reporting (modes 9/1000/1002/1003
with encodings 1005/1006/1015/1016/legacy), focus reporting (DECSET 1004),
DECSCUSR cursor-style overrides, OSC 0/2 window title, wide-character handling
(CJK/emoji combining marks, overwrite-half coherence), ICH/DCH/ECH/REP, tab
stops, BCE, RI, SU/SD, IL/DL, RIS/DECSTR, DA, and bracketed paste.

**Capability queries.** XTGETTCAP (`DCS +q`) answers the conservative truth set:
`TN=xterm-256color`, `Co=256`, `RGB=1`; unknown names get the xterm-style
invalid response. DECRQSS (`DCS $q`) reports live SGR (including extended
underline styles and underline color), DECSCUSR, DECSCA, and DECSTBM.

**Rectangle operations.** DECCRA (snapshot-copy, overlap-safe), DECFRA, DECERA,
DECSERA, DECCARA/DECRARA attribute rectangle ops (bold, underline, blink,
inverse; stream and exact extents via DECSACE), DECSCA protection,
DECSED/DECSEL selective erase, wide-pair edge sanitization.

See [`SPEC.md`](SPEC.md) for the complete architecture and sequence reference.

### Text and rendering quality

GPU-backed via `ab_glyph` into a dynamic glyph atlas. Wide-glyph (CJK/width-2)
atlas slots span two physical cells. Bearing-aware quad geometry sizes each
glyph to its real ink bounds so italic side-bearings and tall glyphs render
uncropped. Optional subpixel AA (`ODYTTY_SUBPIXEL=rgb|bgr`) uses dual-source
blending when the GPU supports it. Tunable coverage gamma (`ODYTTY_TEXT_GAMMA`,
default 1.4). Synthetic bold (double-strike) and italic (12° shear) when real
faces are absent; `ODYTTY_SYNTHETIC_STYLES=off` disables synthesis. Extended
underline styles fully decoded and rendered (`SGR 4:0`–`4:5`: straight, double,
curly, dotted, dashed). Underline color via `SGR 58`/`59`.

### Daily-driver interaction

Scrollback search (`Ctrl+Shift+F`) with next/prev navigation and match
highlights. Refined selection: double-click word, triple-click line, drag-scroll,
scrollback-aware anchors. Clipboard: chunked background writes for large pastes,
bracketed-paste sanitization, line-ending normalization, Linux PRIMARY selection.
Right-edge scroll indicator. Configurable cursor shapes (block/underline/bar),
blink policy, and key bindings (`ODYTTY_KEYBINDS`).

**OSC 8 hyperlinks.** Shell output with OSC 8 sequences renders hover underline
highlighting. Ctrl+click opens links via `xdg-open` through a scheme allowlist
(`http`, `https`, `file`, `mailto`); links are never auto-opened from input.

**OSC 7 working directory.** `file://` URLs from the shell's `chpwd` hook are
parsed and stored as advisory state for native consumers (e.g. open-in-directory).
Localhost-only; foreign hosts are ignored; no filesystem access.

**OSC 52 + dynamic colors.** Clipboard write via OSC 52 with cap and UTF-8
validation. Read disabled by default (`osc52_read = off`); opt-in with
`ODYTTY_OSC52_READ=on`. OSC 10/11/12 and OSC 4 palette entries with full
reset support (OSC 104/110/111/112).

**Synchronized output.** DEC private mode 2026 with 150 ms safety timeout.

### Configuration

Settings load from built-in defaults → `~/.config/odytty/odytty.conf` →
environment variables; env always wins. The config file uses a simple
`key = value` format with `#` comments. The native app polls for live reloads
about once per second. See [`docs/runtime-knobs.md`](docs/runtime-knobs.md)
for the full knob reference and [`docs/odytty.conf.example`](docs/odytty.conf.example)
for an annotated example.

**In-app settings panel.** `Ctrl+Shift+,` opens a keyboard-driven settings
editor covering font, theme, cursor, keybinds, and all runtime knobs. Edits
apply live through the existing reload seam. `Ctrl+S` writes changed rows back
to `odytty.conf` without destroying comments, blank lines, or unknown keys
(preservation-first writeback via same-directory atomic rename). When a
`font_family` edit names a family that can't be resolved — not found, or found
but not monospace — the panel shows a clear, family-named error instead of
silently keeping the previous font.

**Theme picker.** `Ctrl+Shift+T` opens a built-in theme picker. Arrowing through
the library previews each theme immediately, `Enter` persists the selected
theme to `odytty.conf`, and `Esc` restores the theme that was active when the
picker opened.

### HiDPI

Scale-factor changes rebuild the atlas and recompute cell metrics through the
same path as a font-size change. Resize events are debounced to avoid
per-frame reflow during drag.

### Privacy

OdyTTY runs entirely on your machine. No telemetry, no analytics, no crash
reporting, no update pings, no account, no cloud sync — there is no network
client in the terminal because none is built. Your settings, themes, and
scrollback never leave the local filesystem, and `odytty.conf` is a plain file
you own and can read in full. Because the source is open (GPL-3.0), the absence
of any data collection is verifiable rather than promised. The only
network-capable action is Ctrl+click to open a hyperlink — explicit,
user-initiated, routed through `xdg-open`, and gated by a scheme allowlist;
links are never opened from terminal output automatically.

---

## Build and run

```sh
cargo build --release
cargo run -- --native
```

Requires a Vulkan-capable GPU. Tested on Linux (Wayland + X11 via XWayland).

Quick launch examples:

```sh
# Larger text with the Odyssey theme
ODYTTY_FONT_SIZE=18 ODYTTY_THEME=odyssey cargo run -- --native

# Custom font family
ODYTTY_FONT_FAMILY="DejaVu Sans Mono" cargo run -- --native

# RGB subpixel AA
ODYTTY_SUBPIXEL=rgb cargo run -- --native
```

CLI helpers (print and exit, no window): `odytty --list-themes` lists the
built-in themes as stable `name`/appearance/family rows; `odytty --show-config`
prints the effective merged configuration (defaults + `odytty.conf` +
environment overrides) as sorted `key=value` lines for scripts and debugging.

---

## Status

**Active development.** The foundation is solid and the feature surface is
broad. The current focus is visual identity — themes, appearance settings, and
progressive rendering enhancements — while keeping terminal correctness as the
non-negotiable floor.

### What works today

Everything in the Features section above. The full owned byte path is real and
in production. Color emoji, Kitty graphics, Sixel, the Kitty keyboard protocol,
SGR-pixel mouse, the theme palette and user theme file format, the 61-theme
built-in library, the in-window overlay framework, the in-app settings
panel plus live theme picker, the in-app custom theme builder, and CLI config
introspection have all landed. The minimum-contrast readability floor
(`min_contrast` / `ODYTTY_MIN_CONTRAST`) and geometric box-drawing/block/Powerline
rendering (`geometric_boxdraw` / `ODYTTY_GEOMETRIC_BOXDRAW`) are wired into the
live renderer, each default-off / pixel-identical until enabled. Themed
cursor/selection/search roles (`themed_ui_roles`) are live and default-on, so a
theme's authored selection and cursor colors drive the UI out of the box;
`themed_ui_roles = off` restores the classic inverse rendering. A symbol /
Nerd-font fallback for Private-Use-Area prompt icons is wired into the live
atlas behind the `symbol_fallback` setting (with an optional `symbol_font`
path; `ODYTTY_SYMBOL_FALLBACK` / `ODYTTY_SYMBOL_FONT` remain as env overrides),
default-off / byte-identical until enabled. Focus dimming (`focus_dim` /
`ODYTTY_FOCUS_DIM`) perceptually dims the whole grid while the window is
unfocused so it recedes, with the contrast floor keeping text legible;
default-off / focused frames byte-identical.

### On the horizon

- **Readability-first rendering** — smooth scrolling is next. (The perceptual
  color pipeline backs linear-space blending; the minimum-contrast floor
  (`ODYTTY_MIN_CONTRAST`) and geometric box-drawing (`ODYTTY_GEOMETRIC_BOXDRAW`)
  are now live in the renderer, the symbol / Nerd-font fallback
  (`symbol_fallback`) is wired into the atlas, and stem darkening
  (`ODYTTY_STEM_DARKEN`) is available — all default off / passthrough.)
- **Atmospheric effects (opt-in)** — bloom/phosphor glow, CRT/retro profile,
  subtle cursor motion; all off by default, perf- and readability-gated. See
  [`docs/effects.md`](docs/effects.md) for settings and how to enable them.
- **Ligature / stylistic-set shaping** (strategy decided, implementation
  deferred).
- **Shell integration** — native working-directory consumer (OSC 7 core
  tracking already landed).

### Known gaps

- No tabs, panes, sessions, or multiplexing.
- Linux-first; no macOS or Windows support.
- Shell integration beyond OSC 7 cwd core not yet implemented.

---

## Testing

**Testing.** Over 1200 tests passing: 1205 unit/integration, 12 mouse-protocol
(hermetic encoder coverage: legacy byte boundaries, UTF-8 coordinate extension,
SGR and urxvt decimal coordinates, wheel, modifier folding, X10 modifier
stripping, protocol-specific release encoding, motion gating for
normal/button-event/any-event modes, and SGR-pixel (1016) encoder coverage —
press/release/wheel/motion with 1-based pixel coordinates, boundary at `(1,1)`,
large coordinate values, modifier folding, not-1016 guard, and cell-path
pass-through; run via
`cargo test --test mouse_protocol`), 41 pixel-smoke (headless CPU compositor
asserting structural raster invariants for text rendering and graphics
placement; EM3 added two — color-glyph segment draw ordering between coverage
text and above-image layers, and wide color glyph lead-cell quad emission —
ID1 default-on added three covering the now-default themed selection/cursor
colors plus the `themed_ui_roles = off` legacy inverse parity, RV3-dim added
one asserting the perceptual dim delta stays confined to dim cells, and ID2
added two — a focus-dim-off identity gate and an unfocused-dimmed baseline that
recedes while still clearing a raised contrast floor, and RV-COVERAGE added
three — the minimum-contrast floor at its cursor-block under-glyph resolve site,
the focus-dim × floor background-dim precondition, and themed selection/cursor
role resolution against real light and dark built-ins), 4
box-drawing pixel-smoke (geometric box/block/Powerline: corner↔line seam, cross
join, full-block solidity, and the off/on distinction), 11 protocol-fuzz smoke (never-panic, bounded-host-output, post-RIS,
and grid-self-consistency invariants across seven fuzzed surfaces: extended
underline SGR, Kitty keyboard protocol stack, synchronized output mode 2026, OSC
52 / dynamic colors, DECRQM / XTWINOPS, DCS query reports (XTGETTCAP / DECRQSS),
and DEC rectangle / selective-erase ops), 9 PTY alternate-screen smoke, 10
transcript smoke, 8 emoji pixel-smoke (real Noto color-emoji composition,
VS15/VS16 presentation policy, multi-codepoint cluster stitching/fallback, and
monochrome-foreground suppression for resident color-emoji cells), 3 CLI
introspection (theme enumeration, config-dump formatting, and a spawned
`--show-config` over a temp config), 3 GPU composite smoke (one renders a tiny
scene direct-to-swapchain vs. through the offscreen→composite seam and asserts
byte-equality, guarding the plain post-process path; one proves the bloom
pass leaves the off path exact, keeps sub-threshold body text unchanged, and
gives a bright HDR cell a bounded halo; and one proves the CRT pass leaves the
off path exact and dims lit cells only within the capped scanline/vignette band
without zeroing them; adapter-gated), and 3 stem-raster smoke (proving RV5 stem-darkening is wired
through the live glyph-atlas raster: the default-on boost raises midtone
coverage monotonically with the `0`/`255` endpoints pinned, and the `0.0`
opt-out restores the classic raster byte-for-byte), and 1 license-header
guard (asserting every tracked Rust and WGSL source file carries the
`SPDX-License-Identifier: GPL-3.0-only` tag on its first line). Deep fuzz
tiers are `#[ignore]`-gated and run via
`ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored`.
EM2 added three hermetic emoji-probe tests (fixed representative-sequence list,
bounded filename discovery in a temp directory, and non-color format detection
for outline fonts); the host-dependent full probe against an installed Noto Color
Emoji is `#[ignore]`-gated and runs via
`cargo test emoji -- --ignored`.
`cargo bench --bench perf` runs headless throughput benchmarks for the
terminal model and parser separately from the default suite. B3 added four
surface rows: DECFRA full-page fill (~2.3 µs/op, ~1.2 ns/cell), DECCRA
overlapping copy (~3.0 µs/op), DECSERA mixed-protection erase (~5.5 µs/op),
and an SGR colon-subparam storm (`4:n` + `58:2:r:g:b` per cell) that
exercises the extended-underline parse path the semicolon heavy-SGR row never
reaches. A per-cell size diagnostic prints `Cell 36 B / Attrs 20 B` at the
current baseline. Three profiles are selectable via `ODYTTY_PERF_PROFILE`:
`default` (bounded, routine acceptance runs), `legacy` (pre-B2 workload sizes),
and `quick` (smoke); see [`docs/runtime-knobs.md`](docs/runtime-knobs.md).

---

## Project docs

- [odytty.unfinished-works.com](https://odytty.unfinished-works.com) — the OdyTTY website.
- [`DEVLOG.md`](DEVLOG.md) — running record of what has landed.
- [`TODO.md`](TODO.md) — milestone checklist.
- [`SPEC.md`](SPEC.md) — durable product and architecture decisions.
- [`docs/runtime-knobs.md`](docs/runtime-knobs.md) — all settings and launch examples.
- [`docs/themes.md`](docs/themes.md) — theme file format, built-ins, and user theme directory.
- [`docs/odytty.conf.example`](docs/odytty.conf.example) — annotated example config file.
- [`docs/effects.md`](docs/effects.md) — visual effects guide (bloom, CRT profile, plain/fast mode).
- [`docs/graphics.md`](docs/graphics.md) — Kitty and Sixel protocol reference.
- [`docs/visual-architecture.md`](docs/visual-architecture.md) — renderer pipeline and visual-enhancement direction.
- [`docs/full-build-roadmap.md`](docs/full-build-roadmap.md) — full build roadmap (everything still planned).
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — change, commit, and safety conventions.

---

## License

OdyTTY is licensed under the **GNU General Public License v3.0 only**
(GPL-3.0-only). See the [`LICENSE`](LICENSE) file for the full text.

You are free to use, study, share, and modify OdyTTY. If you distribute a
modified version, you must release your changes under the same license (strong
copyleft).

Copyright (C) 2026 Unfinished Works and the OdyTTY contributors.

### Name & branding

OdyTTY's **source code** is free and open source under the GPL-3.0 — you're
welcome to use, study, modify, fork, and redistribute it under that license.

The **OdyTTY name and logo** are a separate matter from the code license.
They're how people recognize this specific project, so we ask one thing: if you
ship a modified version or a fork, please give it your own name and don't
present it as the official OdyTTY or imply it's endorsed by Unfinished Works.
Calling it *"based on OdyTTY"* or *"a fork of OdyTTY"* is perfectly fine and
welcome — just don't call it *OdyTTY*. See the [`NOTICE`](NOTICE) file for the
full note.

Thanks for helping keep the name clear for everyone.

**Contributing:** contributions are welcome under the Developer Certificate of
Origin (DCO) — see [`CONTRIBUTING.md`](CONTRIBUTING.md) for details.
