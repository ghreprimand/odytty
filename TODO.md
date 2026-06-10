# OdyTTY — TODO

Post-prototype checklist for making OdyTTY comfortable enough for repeated short
sessions before broader product features. The first meaningful prototype is
complete; see `DEVLOG.md` for the running record, `SPEC.md` for durable
decisions, and `docs/full-build-roadmap.md` for the staged roadmap.

## Stage 1: Prototype Stabilization

- [x] Add native font-size configuration with safe defaults, parsing, and
      clamps.
- [x] Establish a minimal settings/options path that can grow beyond ad hoc
      environment variables.
- [x] Document current runtime knobs and launch examples for the native
      prototype.
- [x] Keep default startup behavior unchanged unless a setting explicitly
      overrides it.
- [ ] Run a short manual session after stabilization changes and capture new
      friction as concrete packets.

## Stage 2: Terminal Correctness Hardening

- [ ] Expand compatibility only from observed shell/TUI failures or clearly
      documented standards gaps.
- [ ] Add deterministic fixtures for every reproducible terminal-core
      regression.
- [ ] Improve OSC support, including title handling and common shell/editor
      sequences.
  - [x] Core: OSC 0/2 window-title capture with dirty flag; unknown OSC payloads
        consumed (no grid leakage).
  - [x] Native: apply changed OSC window titles to the `winit` window.
- [x] Add mouse reporting modes required by real TUIs.
  - [x] Core: DECSET/DECRST tracking (9/1000/1002/1003) and encoding
        (1005/1006/1015) state plus pure report encoders.
  - [x] Native: route press/release/motion/wheel events through the active mouse
        protocol, with Shift reserved for local selection/scrollback.
  - [x] Core: any-event (1003) no-button hover motion encoding (legacy/SGR/
        urxvt/UTF-8); 1002 still drops no-button motion. Focus reporting (1004)
        state + ESC[I/ESC[O encoders.
  - [x] Native: emit no-button hover motion for any-event tracking and send
        focus-in/out reports to the PTY when 1004 is enabled.
- [ ] Harden alternate-screen behavior with editors, pagers, and full-screen
      apps.
  - [x] PTY smoke: real `less` and `vim` enter alternate screen, accept basic
        interaction, quit, and restore the seeded primary screen.
- [ ] Improve Unicode, wide-character, combining-mark, and ambiguous-width
      handling.
  - [x] Core: wide-cell write/erase coherence — overwrite-half clears the pair,
        wide glyph wraps whole at EOL, erase/ICH/DCH/ECH repair pairs. Ambiguous
        width stays narrow (future setting).
  - [x] Core: zero-width combining marks attach to the preceding cell's grapheme
        (inline per-cell buffer, cap 2); safe no-op at line start. Renderer
        composition of marks is a later packet.
- [x] Grow PTY-backed smoke coverage without making default tests flaky or slow.

## Stage 3: High-Quality Text And Rendering

- [ ] Treat Ghostty-level visible text quality as the baseline target, not a
      stretch goal.
- [ ] Add configurable font family after the settings path is stable.
- [ ] Validate HiDPI scale handling across window sizes and monitor scale
      factors.
- [ ] Improve glyph atlas management, including cache growth, invalidation, and
      missing-glyph behavior.
  - [x] Atlas extracted to `src/atlas.rs`; missing-glyph fallback box, dynamic
        glyph cache with page-append growth (no eviction), and full-rebuild
        size invalidation with a `revision()`/`take_dirty()` re-upload signal.
  - [x] Native render loop calls `ensure()` for non-ASCII cells, re-uploads the
        atlas texture on `take_dirty()`, and rebuilds vertices against the
        current atlas so real resident glyphs render instead of fallback boxes.
- [ ] Decide the shaping strategy for ligatures/stylistic sets behind settings
      while preserving cell correctness.
- [ ] Improve rasterization quality: pixel alignment, baseline consistency,
      padding, gamma, blending, and contrast.
  - [x] Raster side (`src/atlas.rs`): single documented baseline for every
        glyph, nearest-pixel rounding, and a per-slot transparent padding gutter
        that blocks UV bleed and preserves box-drawing edge joins + descenders.
  - [x] Shader gamma/contrast side: `ODYTTY_TEXT_GAMMA` drives a glyph coverage
        correction uniform; `1.0` is the exact legacy blend escape hatch and
        `1.4` is the tuned default for light-on-dark text weight.
  - [ ] Future native rendering work: optional subpixel AA and bearing-aware
        glyph geometry for true beyond-cell overflow.
- [ ] Render text attributes cleanly at multiple sizes: bold, dim, italic,
      underline, strikethrough, inverse, cursor, and selection.
- [ ] Profile redraw, scrolling, resize, and large-output performance.
- [ ] Add visual regression screenshots or pixel-level smoke checks where
      practical.

## Stage 4: Daily-Driver Interaction

- [x] Refine selection: double-click word, line selection, drag beyond viewport,
      and scrollback-aware ranges.
  - [x] Double-click selects words using alphanumeric plus `_`, `.`, `/`, `-`,
        and `~` as word characters.
  - [x] Triple-click selects the full line.
  - [x] Dragging in top/bottom edge bands scrolls the viewport at a bounded
        rate while extending selection.
  - [x] Selection anchors use absolute scrollback rows and project into the
        current viewport for highlight/copy.
- [ ] Improve clipboard behavior, including large paste behavior, diagnostics,
      and primary selection if appropriate.
- [x] Add search in scrollback.
  - [x] Core engine in `src/core/search.rs`: literal case-sensitive/insensitive
        search over scrollback + screen, inclusive absolute-cell match ranges,
        wide/combining-aware spans, soft-wrap-spanning matches, and next/prev
        with wraparound.
  - [x] Native search UI: `Ctrl+Shift+F` opens a minimal search bar, typed text
        updates a case-insensitive query, `Enter`/`Shift+Enter` jump next/prev
        with wraparound, visible matches are highlighted, and `Esc` closes while
        restoring the pre-search viewport.
        Search state closes on resize/reflow so absolute match rows are never
        kept across a layout change.
- [ ] Add viewport affordance such as a scrollbar or scroll position indicator.
- [ ] Add configurable key bindings after settings are available.
- [ ] Add cursor style and blink policy settings.
- [ ] Add window title and focus behavior.
  - [x] Apply OSC title changes to the native window title.
  - [x] Emit DECSET 1004 focus-in/out reports from native window focus events.
- [ ] Improve mouse and keyboard interaction in TUI apps.
  - [x] Emit native mouse reports to PTY apps when DECSET mouse tracking is
        active.

## Archived First Prototype Checklist

## Core Readiness

- [x] Confirm the stack and scope boundaries.
- [x] Stand up the minimal runnable skeleton (owned core, PTY, render seam).
- [x] Owned terminal model using `vte` as the parser.
- [x] PTY shell command path and host-terminal interactive mode.
- [x] Core compatibility primitives: printing, cursor movement, SGR, erase,
      scrollback, alternate screen, save/restore, scroll regions, bracketed
      paste, RI, IL/DL, SU/SD, DECOM, RIS/DECSTR, ICH/DCH, ECH, REP, tab
      stops, DA reply.
- [x] Headless transcript smoke harness with deterministic default fixtures.
- [x] Add further compatibility sequences as the prototype needs them, decided
      from evidence rather than guesswork (e.g. BCE, SU/SD + DECOM).
- [x] Convert any reproducible failures into deterministic fixtures.

## Native Window and Rendering

- [x] Document the native app stack: `winit` event loop, `wgpu` renderer,
      font/text shaping approach, and Linux assumptions.
- [x] Scaffold the `native` module boundary and `--native` entry.
- [x] Add a native window that opens and closes cleanly (`winit`, native
      Wayland verified on Linux/Hyprland).
- [x] Bring up a `wgpu` surface that clears the window and survives resize
      (GPU-pipeline half of text rendering; Vulkan on the hardware adapter).
- [x] Render the owned terminal grid with readable monospaced text (glyph atlas).
- [x] Connect PTY output to the rendered grid.
- [x] Connect keyboard input to the PTY using the existing input mapping.
- [x] Render cursor and basic viewport state.
- [x] Handle window resize by resizing both PTY and terminal model.

## Daily Loop Basics

- [x] Paste into the PTY path, respecting bracketed paste mode.
- [x] Basic mouse text selection.
- [x] Copy from selection.
- [x] Scrollback viewport navigation.
- [x] Validate basic commands interactively: prompt display, `ls --color`,
      `clear`, simple editor/pager enter-exit behavior, and resize.

## Odyssey Layer

- [x] Small theme system with a plain baseline and 1–2 Odyssey presets.
- [x] One optional visual treatment behind a setting, isolated from terminal
      correctness.
- [x] Verify the visual layer can be disabled and does not affect compatibility
      tests.
- [x] Check readability and performance boundaries before adding more effects.

## First Prototype Acceptance

- [x] A native OdyTTY window opens a real local shell.
- [x] Common shell output is readable and responsive.
- [x] Resize, paste, selection/copy, cursor, and scrollback work at a basic level.
- [x] The compatibility test suite and transcript smoke suite pass.
- [x] One Odyssey visual treatment exists and can be disabled.
- [x] Public docs and devlog describe what works, what is deferred, and what
      risks remain.

## Deferred Until After the First Prototype

- [ ] Tabs, panes, sessions, profiles, and multiplexing.
- [ ] Shell integration beyond basic PTY behavior.
- [ ] Plugins, AI features, command palettes, dashboards, or rich nonstandard
      workflows.
- [ ] Heavy animation or effects that can compromise readability or latency.
- [ ] Broad cross-platform support beyond Linux-first validation.
- [ ] Daily-driver claims against Ghostty/Konsole before compatibility and
      performance are proven.
