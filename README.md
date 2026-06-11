# OdyTTY

## What it is

Odyssey Terminal is a reliable terminal emulator with an OdysseyOS visual identity, exploring how motion, themes, effects, and interface details can make command-line work feel more alive without weakening core terminal behavior. Its central question is whether a terminal can add useful, nonstandard features and a richer experience while staying fast, solid, and practical for daily use.

## Status

Active development — well past first prototype, foundations complete. The focus
now shifts toward profiles, configuration introspection, and progressive visual
identity work.

### What works today

**Owned byte path.** Every byte from the PTY to the glyph quad passes through
OdyTTY-owned code. The PTY layer uses Linux `openpt`/`grantpt`/`unlockpt` via
`rustix`, spawning children as session leaders with a controlling terminal.
The VT parser (`src/parser/`) is a clean-room two-layer pipeline built from
primary specifications (vt100.net DEC ANSI diagram, ECMA-48, xterm `ctlseqs`):
a ground-state text/UTF-8 segmenter plus an 8-bit-clean 14-state control
automaton. The terminal model (`src/core/`), renderer geometry (`src/grid.rs`),
and shaders are entirely OdyTTY-owned. External crates are intentional
below-product-line tools — `ab_glyph` (font rasterization), `wgpu` (GPU API),
`winit` (windowing), `arboard` (clipboard), `unicode-width` — and do not own
terminal semantics.

**Terminal compatibility.** Sequences confirmed across the fixture suite: SGR
(all standard attributes, 256-color, truecolor), cursor movement and position
reporting, scroll regions (DECSTBM, DECOM), alternate screen (modes
47/1047/1048/1049 with correct per-mode cursor save/restore semantics), mouse
reporting (modes 9/1000/1002/1003 with encodings 1005/1006/1015/legacy), focus
reporting (DECSET 1004), DECSCUSR cursor-style overrides, OSC 0/2 window title,
wide-character handling (width-2 CJK/emoji, combining marks, overwrite-half
coherence), ICH/DCH/ECH/REP, tab stops, BCE, RI, SU/SD, IL/DL, RIS/DECSTR,
DA, and bracketed paste.

**Graphics.** Sixel DCS streams (`DCS q`) are fully decoded and placed as
cell-anchored RGBA images: the SX1 decoder handles the complete Sixel data
language (RGB/HLS color introducers, repeat, raster attributes, VT340 16-color
default palette), and SX2 integrates the decoder with the graphics scene via the
owned DCS hook/put/unhook path. The GPU image layer (G2.3) renders visible
placements as alpha-blended RGBA8 textured quads between cell backgrounds and
glyphs. The Kitty graphics protocol handles APC `_G` control parsing, base64
payload decode, direct raw RGB/RGBA transmission (`f=24`/`f=32`, `t=d`),
PNG still-image transmission (`f=100`), chunked transfers (`m=1`/`m=0`),
delete/query actions, and file/shared-memory transports (`t=f`/`t=t`/`t=s`)
with conservative security restrictions. The full placement surface is
supported: named placement ids (`p=`), display-stored-image commands (`a=p`),
z-index ordering (`z=`), source-crop geometry (`x=`/`y=`/`w=`/`h=`),
cell-box scaling, and sub-cell pixel offsets (`X=`/`Y=`). Kitty and Sixel
both render through the same shared placement scene.

**Text and rendering quality.** The `wgpu`/Vulkan surface rasterizes via
`ab_glyph` into a dynamic glyph atlas. Wide-glyph (CJK/width-2) atlas slots
span two physical cells so East Asian glyphs render without clipping.
Bearing-aware quad geometry sizes each glyph to its real ink bounds so italic
side-bearings and tall glyphs render uncropped. Backgrounds render before glyphs
so wide-character overflow ink is never erased by a neighbor's background.
Optional subpixel AA (`ODYTTY_SUBPIXEL=rgb|bgr`) uses dual-source blending when
the GPU supports it. A tunable gamma/contrast uniform (`ODYTTY_TEXT_GAMMA`,
default 1.4) corrects coverage weight for dark-background readability. Bold,
italic, and bold-italic atlas slots load discovered style faces; missing style
faces fall back to regular without synthetic emboldening. Underline and
strikethrough render as metric-derived solid quads. Dim, inverse, and hidden
are handled in the vertex path.

**HiDPI.** `GpuState` retains logical font size and current scale factor and
rebuilds the atlas on change. `ScaleFactorChanged` is wired with debounce,
recomputes cell metrics, and drives the grid resize path. 11 headless tests
cover CellSize correctness, grid recompute, debounce, UV seam-freedom, and
rebuild invalidation. See `docs/hidpi-validation.md` for the operator-runnable
manual matrix.

**Daily-driver interaction.** Search (`Ctrl+Shift+F`) runs across scrollback
and screen with next/prev navigation and visible match highlights. Selection
supports double-click word, triple-click line, drag-scroll at viewport edges,
and scrollback-aware absolute row anchors. Clipboard uses chunked background
writes for large pastes, bracketed-paste sanitization, line-ending normalization,
and Linux PRIMARY selection for middle-click paste. A right-edge scroll indicator
shows viewport position when scrolled back. Cursor shapes (block, underline, bar)
and blink policy are configurable via settings and overridable per-application
via DECSCUSR. Key bindings for terminal-local actions are configurable via
`ODYTTY_KEYBINDS`.

**Settings.** Runtime settings load at native startup from built-in defaults,
then `$XDG_CONFIG_HOME/odytty/odytty.conf` (or
`~/.config/odytty/odytty.conf`), then `ODYTTY_*` environment variables.
Environment variables always win, so existing env-based launch scripts remain
bit-exact. The config file is a simple `key = value` format with `#` comments;
bad lines are warned and skipped without aborting startup. The native app polls
the config file about once per second for live reloads: env-overridden keys stay
pinned until restart, deleted files keep the current settings, and invalid
rewrites are ignored without changing the active session. Theme, visual,
font size/family/path, text gamma, subpixel mode, cursor defaults, and key
bindings reload live; the development-only autoclose timer is startup-only.

**Performance.** Lazy scrollback re-wrap stores logical lines and defers deep
re-wrap on width change (~46 ms → ~20 µs for 50k-line scrollback). A fast path
skips reflow entirely on height-only resize (~17 ms → ~58 µs). The vertex
buffer is a reused CPU allocation with a grow-only GPU buffer. Unchanged frames
reuse retained GPU geometry; cursor-blink and overlay-only frames rebuild only
the bounded tail of the vertex stream rather than the full grid. Resize events
are debounced to avoid per-frame reflow during drag.

**Testing.** 714 tests passing: 676 unit/integration, 19 pixel-smoke
(headless CPU compositor asserting structural raster invariants for text
rendering and graphics placement), 9 PTY alternate-screen smoke, and 10
transcript smoke. `cargo bench --bench perf` runs headless throughput
benchmarks for the terminal model and parser separately from the default suite.

### Remaining gaps

- Ligature/stylistic-set shaping is not implemented (strategy decided but
  implementation deferred).
- Profiles and config introspection are not implemented.
- No tabs, panes, sessions, profiles, or multiplexing.
- Shell integration beyond basic PTY behavior is not implemented.
- Linux-first only — no macOS or Windows support.
- Side-by-side visual comparison with Ghostty at matched font/size has not been
  done; visible text quality gaps may still exist.

See [`DEVLOG.md`](DEVLOG.md) for the running record and [`TODO.md`](TODO.md) for
the milestone checklist.

## Why build it

Odyssey Terminal is worth exploring because the terminal is a daily operating surface, not just a utility, and OdysseyOS needs one that carries its own visual identity without compromising trust. It is for the operator who wants command-line work to feel more expressive, polished, and alive while remaining dependable enough for real use. The friction it removes is the gap between solid existing terminals and a more personal, visually distinctive environment: instead of accepting either reliability with generic presentation or flashiness that risks distraction, the project tests whether both can coexist. Scope should stop before novelty damages terminal fundamentals; speed, compatibility, input correctness, readable text, stable rendering, and predictable behavior matter more than effects, themes, or nonstandard features.

## Build direction

The project owns its full byte path by design. The terminal core (PTY, parser,
screen model), renderer geometry, and shaders are OdyTTY-originated code;
external crates handle font rasterization, GPU API access, windowing, clipboard
transport, and Unicode character data — the same boundary the strongest
independent terminals draw. Architecture keeps the terminal core separate from
the Odyssey experience layer so visual experiments do not destabilize terminal
correctness.

The build is Linux-first, written in Rust, GPU-backed (`wgpu`/Vulkan), and
validated against xterm/Ghostty/Konsole behavior as compatibility references —
not implementation sources. Visual capability parity with the strongest GPU
terminals is a floor; surpassing it within OdyTTY's visual identity is the
standing goal.

## Project docs

- [`DEVLOG.md`](DEVLOG.md) — running record of what has landed and current state.
- [`TODO.md`](TODO.md) — milestone checklist from prototype stabilization into
  foundation ownership.
- [`SPEC.md`](SPEC.md) — durable product and architecture decisions.
- [`docs/runtime-knobs.md`](docs/runtime-knobs.md) — current native prototype
  settings and launch examples.
- [`docs/odytty.conf.example`](docs/odytty.conf.example) — annotated example
  config file; copy to `~/.config/odytty/odytty.conf` to use.
- [`docs/full-build-roadmap.md`](docs/full-build-roadmap.md) — staged roadmap
  from prototype stabilization through long-term product work.
- [`docs/graphics.md`](docs/graphics.md) — Kitty graphics protocol and Sixel
  support: action/format/transport matrix, security posture, DECSDM, and
  examples.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — change, commit, and safety conventions.
