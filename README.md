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
below-product-line tools — `ab_glyph` (font rasterization for normal text),
`swash` (emoji shaping/rasterization and color-font probe; normal text remains on
`ab_glyph`), `wgpu` (GPU API), `winit` (windowing), `arboard` (clipboard),
`unicode-width` — and do not own terminal semantics. The EM2 emoji discovery
helpers call `fc-match` directly for Noto Color Emoji, fall back to a bounded
filesystem scan (depth 6, 20 000-file cap) when fontconfig is absent or returns
a non-matching font, and return `None` cleanly when Noto Color Emoji is not
installed so the rest of the stack is not affected. The EM3/EM4 color-glyph
atlas and placement path are OdyTTY-owned renderer plumbing; Noto Color Emoji
CBDT/CBLC shaping and bitmap rasterization are delegated to `swash`, with
VS15/VS16 policy and fallback routing owned by OdyTTY.

**Terminal compatibility.** Sequences confirmed across the fixture suite: SGR
(all standard attributes, 256-color, truecolor; colon-form `38:2::r:g:b` and
`48:2::r:g:b` truecolor also accepted alongside semicolon form), cursor movement and position
reporting, scroll regions (DECSTBM, DECOM), alternate screen (modes
47/1047/1048/1049 with correct per-mode cursor save/restore semantics), mouse
reporting (modes 9/1000/1002/1003 with encodings 1005/1006/1015/1016/legacy), focus
reporting (DECSET 1004), DECSCUSR cursor-style overrides, OSC 0/2 window title,
wide-character handling (width-2 CJK/emoji, combining marks, overwrite-half
coherence), ICH/DCH/ECH/REP, tab stops, BCE, RI, SU/SD, IL/DL, RIS/DECSTR,
DA, and bracketed paste. Terminal capability queries: XTGETTCAP (`DCS +q … ST`)
answers the conservative truth set OdyTTY can currently claim — `TN=xterm-256color`,
`Co=256`, `RGB=1`; unknown or unsupported capability names receive the
xterm-style invalid response rather than a guessed value. DECRQSS (`DCS $q … ST`)
reports live state: SGR including all extended underline styles (`4:2`–`4:5`) and
underline color (`58:…`), DECSCUSR cursor style (`" q`), DECSCA protection state
(`"q`), and DECSTBM scroll margins; unimplemented selectors respond invalid per
xterm convention. Rectangle operations: DECCRA copies a rectangular region
within the visible page (snapshot-copy avoids overlap corruption); DECFRA fills
a rectangle with a single printable character using current SGR and DECSCA state;
DECERA erases a rectangle unconditionally; DECSERA erases only unprotected cells.
DECSCA (`CSI Ps " q`) sets the protection bit on future printed and filled cells;
DECSED (`CSI ? J`) and DECSEL (`CSI ? K`) perform selective erase of unprotected
cells in display/line respectively — regular ED and EL still erase all cells
regardless of protection. Any rectangle operation that clips a wide glyph at its
edge blanks the complete pair rather than leaving an orphan continuation cell.
DECCARA (`CSI Pt;Pl;Pb;Pr;Pm $ r`) changes presentation attributes in a
rectangle: SGR 0 resets all four, 1 sets bold, 4 sets underline, 5 sets blink,
7 sets inverse, 22 clears bold, 24 clears underline, 25 clears blink, 27 clears
inverse; `4:x` extended underline subparameters are ignored in the rect path per
xterm behavior. DECRARA (`CSI Pt;Pl;Pb;Pr;Pm $ t`) toggles: 0 toggles all four,
1/4/5/7 toggle their respective attribute; protection is not modified by either
op per xterm convention. DECSACE (`CSI Ps * x`) selects the extent: Ps=0/1
chooses stream mode (first row from left coordinate to right edge, middle rows
full-width, last row from column 0 to right coordinate), Ps=2 chooses exact
rectangle. The `blink` cell attribute (SGR 5/25) is now storable and is
round-tripped by DECRQSS `m`.

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
default 1.4) corrects coverage weight for dark-background readability. Bold, italic, and bold-italic atlas slots load
discovered style faces. When a requested face is absent OdyTTY synthesizes it:
italic is drawn with a ~12° horizontal shear (tan 12° ≈ 0.2126, applied per
raster row relative to the baseline); bold is rendered by double-striking at a
small rightward embolden offset; bold-italic composes both. Real loaded faces
always win — synthesis is active only for absent slots, so a font family with
all four weights renders those exactly. Synthesis can be disabled with
`ODYTTY_SYNTHETIC_STYLES=off` (config key `synthetic_styles = off`); see
`docs/runtime-knobs.md` for the full knob reference. Extended underline styles are fully
decoded and rendered: `SGR 4` / `4:1` straight; `4:2` double (two parallel
solid quads); `4:3` curly (stepped square-wave approximation within the cell
height); `4:4` dotted; `4:5` dashed; `4:0` or `SGR 24` clears the style.
Underline color is set via `SGR 58` in either semicolon (`58;2;r;g;b`,
`58;5;n`) or colon (`58:2::r:g:b`, `58:5:n`) form and cleared with `SGR 59`;
without an explicit color the effective foreground is used. Strikethrough
renders as a metric-derived solid quad. Dim, inverse, and hidden are handled
in the vertex path.

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

**OSC 8 hyperlinks.** Shell output that emits OSC 8 link sequences (e.g. `ls
--hyperlink`, `man` references, git output) renders with hover underline
highlighting when the cursor moves over a linked cell. Ctrl+click (or
Ctrl+Shift+click when mouse reporting is active) opens the link via `xdg-open`.
The open action is gated behind a scheme allowlist (`http`, `https`, `file`,
`mailto`) — the URI is passed directly as an argument with no shell
interpolation. Links are never opened automatically from terminal input; the
allowlist check only runs on an explicit user action.

**Kitty keyboard protocol.** OdyTTY implements progressive keyboard enhancement
as a negotiated overlay on the DEC/xterm key encoder. Applications enable
enhancement by pushing flag values onto a per-screen stack: flag 1 (disambiguate)
encodes ambiguous control/Alt and named keys as CSI-u sequences; flag 2
(event types) adds repeat (`:2`) and release (`:3`) event subfields; flag 4
(alternate keys) includes shifted and base-layout key-code subfields in CSI-u
character events; flag 8 (report-all) extends encoding to ordinary printable
keys and recovery keys; flag 16 (associated text) appends generated printable
text as the third CSI-u parameter when combined with report-all. The stack is
managed via `CSI > flags u` (push current flags and set new), `CSI < n u` (pop
n saved states), `CSI = flags ; mode u` (set/OR/NAND the active flags), and
`CSI ? u` (query — terminal responds `CSI ? flags u`). The stack is bounded at
16 entries with oldest-entry eviction; `RIS` and `DECSTR` reset it. Primary and
alternate screens maintain independent stacks. At flags 0 (no enhancement),
OdyTTY emits byte-identical legacy key bytes.

**Synchronized output.** OdyTTY supports DEC private mode 2026
(`DECSET/DECRST ?2026h/l`) for tear-free batch redraws. While the mode is set,
the native layer defers GPU content uploads so the display is not updated
mid-frame, letting TUIs (e.g. tmux, `lazygit`) compose a full screen update
before it becomes visible. A 150 ms safety timeout (`SYNCHRONIZED_OUTPUT_TIMEOUT`)
releases the hold automatically — a crashed application that never sends the
DECRST cannot leave the window frozen indefinitely. Cursor blink continues live
during the hold via a lightweight cursor-only redraw path, so the cursor
animates smoothly even while grid content is deferred.

**OSC 52 clipboard and dynamic colors.** Shell programs can write to the
clipboard via `OSC 52 ; selector ; base64 ST`: OdyTTY decodes the base64
payload (cap 64 KiB decoded), validates UTF-8, and routes the write to the
host clipboard through an explicit queue. Selectors `c` and `p` target the
regular clipboard and PRIMARY selection; an empty selector defaults to `c`.
Clipboard read (`... ; ? ST`) is disabled by default — a terminal that replies
to read requests lets any remote program exfiltrate local clipboard contents,
so the default `osc52_read = off` queues no request and sends no reply. Only
an explicit `osc52_read = on` / `ODYTTY_OSC52_READ=on` opt-in enables read
replies. Dynamic color controls are fully supported: `OSC 10`/`11`/`12` set
and query default foreground, background, and cursor colors; `OSC 4` sets and
queries individual palette entries; `OSC 104`/`110`/`111`/`112` reset the
palette and default-color overrides back to the active theme baseline.

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

**Testing.** 895 tests passing: 825 unit/integration, 12 mouse-protocol
(hermetic encoder coverage: legacy byte boundaries, UTF-8 coordinate extension,
SGR and urxvt decimal coordinates, wheel, modifier folding, X10 modifier
stripping, protocol-specific release encoding, motion gating for
normal/button-event/any-event modes, and SGR-pixel (1016) encoder coverage —
press/release/wheel/motion with 1-based pixel coordinates, boundary at `(1,1)`,
large coordinate values, modifier folding, not-1016 guard, and cell-path
pass-through; run via
`cargo test --test mouse_protocol`), 25 pixel-smoke (headless CPU compositor
asserting structural raster invariants for text rendering and graphics
placement; EM3 added two: color-glyph segment draw ordering between coverage
text and above-image layers, and wide color glyph lead-cell quad emission), 11 protocol-fuzz smoke (never-panic, bounded-host-output, post-RIS,
and grid-self-consistency invariants across seven fuzzed surfaces: extended
underline SGR, Kitty keyboard protocol stack, synchronized output mode 2026, OSC
52 / dynamic colors, DECRQM / XTWINOPS, DCS query reports (XTGETTCAP / DECRQSS),
and DEC rectangle / selective-erase ops), 9 PTY alternate-screen smoke, 10
transcript smoke, and 3 emoji pixel-smoke (real Noto color-emoji composition:
VS15/VS16 presentation policy and monochrome-foreground suppression for resident
color-emoji cells). Deep fuzz tiers are `#[ignore]`-gated and run via
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
reaches. A per-cell size diagnostic prints `Cell 36 B / Attrs 20 B` at the current
baseline; `Attrs` was 28 B after B2/B3 (`Attrs` held eight `bool` fields after
US1 and RC1 growth) and is now 20 B after PERF1b packed those eight bools into
a private `u16` flags field. `Cell` correspondingly dropped from 44 B to 36 B.
PERF1b resolved the flagged B3 seq regression: the scroll-heavy `seq` row
recovered from 9.6 to 11.9 MB/s (+24% on the legacy profile), and no bench row
regressed. Three profiles are selectable via
`ODYTTY_PERF_PROFILE`: `default` (bounded, routine acceptance runs), `legacy`
(pre-B2 workload sizes), and `quick` (smoke); see
[`docs/runtime-knobs.md`](docs/runtime-knobs.md).

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
