# OdyTTY — Devlog

Public running record of how OdyTTY is built, in reverse-chronological order.
Each entry captures what landed, the current state, and the known gaps toward
the first meaningful prototype. See `TODO.md` for the milestone checklist and
`SPEC.md` for durable product/architecture decisions.

---

## 2026-06-10 — Native render-loop perf: vertex reuse and resize debounce

This packet applies the native-side mitigations from the P1 findings: reduce
per-frame allocation around geometry rebuilds, and avoid paying core resize /
PTY winsize cost on every compositor resize event during window drags.

### What landed

- **Reusable vertex generation** — `grid::build_vertices_into` refills an
  existing `Vec<Vertex>` while keeping the existing `build_vertices` API as a
  compatibility wrapper.
- **Grow-only native vertex storage** — `GpuState` now owns the CPU vertex
  vector and a GPU vertex buffer capacity. Steady-state frames clear/refill the
  CPU vector and upload with `queue.write_buffer`; the GPU buffer is recreated
  only when the required byte capacity grows.
- **Resize debounce** — the GPU surface still reconfigures immediately on every
  `WindowEvent::Resized`, but terminal model reflow and PTY `TIOCSWINSZ` are
  applied immediately at most once per 40 ms, with the latest pending size
  applied on a trailing wake. During a drag, the old terminal grid remains
  rendered in fixed cell pixels over the resized surface until the debounced
  model resize lands; this avoids grid tearing while bounding reflow work.

### Verified

- Added native tests for the resize debounce state machine (time-injected, no
  sleeps), grow-only vertex capacity, and `build_vertices_into` allocation
  reuse.
- `cargo test --lib native::tests` passes (`47` passed, `1` ignored).
- Ran `cargo bench --bench perf` after the change. The harness still reports
  `build_vertices()` at ~95.7 us/op because it intentionally calls the
  allocating compatibility wrapper; P1's baseline was ~95.6 us/op. The native
  render path now removes the extra per-frame CPU allocation and GPU buffer
  recreation around that geometry build.

### Known gaps

- Region-dirty redraw skipping is still deferred; it needs finer core
  `DirtyRegion` granularity before native can skip unchanged rows safely.
- Core resize/reflow cost is still being addressed separately; native debounce
  reduces event-burst frequency but does not make each core reflow cheaper.

---

## 2026-06-10 — Performance profiling harness (evidence)

Stage 3 evidence packet: a headless benchmark harness through the owned terminal
model, plus a findings doc with ranked optimization proposals. Measure first —
no optimization landed in this packet.

### What landed

- **`benches/perf.rs`** — dependency-free (`harness = false`, registered in
  `Cargo.toml`, excluded from `cargo test`). Run with `cargo bench --bench perf`.
  Workloads: feed throughput (seq, plain ASCII, heavy SGR, scroll-region churn,
  full repaint), per-frame cost (`snapshot()`, `snapshot_with_scrollback()`,
  `build_vertices()`, combined redraw), and resize/reflow with deep vs shallow
  vs height-only scrollback.

### Findings (headline)

- **Resize/reflow is O(total scrollback)** — ~46 ms/op at 50k lines vs ~8 µs
  with shallow scrollback, and ~17 ms even when width is unchanged (no re-wrap
  needed). The dominant hotspot; a window drag at any real scrollback depth
  hitches.
- **`build_vertices` is the per-frame hotspot** — ~96 µs, 56× `snapshot()`,
  rebuilding all geometry every frame because dirty tracking is all-or-nothing.
- **Feed throughput is healthy** (135–270 MB/s); `snapshot()` (1.7 µs) and
  dirty tracking are cheap.

### Verified

- 266 lib + 2 integration + 10 smoke tests pass; the bench is absent from
  `cargo test`. `cargo fmt` clean; clippy clean (incl. the bench) except the
  pre-existing core derive lint. Ranked proposals (resize fast path, bounded
  reflow, vertex-buffer reuse, region dirty) captured for future packets.

---

## 2026-06-10 — Shader text gamma and contrast

Stage 3 text quality now includes a native shader-side coverage correction for
glyph blending. The atlas still stores linear R8 coverage; the GPU adjusts that
coverage immediately before compositing foreground glyph quads over cell
background quads.

### What landed

- **`ODYTTY_TEXT_GAMMA` setting** — new runtime knob parsed through
  `Settings`, clamped to `0.5..=3.0`, and passed into `NativeOptions`. Invalid
  values fall back to the default with one warning.
- **Default `1.4`** — chosen from the low end of the R2 finding's recommended
  `1.4..=1.8` starting range. It gives light-on-dark text more perceptual
  weight without jumping to the heavier end of the range.
- **Exact legacy escape hatch** — `ODYTTY_TEXT_GAMMA=1.0` takes an explicit
  shader branch that uses raw atlas coverage, preserving the previous linear
  blend path instead of relying on `pow(coverage, 1.0)` backend behavior.
- **Uniform plumbing** — the cell shader uniform now packs surface size,
  optional visual-effect params, and text params in one 32-byte buffer. Glyphs
  apply `pow(coverage, 1.0 / gamma)` before straight-alpha compositing; ambient
  scanlines still affect backgrounds only.

### Verified

- Added settings tests for parse/default/invalid/clamp behavior and native
  tests for settings propagation, text-param packing, `1.0` legacy value, and
  the 32-byte uniform layout.
- `cargo test` passes (`271` lib tests passed, `1` ignored; PTY smoke `2`
  passed; transcript smoke `10` passed, `1` ignored).
- `cargo fmt --check` passes.
- Native autoclose smoke exits 0 at default settings, with
  `ODYTTY_FONT_SIZE=18`, and with `ODYTTY_TEXT_GAMMA=1.0`.

### Manual observation

- Full-screen Wayland screenshots were captured for default gamma and
  `ODYTTY_TEXT_GAMMA=1.0`. On the dark OdyTTY prompt, the `1.4` default appears
  slightly fuller/brighter than the legacy path without changing cell layout.
  This was a short visual check, not a long operator comfort pass.

### Known gaps

- This does not add subpixel AA. R2 recommends keeping that as a later optional
  packet because it needs RGB coverage, dual-source blending, and per-monitor
  gating.
- True beyond-cell glyph overflow still needs future bearing-aware geometry.

---

## 2026-06-10 — Rasterization quality: baseline, rounding, padding gutter

Stage 3 raster-quality work on the glyph atlas (`src/atlas.rs`), all CPU-side:
no native, shader, or per-cell layout changes — `CellSize` values and the 1:1
cell→quad contract are unchanged, so the renderer is untouched.

### What landed

- **Single documented baseline** — every glyph (ASCII, accents, box-drawing,
  dynamic) is positioned on one integer baseline (the font ascent rounded),
  replacing the prior split where `cell.baseline` was rounded but glyphs were
  drawn at the raw float ascent. Mixed glyphs now share a consistent line.
- **Per-slot padding gutter** (`ATLAS_PAD = 1`) — each atlas slot reserves a
  transparent 1px border while `uv_rect` still hands out only the inner
  `cell.width × cell.height` rect. The gutter (a) stops bilinear sampling at
  non-integer scale factors from bleeding a neighbor's coverage into a glyph
  edge, and (b) absorbs bearing-driven edge overflow so box-drawing joins and
  descenders are preserved instead of hard-cropped.
- **Rounded placement** — rasterization rounds to the nearest atlas pixel
  instead of truncating, and clips to the slot (cell + its own gutter) rather
  than the bare cell, so a glyph's final row/column is no longer dropped.

### Verified

- +4 atlas fixtures (padding-gutter separation, box-drawing U+2500/U+2502 reach
  the cell edges, glyphs share one baseline, descender not cropped); existing
  atlas tests updated for the padded layout. 261 lib + 2 integration + 10 smoke
  pass. `cargo fmt` clean; clippy clean except the pre-existing core derive lint;
  native autoclose smoke exit 0 at default and `ODYTTY_FONT_SIZE=18`.

### Deferred (findings for a future native packet)

- Shader **gamma/contrast** blending (biggest remaining visible win), optional
  **subpixel** AA, and true **beyond-cell** glyph overflow (needs a grid/native
  geometry change, not a raster change). Written up as findings.

---

## 2026-06-10 — Native scrollback search UI

The Q1 scrollback search engine is now wired into the native window, giving the
prototype an in-terminal search loop without touching core semantics or the GPU
shader path.

### What landed

- **Native search state** — new `src/native/search_ui.rs` owns the open/closed
  state, query text, current match, case-insensitive default, next/previous
  navigation, viewport-jump math, and snapshot-only rendering helpers.
- **Keyboard loop** — `Ctrl+Shift+F` opens/closes search; typed characters build
  the query; `Backspace` edits; `Enter` and `Shift+Enter` jump next/previous
  with wraparound through the Q1 `find_next`/`find_prev` behavior; `Esc` closes
  and restores the pre-search viewport.
- **PTY isolation while searching** — when the bar is open, keyboard input is
  consumed by native search rather than sent to the shell. Mouse and PTY output
  behavior otherwise stay on the existing paths.
- **Viewport jumps and highlights** — the current match scrolls into view using
  the absolute row convention shared with selection. All visible matches are
  highlighted by mutating the snapshot copy before vertex generation; the
  current match uses a distinct indexed highlight. The search bar itself is a
  bottom-row snapshot overlay, not terminal state.
- **Resize/reflow reset** — native resize closes search and returns to the live
  bottom so stale absolute match rows are never carried across reflow.

### Verified

- Added headless tests for the search query state machine, case-insensitive
  refresh, next/previous wraparound, viewport jump math, and snapshot-only
  overlay rendering.
- `cargo test` passes (`262` lib tests passed, `1` ignored; PTY smoke `2`
  passed; transcript smoke `10` passed, `1` ignored).
- `cargo fmt --check` passes.
- Native autoclose smoke exits 0.

### Known gaps

- The search bar is deliberately minimal: no case-sensitivity toggle, no
  persistent query history, and no dedicated search UI theme yet.
- Manual interactive search validation is still useful before treating this as
  daily-driver-comfortable.

---

## 2026-06-10 — Native dynamic glyph atlas wiring

The native renderer now uses the dynamic glyph cache from the atlas layer during
live rendering. Non-ASCII cells no longer have to stay fallback boxes once the
loaded font can rasterize the codepoint.

### What landed

- **Font retained in the renderer** — `GpuState` keeps the loaded `FontVec` next
  to the atlas so frame rebuilds can populate dynamic glyph slots without
  touching terminal-core state.
- **Batched per-snapshot ensure** — before rebuilding vertex geometry, native
  scans the snapshot for non-ASCII, non-continuation cells and calls
  `GlyphAtlas::ensure()` for each. ASCII still uses the fixed startup region.
- **Texture refresh on dirty atlas** — if `take_dirty()` reports inserted glyphs
  or atlas growth, the renderer recreates and re-uploads the R8 atlas texture
  and bind group once for that rebuild, then builds vertices against the current
  atlas dimensions.

### Verified

- Added a headless native test proving snapshot scanning populates a dynamic
  non-ASCII slot once and does not dirty the atlas again for resident glyphs.
- `cargo test --lib` passes in the shared tree (`257` passed, `1` ignored;
  includes OPUS's in-flight core search tests).
- `cargo fmt --check` passes for the whole repository.
- Native autoclose smoke exits 0 at the default font size and with
  `ODYTTY_FONT_SIZE=18`.
- A live native PTY smoke using a temporary shell that prints `é ─ Ω 世` exits 0,
  exercising the non-ASCII atlas path in the window loop.

### Known gaps

- Complex shaping remains out of scope: combining-mark composition, ligatures,
  stylistic sets, emoji policy, and font fallback are later text-quality work.

---

## 2026-06-10 — Scrollback search engine

Stage 4 search begins with a pure, rendering-free core engine that finds literal
queries across the combined scrollback + visible buffer and reports matches as
absolute cell ranges a front end can later highlight and jump to.

### What landed

- **New `src/core/search.rs` module** (with sibling `src/core/search_tests.rs`
  per the modularity directive), re-exported from `core/mod.rs`.
- **`search_rows(rows, query, options)`** returns every non-overlapping match in
  reading order as an inclusive absolute-cell range (`AbsolutePoint { row,
  column }`), using the same absolute-row convention as selection (row 0 =
  oldest scrollback).
- **Case-sensitive and case-insensitive** modes (per-`char` simple lowercase
  fold, kept 1:1 so column mapping stays exact).
- **Correctness across hard cases** — a match covering a wide glyph spans both
  columns; combining marks fold into the base cell's grapheme; matches spanning
  soft-wrapped rows report `start`/`end` on different absolute rows, while hard
  line breaks never join.
- **`find_next`/`find_prev`** walk matches from an absolute position with
  wraparound. Trailing blank padding is trimmed; interior blanks preserved.
- **Bridge** — `Screen::search` / `Terminal::search` assemble `scrollback ++
  rows` and call the engine. No native/text/atlas edits.

### Verified

- 23 deterministic fixtures cover each behavior. 256 lib + 2 integration + 10
  smoke tests pass (234 lib baseline + 23 new). `cargo fmt` clean; clippy clean
  except the pre-existing core derive lint. All core files remain under ~2000
  lines (search.rs 261, search_tests.rs 285).

### Documented limitations

- Per-`char` case fold (no `ß`→`ss`); no Unicode normalization (precomposed vs
  decomposed are distinct); non-overlapping greedy matching; wide pairs never
  straddle a wrap boundary. Native search UI (overlay, highlight, jump) is a
  later packet.

---

## 2026-06-10 — Native modularity split

The native module has been mechanically split from one large `src/native.rs`
file into focused sibling modules under `src/native/`, with the public
`odytty::native::{NativeOptions, run_native}` entry point preserved.

### What landed

- **`src/native/mod.rs`** now owns the public native entry point and wires the
  submodules together.
- **Focused native modules** separate the event-loop app handler, GPU state,
  clipboard/paste helpers, key/mouse/focus bindings, options/errors, PTY pump,
  viewport helpers, and native tests.
- **Extracted tests** moved from the old inline module into
  `src/native/tests.rs` with explicit imports, keeping the same test coverage
  while reducing the runtime module surface.

### Verified

- `cargo fmt --check`
- `cargo test` (`234` lib tests passed, `1` ignored; integration smoke tests
  passed with no test-count change)
- `WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`

All resulting native source files are below the ~2000-line modularity limit.

---

## 2026-06-10 — Core split: cohesive submodules under src/core/

Stage 1.5 modularity continues. The 4284-line `src/core/mod.rs` was split into
focused submodules — a pure mechanical reorganization with no behavior or API
changes. Every move is verbatim and the full public surface is re-exported from
`mod.rs`, so all call sites (`native`, `grid.rs`, lib re-exports) compile
unchanged.

### What landed

- **`src/core/types.rs`** — data types: geometry, color, attributes, mouse
  enums, `Cell`, `Snapshot`, `DirtyRegion`, `TerminalModel`.
- **`src/core/screen.rs`** — `Line`, `Screen`, `Terminal`: the grid buffer,
  resize/reflow, and parser dispatch.
- **`src/core/encoding.rs`** — pure mouse and focus event encoders.
- **`src/core/tests.rs`** + **`src/core/encoding_tests.rs`** — the 2186-line test
  module split into Terminal/Screen-driven tests and encoder tests.
- **`src/core/mod.rs`** — module declarations and `pub use` re-exports.
- Two crate-internal visibility tweaks (`MAX_COMBINING`, `Cell::push_combining`
  -> `pub(crate)`); no public API widened. All resulting files are under ~2000
  lines.

### Verified

- 234 lib + 2 integration + 10 smoke tests — exactly the baseline, zero
  test-count change. `cargo fmt` clean; clippy clean except the pre-existing core
  derive lint (relocated to `types.rs`). Native Wayland autoclose smoke exits 0.
  Verbatim-move check: only rustfmt reflow and one `MouseTracking` import line
  differ from the original; zero logic changes.

---

## 2026-06-10 — Glyph atlas management: fallback box and dynamic cache

Stage 3 high-quality-text work begins with the glyph atlas. The build-once
ASCII grid grew into a managed atlas with a missing-glyph fallback, an
on-demand dynamic region, and size-change invalidation. The atlas was also
extracted from `text.rs` into its own `src/atlas.rs` module.

### What landed

- **New `src/atlas.rs` module** — the `GlyphAtlas`/`CellSize` types moved out of
  `text.rs`, which now keeps only font loading and color resolution and
  re-exports the atlas types so `native.rs`/`grid.rs` compile unchanged.
- **Missing-glyph fallback** — slot 0 is a synthesized hollow box drawn
  font-independently. `uv_rect()` resolves any unsupported *printable* codepoint
  to it, so `é`, box-drawing, CJK, and emoji now render a visible box instead of
  a blank cell. Spaces and control characters still draw nothing, and
  wide-continuation spacer cells still emit no quad (no double-draw).
- **Dynamic region with growth** — `ensure()` rasterizes a real non-ASCII glyph
  into the next free slot, appending pages of rows when the region fills. There
  is no eviction and existing slots never move, so UV rects handed out before a
  growth stay valid. A hard slot cap bounds worst-case growth; beyond it new
  glyphs degrade to the fallback box.
- **Invalidation** — `build()` always returns a pristine single-size atlas, so a
  font-size or future font-family change is a full rebuild with no mixed-size
  glyphs. A `revision()` counter and `take_dirty()` flag mark when the texture
  needs re-uploading.

### Verified

- Seven headless atlas tests (fallback visible-but-hollow, fallback selection,
  `ensure` insert/cache/dirty, missing-glyph uses fallback without a slot,
  growth preserves existing glyphs, rebuild invalidation) plus grid tests for
  the fallback glyph quad and the wide-spacer no-double-draw rule.
- Full suite green; formatting clean; native autoclose smoke exits 0 at the
  default font size and at `ODYTTY_FONT_SIZE=18`.

### Known gaps

- The live render path uses the immutable `uv_rect()` (ASCII plus fallback
  boxes from the startup texture). Wiring `ensure()` per non-ASCII cell and
  re-uploading the texture on `take_dirty()` — the path that makes real
  non-ASCII glyphs appear on screen — is a later native packet.
- Rasterization quality (gamma-correct coverage blending, tall-glyph cell-clip,
  `ascent.round()` baseline, no sub-pixel) is unchanged here and is the basis
  for a later Stage 3 rasterization packet.

---

## 2026-06-10 — Selection refinement and scrollback-aware ranges

Stage 4 daily-driver interaction now has richer native selection behavior:
double-click word selection, triple-click line selection, drag-edge viewport
scrolling, and selection anchors stored against absolute scrollback rows.

### What landed

- **Click selection** — same-cell clicks within 500 ms are counted. Single click
  starts the normal drag selection, double-click selects the word under the
  pointer, and triple-click selects the full line.
- **Word boundary policy** — word selection includes alphanumeric characters
  plus `_`, `.`, `/`, `-`, and `~`, matching common shell/path fragments such as
  `./src/foo-bar~`.
- **Scrollback-aware ranges** — native selection anchors are stored as absolute
  rows in the current scrollback+screen space, then projected into the current
  viewport for highlight/copy. Moving the viewport no longer discards an
  existing selection; resize/reflow still clears it because row identity changes.
- **Drag autoscroll** — dragging in the top or bottom cell-height band scrolls
  the viewport at a bounded 80 ms cadence while preserving the selection
  anchor/focus in absolute rows.

### Verified

- Headless native tests cover word-boundary detection, click-count reset rules,
  absolute-row projection, visible-to-absolute conversion, and drag autoscroll
  edge bands. Full verification is recorded with the local S3 commit.

---

## 2026-06-10 — Native hover motion and focus reporting wiring

The native front end now consumes the C3 core mouse/focus additions. Any-event
mouse tracking sends true no-button hover motion, and windows emit focus-in/out
reports to PTY apps only when DECSET 1004 has enabled them.

### What landed

- **Any-event hover** — native pointer motion with no held mouse button now uses
  `MouseButton::NoButton` instead of the N1 placeholder left-button report when
  tracking mode 1003 is active. Button-held motion still reports the held
  button, and non-any-event modes do not emit no-button hover.
- **Focus reporting** — `WindowEvent::Focused(true/false)` is translated through
  `encode_focus_event(terminal.focus_reporting(), focused)` and written to the
  PTY. The core encoder gates output, so focus changes are silent unless the app
  requested mode 1004.
- **Tests** — native unit seams cover no-button hover fallback selection and
  focus-report gating/direction through the terminal state.

### Verified

- Targeted native tests pass; full verification is recorded with the local N2
  commit.

---

## 2026-06-10 — Any-event hover motion and focus reporting

Stage 2 mouse hardening. The core mouse encoder now produces correct no-button
hover reports for any-event tracking (1003), and the model tracks focus
reporting (1004) with pure focus-event encoders. Native wiring (emitting hover
and focus events) is a follow-up; this is the model/encoder layer only.

### What landed

- **No-button hover motion** — `MouseButton` gains a `NoButton` variant
  (encoded with xterm's "no button" base code 3). `encode_mouse_event` emits
  hover motion across all encodings: legacy/UTF-8 `Cb = 3 + 32` (+32 offset),
  SGR `CSI < 35 ; x ; y M`, urxvt `CSI 67 ; x ; y M`.
- **Tracking gate** — any-event (1003) passes no-button hover; button-event
  (1002) drops it while still reporting button-held drags. This lets the native
  layer replace its placeholder `Left`-button hover with a true `NoButton`.
- **Focus reporting (1004)** — DECSET/DECRST 1004 toggles a `focus_reporting`
  flag exposed via `Terminal::focus_reporting()`. The pure
  `encode_focus_event(reporting, focused)` returns `ESC [ I` on focus-in and
  `ESC [ O` on focus-out, or `None` when reporting is off. RIS resets the flag.

### Verified

- 8 new fixtures: hover encoding in legacy/SGR/urxvt/UTF-8, the 1002-vs-1003
  gate, focus set/reset, RIS reset, and the gated directional focus encoder.
  Full suite: 220 lib + 10 smoke pass; fmt and clippy clean (except the
  pre-existing `Color` derive note).

### Remaining

- Native emit of hover/focus events (swap the placeholder hover button, send
  focus reports on window focus changes) is a native-layer follow-up.

---

## 2026-06-10 — PTY-backed alternate-screen smoke coverage

Stage 2 evidence coverage now includes real editor/pager binaries running
through a PTY and rendering into the owned terminal model. The tests focus on
alternate-screen enter/exit behavior and primary-screen restoration without
editing terminal-core semantics.

### What landed

- **`tests/pty_alt_screen_smoke.rs`** — new default-running integration smoke
  harness for real PTY programs. It seeds the primary screen, spawns a bounded
  PTY command, feeds output into `Terminal`, and writes
  `Terminal::take_host_output()` replies back to the PTY so full-screen apps can
  answer terminal queries.
- **`less` smoke** — opens a generated fixed file, verifies alternate-screen
  content hides the seeded primary screen, scrolls down/up, quits, and asserts
  the primary marker returns with no pager content leaking.
- **`vim` smoke** — launches `vim` with `-u NONE -U NONE -i NONE -n
  --noplugin`, opens a generated fixed file, enters insert mode, types through
  the PTY, quits without saving, asserts the primary marker returns, and checks
  the file stayed unchanged.
- **Hermetic behavior** — tests return early with a notice when `less` or `vim`
  is absent, pin `TERM`/`LANG`/`LC_ALL`, use generated temp files, and poll for
  expected screen state with deadlines instead of sleeping.

### Remaining

- `man` is not included yet; host manpage availability and pager configuration
  add more nondeterminism than this default smoke packet should carry.

### Verified

- Targeted smoke: `cargo test --test pty_alt_screen_smoke` passes in about a
  tenth of a second on the current host with both `less` and `vim` present.

---

## 2026-06-10 — Combining marks attach to the preceding cell

Stage 2 Unicode hardening, second half. Zero-width combining marks now attach to
the base cell the cursor last advanced past instead of being discarded, so the
model carries the full grapheme cluster for a future renderer and for copy/text
queries. Completes the C2 Unicode-width packet (wide-cell coherence landed in the
previous commit).

### What landed

- **`Cell` grapheme storage** — `Cell` keeps a small inline combining buffer
  (`MAX_COMBINING = 2`) and stays `Copy`, so marks travel with the cell through
  scroll, insert/delete, erase, and resize-reflow for free. `ch` remains the
  renderer-facing base char; new `Cell::combining()` and `Cell::grapheme()`
  expose attached marks. Construction moved to `Cell::new`/`Cell::wide_spacer`.
- **Attachment rule** — a width-0 mark appends to the cell left of the cursor,
  stepping back over a wide continuation spacer to reach its lead, and honoring
  pending-wrap so a mark after a last-column char lands on that char (no
  premature wrap). A mark at line start, or after capacity is reached, is a
  safe no-op — never panics.
- **`plain_text`** now emits full grapheme clusters (base + marks).
- **Bounded limitation** — more than two combining marks on one base are
  dropped; ambiguous-width remains narrow (a future setting, not built).

### Verified

- 6 new fixtures: attach-to-base, attach-to-wide-lead (not spacer),
  line-start no-op, capacity clamp, overwrite clears marks, and pending-wrap
  attach. Full suite: 212 lib + 10 smoke pass; fmt and clippy clean (except the
  pre-existing `Color` derive note); native autoclose smoke exit 0. The only
  native touch was migrating one `#[cfg(test)]` snapshot helper to `Cell::new`.

---

## 2026-06-10 — Native title and mouse reporting wiring

The native front end now consumes the C1 core title/mouse groundwork. Window
titles set by shells or editors are applied to the `winit` window, and
mouse-aware TUIs can receive pointer reports through the PTY.

### What landed

- **Window title** — the native redraw path polls
  `Terminal::take_title_changed()` and applies `Terminal::title()` to the OS
  window. The default title stays `OdyTTY` until a title is set; an explicit
  empty title remains valid.
- **Mouse reporting** — native pointer press/release/motion/wheel events are
  translated from window pixels to 1-based terminal cells and passed through
  `Terminal::mouse_protocol()` plus `encode_mouse_event(...)` before writing to
  the PTY.
- **Interaction policy** — when mouse tracking is active, pointer events go to
  the host app and local selection is suppressed. Holding Shift forces local
  selection/scrollback behavior, matching common xterm-family convention. When
  tracking is off, existing selection and scrollback behavior stays unchanged.
- **Tests** — native unit seams cover title polling, one-based mouse
  coordinates, modifier mapping, button mapping, and wheel-button translation.

### Remaining

- Manual validation in a mouse-aware TUI is still needed to confirm behavior
  against a real full-screen app.
- Any-event hover reporting is limited by the current core button-only encoder;
  a no-button motion representation can be a follow-up if real TUIs require it.

---

## 2026-06-10 — Wide-cell write/erase coherence

Stage 2 Unicode hardening, first half: keep wide-character cell pairs coherent
under overwrites, end-of-line wrapping, and erases. A wide glyph (East Asian
Wide/Fullwidth, many emoji) occupies a printable lead cell plus a
`wide_continuation` spacer; the model now guarantees no half-wide orphan ever
survives an edit. Combining-mark attachment is the second half and lands in a
follow-up packet (it needs a new `Cell` field, deferred to avoid colliding with
concurrent native-layer work in the shared tree).

### What landed

- **Overwrite-half clears the pair** — `print_char` calls a new O(1)
  `clear_wide_orphans` before writing: overwriting a wide lead clears its
  continuation, overwriting a continuation clears its lead, and a new wide glyph
  that straddles two existing pairs clears both dangling halves.
- **No split across rows** — a wide glyph that does not fit in the last column
  blanks the trailing cell and soft-wraps whole onto the next row (xterm
  behavior), so resize can still rejoin the logical line.
- **Erase coherence** — `erase_line_from_cursor`/`erase_line_to_cursor` now
  sanitize wide pairs at the erase boundary; ICH/DCH/ECH already repaired pairs
  via `sanitize_wide_row`. Cursor movement counts cells, not graphemes.
- **Ambiguous width** stays narrow by default (a future setting, not built yet).

### Verified

- 7 new deterministic fixtures: overwrite-lead, overwrite-continuation,
  straddle-two-pairs, wrap-at-boundary, erase-from/to-cursor orphan clears, and
  alternate-screen coherence. Full suite: 206 lib + 10 smoke pass; fmt and
  clippy clean (except the pre-existing `Color` derive note); native autoclose
  smoke exit 0.

### Remaining

- Combining marks (zero-width, attach to the preceding cell's grapheme) land in
  the follow-up C2b packet, sequenced after the native title/mouse wiring so the
  `Cell` representation change does not break concurrent native edits.

---

## 2026-06-10 — Core OSC title and mouse reporting state

Stage 2 correctness work added the terminal-core side of window-title reporting
and mouse tracking. This is the model and encoder layer only; wiring the native
front end to emit mouse reports and apply the title is a later packet.

### What landed

- **OSC title (OSC 0/2)** — `osc_dispatch` now stores the window title; OSC 1
  (icon name) is consumed without changing the title. `Terminal::title()` reads
  the current title and `take_title_changed()` polls-and-clears a dirty flag.
  An explicitly empty title is `Some("")`, distinct from never-set `None`;
  embedded semicolons are preserved and invalid UTF-8 is replaced (no panic).
- **Unknown OSC safety** — OSC 4/7/8/10/11/12/52/133 and friends are consumed
  rather than printed, so payloads never leak into the grid.
- **Mouse modes via DECSET/DECRST** — 9/1000/1002/1003 select a single
  `MouseTracking` mode and 1005/1006/1015 select a single `MouseEncoding`, using
  xterm shared-variable semantics (later DECSET wins; any tracking DECRST clears
  tracking; any encoding DECRST resets to default). `Terminal::mouse_protocol()`
  exposes the active mode/encoding.
- **Pure encoders** — `encode_mouse_event(...)` produces exact report bytes for
  legacy (with the 223 coordinate cap), UTF-8, SGR, and urxvt encodings, gated
  by the active tracking mode.
- **RIS** resets mouse state; the title persists across RIS.

### Verified

- 28 new deterministic tests cover title set/empty/UTF-8, OSC payload
  containment, mode selection precedence, DECRST clearing, and every encoder
  path. Full suite: 195 lib + 10 smoke pass; fmt and clippy clean (except the
  pre-existing `Color` derive note).

### Remaining

- Native front end does not yet emit mouse reports or apply the OSC title to the
  window; that wiring is the next native packet.

---

## 2026-06-10 — Settings path and native font-size knob

Stage 1 stabilization now has a minimal settings module that loads prototype
runtime knobs once at native startup. The native renderer can be launched with a
configured font size without editing source code.

### What landed

- **`src/settings.rs`** — added typed `Settings` loaded from environment
  variables. It currently covers `ODYTTY_THEME`, `ODYTTY_VISUAL`, `ODYTTY_FONT`,
  `ODYTTY_FONT_SIZE`, and `ODYTTY_NATIVE_AUTOCLOSE_MS`.
- **`ODYTTY_FONT_SIZE`** — new logical-pixel font-size knob. The default remains
  `14.0`; valid values are clamped to `6.0..=72.0`; invalid values fall back to
  the default with one stderr warning.
- **`src/native.rs` / `src/text.rs` / `src/theme.rs`** — migrated native runtime
  knobs through `Settings` instead of scattered environment reads. Font size now
  flows into glyph atlas rasterization, cell metrics, initial window sizing, and
  resize grid fitting.
- **`docs/runtime-knobs.md`** — documented current prototype settings and launch
  examples.

### Verified

- Focused settings and native option tests cover default, valid override,
  invalid fallback, empty fallback, and clamp behavior.

### Remaining stabilization work

- The settings source is still environment variables, not a config file or UI.
- Font family/path remains a file-path override; configurable font family is
  still deferred until the settings path is more stable.

---

## 2026-06-09 — First meaningful prototype reached

OdyTTY now has the first meaningful prototype slice: a native Wayland window
opens a real shell, renders readable GPU text, handles resize, keyboard input,
paste, selection/copy, scrollback navigation, cursor rendering, and enough
terminal compatibility for the validated daily loop.

### What is validated

- Real shell startup and prompt rendering in the native window.
- Common command output including `ls --color` and `clear`.
- Pager/editor basics: `less` enter/exit and `nano` launch.
- Resize preserves content through shrink/grow reflow.
- Native paste respects bracketed paste mode, and selection copy exports plain
  text through the Wayland clipboard path.
- Fish completion redraws correctly after DSR replies and default-one CSI count
  handling for bare cursor moves.
- `ODYTTY_VISUAL=ambient` provides a visible, subtle scanline treatment while
  `off`/unset keeps the baseline renderer.

### Verified

- `cargo test`: **167 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0`, with no lingering `odytty` process.
- Operator manual validation on Hyprland covered prompt display, color output,
  `clear`, resize, copy/paste, scrollback, fish completion, pager/editor basics,
  and the ambient visual layer.

### Deferred / risks

- No font-size configuration yet; the prototype uses the fixed native default.
- No settings file or UI; prototype knobs are environment variables.
- Selection is basic and visible-grid oriented; no advanced selection model.
- Tabs, panes, profiles, shell integration, and broad cross-platform support are
  deferred until after this prototype.
- OdyTTY is still a prototype, not a daily-driver terminal claim.

---

## 2026-06-09 — Fish completion DSR replies and visible ambient pass

Manual validation found two remaining prototype issues: fish tab completion
could desync its completion pager, and `ODYTTY_VISUAL=ambient` was too subtle to
evaluate. Both now have scoped fixes awaiting final real-compositor retest.

### What landed

- **`src/core/mod.rs`** — OdyTTY now answers DSR status (`CSI 5 n`) and cursor
  position (`CSI 6 n`) reports through the existing host-output path. The
  cursor-position reply is 1-based and honors DECOM/origin-mode scroll regions,
  matching the row semantics used by cursor movement. Fish uses this handshake
  while drawing completions and multi-line prompts.
- **`src/core/mod.rs`** — count/position CSI controls now treat omitted or zero
  parameters as one where ECMA-48 expects a default count. This fixes bare
  relative cursor moves such as `ESC [ A`, which fish uses to return from its
  completion candidate row to the command line before clearing the old pager.
- **`src/theme.rs`** — the ambient scanline treatment was retuned from an
  extremely fine, low-contrast pattern to a still-subtle but visible background
  wash. The off path remains an exact zero-strength no-op.

### Verified

- `cargo test`: **167 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with the visual unset and with
  `ODYTTY_VISUAL=ambient`, with no lingering `odytty` process.
- A distilled fish completion redraw regression now verifies that narrowing the
  prefix clears stale candidate rows.

### Known gaps

- The operator should re-run the exact fish completion case:
  `less b<Tab>`, continue typing a prefix, and confirm the candidate list and
  command line refresh normally.
- The operator should compare `ODYTTY_VISUAL=off` and
  `ODYTTY_VISUAL=ambient` and confirm the effect is visible without hurting
  readability.

---

## 2026-06-09 — Wayland clipboard export

Manual validation showed OdyTTY copy/paste worked inside OdyTTY but did not
export selected text reliably to other Wayland apps. The native clipboard path
now enables `arboard`'s Wayland data-control backend while keeping the
persistent clipboard owner added earlier.

### What landed

- **`Cargo.toml` / `Cargo.lock`** — enabled `arboard`'s
  `wayland-data-control` feature so Hyprland/Wayland sessions can publish
  clipboard text through the Wayland clipboard backend instead of only the X11
  fallback path.
- **`src/native.rs`** — kept copy as a plain text-only payload and added focused
  tests around the selected-text helper to guard against empty selections and
  non-text copy-path regressions.

### Verified

- `cargo test`: **159 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with no lingering `odytty` process.

### Known gaps

- This still needs the operator's real-compositor retest: select text in
  OdyTTY, press `Ctrl+Shift+C`, and paste into an external Wayland app.

---

## 2026-06-09 — Manual validation fixes: clipboard and resize reflow

Manual native validation exposed two first-prototype blockers: Linux clipboard
ownership was unreliable after copy, and narrowing then widening the window
could permanently lose text. Both are now fixed in scoped packets.

### What landed

- **`src/native.rs`** — native copy/paste now keeps a clipboard owner alive for
  the app lifetime instead of creating and dropping an `arboard::Clipboard`
  immediately after `set_text`. Clipboard failures stay non-fatal and now emit
  concise diagnostics.
- **`src/core/mod.rs`** — resize now reflows primary-screen content instead of
  truncating rows. Soft-wrap markers let wrapped physical rows rejoin into
  logical lines across scrollback + visible rows and re-wrap to the new width.
- Alternate-screen resize remains isolated: TUI apps keep their app-managed
  alternate grid and repaint on resize, while the stored primary screen behind
  it is reflowed coherently.

### Verified

- `cargo test`: **157 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with the plain renderer and with
  `ODYTTY_VISUAL=ambient`, with no lingering `odytty` process.

### Known gaps

- The clipboard fix still needs the operator's real-compositor retest:
  select text, `Ctrl+Shift+C`, paste into another app, then paste external text
  back into OdyTTY with `Ctrl+Shift+V`.
- Resize reflow is intentionally bounded for the first prototype. It preserves
  normal wrapped text, hard line breaks, scrollback round trips, cursor mapping,
  and alternate-screen isolation, but complex wide-glyph edge cases remain
  conservative.

---

## 2026-06-09 — Optional ambient visual treatment

OdyTTY now has a small disableable Odyssey visual treatment. It is deliberately
presentation-only: the terminal core, PTY path, input mapping, selection state,
and stored cell attributes do not know it exists.

### What landed

- **`src/theme.rs`** — added `VisualEffect`, selected by `ODYTTY_VISUAL`.
  `off`, `none`, and `plain` disable the treatment; `ambient` and `scanlines`
  enable it. Unset, empty, or invalid values fall back to off.
- **`src/shaders/cell.wgsl`** — added a faint static scanline wash over cell
  backgrounds only. Glyph fragments bypass the effect so text coverage remains
  full contrast.
- **`src/native.rs`** — packs visual-effect parameters into the existing
  viewport uniform slot and exposes an off path with zero strength.

### Verified

- `cargo test`: **149 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with the visual unset, with
  `ODYTTY_VISUAL=ambient`, and with invalid visual fallback, with no lingering
  `odytty` process.

### Known gaps

- The effect has not had a human readability pass in a real interactive shell.
- The current treatment is static and intentionally subtle; no motion or richer
  effect stack exists yet.

---

## 2026-06-09 — Theme system and daily-loop smoke fixtures

OdyTTY now has the first Odyssey presentation hook: a small theme system that
can change default rendering colors without changing terminal semantics. The
daily-loop smoke suite also gained deterministic coverage for prompt/command
output and clear-style Background-Color Erase behavior.

### What landed

- **`src/theme.rs`** — added a source-agnostic `Theme` model with a plain
  baseline plus `odyssey` and `odyssey-noir` presets. `ODYTTY_THEME` selects a
  preset and falls back to plain when unset, empty, or invalid.
- **Presentation-only wiring** — the native renderer now uses the active
  theme's clear color, and `Color::Default` foreground/background resolution is
  overridden at native startup. The terminal core and stored cell attributes
  remain theme-unaware.
- **`tests/transcript_smoke.rs`** — added smoke fixtures for a prompt →
  command → colored output → prompt loop and for clearing while an active
  background color is set.

### Verified

- `cargo test`: **141 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` for the plain default,
  `ODYTTY_THEME=odyssey`, and invalid-theme fallback, with no lingering
  `odytty` process.

### Known gaps

- This is a color theme system only, not the optional Odyssey visual treatment.
- Real interactive validation still needs a human at the Hyprland display for
  prompt responsiveness, external commands, clipboard behavior, and resizing.

---

## 2026-06-09 — Native scrollback navigation

The native window can now navigate scrollback instead of only showing the live
bottom. This wires the earlier core `snapshot_with_scrollback` API into the GPU
render path while preserving normal terminal input behavior.

### What landed

- **`src/native.rs`** — added a clamped native viewport offset. Mouse wheel
  scrolls by rows, and `Shift+PageUp` / `Shift+PageDown` page through history.
  Plain `PageUp` / `PageDown` still go to the PTY.
- Rendering now rebuilds from `Terminal::snapshot_with_scrollback(offset)`.
  Offset `0` is live; nonzero offsets use the core policy that hides the cursor.
- New PTY output keeps a scrolled-back viewport anchored to the same absolute
  rows. Any typed key or paste that writes to the PTY returns to live. Selection
  is cleared when the viewport changes.

### Verified

- `cargo test`: **134 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- No scrollbar, viewport indicator, top/bottom hotkeys, or scrollback selection.
- Scrollback storage is still unbounded.

---

## 2026-06-09 — Native mouse selection and copy

The native window now supports basic visible-grid text selection and copying.
This is intentionally simple: selection is native UI state over the current
snapshot, with no terminal-core mutation and no scrollback selection yet.

### What landed

- **`src/selection.rs`** — added source-agnostic helpers for mapping physical
  pointer coordinates to terminal cells, normalizing row-major ranges,
  extracting row-spanning selected text, and applying inverse-cell highlight to
  a snapshot copy.
- **`src/native.rs`** — left mouse drag tracks a visible-grid selection using
  the active glyph atlas cell size and current grid dimensions. Redraw applies
  highlight to a snapshot copy before building vertices.
- **`Ctrl+Shift+C` copy** — copies the current visible selection to the system
  clipboard with `arboard`, quietly ignoring clipboard failures. Plain `Ctrl-C`
  remains shell input.

### Verified

- `cargo test`: **124 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Selection is visible-grid only; no scrollback selection, word selection, or
  primary-selection integration.
- Copy is `Ctrl+Shift+C` only.

---

## 2026-06-09 — Scrollback viewport snapshots

The core can now produce snapshots for historical scrollback viewports without
changing the live rendering path. This gives the native UI a clean model API for
future scrollback navigation while keeping terminal semantics and rendering
separate.

### What landed

- **`src/core/mod.rs`** — added `Screen::snapshot_with_scrollback(offset_rows)`
  and `Terminal::snapshot_with_scrollback(offset_rows)`. Offset `0` returns the
  same live snapshot as `snapshot()`. Positive offsets page upward into
  scrollback and clamp at the oldest available history.
- Snapshot rows are composed from `scrollback` plus live rows and normalized to
  the active grid width, so callers still receive the existing `Snapshot` shape.
- Cursor policy is explicit: live offset preserves cursor state, while any
  historical offset hides the cursor. Alternate-screen snapshots stay isolated
  from primary-screen scrollback.

### Verified

- `cargo test`: **118 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Native scrollback navigation is not wired yet; this packet only adds the core
  snapshot API needed to implement it cleanly.

---

## 2026-06-09 — Native bracketed paste

The native window can now paste text into the PTY with `Ctrl+Shift+V`. Paste
uses the same source-agnostic encoding policy as the headless crossterm path, so
bracketed paste behavior stays consistent across front ends.

### What landed

- **`src/input.rs`** — paste encoding moved into shared helpers:
  `encode_paste(text, bracketed_paste)` and `sanitize_paste`. Bracketed mode
  wraps pasted bytes with `ESC[200~` / `ESC[201~` and strips embedded end
  markers so clipboard text cannot break out of the paste guard early.
- **`src/app.rs`** — headless/crossterm paste now uses the shared encoder.
- **`src/native.rs`** — `Ctrl+Shift+V` reads text from the platform clipboard
  with `arboard`, reads bracketed-paste state under the terminal lock, drops
  that lock, then writes and flushes encoded paste bytes to the PTY writer.
  Clipboard access failures are quiet and non-fatal.

### Verified

- `cargo test`: **113 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Native paste is currently `Ctrl+Shift+V` only; no menu or compositor paste
  event path is wired yet.
- Selection/copy and scrollback navigation are still open Daily Loop items.

---

## 2026-06-09 — SU/SD scrolling and DECOM origin mode

The owned terminal core now covers the next bounded compatibility packet needed
for common shell and TUI behavior: scroll-up/down region commands and origin
mode addressing. This keeps compatibility work evidence-driven while leaving the
renderer and native event loop untouched.

### What landed

- **`src/core/mod.rs`** — `CSI Ps S` (SU) and `CSI Ps T` (SD) scroll the active
  region up or down by a count, clamp to the region height, fill with
  BCE-aware blank rows, and never add lines to scrollback.
- **DECOM origin mode** (`CSI ? 6 h/l`) — when enabled, CUP/HVP/VPA row
  addressing is relative to the active scroll-region top and clamps to the
  region bottom. Disabling DECOM returns addressing to full-screen absolute
  behavior and homes the cursor to the screen origin.
- Origin mode is saved/restored across the alternate screen and cleared by RIS
  and DECSTR. DECSTBM now homes consistently with the active origin mode.

### Verified

- `cargo test`: **109 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- DECOM is vertical-origin only; horizontal margins/DECLRMM remain out of scope
  for the first prototype.
- No new transcript smoke fixture was added because the behavior is covered by
  targeted deterministic core tests.

---

## 2026-06-09 — Native resize reflows PTY and model

The native window resize path now updates the actual terminal size, not only the
GPU surface. Resizing the window recomputes the whole-cell grid from the
rasterized glyph cell metrics, resizes the owned terminal model, and sends the
new size to the PTY so shells and TUIs receive updated `$COLUMNS`/`$LINES`.

### What landed

- **`src/native.rs`** — `WindowEvent::Resized` still reconfigures the `wgpu`
  surface, then derives the terminal grid from the atlas cell dimensions used by
  grid rendering. Partial trailing pixels are ignored with floor division, and
  dimensions clamp to at least `1x1`.
- The PTY session is now shared with the app behind `Arc<Mutex<_>>` so resize
  events can call `PtySession::resize` while shutdown still kills and reaps the
  child shell deterministically.
- Resize work is idempotent: duplicate events or sub-cell pixel changes that do
  not alter the whole-cell grid skip model and PTY resize.

### Verified

- `cargo test`: **96 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Resize uses the existing model resize behavior; scrollback-aware reflow of
  already-wrapped lines is still deferred.
- Paste, selection/copy, and scrollback navigation remain the next daily-loop
  gaps.

---

## 2026-06-09 — Cursor rendering and BCE fills

The native renderer now draws the terminal cursor, and the owned terminal model
implements xterm-style Background-Color Erase for common blank-fill paths. The
prototype is closer to a useful daily loop: shell output is readable, keyboard
input reaches the PTY, the cursor is visible in the GPU window, and colored
erase/scroll fills preserve the active SGR background.

### What landed

- **`src/grid.rs`** — `build_vertices` appends a block cursor from
  `Snapshot.cursor` when `cursor_visible` is true. The cursor is drawn as an
  inverse block: the cell foreground becomes the cursor block color, and any
  glyph under the cursor is redrawn in the cell background color. The cursor
  position is clamped to the snapshot dimensions so stale positions cannot index
  outside the grid.
- **`src/core/mod.rs`** — erase and fill operations now preserve the active
  background color while resetting other attributes. Covered paths include
  ED/EL/ECH, full-screen and scroll-region scroll-in rows, RI, IL/DL, and
  ICH/DCH fill cells.

### Verified

- `cargo test`: **91 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Resize reflow of both the PTY and terminal model is still next.
- Cursor rendering reflects the live snapshot only; scrollback viewport offsets
  remain deferred until scrollback navigation lands.

---

## 2026-06-09 — Keyboard input + shared key encoder

The native window is now **interactive**: `cargo run -- --native` opens a real
shell you can type into. `ls`, `echo hi`, line editing with Backspace and
arrows, `Ctrl-C` to interrupt, and `Ctrl-D` at an empty prompt (which exits the
shell and closes the window) all work. This completes the read+write loop on top
of the PTY writer plumbed last packet.

### What landed

- **`src/input.rs`** (new) — a source-agnostic key encoder that is the **single
  source of truth** for the byte sequences sent to the PTY:
  - `enum Key` (Char + named keys), `struct Modifiers { ctrl, alt, shift }`,
    `fn encode_key(Key, Modifiers) -> Vec<u8>`, and `fn ctrl_char`.
  - No windowing, GPU, or crossterm dependency — both front ends depend on it
    without depending on each other, so the escape table lives in exactly one
    place and cannot drift.
  - `\r` for Enter, `0x7f` Backspace, `ESC[A..D` arrows, control bytes for
    Ctrl-letter, `ESC` prefix for Alt. Empty result = "ignore".
- **`src/app.rs`** — refactored to map crossterm `KeyEvent` → neutral
  `Key`/`Modifiers` (via a new `map_keycode`) and defer byte production to
  `input::encode_key`. The `ctrl_char` table moved into `input`. The Ctrl-Q quit
  affordance stays in `app.rs` (it's a debug-mode concern, not a real terminal
  byte). Both existing key tests pass **unchanged**.
- **`src/native.rs`** — winit keyboard wired to the PTY:
  - `WindowEvent::ModifiersChanged` caches Ctrl/Alt/Shift; `KeyboardInput`
    (Pressed only; repeats kept for autorepeat) maps the winit `logical_key`
    (`Character` / `Named`) to the neutral `Key` via `map_named_key`, encodes,
    and writes+flushes to the shared PTY writer.
  - `map_named_key` resolves Shift-Tab → BackTab and maps Space to `Char(' ')`
    so Ctrl-Space encodes to NUL through the shared encoder.
  - The writer (previously held unused for "next packet") is now the live input
    sink; docs updated to drop the stale "keyboard input absent" notes.

### Verified

- `cargo test`: **81 lib + 8 smoke** green (1 ignored live-PTY each). New: 7
  `input::encode_key` unit tests (printable, Enter/Backspace, arrows, Ctrl-C/D,
  Ctrl-with-no-mapping, Alt-prefix, Ctrl punctuation) + 2 native `map_named_key`
  tests (Shift-Tab, Space→NUL-under-Ctrl). The two existing `app.rs` key tests
  still pass with identical assertions.
- `cargo fmt --check` clean. `cargo clippy` clean for touched files (only the
  pre-existing `core/mod.rs` derive note remains).
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS` …) exits `0`,
  no validation errors, no zombies/lingering processes.

### Known gaps (unchanged this packet)

- Window-resize reflow of the PTY/model is still deferred (viewport-only).
- No paste/bracketed-paste, mouse selection, or scrollback navigation yet —
  those are the next Daily-Loop plan items.

---

## 2026-06-09 — Live PTY output in the native window

The native window now renders a **real shell**. The seeded demo snapshot is
gone; `cargo run -- --native` spawns `$SHELL` on a PTY and renders its live
startup output (prompt + any banner) as it arrives. This proves the
shell → core → pixels path end to end. Keyboard input is still deliberately out
of scope (next packet), so you can't type yet.

### What landed

- **`src/native.rs`** — shell wired in behind the renderer:
  - `run_native` spawns `PtySession::spawn_default_shell(initial_grid)`, shares
    a `core::Terminal` as `Arc<Mutex<Terminal>>`, and starts a pump thread.
  - **Pump thread** (`spawn_pty_pump`) reads PTY bytes, advances the shared
    terminal under the lock, drains/writes `take_host_output()` responses back
    so query-driven prompts don't stall, and wakes the UI with a `winit`
    `EventLoopProxy<UserEvent>`. EOF/read-error sends `UserEvent::ShellExited`.
  - **Redraw coalescing**: each pump wake sets `needs_rebuild` + one
    `request_redraw()`; `winit` merges redundant redraw requests, and the
    snapshot+vertex rebuild happens at most once per presented frame. The
    terminal is snapshotted under the lock, then the lock is dropped *before*
    any GPU call — the mutex is never held across `wgpu`.
  - `GpuState` now stores the `GlyphAtlas` and gains `update_from_snapshot`,
    which rebuilds the vertex buffer (small grid → cheap to recreate per
    update).
  - **Single shared writer**: `portable-pty`'s `take_writer` yields once, so the
    writer is wrapped in `Arc<Mutex<…>>` — the pump thread uses it for host
    responses now; the App keeps a clone for next packet's input path.
  - **Clean teardown**: on loop exit the child is `kill()`ed + `wait()`ed, the
    master is dropped (unblocking the pump `read`), and the pump thread is
    `join()`ed — verified no zombies and no lingering `odytty` processes.

### Deferred this packet (noted, not done)

- **Window resize → PTY/model resize**: window resize updates only the GPU
  viewport uniform; the PTY rows/cols and terminal model stay at `initial_grid`.
  Full resize coherence (resize both PTY and model, reflow) is a later plan
  item — resizing the window does not crash, it just doesn't reflow yet.
- Keyboard input, mouse/selection, scrollback, themes/effects — all later.

### Test status (verified 2026-06-09)

- `cargo test`: 72 lib + 8 smoke green; +1 `#[ignore]`d live-PTY integration
  test (`pty_output_pumps_into_terminal_snapshot`) that spawns a one-shot
  command on a real PTY, pumps it into a `Terminal`, and asserts the snapshot
  contains the output. Verified passing via `cargo test -- --ignored`.
- `cargo fmt --check`: clean. `cargo clippy`: clean for this packet (only the
  pre-existing `core` derive suggestion remains, untouched).
- Wayland-native smoke:
  `WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`
  exits 0 with a real shell spawned, no validation errors.

---

## 2026-06-09 — Glyph atlas wired into the native renderer (readable text)

The window now shows readable monospaced text. This is the GPU half of the
text-rendering milestone: the `src/text` atlas is uploaded to a texture and the
owned-core `Snapshot` is drawn as textured quads with the `cell.wgsl` pipeline.
Content shown is a static seeded snapshot — PTY output, keyboard input, and the
theme layer are deliberately later packets.

### What landed

- **`src/grid.rs`** (GPU-agnostic, unit-tested): a `#[repr(C)]` `Pod` `Vertex`
  and `build_vertices(&Snapshot, &GlyphAtlas) -> Vec<Vertex>`. Per cell it emits
  a background quad and, for inked printable glyphs, a foreground glyph quad
  with the atlas UV. `attrs.inverse` swaps fg/bg; `wide_continuation` spacers
  are skipped (wide lead cells span two columns); non-ASCII/control cells emit
  background only. Geometry is pixel-space so a resize never rebuilds it.
- **`src/native.rs`** (`GpuState`): uploads the atlas to an `R8Unorm` texture
  (+ nearest/clamp sampler), adds a `Viewport` uniform updated on resize, builds
  the `cell.wgsl` render pipeline with straight-alpha blending, and draws the
  cell vertex buffer over the existing neutral clear in the same pass. The atlas
  is rasterized at `font_size_px * scale_factor` physical px for crisp HiDPI.
- **Seeded demo content**: `GpuState::new` drives a real `core::Terminal`
  (title line + an ANSI-colored sample row + a bold/inverse row) and renders its
  snapshot, so SGR/colors exercise the genuine parsing path. Marked in-code as
  placeholder for the next (PTY) packet.
- **Resize choice**: geometry is stable across resize; only the viewport uniform
  is rewritten with the new physical size.
- **wgpu 29 API notes**: `ImageCopyTexture`/`ImageDataLayout` are now
  `TexelCopyTextureInfo`/`TexelCopyBufferLayout`; `PipelineLayoutDescriptor`
  uses `immediate_size` (no `push_constant_ranges`); `RenderPipelineDescriptor`
  uses `multiview_mask: Option<NonZeroU32>` (not `multiview`);
  `bind_group_layouts` takes `&[Some(&layout)]`; sampler `mipmap_filter` wants
  `MipmapFilterMode`.

### Test status (verified 2026-06-09)

- `cargo test`: 72 lib + 8 smoke (1 ignored) green — adds 5 `build_vertices`
  unit tests (vertex count, blank→bg-only, inverse swap, non-ASCII→no glyph,
  ANSI palette color).
- `cargo fmt --check`: clean. `cargo clippy`: clean for this packet (one
  pre-existing `core` derive suggestion is untouched).
- Wayland-native smoke:
  `WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`
  exits 0 with no errors/validation warnings (Vulkan adapter).

### Gaps toward the prototype

- Text is a static seeded snapshot; live PTY output is the next packet.
- No keyboard input, selection/copy, scrollback, or theme layer yet.
- Atlas covers printable ASCII only; wide/CJK glyphs render background-only.
- Seeded grid uses the coarse default window size, so the drawn grid may not
  exactly fill the window — cosmetic until PTY-driven sizing lands.

---

## 2026-06-09 — Monospace glyph atlas + cell shader (CPU foundation)

The CPU-side foundation for readable text: a GPU-agnostic glyph atlas module and
the cell shader it will feed. This is the rasterization/color half of the
text-rendering milestone, committed separately from the GPU wiring so it can be
unit-tested without a window and reviewed on its own. The atlas is not yet
uploaded to a texture or drawn — wiring it into `src/native.rs` is the next
packet.

### What landed

- **`ab_glyph 0.2` + `bytemuck 1` (derive)** dependencies. `ab_glyph` rasterizes
  outlines to coverage bitmaps; `bytemuck` will back the GPU vertex/instance
  buffers in the wiring packet.
- **`src/text.rs`** (GPU-agnostic, unit-tested):
  - Font sourcing: `ODYTTY_FONT` env override, else a probe list of common Linux
    monospace paths. No font is bundled into the public repo yet (deliberate —
    avoids committing a binary + license); falls back with a clear error.
  - `GlyphAtlas::build` rasterizes printable ASCII (`0x20..=0x7E`) into a single
    R8 coverage bitmap on a fixed equal-cell grid, with shared monospace
    `CellSize` metrics and `uv_rect` for per-cell UVs.
  - Color resolution: sRGB→linear conversion (surface is sRGB), the full xterm
    256-color palette (16 ANSI + 6×6×6 cube + grayscale ramp), and
    `foreground_linear` / `background_linear` for `core::Color`.
- **`src/shaders/cell.wgsl`**: pixel-space → NDC vertex stage (Y-flipped) driven
  by a viewport-size uniform so resize only updates the uniform; fragment stage
  samples the R8 atlas as coverage/alpha for glyph quads and passes solid color
  for background quads.

### Test status (verified 2026-06-09)

- `cargo test`: 67 lib tests + 8 smoke pass, 1 live-PTY ignored.
- `cargo fmt --check`: clean.
- New `text.rs` tests cover sRGB endpoints, the 256-color cube/grayscale, RGB
  passthrough, and atlas metrics/coverage/UV coverage (atlas test self-skips
  when no system font is present).

### Next

- Wire the atlas into `src/native.rs`: upload the bitmap to an R8 texture, build
  per-cell background + glyph instance quads from a `core::Snapshot`, and draw
  them through `cell.wgsl`. That turns the placeholder clear into readable text.

---

## 2026-06-09 — GPU surface clears the window (wgpu bring-up)

The `--native` window now has a live `wgpu` surface. Each frame is cleared to a
neutral placeholder color and presented; the surface reconfigures on resize.
This is the GPU-pipeline half of the text-rendering milestone, split out so GPU
bring-up is verified before any glyph work. No glyph atlas, PTY wiring, input,
or theme layer yet — the clear color is a placeholder, not the theme system.

### What landed

- **`wgpu 29` + `pollster 0.4`** dependencies. `pollster` drives `wgpu`'s async
  adapter/device requests to completion inside `winit`'s synchronous handlers.
- **`GpuState`** (`src/native.rs`): owns the surface, device, queue, and surface
  configuration. Picks an sRGB surface format when available, uses `Fifo`
  (vsync) present mode, clears to the placeholder color via a render pass, and
  presents. Resize reconfigures the surface; lost/outdated/suboptimal surfaces
  are recovered by reconfiguring before the next frame (modeled as a small
  `FrameOutcome`). New `NativeError` variants cover surface/adapter/device
  bring-up failures.
- **Window holds an `Arc<Window>`** so the `wgpu` surface can borrow it for
  `'static`.

### Wayland / Hyprland (verified 2026-06-09)

- Ran with `DISPLAY` unset and only `WAYLAND_DISPLAY` set: the window opens and
  presents, so the path is **native Wayland, not XWayland**.
- `wgpu` selected the **Vulkan backend on the AMD hardware adapter** (a lavapipe
  software ICD is present only as a fallback). This is the intended Hyprland
  path.

### Test status (verified 2026-06-09)

- `cargo test`: 62 lib unit tests + 8 smoke tests pass, 1 live-PTY test ignored.
- `cargo fmt --check`: clean.

### Remaining for the next native packet

- Build the CPU-rasterized monospace glyph atlas and draw the owned core's
  `Snapshot` as readable text into this surface, then wire PTY output + keyboard
  input.

---

## 2026-06-09 — Native window opens and closes cleanly

First real native window. The `--native` path now brings up an OS window via
`winit` and runs the event loop until the window is closed, replacing the
not-implemented scaffold. Kept deliberately narrow: no `wgpu`, no text renderer,
no PTY wiring, no input — those are separate later packets.

### What landed

- **`winit` dependency** (`winit 0.30`): the first piece of the GPU stack. `wgpu`
  is still not added; it arrives with the rendering packet.
- **`run_native` lifecycle** (`src/native.rs`): an `ApplicationHandler` that
  creates the window lazily on `resumed` (per `winit`'s portability contract),
  exits on `CloseRequested`, and surfaces window-creation failures as
  `NativeError::WindowCreation` after the loop returns. `NativeError` now carries
  `EventLoop` and `WindowCreation` variants instead of `NotImplemented`.
- **Grid-derived window size**: `NativeOptions::cell_metrics` / `window_logical_size`
  size the window from the requested grid using coarse monospace metrics
  (~0.6em advance, ~1.2em line height) — realistic dimensions ahead of real font
  measurement, unit-tested without a display.
- **Headless lifecycle check**: `ODYTTY_NATIVE_AUTOCLOSE_MS` auto-closes the
  window after a delay so open/close can be exercised non-interactively. Verified
  end-to-end (window opens, auto-closes, exit 0).

### Test status (verified 2026-06-09)

- `cargo test`: 61 lib unit tests + 8 smoke tests pass, 1 live-PTY test ignored.
- `cargo fmt --check`: clean.

### Remaining for the next native packet

- Add `wgpu`, then render the owned grid as readable monospaced text and wire
  PTY output + keyboard input into the window.

---

## 2026-06-08 — Native window / rendering boundary (scaffold)

Architecture spike toward the first native GPU-rendered prototype, kept to
buildable seams rather than a partial subsystem.

### What landed

- **`native` module** (`src/native.rs`): the boundary where the native app will
  live. Defines `NativeOptions` (window title, initial grid, monospace font
  family, font size) with documented Linux-first defaults, a `NativeError` type,
  and `run_native`, which currently returns `NativeError::NotImplemented`.
- **`--native` CLI path** (`src/main`): wired to fail loudly with a clear
  not-implemented message and a non-zero exit, instead of silently doing nothing.
- **Presentation seam** (`src/render`): `CellMetrics` computes per-cell pixel
  origins and full-grid surface size — GPU-agnostic, unit-tested, and free of
  terminal semantics so the future text renderer has a tested foundation.

### Stack decisions

- The native app stays a distinct boundary from the terminal core: `winit` for
  the event loop, `wgpu` for surface/rendering, a CPU-rasterized monospace glyph
  atlas for text, and grid presentation driven by the core's snapshot.
- `winit`/`wgpu` are intentionally **not** added as dependencies yet. They arrive
  with the packet that implements the actual window, so the dependency tree only
  carries exercised code. This keeps the spike buildable and fast.
- First-prototype text is a single monospace font with no complex shaping (no
  ligatures or BiDi); cell width comes from `unicode-width`, as in the core.

### Unchanged

- The existing headless and `crossterm` host-terminal interactive paths are
  untouched and still pass.

### Remaining for the next native packet

- Add `winit`/`wgpu`, open and close a real window cleanly, then render the grid
  with readable monospaced text and wire PTY output + keyboard input into it.

---

## 2026-06-08 — Owned terminal core, PTY path, and smoke harness

### Direction

OdyTTY is built as original, from-scratch terminal work — not a fork or skin of
another emulator. The first spike is Linux-first, written in Rust, and built
around an OdyTTY-owned terminal model. It uses `vte` as an escape-sequence
parser, not as a terminal core. Ghostty, xterm, and other mature terminals are
compatibility references only.

### Stack as it stands

- Rust (edition 2024).
- `vte` for escape-sequence parsing into the owned model.
- `portable-pty` for spawning and driving a real local shell.
- `crossterm` for the current host-terminal interactive path.
- `unicode-width` for character-width handling.
- `anyhow` / `thiserror` for errors, `tracing` for diagnostics.

The GPU rendering path (`winit` + `wgpu`) is intentionally **not** wired up yet;
it is a planned prototype milestone, not current state.

### What works today

- **Owned terminal model** (`src/core`): a grid of cells with attributes, cursor
  state, scrollback, and an alternate screen, driven by a `vte` parser feeding an
  OdyTTY-owned state machine. The public surface exposes `Terminal::advance`,
  `screen()`, `plain_text()`, host-reply output, and resize.
- **PTY path** (`src/pty`): `PtySession` spawns the default shell or a one-shot
  shell command and streams bytes into the model.
- **Host-terminal interactive mode** (`src/app`): `run_interactive` connects a
  real shell PTY to the current terminal via `crossterm` (alternate screen, raw
  mode, bracketed paste), as a stepping stone before the native GPU window.
- **Render seam** (`src/render`): a `Renderer` trait with a `NullRenderer` so the
  core can be driven and verified headlessly; the real GPU renderer plugs in here
  later.
- **CLI entry points** (`src/main`): a default skeleton print, `--dump-command`
  to render a command's output through the model, and `--interactive`.

### Compatibility primitives landed

The owned core currently handles, with unit coverage: basic printing and
wrapping, cursor movement, SGR attributes/colors, erase (ED/EL), scrollback,
alternate screen, cursor save/restore, scroll regions, bracketed paste, reverse
index (RI), insert/delete line (IL/DL), reset (RIS/DECSTR), insert/delete
character (ICH/DCH), erase character (ECH), repeat (REP), tab stops (HT/HTS/TBC),
and a primary Device Attributes reply.

### Transcript smoke harness

A headless transcript smoke harness (`tests/transcript_smoke.rs`) feeds synthetic
byte transcripts through the public `Terminal` API and asserts coarse,
host-independent invariants: clear/redraw, `ls --color`-style SGR, alt-screen
restore plus scrollback isolation, tab-stop alignment, carriage-return progress
overwrite, resize coherence, and a DA query round-trip. A single live-PTY test
exists but is `#[ignore]`d so the default suite stays deterministic; run it with
`cargo test -- --ignored`.

### Test status (verified 2026-06-08)

- `cargo test`: 54 lib unit tests + 8 smoke tests pass, 1 live-PTY test ignored.
- `cargo fmt --check`: clean.

### Remaining gaps to the first prototype

- No native window yet: `winit` event loop, `wgpu` renderer, and font/text
  shaping are not implemented.
- The grid is not yet drawn to a GPU surface; PTY output and keyboard input are
  not yet wired to a native window.
- Daily-loop interactions in a native window — mouse selection, copy, scrollback
  navigation, paste honoring bracketed-paste — are not implemented there yet.
- No Odyssey visual layer yet (themes/effects behind a toggle).
- Compatibility coverage is meaningful but not exhaustive; further sequences will
  be added from evidence as the prototype needs them.
