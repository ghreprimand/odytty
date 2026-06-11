# OdyTTY — TODO

Post-prototype checklist for making OdyTTY comfortable enough for repeated short
sessions before broader product features. The first meaningful prototype is
complete; see `DEVLOG.md` for the running record, `SPEC.md` for durable
decisions, and `docs/full-build-roadmap.md` for the staged roadmap.

## Stage 4.5: Foundation Ownership

- [x] Replace the former parser dependency with an OdyTTY-owned DEC ANSI state
      machine.
  - [x] PA1: parser skeleton with ground/escape/CSI/OSC states, mid-stream
        UTF-8 decoding, an OdyTTY dispatch trait, and an oracle harness against
        the existing fixture corpus during the transition.
    - [x] `src/parser/` introduced `OdyParser` (14-state DEC ANSI machine,
          split-codepoint UTF-8, 32-slot param cap with saturating accumulate,
          2-byte intermediate cap), an owned `Params`, and a `VtDispatch` trait
          plus first-class `apc_dispatch`.
    - [x] Core seam additive + zero behaviour change: shared `dispatch_*`
          helpers; the owned parser's `VtDispatch` path delegates into the same
          terminal semantics.
    - [x] The transition oracle asserted byte-identical Screen state (snapshot
          at every offset + cursor + style/blink + modes + title + host output)
          across the corpus fed whole and at every byte split, plus SGR storms,
          excess intermediates, value saturation, invalid/split UTF-8, DCS, and
          APC-invisibility.
  - [x] PA2: edge-case hardening for C1 controls, cancel/abort semantics,
        parameter limits, OSC terminators, DCS/APC plumbing, malformed UTF-8,
        and fuzzing.
    - [x] Decisions pinned: C1/UTF-8 precedence (UTF-8 wins; lone 8-bit C1
          bytes execute and do not introduce sequences; C1-via-2-byte-UTF-8
          follows the canonical print/execute split), and DCS/APC payload policy
          (DCS unbuffered streaming passthrough; APC buffered + bounded by
          `MAX_APC_RAW`, over-cap APC dropped not truncated).
    - [x] 35 curated edge fixtures folded into the oracle `corpus()` (C1
          singles/introducers, C1-via-multibyte, abort-in-string-states, OSC
          terminator variants, param edge shapes), each also covered at every
          byte split and on a narrow grid.
    - [x] Three committed deterministic parser fuzzers (byte-soup, two-chunk
          split, structure-aware) with an `ODYTTY_FUZZ_ITERS` budget.
  - [x] PA2-r: clean-room rebuild of the parser state core under the
        originality contract — primary sources only (vt100.net DEC ANSI
        diagram, ECMA-48, xterm `ctlseqs`); transition oracle stayed green.
    - [x] Two-layer pipeline: `src/parser/segmenter.rs` owns Ground +
          ALL UTF-8 (bulk validation + `chars()` dispatch + partial-codepoint
          carry); `src/parser/machine.rs` is an 8-bit-clean control automaton
          driven by `classify(byte) -> ByteClass` (~13 classes) and a flat
          `match (state, class) -> Action`. `src/parser/action.rs` keeps the
          state machine sink-agnostic; `src/parser/driver.rs` is the thin
          action→`VtDispatch` adapter.
    - [x] `Params` reimplemented allocation-free: inline `[u16; 32]` + `u32`
          boundary bitmap + `closed: bool`.
    - [x] String caps revised: OSC 128 KiB; APC 1 MiB drop-not-truncate; DCS
          streaming passthrough (no parser buffer).
    - [x] OdyTTY parser policies pinned: C1-via-UTF-8 uniform-execute, OSC/APC
          caps, and no-byte-loss partial UTF-8 completion.
    - [x] Hot-path tightening: `Machine::step` peels the `CsiParam` digit /
          `;`/`:` / final byte arm off the giant `(state, class)` match for
          inlining; driver short-circuits `Action::None` and reduces
          per-byte state-transition cleanup to one APC-cancel check.
    - [x] Parser-only feed benches added (`benches/perf.rs`); PA1→PA2-r
          deltas across five workloads land within noise on plain text and
          improve heavy CSI by ~15%.
  - [x] PA3: production cutover to `OdyParser`, remove `vte` from Cargo, port
        the oracle suite to compact golden fingerprints and self-consistency
        fuzzers, and update public ownership-boundary docs.
- [x] P0: replace `portable-pty` with an OdyTTY-owned Linux PTY layer.
  - [x] PTY allocation uses `openpt`/`grantpt`/`unlockpt`/`TIOCGPTPEER`, sets
        `TIOCSWINSZ`, spawns children as session leaders with a controlling
        terminal, and normalizes Linux PTY-master EOF behavior.
  - [x] The native and smoke paths keep the existing `PtySession` seam while
        using OdyTTY's own command builder and reader/writer clones.
- [x] Retire `crossterm` from the headless input path.
  - [x] Headless interactive mode now owns raw-mode restore via termios, uses
        ANSI screen control directly, forwards raw stdin bytes, and reserves
        Ctrl-Q as the local quit affordance.
- [ ] Graphics-protocol architecture lands on the owned DCS/APC parser plumbing
      after PA2.

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
- [x] Harden alternate-screen behavior with editors, pagers, and full-screen
      apps.
  - [x] PTY smoke: real `less` and `vim` enter alternate screen, accept basic
        interaction, quit, and restore the seeded primary screen.
  - [x] A1: 30 deterministic mode-matrix fixtures (1049, 1048, 47, 1047, ED 2/3
        in alt, scrollback isolation, re-entrancy, DECSC/DECRC interaction,
        RIS/DECSTR inside alt, resize + primary reflow, modal-state persistence).
  - [x] A1: PTY smoke for nano, htop, and git-log-pager alt-screen restore.
  - [x] A1: modes 47/1047/1048 now handled (previously silently ignored).
  - [x] A2-F2: distinct 47/1047/1049 semantics per xterm ctlseqs — parameterized
        enter/leave, 1049 dispatches DECSC/DECRC, 47/1047 no cursor save/restore.
  - [x] A2-F3: `cursor_visible` saved/restored in StoredScreen.
  - [x] A2-F4: `current_attrs` saved/restored in StoredScreen.
  - [x] A2: 11 new fixtures pinning per-mode cursor, cursor_visible, and attrs.
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
- [x] Add configurable font family after the settings path is stable.
  - [x] `ODYTTY_FONT_FAMILY` resolves a monospace face by family name (system
        font lookup across standard Linux dirs) or a direct `.ttf`/`.otf`/`.ttc`
        path, validated as monospace; proportional/unresolved values and bad
        `ODYTTY_FONT` paths fall back to the probe list with one notice rather
        than aborting startup. `ODYTTY_FONT` takes precedence. Dependency-free.
  - [x] Multi-style atlas groundwork: `FontStyle` enum + `(style, char)`-keyed
        dynamic region with `uv_rect_styled`/`ensure_styled`; native rendering
        now consumes the styled path for bold/italic attrs, with regular-face
        fallback when a style face is absent.
- [ ] Validate HiDPI scale handling across window sizes and monitor scale
      factors.
  - [x] H1: scale-agnostic atlas re-raster seam — `GpuState` retains logical
        `font_size_px` + clamped `scale`; `physical_font_px` folds scale with a
        documented `>= 1.0` clamp; `set_scale`/`set_font_px` rebuild the atlas,
        texture, and bind group and republish `atlas.cell` (reusable for a
        future live font-size reload). Invalidation is by construction (fresh
        build, empty dynamic region).
  - [x] H2: native `ScaleFactorChanged` wiring acknowledges the physical inner
        size, drives `GpuState::set_scale`, re-reads rebuilt cell metrics, and
        feeds the existing debounced grid/PTY resize reset path. Headless tests
        cover debounce, metric recompute, and repeated-scale no-ops.
  - [x] H3: headless scale-matrix tests (11 tests: CellSize integrality/
        monotonicity across 5 scales × 2 font sizes, grid_dimensions_for at 50
        combos, rebuild invalidation, debounce final-scale, UV seam-free at
        fractional scales) + `docs/hidpi-validation.md` operator-runnable manual
        matrix (23 cells across 5 sections). All H1/H2 seams confirmed correct.
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
  - [x] Bearing-aware glyph geometry (`src/atlas.rs` + `src/grid.rs`, R3): each
        atlas slot reserves an overflow margin and records per-slot inked bounds;
        `glyph_quad` sizes each glyph quad to its real ink so overflow renders
        uncropped, with a two-pass (backgrounds-then-glyphs) emission so neighbor
        backgrounds never erase overflow ink.
  - [x] Optional subpixel AA: `ODYTTY_SUBPIXEL=rgb|bgr` builds an RGBA coverage
        atlas and uses dual-source blending when the adapter supports it;
        unsupported adapters fall back to grayscale with one stderr notice.
- [x] Render text attributes cleanly at multiple sizes: bold, dim, italic,
      underline, strikethrough, inverse, cursor, and selection.
  - [x] Core attrs expose bold, dim, italic, underline, strikethrough, inverse,
        hidden, foreground, and background; SGR 22 clears both bold and dim.
  - [x] Native/grid path selects atlas `FontStyle` from bold/italic attrs,
        scales dim foregrounds, suppresses hidden glyphs, keeps inverse/cursor/
        selection behavior in the existing attr path, and draws underline +
        strikethrough as metric-derived solid quads.
  - [x] Bold/italic style faces are loaded when discovered; missing style faces
        fall back to regular without synthetic emboldening.
- [x] Profile redraw, scrolling, resize, and large-output performance.
  - [x] Headless `cargo bench --bench perf` harness (evidence-only). Hotspots:
        resize/reflow is O(total scrollback) (~46 ms at 50k lines; ~17 ms even
        width-unchanged), `build_vertices` per-frame (~96 µs, 56× snapshot).
        Optimization packets ranked in findings.
  - [x] Native render-loop mitigation: reusable CPU vertex storage plus a
        grow-only GPU vertex buffer remove steady-state vertex-buffer
        allocation/recreation, and resize debounce coalesces drag bursts before
        core reflow + PTY winsize while still applying the final size exactly.
  - [x] Core resize fast path (`src/core/reflow.rs`): width-unchanged resize
        skips the per-cell reflow (height-only deep ~16,905 µs → ~58 µs, ~293×),
        proven byte-identical to the full reflow by differential oracle tests.
  - [x] Lazy scrollback re-wrap on width change (`src/core/scrollback.rs`, P1-b):
        scrollback stored as logical lines with a memoized physical projection;
        resize re-wraps only the trailing lines needed for the new window and
        defers deep history (re-wrap on access). Width-changed deep resize
        ~46 ms → ~20 µs (~2300×); height-only deep ~58 µs → ~6.6 µs. Proven
        byte/coordinate-identical to eager reflow by a 900-scenario differential
        parity sweep; zero `Snapshot`/`TerminalModel` API change.
- [x] Add visual regression screenshots or pixel-level smoke checks where
      practical.
  - [x] V1: `tests/pixel_smoke.rs` — a headless CPU compositor rasterizes the
        real `grid::build_vertices*` geometry (default-path blend) and asserts
        structural invariants: blank-cell purity, glyph ink within bounds,
        inverse fg/bg swap, dim luminance drop, underline/strikethrough rows,
        box-drawing `U+2500` seam continuity, wide-char single-draw, bar-cursor
        stripe. Structural over byte-exact goldens for host-font portability;
        runs in the default suite. Optional hash-golden layer deferred.

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
  - [x] Large paste writes use a background PTY writer thread and 16 KiB chunks,
        with one writer lock held for the whole paste to preserve byte order and
        avoid event-loop blocking.
  - [x] Bracketed paste emits one begin/end guard around the full payload and
        strips embedded `ESC[201~` end markers from clipboard text.
  - [x] Non-bracketed native paste normalizes LF, CRLF, and CR to terminal
        carriage returns before writing.
  - [x] Linux PRIMARY selection support uses `arboard`: local selection writes
        PRIMARY when available, and middle-click paste reads PRIMARY through the
        hardened native paste path.
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
- [x] Add viewport affordance such as a scrollbar or scroll position indicator.
  - [x] Native right-edge scroll indicator appears when scrolled back, shows
        position/proportion in scrollback+screen, stays hidden at live tail and
        in alternate-screen/live-clamped views, and uses the existing quad path.
- [x] Add configurable key bindings after settings are available.
  - [x] `ODYTTY_KEYBINDS` parses comma/semicolon-separated `chord=action`
        entries for native terminal-local actions: search, copy, paste,
        scroll-up, and scroll-down.
  - [x] Unset preserves today's defaults exactly; valid entries override one
        action at a time; invalid entries log and skip; duplicate chords use
        the last valid binding. PTY input mapping remains unchanged.
- [x] Add cursor style and blink policy settings.
  - [x] DECSCUSR (`CSI Ps SP q`) styles 0-6: host default, blinking/steady
        block, blinking/steady underline, blinking/steady bar; `RIS`/`DECSTR`
        reset to the host default.
  - [x] `ODYTTY_CURSOR_STYLE` (block|underline|bar) and `ODYTTY_CURSOR_BLINK`
        (on|off|auto, default auto) set the host default policy; DECSCUSR
        overrides at runtime; bad values warn once and fall back.
  - [x] Render the three cursor shapes through the existing quad path; blink is
        focus-aware and busy-redraw-free (solid with no scheduled wake when the
        style does not blink or the window is unfocused).
- [ ] Add window title and focus behavior.
  - [x] Apply OSC title changes to the native window title.
  - [x] Emit DECSET 1004 focus-in/out reports from native window focus events.
- [x] Improve mouse and keyboard interaction in TUI apps.
  - [x] Emit native mouse reports to PTY apps when DECSET mouse tracking is
        active.
  - [x] PTY evidence (T1): real `less --mouse` accepts SGR wheel reports and
        scrolls; real `vim` with `mouse=a ttymouse=sgr` accepts SGR click and
        wheel reports; `bash` readline accepts the current normal-mode
        Left/Home/End/Delete/Enter byte sequences.
  - [x] Follow-up: make keyboard encoding mode-aware for DECCKM/application
        cursor mode and keypad mode (`ESC[?1h`, `ESC=`).
  - [x] Follow-up: encode modifiers for named keys such as `Ctrl+Arrow` using a
        documented xterm-compatible form.

## Stage 5: Settings and Profiles

- [x] CF1 file-based configuration: load
      `$XDG_CONFIG_HOME/odytty/odytty.conf` or
      `~/.config/odytty/odytty.conf` once at native startup with precedence
      defaults < config file < environment variables. The format is simple
      `key = value` plus `#` comments, uses a hand-rolled parser, mirrors every
      current `ODYTTY_*` knob, keeps malformed/missing/unreadable files
      non-fatal, and routes all values through the single `Settings` struct.
- [x] CF2 live config reload for settings with existing rebuild seams.
  - [x] Dependency-free mtime+size polling on the native event-loop wake path
        with a bounded one-second cadence; no watcher thread or notify/inotify
        dependency.
  - [x] Startup env precedence remains pinned during reload: env-overridden
        keys never change until restart, while config-sourced values can
        refresh.
  - [x] Reloadable values: theme, visual, font path/family/size, text gamma,
        subpixel mode, cursor defaults, and key bindings. Font changes rebuild
        the atlas, recompute the grid, and push PTY winsize through the same
        path as HiDPI scale changes.
  - [x] Robustness policy: invalid rewrites and deleted files leave the current
        settings untouched; `native_autoclose_ms` remains startup-only.
- [ ] Profiles and CLI config introspection.

## Visual Capability Parity (Stage 6 parity half)

Operator directive: visual capability parity with the strongest GPU terminals is
a floor; surpassing it is the standing ambition.

- [ ] Wide-glyph raster quality: double-width (CJK/wide) atlas slot sizing.
  - [x] W1 audit: width-2 glyphs were clipped — a single-cell atlas slot caps
        ink at `cell.width + overflow_margin` (~`cell.width + cell.height/4`),
        losing the rightmost ~27% of a full-em width-2 glyph, and the slot is
        physically too narrow to hold it. R3 bearing-aware quads did not help
        (the clip is at raster time, in the slot).
  - [x] W1 fix: width-2 codepoints (`UnicodeWidthChar::width == Some(2)`, the
        same rule core uses) reserve two consecutive atlas slots in one row
        (a filler slot avoids a row wrap), rasterize across the full 2-cell
        drawable region, and report a 2-cell `slot_uv` / bearing-aware bounds.
        Grid and native paths unchanged; box-drawing seams and bg-then-glyph
        order preserved. Tests: font-independent clip-width proof (always runs)
        + CJK-gated full-path + pixel_smoke seam-continuity / no-double-draw /
        narrow-neighbour checks. Color emoji (RGBA atlas) remains out of scope.
- [x] Subpixel anti-aliasing behind a setting (R2 finding C).
- [x] Image/graphics protocol decision spike (Kitty graphics + Sixel) —
      sequenced after the owned parser.
- [x] G2.1 shared graphics scene: terminal-owned RGBA image store, bounded
      memory/eviction, cell-anchored placement model, primary/alternate
      isolation, scroll/clear/resize/reset hooks, visible-placement accessor,
      and raw Kitty APC / Sixel DCS routing seam.
- [x] G2.3 native GPU image layer: visible scene placements render as
      alpha-blended RGBA8 textured quads between cell backgrounds and glyphs,
      with lazy image-id uploads, visible-set cache eviction, scrollback-aware
      placement geometry, and headless geometry/cache tests.
- [x] G2.2 Kitty direct still-image MVP: `src/core/kitty.rs` — APC `_G`
      control parsing, in-tree base64 decoder, direct raw RGB/RGBA
      transmit/display (`a=t`/`a=T`, `f=24`/`f=32`, `t=d`), chunk reassembly
      with caps, image/placement ids, Kitty OK/error responses via the
      host-output seam, cursor policy (`C=1`), quiet mode, and deterministic
      protocol fixtures including robustness cases.
  - [x] G2.2b: PNG (`f=100`) payload decode via a constrained direct `png`
        crate dependency; header-level cap checks, RGBA8 normalization, chunked
        PNG fixtures, and explicit malformed/oversized/dimension-mismatch
        errors.
  - [x] G2.5: file/shared-memory transports (`t=f`, `t=t`, `t=s`) with security
        hardening: temp-dir path restriction, O_NOFOLLOW symlink rejection,
        delete-before-decode for t=t, immediate shm_unlink for t=s, size caps;
        25 integration tests.
- [x] K3 Kitty placement surface: `p=` placement ids with multiple named
      placements per image and same-`(i,p)` replacement; `a=p` display of a
      previously transmitted image by protocol id; `z=` z-index with the
      canonical bg → negative-z → glyphs → non-negative-z render order in the
      GPU image layer; `x/y/w/h` source-rect crop; `c/r` cell-box scaling via
      live CellMetrics; `X/Y` pixel offset within the anchor cell; animation
      (`a=f/a=a`) and Unicode placeholders (`U=1`) out of scope (rejected /
      ignored, documented). Fixed a pre-existing `d=i,p=` bug (matched the
      internal placement id instead of the protocol `p=`); 12 fixtures.
- [x] K2 Kitty delete/query + DECSDM: `a=d` delete variants (d=a/A, i/I+p=,
      c/C, p/P+x=/y=) with uppercase image-data GC, `a=q` validation-only
      query responses, and DECSET/DECRST 80 sixel cursor policy (anchor at
      cursor vs cursor-below) with RIS/DECSTR resets; 17 fixtures.
- [x] SX3 live cell metrics: `Terminal::set_cell_metrics()` replaces the
      provisional 8×16 px cell in graphics extent/cursor math; native wires
      metrics at GPU init and on every grid resize; new-placements-only
      recompute policy.
- [x] SX1/SX2 Sixel decoder and terminal integration: pure payload decoder
      then shared-scene placement and cursor/scroll policy.
  - [x] SX1: `src/graphics/sixel.rs` — full Sixel data language (raster attrs,
        RGB/HLS color introducers, repeat, CR/LF, 6-bit data bytes), VT340
        16-color default palette, HLS-to-RGB, 40 MiB / 10 kpx hard caps,
        P2 transparency, robustness against malformed input, 27 tests.
  - [x] SX2: `src/core/graphics_routing.rs` — DCS hook/put/unhook routing
        extracted from screen.rs; on DCS q unhook decode payload via SX1,
        insert RGBA into ImageStore, create cell-anchored placement; cursor
        moves to row below image (DECSDM-off); decode errors counted but
        never disturb terminal state; 21 end-to-end tests.
- [ ] Side-by-side visual comparison vs Ghostty at matched font/size.

## Archived First Prototype Checklist

## Core Readiness

- [x] Confirm the stack and scope boundaries.
- [x] Stand up the minimal runnable skeleton (owned core, PTY, render seam).
- [x] Owned terminal model and owned parser.
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
