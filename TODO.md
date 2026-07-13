# OdyTTY — TODO

Post-prototype checklist for making OdyTTY comfortable enough for repeated short
sessions before broader product features. The first meaningful prototype is
complete; see `DEVLOG.md` for the running record, `SPEC.md` for durable
decisions, and `docs/full-build-roadmap.md` for the full build roadmap.

## Stage 4.5: Foundation Ownership

- [x] Replace the former parser dependency with an OdyTTY-owned DEC ANSI state
      machine.
  - [x] PA1: parser skeleton with ground/escape/CSI/OSC states, mid-stream
        UTF-8 decoding, an OdyTTY dispatch trait, and an oracle harness against
        the existing fixture corpus during the transition.
    - [x] `src/parser/` introduced `OdyParser` (15-state DEC ANSI machine,
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
- [x] Graphics-protocol architecture lands on the owned DCS/APC parser plumbing
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
  - [x] Core reporting probes: DECRQM/DECRPM, XTWINOPS size reports, Secondary
        DA, and XTVERSION.
  - [x] IRM (insert/replace mode, ANSI mode 4 `CSI 4 h`/`CSI 4 l`): printing in
        insert mode shifts cells at and right of the cursor toward the right edge
        (dropping cells past the edge), reset by RIS/DECSTR, with DECRQM
        reporting the live set/reset state. Closes the macOS `pico`/`nano`
        incremental-redraw corruption gap.
- [ ] Add deterministic fixtures for every reproducible terminal-core
      regression.
- [ ] Improve OSC support, including title handling and common shell/editor
      sequences.
  - [x] Core: OSC 0/2 window-title capture with dirty flag; unknown OSC payloads
        consumed (no grid leakage).
  - [x] Native: apply changed OSC window titles to the `winit` window.
  - [x] OSC 8 hyperlinks: core cell association and id dedup; native hover
        underline plus explicit Ctrl+click open with scheme allowlist and no
        shell interpolation.
  - [x] OSC 52 clipboard write path with regular/PRIMARY selectors, bounded
        base64 decoding, and default-deny read/query policy behind explicit
        `osc52_read` opt-in.
  - [x] Dynamic colors: OSC 10/11/12 default color set/query, OSC 4 palette
        set/query, and OSC 104/110/111/112 reset behavior.
  - [x] SI1: OSC 7 working-directory tracking core half — parse
        `file://host/path`, percent-decode the path, accept empty/`localhost`
        hosts (foreign hosts ignored), store advisory cwd string state with a
        `take_working_directory_changed` poll flag, and survive RIS. Malformed
        URLs / truncated escapes / `%00` / oversized payloads are ignored
        non-panicking; OSC 7 emits no response and never leaks into the grid.
        OSC 6 accepted-and-ignored. Native consumer is follow-up work.
- [x] Add mouse reporting modes required by real TUIs.
  - [x] Core: DECSET/DECRST tracking (9/1000/1002/1003) and encoding
        (1005/1006/1015) state plus pure report encoders.
  - [x] MS1: SGR-pixel mode 1016 core half — DECSET/DECRST 1016 selects the
        `SgrPixel` encoding on the single-active axis, DECRQM reports it
        set/reset, and a pure `encode_mouse_event_pixel` seam emits
        `CSI < Cb ; Px ; Py M|m` from caller-owned 1-based pixel coordinates
        (core never derives pixels from cells; the native pixel seam is a
        follow-up work).
  - [x] MS2: SGR-pixel mode 1016 native pixel seam — the native mouse handler
        routes true 1-based physical pixel coordinates (floored from the winit
        cursor position, clamped to the grid pixel extent) to the core pixel
        encoder when 1016 is active; all other encodings keep the cell path.
        Cursor-outside-grid saturates to the nearest edge pixel.
  - [x] Native: route press/release/motion/wheel events through the active mouse
        protocol, with Shift reserved for local selection/scrollback.
  - [x] Core: any-event (1003) no-button hover motion encoding (legacy/SGR/
        urxvt/UTF-8); 1002 still drops no-button motion. Focus reporting (1004)
        state + ESC[I/ESC[O encoders.
  - [x] Native: emit no-button hover motion for any-event tracking and send
        focus-in/out reports to the PTY when 1004 is enabled.
  - [x] MP1: hermetic integration coverage inventories mouse modes
        9/1000/1002/1003/1004/1005/1006/1015/1016, pins exact report bytes
        across legacy/UTF-8/SGR/urxvt encodings, and identified 1016 SGR-pixel
        as the parity gap closed core-side by MS1.
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
        composition of marks is later work.
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
  - [x] Symbol/Nerd-font fallback: `symbol_fallback` defaults on, backed
        by a bundled `SymbolsNerdFontMono` face with explicit > bundled > host
        precedence so PUA prompt icons render out of the box; the classifier
        covers PUA plus standard symbol blocks (Arrows, Misc Technical, Geometric
        Shapes, Misc Symbols, Dingbats incl. `❯`, Misc Symbols+Arrows).
        `ODYTTY_SYMBOL_FONT`/`symbol_font`/SYMMAP/settings override; `--show-config`
        reports `symbol_fallback` + `symbol_font_source`.
  - [x] Universal glyph pack — v2+v3 chain: the single fallback face became an
        ordered chain (`fallback_chain: Vec<Arc<FontVec>>`) that rasterizes each
        glyph from the first face that has it, so coverage is the union. Bundled
        a second symbols face (Nerd Fonts **v2.3.3**) after the v3 face, so PUA
        icons render whatever codepoint era a config emits (fixes the v2 archway
        `U+F557` / python `U+F81F` tofu). `symbol_font_source` reports the full
        chain joined with ` > `.
  - [x] Grouped font picker: families split into **Bundled Fonts** (Victor Mono,
        JetBrains Mono — always present) and **System Fonts** (host families),
        with a host copy of a bundled family de-duplicated into the bundled
        group. Headers are non-selectable; navigation/filtering skip them; either
        group resolves with zero config.
  - [x] Geometric Symbols for Legacy Computing: sextants, octants,
        triangles, eighth strips/ladders, L-combo eighth blocks
        (`U+1FB7C..1FB81`), and segmented digits (`U+1FBF0..1FBF9`) render from
        cell geometry. Deferred (post-v0.1.6): diagonal-edged blocks
        `U+1FB3C..1FB67` and negative diagonals `U+1FBBD..1FBBF` (need a general
        antialiased polygon filler).
- [ ] Decide the shaping strategy for ligatures/stylistic sets behind settings
      while preserving cell correctness.
- [ ] Improve rasterization quality: pixel alignment, baseline consistency,
      padding, gamma, blending, and contrast.
  - [x] Raster side (`src/atlas.rs`): single documented baseline for every
        glyph, nearest-pixel rounding, and a per-slot transparent padding gutter
        that blocks UV bleed and preserves box-drawing edge joins + descenders.
  - [x] Shader gamma/contrast side: `ODYTTY_TEXT_GAMMA` drives a glyph coverage
        correction uniform; `1.0` is the exact legacy blend escape hatch and
        `1.5` is the tuned default for light-on-dark text weight.
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
  - [x] Bench health follow-up (`B2`): feed rows now print progress before
        timing, default workloads are bounded, legacy P1/P2-sized workloads are
        available via `ODYTTY_PERF_PROFILE=legacy`, and text-only scrolls no
        longer force lazy scrollback projection for graphics eviction.
  - [x] Bench refresh + cell-footprint fix (`B3`/`PERF1`/`PERF1b`): `B3` added
        rect/SGR-subparam rows + a `size_of` diagnostic and flagged a -23% `seq`
        regression from `Cell` growth; `PERF1` root-caused it to per-cell write /
        blank-row fill cost (not scroll memmove); `PERF1b` packed `Attrs`'s eight
        `bool` fields into a private `u16` (Attrs 28->20 B, Cell 44->36 B),
        recovering `seq` +24% with parser-oracle goldens unchanged. Public
        `bold`..`hidden` fields became getters/setters.
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
  - [x] Render invalidation split (`P2-b`): core exposes a monotonic render
        revision, native keys retained buffers by terminal revision + viewport
        + selection/search + cursor phase + visible graphics + presentation
        epoch, retained frames skip geometry rebuilds, and blink-only frames
        rewrite only the bounded cursor/overlay tail. Region-level row
        granularity remains deferred until evidence justifies the added
        complexity.
  - [ ] Instanced cell geometry: the grid renderer expands each cell to
        six vertices and re-uploads the full visible geometry on every
        content change (~4.6 MiB on a dense 160x50 grid). Moving
        quad-corner expansion into the vertex shader and uploading one
        compact instance per primitive would cut CPU fill and upload
        bandwidth substantially. Held until the change can be visually
        verified across the Vulkan, Metal, and DX12 backends, since it
        is shader-side and the automated suite does not compare rendered
        pixels. Row-granular dirty regions (today mark_dirty always
        promotes to a full rebuild) are a separate follow-on.
- [x] Add visual regression screenshots or pixel-level smoke checks where
      practical.
  - [x] V1: `tests/pixel_smoke.rs` — a headless CPU compositor rasterizes the
        real `grid::build_vertices*` geometry (default-path blend) and asserts
        structural invariants: blank-cell purity, glyph ink within bounds,
        inverse fg/bg swap, dim luminance drop, underline/strikethrough rows,
        box-drawing `U+2500` seam continuity, wide-char single-draw, bar-cursor
        stripe. Structural over byte-exact goldens for host-font portability;
        runs in the default suite. Optional hash-golden layer deferred.
  - [x] V2: graphics-path pixel checks — the CPU compositor composites
        `visible_placements()` in the GPU's bg -> z<0 -> glyphs -> z>=0 order;
        fixtures assert z-order overdraw + equal-z generation order, source
        crop, c/r cell-box fill, X/Y offset, anchor scroll, and a decoded-sixel
        placement (11 -> 19 fixtures).

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
  - [x] `selection_drag_extend` (default on): double-click-then-drag extends by
        word, triple-click-then-drag extends by line, and Shift+click extends
        from the existing anchor; the grown range is a union of absolute ranges.
        PRIMARY is written only when a drag actually extended a unit, so a plain
        multiclick stays byte-identical to before; OFF restores the historical
        finalize-on-multiclick path. Replaces the `selecting` bool with a typed
        `PointerDrag` scaffold reserved for autoscroll/scrollbar/rect packets.
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
- [x] Keyboard pattern quick-select for URLs, paths, and identifiers.
  - [x] `Ctrl+Shift+L` scans the visible screen for URL, path, and SHA-like
        patterns, labels each match with a short home-row key sequence, and
        copies the match the user types; `Esc` dismisses. Labels are
        prefix-free (no mismatch on multi-character labels). Binding is
        configurable via `ODYTTY_KEYBINDS` (action name `hints`).
- [x] Keyboard copy mode (`Ctrl+Shift+Space` by default; configurable via
      `ODYTTY_KEYBINDS` action `copy-mode`): keyboard-driven scrollback
      selection with vim-style motions.
  - [x] `h/j/k/l`, `w/b/e`, `0/^/$`, `gg/G` move the caret; arrow keys,
        PageUp/Down, Home/End, and `Ctrl-u/d/b/f` paging are also bound.
  - [x] `v` starts character selection, `V` starts line selection, `o` swaps
        ends; `y` / Enter yanks selected text to clipboard; `Esc`/`q` cancel.
  - [x] Terminal core state is never modified — copy mode is a presentation
        overlay; the default frame and input routing are byte-identical while
        copy mode is inactive.
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
- [x] Opt-in cursor animations (all off by default).
  - [x] Cursor easing (`cursor_easing`): opacity fades across each blink edge
        (180 ms ease) instead of a hard toggle; unfocused and steady cursors
        stay fully opaque with no animation overhead.
  - [x] Cursor slide (`cursor_motion`): cursor glides between adjacent cells
        (55 ms ease-out-cubic) instead of jumping; snaps on first frame, large
        jumps (> 6 cells), resize, scrollback, and unfocus.
  - [x] Cursor trail (`cursor_trail`): a short fading after-image trails the
        cursor as it glides, drawn behind the cursor block in the theme cursor
        color; only visible while cursor slide is on; fully decays on settle.
  - [x] Cursor glow (`cursor_glow`): three faint concentric rings in the theme
        foreground color drawn behind the cursor block; faint enough to keep
        nearby text readable.
- [ ] Add window title and focus behavior.
  - [x] Apply OSC title changes to the native window title.
  - [x] Emit DECSET 1004 focus-in/out reports from native window focus events.
- [x] Handle the bell (BEL `0x07`). Core latches a one-shot `bell_pending`
      drained edge-not-level by `Terminal::take_bell()`; never touches the grid.
      Native presentation gated by the `bell` setting: `off`, `visual` (decaying
      full-viewport flash, RV1-safe), `urgent` (default; window attention when
      unfocused), `all`. No audible bell (no audio backend).
- [x] Enable IME composition input. `set_ime_allowed` at window creation;
      `Ime::Commit` writes to the PTY; `Ime::Preedit` renders inline at the
      cursor with an underline and positions the candidate area. Makes CJK input
      methods and compose-key/dead-key accents work.
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
  - [x] Kitty keyboard protocol progressive enhancement: core flag
        stack/query state (`CSI >/< /=/? ... u`) plus native CSI-u encoding for
        disambiguation/report-all flags, preserving legacy bytes when inactive.
  - [x] Kitty keyboard protocol completion: event-type repeat/release
        subfields, alternate key fields, and associated text fields for the
        negotiated flags.

## Stage 5: Settings and Profiles

- [x] File-based configuration: load
      `$XDG_CONFIG_HOME/odytty/odytty.conf` or
      `~/.config/odytty/odytty.conf` once at native startup with precedence
      defaults < config file < environment variables. The format is simple
      `key = value` plus `#` comments, uses a hand-rolled parser, mirrors every
      current `ODYTTY_*` knob, keeps malformed/missing/unreadable files
      non-fatal, and routes all values through the single `Settings` struct.
- [x] Live config reload for settings with existing rebuild seams.
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
- [x] Settings overlay framework: presentation-only in-window panel layer rendered
      through cells, keyboard-driven, and isolated from terminal state.
- [x] Read-only settings panel scaffold.
  - [x] `Settings::setting_info()` inventories every runtime setting in stable
        grouped order with current display value, type hint, range/options, env
        key, reloadability, and non-empty human-readable description.
  - [x] `Ctrl+Shift+,` opens a scrollable settings panel; `Up`/`Down`,
        `PageUp`/`PageDown`, `Home`/`End`, and `Esc` navigate/close it.
        The panel consumes input while open and never writes config or mutates
        terminal state.
- [x] In-panel setting editing with live apply.
  - [x] Settings rows are editable from the overlay: booleans toggle, enums
        cycle, numeric values clamp through the same parser path as config/env,
        and text/path/list settings use an in-row text buffer.
  - [x] Committed edits live-apply through the same native reload seam as file
        reload. Startup-only settings are marked non-editable, and no config
        file writeback happens in this slice.
  - [x] Edits are tracked as a diff over the loaded settings so the writeback
        step can serialize only changed rows; reverting a row clears it from the diff.
- [x] Atomic settings writeback.
  - [x] `Ctrl+S` in the settings panel persists the live-applied diff to the
        same `odytty.conf` path used by startup/live reload.
  - [x] Writeback preserves comments, blank lines, key order, and unknown or
        future keys; only changed keys are rewritten, and missing changed keys
        are appended under an OdyTTY settings-panel section.
  - [x] Saves use a same-directory temp file plus rename, create the config
        directory when missing, and surface non-fatal in-panel errors on write
        failure.
- [x] Theme picker with live preview.
  - [x] `Ctrl+Shift+H` opens a built-in theme picker; the settings panel's
        theme row also opens it with `Left`/`Right` while `Enter` remains the
        text-edit path for custom theme names and files.
  - [x] Arrow navigation applies each built-in immediately through the same
        native live-apply seam as settings-panel edits.
  - [x] `Enter` persists `theme = <name>` through the preservation-first
        `odytty.conf` writeback path; `Esc` restores the originally active theme
        without persisting the preview.
  - [ ] User theme directory enumeration in the picker; user theme files are
        still selectable through the settings-panel theme text edit.
- [x] In-app theme builder.
  - [x] Opened from the settings panel by navigating to the theme row and
        pressing `B`; clone any existing built-in or start from the default
        template, adjust individual color roles with live preview applied
        immediately to the running terminal, and save the result as a named
        `.theme` file to the user theme directory (same parent as
        `odytty.conf`, under `themes/`). `Esc` cancels and restores the
        original theme without saving.
- [x] Mouse-driven settings overlay.
  - [x] The settings panel is now operable by mouse: left-click toggles a
        boolean, cycles an enum, opens the theme picker on the theme row, or
        starts text-edit on numeric/string/path/list rows; right-click cycles an
        enum backward; the wheel free-scrolls the list; clicking outside the
        panel dismisses it exactly like `Esc` (the theme picker restores the
        original theme on dismiss). A single shared `overlay_rect()` is the sole
        geometry source and one `build_visible_rows` walker backs both the
        rendered rows and the click hit-map, so what is drawn is exactly what is
        clickable. All value changes funnel through the existing live-apply
        commit seam and the keyboard path is unchanged and fully additive.
        Overlay pointer events take precedence over selection, TUI mouse
        reporting, hyperlink, and viewport-scroll handling while the overlay is
        open, and opening an overlay clears any held TUI mouse-report button so
        no stale report can survive behind it.
  - [x] Numeric settings rows gain a draggable slider and click-to-type entry.
        Each numeric setting carries a `NumericSpec { min, max, step, unit }`
        and the displayed range label is derived from it (no duplicated range
        string). Dragging the slider track sets the value within `[min, max]`
        snapped to `step` with the readout reflecting the live value; clicking the
        readout begins in-place text entry through the same parser/clamp path as
        keyboard edit. Pointer Move/Release routing extends the overlay pointer
        seam; the drag is mutually-exclusive state that is cleared on release,
        overlay close/reopen, mode switch, and window focus loss, so a lost
        release (alt-tab mid-drag) cannot leave a phantom drag that commits a
        stray value on focus regain. Keyboard path unchanged and additive;
        plain/fast render path untouched.
  - [x] Coherent effect grouping and clearer setting labels: `setting_info()`
        stable-sorts rows into contiguous groups (Theme, Font, Rendering,
        Post-process, Cursor, Input, Connections, Sessions, Clipboard,
        Accessibility, Development) and cryptic keys
        gained clear display labels + help text (e.g. `osc52_read` →
        "Allow clipboard read (OSC 52)", `render_quality` → "Renderer profile",
        `crt_scanline_period` → "CRT scanline spacing", `symbol_font` →
        "Symbol font file"). Labels/help only — no config-key/env renames, so
        existing config files keep working; plain/fast render path unchanged.
- [x] Settings description clarity sweep — rewrote in-panel descriptions for
      `text_gamma`, `visual`, `cursor_style`, and `cursor_blink` to lead with
      what each setting does and the effect of changing it.
  - [x] Clarify `cursor_blink = auto`: on Linux it intentionally resolves to
        the conventional blinking terminal default because `winit` exposes no
        OS caret-blink preference; the settings help text now says this plainly.
- [ ] Profiles and CLI config introspection.
  - [x] `--list-themes`: enumerate the 142 built-in themes as
        `name`/appearance/family rows.
  - [x] `--list-fonts`: enumerate discoverable font files (path, filename-stem
        name, monospace on/off) from the renderer's bounded search directories.
        Pure introspection; no settings key or render-path change.
  - [x] `--show-config`: print the current stable effective-config dump. The
        full settings authority remains `docs/runtime-knobs.md`.

## Visual Capability Parity (Stage 6 parity half)

Operator directive: visual capability parity with the strongest GPU terminals is
a floor; surpassing it is the standing ambition.

- [ ] Add color emoji rendering.
  - [x] Decision spike: selected `swash`, a separate premultiplied-RGBA
        emoji atlas/draw segment, Linux Noto Color Emoji CBDT/CBLC first,
        VS15/VS16 + degradation in the first implementation increment, and
        deferred-not-blocked COLR v1 / SVG-in-OT support.
  - [x] Emoji font discovery and swash proof module: `src/emoji/` discovers
        Noto Color Emoji via fontconfig or bounded
        search, records advertised color glyph formats, and shapes
        representative single, variation-selector, skin-tone, flag, keycap,
        and ZWJ-family sequences. Host-dependent Noto fixture is ignored when
        absent; no atlas/GPU path changes yet.
  - [x] Separate RGBA color-glyph atlas and dedicated shader path:
        `ColorGlyphAtlas` stores premultiplied synthetic RGBA glyphs keyed by
        shaped font/glyph-or-cluster identity, and native now has a dedicated
        color-glyph pass ordered after coverage glyphs/decorations and before
        cursor/overlays. Real color font decoding is follow-up work.
  - [x] Noto Color Emoji CBDT/CBLC rendering with VS15/VS16 policy:
        `EmojiRasterizer` shapes eligible cell graphemes with `swash`, renders
        color bitmaps into the premultiplied atlas, drives live color-glyph
        runs, keeps VS15 on coverage, sends VS16/default emoji to color when
        resident, and degrades to coverage/fallback when the color path cannot
        resolve a bitmap.
  - [x] Emoji presentation gate narrowed to Unicode `Emoji_Presentation`
        property ranges for the non-pictographic symbol blocks, so text-default
        Dingbats/markers such as `U+2731`, `U+25CF`, and `U+25CB` use the
        monochrome coverage/symbol fallback path; missing color-face coverage
        also emits no color run and falls through to the same mono path.
  - [x] Emoji cluster coverage for flags, keycaps, skin tones, and common
        ZWJ sequences. The renderer reconstructs bounded clusters from the
        snapshot, keys atlas entries by full cluster, emits one color glyph when
        Noto resolves a single bitmap, and falls back visibly otherwise.
  - [x] ColorGlyphAtlas capacity audit: the atlas is already bounded at
        4096 resident glyph/cluster slots, grows in fixed row chunks, returns
        `Full` without overwrite or dirtying at capacity, and leaves fallback
        rendering visible; no eviction added without observed need.
  - [ ] Scalable color font expansion (COLR/CPAL first, SVG-in-OT only
        from evidence).
- [ ] Wide-glyph raster quality: double-width (CJK/wide) atlas slot sizing.
  - [x] Audit: width-2 glyphs were clipped — a single-cell atlas slot caps
        ink at `cell.width + overflow_margin` (~`cell.width + cell.height/4`),
        losing the rightmost ~27% of a full-em width-2 glyph, and the slot is
        physically too narrow to hold it. R3 bearing-aware quads did not help
        (the clip is at raster time, in the slot).
  - [x] Fix: width-2 codepoints (`UnicodeWidthChar::width == Some(2)`, the
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
- [x] Shared graphics scene: terminal-owned RGBA image store, bounded
      memory/eviction, cell-anchored placement model, primary/alternate
      isolation, scroll/clear/resize/reset hooks, visible-placement accessor,
      and raw Kitty APC / Sixel DCS routing seam.
- [x] Native GPU image layer: visible scene placements render as
      alpha-blended RGBA8 textured quads between cell backgrounds and glyphs,
      with lazy image-id uploads, visible-set cache eviction, scrollback-aware
      placement geometry, and headless geometry/cache tests.
- [x] Kitty direct still-image MVP: `src/core/kitty.rs` — APC `_G`
      control parsing, in-tree base64 decoder, direct raw RGB/RGBA
      transmit/display (`a=t`/`a=T`, `f=24`/`f=32`, `t=d`), chunk reassembly
      with caps, image/placement ids, Kitty OK/error responses via the
      host-output seam, cursor policy (`C=1`), quiet mode, and deterministic
      protocol fixtures including robustness cases.
  - [x] PNG (`f=100`) payload decode via a constrained direct `png`
        crate dependency; header-level cap checks, RGBA8 normalization, chunked
        PNG fixtures, and explicit malformed/oversized/dimension-mismatch
        errors.
  - [x] File/shared-memory transports (`t=f`, `t=t`, `t=s`) with security
        hardening: temp-dir path restriction, O_NOFOLLOW symlink rejection,
        delete-before-decode for t=t, immediate shm_unlink for t=s, size caps;
        25 integration tests.
- [x] Graphics-surface fuzzing: deterministic never-panic + bounded-memory
      harness (`src/core/graphics_fuzz_tests.rs`) over the whole Kitty/Sixel
      display surface — structured APC `_G` control soup (overflow numerics,
      duplicate/unknown keys, truncated base64, `m=` chunk abuse with
      interleaved sequences, malformed terminators), SAFE transport-path fuzz
      (nonexistent/traversal/over-long/NUL paths; self-created shm only),
      bounded sixel-token + DCS fuzz, and a mixed graphics+text+control stream.
      Invariants: no panic, store caps held, parser never wedges, text stays
      coherent. Smoke tier in default `cargo test`; `#[ignore]` deep tier at
      `ODYTTY_FUZZ_ITERS=40000` ran clean (120k+ iters, no defects). Two
      bounded performance observations on `decode_sixel` recorded as follow-ups
      (eager raster-canvas alloc; O(area) incremental-width re-layout) — both
      cap-bounded, not panics.
  - [x] Sixel memory-behavior hardening (follow-up fixes from the graphics-surface fuzzing audit):
        `raster_attrs` no longer eagerly allocates the declared canvas — it
        records + cap-validates the declared size and the buffer fills lazily
        (header-only streams now cost zero, ~144 MB/seq → 0); the pixel buffer
        separates geometric physical capacity (`cap_w` stride / `cap_h` rows)
        from the drawn extent so incremental width growth is amortized O(area)
        instead of O(N²) (`!9999~` 48 ms → 0.19 ms). Caps, never-panic, and
        declared-size authority unchanged; +5 sixel fixtures + a relaxed-token
        regression fuzzer; deep tier re-run at 40k clean.
- [x] Kitty placement surface: `p=` placement ids with multiple named
      placements per image and same-`(i,p)` replacement; `a=p` display of a
      previously transmitted image by protocol id; `z=` z-index with the
      canonical bg → negative-z → glyphs → non-negative-z render order in the
      GPU image layer; `x/y/w/h` source-rect crop; `c/r` cell-box scaling via
      live CellMetrics; `X/Y` pixel offset within the anchor cell; animation
      (`a=f/a=a`) and Unicode placeholders (`U=1`) out of scope (rejected /
      ignored, documented). Fixed a pre-existing `d=i,p=` bug (matched the
      internal placement id instead of the protocol `p=`); 12 fixtures.
- [x] Kitty delete/query + DECSDM: `a=d` delete variants (d=a/A, i/I+p=,
      c/C, p/P+x=/y=) with uppercase image-data GC, `a=q` validation-only
      query responses, and DECSET/DECRST 80 sixel cursor policy (anchor at
      cursor vs cursor-below) with RIS/DECSTR resets; 17 fixtures.
- [x] Live cell metrics for graphics: `Terminal::set_cell_metrics()` replaces the
      provisional 8×16 px cell in graphics extent/cursor math; native wires
      metrics at GPU init and on every grid resize; new-placements-only
      recompute policy.
- [x] Sixel decoder and terminal integration: pure payload decoder
      then shared-scene placement and cursor/scroll policy.
  - [x] Decoder (`src/graphics/sixel.rs`): full Sixel data language (raster attrs,
        RGB/HLS color introducers, repeat, CR/LF, 6-bit data bytes), VT340
        16-color default palette, HLS-to-RGB, 40 MiB / 10 kpx hard caps,
        P2 transparency, robustness against malformed input, 27 tests.
  - [x] Integration (`src/core/graphics_routing.rs`): DCS hook/put/unhook routing
        extracted from screen.rs; on DCS q unhook decode payload via the sixel decoder,
        insert RGBA into ImageStore, create cell-anchored placement; cursor
        moves to row below image (DECSDM-off); decode errors counted but
        never disturb terminal state; 21 end-to-end tests.
- [x] Color emoji — RGBA color-glyph path, `swash` shaping/rasterization,
      Noto Color Emoji CBDT/CBLC on Linux, VS15/VS16 presentation,
      ZWJ/cluster support; COLR v1 and SVG-in-OT deferred but
      architecturally permitted.
  - [x] `swash` dependency and fontconfig emoji font discovery.
  - [x] RGBA color-glyph atlas (`ColorGlyphAtlas`) and dedicated
        shader/draw segment; premultiplied-RGBA source pixels, no SGR
        foreground tint.
  - [x] Noto Color Emoji CBDT/CBLC rendering with VS15/VS16 presentation,
        wide-cell placement, and graceful no-font degradation.
  - [x] Emoji clusters: flags, keycaps, skin-tone modifiers, ZWJ
        sequences; regression fixtures per category; defined fallback for
        unsupported clusters.
  - [x] ColorGlyphAtlas capacity audit: bounded growth to 4096 slots,
        deterministic `Full` at cap, and no slot overwrite or dirtying on
        overflow.
  - [ ] COLR/CPAL and alternate color-font formats; SVG-in-OT via
        `resvg` if real installed-font evidence requires it.
- [x] Perceptual color pipeline: linear-space blending active in the render
      path; OKLab / OKLCH helpers (`dim_perceptual`, `mix_oklab`, `src/color.rs`)
      used by the minimum-contrast lift and the SGR dim-text resolve step.
      SGR dim now uses `dim_perceptual` (hue-preserving, chroma-aware, calibrated
      to match the perceived brightness of the prior linear ×0.5).
- [x] Minimum-contrast floor (`ODYTTY_MIN_CONTRAST`, `min_contrast`):
      configurable WCAG contrast ratio floor applied at render time. Default
      `16.0` is the fresh-install readability floor; `1.0` is the exact
      passthrough opt-out. The floor is measured via WCAG
      relative luminance; the lift bisects OKLab lightness while preserving hue
      and chroma (`src/color.rs:enforce_min_contrast`).
  - [x] Universal legibility guarantee: the contrast floor now provably
        covers every text color. The glyph path is color-type-agnostic, so
        256-color and truecolor foregrounds already pass through the same single
        floor as ANSI/default; the one remaining gap — an explicit SGR underline
        color (SGR 58) painted without the floor — now routes through the same
        `enforce_contrast_rgba` lift. `min_contrast = 1.0` stays exact
        passthrough (byte-identical opt-out pixels, including the new underline
        path). A
        pixel-smoke frame (256-color + truecolor + explicit-underline-color
        cells) certifies the no-op at default, and coverage units prove each
        color type is lifted with hue and chroma preserved when the floor is up.
- [x] Geometric box-drawing / Powerline rendering (`ODYTTY_GEOMETRIC_BOXDRAW`,
      `geometric_boxdraw`): U+2500–257F, U+2580–259F, Braille, and Powerline
      separators rendered as pixel-perfect geometry at exact cell size rather
      than font glyphs. Off by default; enable via setting or env var.
- [x] Stem-darkening ships default-on at `0.5` (crisper
      light-on-dark text); `ODYTTY_STEM_DARKEN` / `stem_darken = 0.0` is the
      byte-identical opt-out. Applied at startup and live reload before
      glyph-atlas rasterization. Native warns if the GPU surface falls back to
      a non-sRGB format.
- [x] Symbol / Nerd-font fallback chain for PUA prompt icons (starship,
      powerlevel10k, eza).
  - [x] Core wiring landed: the fallback path is live; automatic font search or
        a user-specified font path cover common Nerd-font installs; the resolver
        now falls through to the bundled Symbols Nerd Font Mono face.
  - [x] First-class `symbol_fallback` / `symbol_font` settings knob with
        in-panel control, config round-trip, and help text; `ODYTTY_SYMBOL_FALLBACK`
        / `ODYTTY_SYMBOL_FONT` remain as env overrides. `symbol_fallback` now
        defaults on, with `off` preserving the explicit tofu path.
- [x] Themed cursor, selection, and search roles (`themed_ui_roles`,
      default on): cursor uses the theme cursor color, selection uses the theme
      selection color, and search highlight uses the theme search color rather
      than raw cell inversion. `ODYTTY_THEMED_UI_ROLES=off` restores the
      classic inversion behavior.
- [x] Focus dimming (`focus_dim` / `ODYTTY_FOCUS_DIM`, default 0.0 = off):
      perceptually dims the whole grid (text + background) in OKLab while the
      window is unfocused so it recedes; the dim runs before the contrast floor
      so text stays legible. Focused frames are byte-identical to the pre-feature
      renderer.
- [x] Window padding (`window_padding` / `ODYTTY_WINDOW_PADDING`, default
      4 logical px): an adjustable inset between the window edge and the terminal
      grid so text no longer touches the frame. The padding offsets the full
      pixel-cell seam in both directions — forward (glyph/cursor/quad/image
      vertices and the scroll indicator) and inverse (selection hit-test, drag
      autoscroll, SGR-1016 pixel mouse reports) — so mouse and selection stay
      aligned. `0.0` restores the historical edge-to-edge layout, byte-identical
      to the pre-feature renderer (guarded by a pixel-smoke).
- [x] Post-process pipeline:
  - [x] Lazy offscreen render target + passthrough composite wired into the
        native GPU renderer; `post_active()` false = no offscreen allocation and
        no extra draw pass — direct-to-swapchain path is byte-identical to the
        pre-feature renderer. Pixel-smoke guards the seam (direct vs.
        offscreen→passthrough composite asserts byte-equality).
  - [x] Linear HDR intermediate (Rgba16Float) so HDR overshoot (linear values
        above `1.0`) survives the offscreen round-trip for bloom; a
        filterable-format GPU probe auto-disables the HDR path on weak adapters
        with one stderr notice and falls back to the direct sRGB path; composite
        smoke extended to cover Rgba16Float offscreen → passthrough → sRGB.
  - [x] Bloom / phosphor glow effect: thresholded bright-pass, half-resolution
        separable blur, additive composite, enabled in the fresh-install ambient
        baseline and adapter-gated.
  - [x] CRT post-process core: bounded scanlines + vignette share the same
        offscreen scene render and final composite pass as bloom; curvature and
        chromatic aberration are deferred.
  - [x] Cursor trail (`cursor_trail`, off by default): fading after-image that
        trails the cursor as it glides (rides cursor slide; fully decays on
        settle); themed window border (`window_border`, off by default): a thin
        DPI-scaled frame in the theme border color inside the padding band.
  - [x] New-output fade (`new_output_fade`, off by default): freshly arrived
        output rows fade in over a short ramp at the live tail; scrollback and
        resize snap.
- [x] Follow-OS dark/light theme (`follow_os_theme`, off by default):
      switches between `os_theme_dark` and `os_theme_light` based on the
      desktop color-scheme signal. Live on Wayland; on X11 seed direction at
      launch with `ODYTTY_APPEARANCE=dark|light` (no live X11 signal).
- [x] Close confirmation (`confirm_close`, default on): brief in-window prompt
      before closing when a foreground program is running; idle shell exits
      without prompting.
- [ ] Side-by-side visual comparison vs Ghostty at matched font/size.

## Stage 7: Shell Integration, Perceptual Moat, and Pointer Excellence

Recent work past the prototype: the terminal cooperating with the shell, a
readability-first perceptual color toolchain, and a first-class mouse surface.
Visual/effect items stay behind settings with explicit opt-outs and a
pixel-identical plain path; the readability floor is the safety net every color
feature validates against.

- [x] Shell integration on OSC 133 semantic prompt marks.
  - [x] OSC 133 prompt/command/output boundary marking — the parser arms
        the sequence and per-row prompt marks are stored with no render change,
        the foundation for command-aware UX.
  - [x] Command-aware UX: bindable jump to previous/next prompt
        (viewport-top reference, top-aligned, byte-identical fall-through at the
        ends) and an off-by-default success/fail status gutter (a thin left-edge
        bar per finished command, green/red from the ANSI palette so it adapts
        under the colorblind remap). A prompt-marks epoch folded into the render
        signature guarantees the bar repaints on a pure status transition.
  - [x] Core: absolute cell range for a command's output, so the native
        select/copy path can highlight an exact command's output span.
  - [x] Click-to-position cursor via OSC 133 click events (`sh_click`, on by
        default since the F2 rework): the click slice only, not a shell-input
        takeover, and inert unless a cooperating shell advertises click support.
        The bundled bash/zsh/fish and PowerShell snippets all advertise
        `click_events=1` on the prompt-start mark. The F2 rework routes the
        click through the core `InputRegion` (rune-precise on the exact tier,
        grapheme-cell heuristic otherwise), supports soft-wrapped multi-row
        input, and no-ops on hard-newline buffers and unknown geometry.
  - [x] Windows PowerShell integration: `ShellKind::PowerShell` snippet emits
        OSC 133 A/B/C/D (PSReadLine drives the command-start mark, `D` carries
        `$LASTEXITCODE`), injected on a `powershell`/`pwsh` spawn via
        `-NoExit -Command`; `cmd.exe` stays unsupported. On-device ConPTY pass
        verifies the spawn wiring.
  - [x] Honest fail-safe selection-delete: with a selection but no known prompt
        boundary, Delete/Backspace clears the stale selection and surfaces the
        shell-integration hint instead of sending blind edit bytes.
- [ ] Perceptual color moat (OKLab/OKLCH pipeline + readability floor).
  - [x] Universal legibility: the contrast floor provably covers every
        text color type (ANSI, 256-color, truecolor, explicit underline color),
        lifting foregrounds in OKLab while preserving hue and chroma. Default
        floor is exact passthrough.
  - [x] Perceptual-safe theme builder: OKLCH lightness/chroma/hue editing
        with a live contrast readout and snap-to-floor against a dedicated
        authoring floor, so the builder cannot author an unreadable theme; hex
        entry stays as the expert fallback.
  - [x] Contrast-aware palette generation: seed a color and generate a
        readable, floor-validated starting palette to then refine in the
        builder.
  - [x] Perceptual colorblind palette adaptation: remap the ANSI palette in
        OKLCH for protan/deutan/tritan, adaptive on output, in an Accessibility
        settings group. The contrast floor, CVD modes, focus dim, and bell are
        described for users in `docs/accessibility.md`.
  - [x] Readability-scrim primitive for background treatments (core) —
        pure math that caps (dark) or lifts (light) the composited background
        luminance so a treatment cannot breach the contrast floor; native
        wiring landed.
  - [x] Background treatments (`background_treatment = gradient / vignette`,
        off by default): position-based per-cell background darkening —
        gradient darkens toward the bottom, vignette toward the edges and
        corners. Readability is safe-by-construction: the treatment runs before
        the minimum-contrast floor so the floor re-lifts the foreground over the
        treated background. Pixel-identical to before when off; forced off under
        the plain renderer profile.
  - [x] Static image background support: `background_treatment = image` draws a
        PNG/JPEG/WebP wallpaper behind the grid, with `background_image`,
        `background_blur_radius`, `cell_bg_opacity`, and optional
        `background_image_scrim` tied to the contrast-floor scrim. The settings
        panel uses an inline path picker for `background_image` without blocking
        overlay navigation while directories are enumerated. Blur-behind
        transparency remains future work.
  - [x] Window transparency (`window_transparency`, default off; `window_opacity`
        percent, default 85): the terminal background and chrome bands draw
        translucent so the desktop shows through, while text, cursor, selection,
        and every overlay stay fully opaque — the readability boundary. The
        surface alpha mode is chosen explicitly (premultiplied → postmultiplied →
        opaque fallback) and degrades cleanly where the display server offers no
        alpha compositing (X11 with no compositor); the opaque path is
        byte-identical, and wallpaper backgrounds compose under the same window
        alpha. Windows composites through DWM.
- [ ] Pointer excellence — make the mouse a joy, without disturbing TUI mouse
      reporting (Shift stays the selection-vs-passthrough seam).
  - [x] Extend an existing selection: Shift+click, double-click-then-drag by
        word, triple-click-then-drag by line.
  - [x] Rectangular/block (column) selection via Alt+drag.
  - [x] Velocity-proportional drag-autoscroll past the edge band.
  - [x] Optional copy-on-select to the clipboard (off by default).
  - [x] Draggable scroll-thumb to scrub through scrollback.
  - [x] Configurable wheel scroll speed plus modifier+wheel font-size zoom
        (only when TUI mouse reporting is off).
- [x] Interactive / clickable file paths (`interactive_paths`, master gate,
      default off): the renderer scans cells for file/directory paths and
      Ctrl+click opens them through the OS opener (`xdg-open` on Linux, `open`
      on macOS) with the same scheme-allowlist / argv-only safety as OSC 8.
      Everything below is inert until the master gate is on.
  - [x] Sub-gates default on under the master gate:
        `interactive_paths_barewords` (extension-bearing basenames),
        `interactive_paths_click_hint` (a "Ctrl+click to open" teaching chip),
        and `interactive_paths_image_inline`; `interactive_paths_editor` is an
        empty editor-command template for `:line[:col]` jump (`{file}`/`{line}`/
        `{col}` argv template, else `$EDITOR`/`$VISUAL`, else the default opener).
  - [x] In-app image lightbox: Ctrl+click a resolved `png`/`jpg`/`jpeg`/`webp`
        path (or the right-click "Open in OdyTTY" item) opens an in-window image
        viewer; `Esc` or click-outside dismisses it.
  - [x] Path right-click menu: Open, Open in OdyTTY (images), Open With… (the
        `xdg-mime` / macOS app-picker overlay), Copy Path, Copy File, and Reveal
        in File Manager. See `docs/keybindings.md` for the chord reference.
- [ ] Theme library and config UX.
  - [x] Built-in theme library expanded to 142 contrast-validated themes
        (data-only, ongoing).
  - [x] Mouse-driven settings overlay with sliders and click-to-type numeric
        entry.
  - [x] Surface font-load failures in the overlay instead of failing silently.
  - [x] In-app keybinding editor: the settings panel's Keybindings row opens a
        dedicated editor where the 12 core non-tab actions are listed; pressing
        a row captures a new chord, `Backspace` resets a row to its default,
        `R` resets all bindings, and conflicts prompt before replacing. Changes
        are written to `odytty.conf` via the preservation-first writeback path;
        all bindable actions, including command-palette, tab, and pane actions,
        remain configurable through `ODYTTY_KEYBINDS` / `keybinds`.
  - [x] First-run onboarding and settings search: on first launch (no config
        file yet, or `ODYTTY_ONBOARDING=1`) a welcome card shows the core
        keyboard shortcuts, read live from the active bindings so rebinds are
        reflected immediately; dismissed with Enter/Esc/Space. First-run memory
        is the config file's existence — no flag file, no telemetry, no account.
        `/` in the settings panel filters rows by name, key, description, or
        group; Esc once clears the filter, a second Esc closes the panel.

## Stage 8: Multi-session tabs

- [x] Multi-session native model: per-session terminal, PTY, scrollback,
      selection, viewport, search, cursor animation, synchronized-output hold,
      and render invalidation state.
- [x] Session-id routed PTY pump and native input routing, so background shells
      keep their own state and the active tab alone receives keyboard/pointer
      input.
- [x] Conventional tab keybindings: `Ctrl+Shift+T` new tab,
      `Ctrl+Shift+W` close tab, `Ctrl+PageDown` next tab, `Ctrl+PageUp`
      previous tab.
- [x] Visible one-row tab bar once two or more sessions exist, with active and
      hover styling, click-to-switch, close affordance, new-tab affordance, and
      context-menu entries.
- [x] Offset in-band image placements correctly while the tab bar is visible.
- [x] Custom tab renaming if shell titles prove insufficient.
- [x] Close Tab vs Close Pane semantics: closing a tab reaps the **whole** active
      tab (every leaf session in its layout tree) via `TabSet::close_active_tab`,
      distinct from Close Pane which collapses a single leaf and keeps a
      multi-pane tab alive. App exit keys on the last *tab* (`tab_count() <= 1`),
      never the last *pane*; single-pane close stays byte-identical. Fixing this
      also cleared the downstream stale-survivor geometry that broke mouse
      new-tab routing after a mislabeled close.
- [x] Active-tab outline: a thin, fully-opaque themed ring (four `SolidQuad`
      edges, `border` role) frames the active tab so it reads clearly over
      background images/treatments. Single-pane windows show no tab bar, so the
      plain/fast path is inert.
- [x] Workspace layer above tabs: a `WorkspaceSet` groups tabs into workspaces,
      each with its own tab strip and focus; the session arena stays flat.
  - [x] Workspace rail chrome: the rail lists workspaces (tabs are top-only);
        `workspace_rail` = auto/always/left/right; rail `+` creates, in-place
        rename.
  - [x] Workspace keyboard + palette: six bindable actions (new/close/rename/
        next/prev/picker); Next/Prev default to `Ctrl+Shift+PageDown/PageUp`.
  - [x] Move a tab between workspaces from the tab context menu.
- [x] Context-aware right-click menus: per-surface compositions (tab slot,
      empty tab strip, terminal content, workspace rail), each targeting the
      clicked surface.
- [x] Seamless remote SSH terminals: the connect path builds the remote argv
      through an owned builder (`src/ssh_connect.rs`).
  - [x] i1 shell-integration bootstrap: inline bash-only base64 rcfile exec'd on
        the remote, nothing persisted there, plain-ssh degrade for non-bash or
        failure; `remote_integration` default on, per-host `Integration on|off`.
  - [x] i3 connection reuse: `ControlMaster=auto`/`ControlPersist` over an
        OdyTTY-owned socket; `remote_reuse` default on, per-host `Reuse on|off`;
        compiled out on a Windows client.
  - [x] i4 dropped-connection reconnect: a dropped remote tab is held open with
        an in-pane prompt (Enter reconnects in place, Esc/Ctrl+D closes).
  - [x] i5 tmux persistence: `tmux new-session -A -s odytty` inside the
        bootstrap; `remote_tmux` default off, per-host `Tmux on|off`; degrades to
        plain integrated bash when the remote lacks tmux.
  - [x] i7 image paste-through: pasting a clipboard image into an integrated
        remote tab arms a confirm-first upload (`remote_image_paste`, default
        `ask`); the image streams over the existing `ssh` connection to a `0600`
        temp file on the remote, then a one-line notice reports the path and
        copies it to the local clipboard (never typed into the shell),
        size-capped, cleaned up best-effort. Works on reconnected and restored
        remote tabs.
  - [x] W5 workspace default host: a workspace can bind a saved host so New Tab
        connects there, with a New Local Tab escape; unbound is byte-identical.
- [x] Workspace-shape persistence and restore-on-launch: an atomic shape
      snapshot (workspace names, tab order, pane split tree + ratios, per-pane
      cwd, and the remote host a remote pane was connected to) written to the
      platform state dir; never grid content, scrollback, or commands.
  - [x] `restore_workspaces` (default off) reopens the previous shape on a bare
        `odytty` launch for the primary instance only; any CLI arg suppresses it;
        debounced autosave on shape change plus save on clean exit. A secondary
        window (lock held by a live primary) shows a one-line notice explaining
        it will not restore or autosave.
  - [x] Remote panes reconnect on restore: a pane connected to a remote host
        respawns through the `ssh` connect path as a fresh remote login shell at
        the host's default directory; an unresolvable host degrades to a local
        shell, and pre-remote snapshots reopen those panes locally.
  - [x] Named layouts: Save All Workspaces as Layout (whole-app capture) and
        Save Workspace as Layout (a single workspace), Open Layout, and Delete
        Layout — from the command palette and the workspace-rail / content /
        empty-tab-strip right-click menus. Saving over an existing name prompts
        replace-or-rename; opening a layout onto a populated window prompts
        Replace / Add / Cancel, while a single pristine default workspace is
        consumed silently.
  - [x] Unix session-host reattach on restore: an alive per-pane session-host id
        reattaches; a dead one opens a fresh shell silently, with a compact
        "N of M sessions reattached" notice. Windows stores no ids (all fresh).
- [x] Resumable-session architecture decision: use an OdyTTY-owned detached
      session-host process, with live PTYs owned outside the window process and
      reattach over a per-user local-only socket.
- [x] Core snapshot-envelope foundation: `ODYTTY-SNAPSHOT` magic, versioned
      section table, required/optional section handling, and owned DTOs for
      dimensions, visible grid, bounded scrollback, cursor, and basic modes.
- [x] Snapshot envelope v2 sections: dynamic colors, OSC title/cwd metadata,
      prompt marks, scroll region, and tab stops, with v1 decode defaults and
      deterministic round-trip tests.
- [x] Core snapshot restore path: decoded envelopes can rebuild a live
      `Terminal`/`Screen` model with active grid, bounded scrollback, captured
      modes, cursor state, dynamic colors, metadata, prompt marks, scroll
      region, and tab stops.
- [x] Session-host foundation: hidden root-level `src/session_host/` process,
      protocol, socket, and lifecycle substrate for the first detach/attach
      slice. It owns one PTY + terminal model outside the window process,
      accepts local Unix-domain attach clients under `$XDG_RUNTIME_DIR/odytty/`,
      enforces `0700` runtime-directory permissions + owner checks, rejects
      protocol/snapshot version mismatches, sends an initial snapshot followed
      by output/invalidation frames, keeps detached sessions alive until the
      bounded idle timeout, and reaps child processes cleanly.
- [x] Persistent session-host attach/detach lifecycle: explicit client detach
      and socket close remove only that client while the PTY and terminal model
      continue running; reattach by id receives a fresh `SnapshotEnvelope` of
      the current bounded scrollback/live grid before live output resumes; host
      shutdown drains PTY EOF, broadcasts session exit, removes the socket, and
      returns when the child exits or the detached idle timeout kills it.
- [x] Detached-session CLI surface: `odytty new --detached` starts a local
      session host and prints `id=...`; `odytty list` reports live sessions in
      metadata-only script rows (`id`, `name`, `state`, `age_ms`, `panes`);
      `odytty attach <id>` opens a live native window and reattaches the hosted
      session as a focused tab repainted from the host snapshot; `odytty attach
      --diagnostic <id>` preserves the headless script/CI status dump.
- [x] Native window-as-client attach core (`src/native/attach.rs`): builds the
      GUI client against the public session-host wire contract. `AttachClient`
      does the handshake, decodes the initial `SnapshotEnvelope` v2 under bounded
      caps, and restores a full mirror `Terminal` (grid + scrollback + modes +
      cursor). A split-socket pump (`spawn_attach_pump`) applies live
      `Output`/`Invalidate` frames to the mirror and wakes the UI through an
      `AttachEventSink`; `SessionExit`/`Error`/EOF signal session end. Input,
      resize, and a clean `Detach` (host survives) are framed back to the host.
      Render mirror discards device-query replies (host is authoritative).
      Additive alternate session source; the local-PTY path stays
      byte-identical. Headlessly tested via an in-process fake host.
- [x] Native window-as-client live wiring: a `SessionSource` enum backs each
      session as `Local { pty }` (byte-identical default) or `Attached { client }`,
      routing resize (`TIOCSWINSZ` vs. a `Resize` frame) and close (kill+reap vs.
      a clean `Detach` that keeps the host alive). Input is unchanged — an attached
      session's `writer` is an `AttachInputWriter` boxed into the same `PtyWriter`,
      so the app-side input path is identical. `TabSet::attach_in_new_tab` /
      `App::attach_session_in_new_tab` present a hosted session as a live tab
      (restored mirror + live repaint); `NativeOptions.attach_session` (default
      `None`) is the opt-in startup seam `odytty attach <id>` sets. SessionExit /
      link-drop closes the attached tab exactly like a local shell exit. Guarded by
      `default_session_source_is_local` + `local_session_resize_routes_to_pty_unchanged`
      + the `gpu_composite_smoke` pixel guard; headlessly tested via a fake host.
      (Phase 2 remainder: output replay/scrubbing + daemon survival across full
      window close.)
- [x] Real-process daemon-survival e2e (`src/native/tests/attach_e2e.rs`):
      glues a real `odytty session-host` subprocess to the real native
      `AttachClient` across a true client disconnect. Proves attach restores the
      pre-attach scrollback, mid-attach input folds into the host's own terminal
      model, a clean detach leaves the host process alive with its socket intact,
      reattach by id restores the scrollback produced before + during the first
      attach, and the host reaps cleanly (no orphaned daemon, no stale socket)
      when its child exits. Hermetic synthetic runtime dir, controlled child,
      bounded timeouts. Closes the real-process detach/reattach integration gap.
- [x] Output replay / scrubbing overlay (Phase 2 differentiator): opt-in
      per-session output recording (`session_replay`, off by default) into a
      bounded in-memory ring (`src/native/output_recorder.rs`) capped by both
      600 frames and 24 MiB (oldest evicted; never unbounded). The PTY pump
      records the live screen snapshot off the render path behind an atomic gate,
      so the default-off path is byte-identical and zero-overhead. A keyboard-
      scrubbable replay overlay (`src/native/replay_overlay.rs`, an
      `OverlayMode::Replay` reusing the overlay framework) scrubs a frozen, fully
      decoupled clone of the ring (←/→ step, PgUp/PgDn ten, Home/End ends, Esc
      close). Presentation-only — proven by `replay_isolation` tests that the
      live terminal frame is byte-identical whether or not replay is active, plus
      ring-bound eviction, recording-off, scrub-navigation, and overlay-closed-
      inert tests. Opened via the unbound `session-replay` action. Recording is
      local-only: frames live only in memory (no disk, no network) and are
      dropped on close / disable. Closes the Phase 2 tests-box replay-overlay-
      isolation part.
- [x] Native splits / panes: a tab owns a binary pane layout tree (leaf =
      session, node = H/V split + ratio) backing a two-level `TabSet` model;
      single-pane tabs stay byte-identical. Per-pane render dispatch lays out
      multiple grids in one window with a themed 1px divider, routes PTY output
      per-pane, and gives only the focused pane keyboard/pointer input. Each pane
      keeps its own scrollback, viewport, selection, search, and cursor.
- [x] Pane resize: dragging a divider reflows both panes (PTY `TIOCSWINSZ` +
      core reflow through the debounced resize path).
- [x] Per-pane focused selection + search overlays via a pane-scoped overlay
      context.
- [x] tmux-style pane keybindings via a configurable prefix (`pane_prefix`,
      default `Ctrl+b`; doubled prefix sends a literal prefix to a nested
      multiplexer): split columns/rows (`%`/`"`), directional + next focus
      (arrows/`o`), close (`x`), zoom/toggle-fullscreen-pane (`z`), and equalize
      (`Space`/`=`). The prefix is captured only when the active tab has more
      than one pane; single-pane tabs pass `Ctrl+b` through byte-identically,
      and `pane_prefix=off` frees the chord in multi-pane tabs too. Pane actions
      are rebindable via `keybinds`.
- [x] Optional inactive-pane focus dim (`inactive_pane_dim`,
      `ODYTTY_INACTIVE_PANE_DIM`, float `0.0..=1.0`, default `0.0`): a subtle
      OKLab dim on the non-focused panes of a multi-pane tab via the existing
      per-`PaneRender` `focus_dim` path. Default off and byte-identical; the
      focused pane is never dimmed, single-pane tabs are unaffected, and the
      plain renderer profile forces it off.
- [x] Per-pane interactive overlays: selection and search-match highlighting
      render for each pane from its own state (not the focused pane only), so a
      selection or a search match shows in the correct pane regardless of which
      pane holds focus; the interactive search query bar stays on the focused
      pane. Painter routing only, inert and byte-identical on a single-pane tab.
- [x] Per-pane inline graphics: Kitty and Sixel placements rasterize and clip to
      each pane's sub-rect inside a split, so an image renders in the correct
      pane without bleeding across a divider (single-pane rendering byte-identical).
- [x] Public live reattach: `odytty attach <id>` routes through the native
      window attach seam, while `odytty attach --diagnostic <id>` remains the
      no-window script/CI form.
- [x] In-window Manage Sessions overlay (`session-attach` bindable action,
      default `Ctrl+Shift+A`; also the "Manage Sessions" right-click item): a
      keyboard-driven list of live detached sessions that attaches the selected
      session without leaving the window. The default chord and the full bindable
      action set are catalogued in `docs/keybindings.md`.
  - [x] Attach dedup + New tab / Replace prompt: attaching a session already
        open in a tab focuses the existing tab; otherwise the `AttachChoice`
        dialog offers New tab (attach alongside) or Replace (close the current
        tab and attach in its place).
  - [x] Kill a session from the manager: a right-click / confirm flow reaps the
        selected hosted session and removes it from the list.
- [x] Detach & switch (right-click "Detach & switch"): spawn a fresh managed
      session in the focused pane's current working directory (OSC 7 cwd) and
      switch to it, with an honest spawn-not-live-migration framing and a
      Swap / Keep both / Cancel dialog.
- [ ] Profiles remain future work.

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
  - [x] `scroll_wheel_lines` (float `1.0..=10.0`, default `6`): configurable
        local wheel scroll step, driven by the overlay slider. Sets how many
        rows one wheel notch advances local scrollback; the same amount also
        drives alternate-scroll (DECSET 1007) arrow emulation, so classic pagers
        (`less`, `man`, `git log`) scroll at the same rows-per-notch as the
        viewport. Full mouse-reporting TUIs own the wheel (their report carries
        direction, not magnitude), overlay free-scroll, and continuous touchpad
        pixel deltas are unaffected.
  - [x] `copy_on_select` (default off): when on, finishing a selection also
        writes the clipboard (in addition to PRIMARY); off keeps the prior
        PRIMARY-only behavior byte-identical.
- [x] Validate basic commands interactively: prompt display, `ls --color`,
      `clear`, simple editor/pager enter-exit behavior, and resize.

## Visual Experience Layer

- [x] Small theme system with a plain baseline and 1–2 OdyTTY presets.
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
- [x] One OdyTTY visual treatment exists and can be disabled.
- [x] Public docs and devlog describe what works, what is deferred, and what
      risks remain.

## First Linux Release

- [x] Make plain `odytty` launch the native terminal; keep `--native` as a
      compatibility alias and move the legacy parser smoke output to
      `--core-smoke`.
- [x] Add a Freedesktop desktop entry for app-launcher registration.
- [x] Document source installs, user-local installs, Odyssey/LFS pacman-tracked
      source packaging, and default-terminal limitations.
- [x] Add OdyTTY icon assets and install them under the hicolor icon theme.
- [x] Add AppStream metainfo once the icon and release metadata are stable.
- [x] Add `odytty -e command args...` plus `--working-directory` and `--title`
      so OdyTTY can advertise `X-TerminalArgExec` and work with
      `xdg-terminal-exec`/default-terminal integrations.
- [x] Decided: keep `TERM=xterm-256color` (plus `COLORTERM=truecolor`). OdyTTY
      implements the xterm sequence set and supersets it (Kitty keyboard/graphics,
      etc.), and `xterm-256color` is installed on essentially every host, so it
      works over SSH and in any terminfo database. A custom `TERM=odytty` entry
      would break the moment OdyTTY talks to a machine that lacks it (the classic
      "terminal type unknown" SSH papercut), for no compatibility gain. Not
      shipping a custom terminfo entry.
- [x] Document GitHub Release/tag/checksum expectations and Odyssey-Mon
      upstream tracking.
- [x] Cut a `v0.1.4` source release with checksums and an Odyssey PKGBUILD.
- [x] Cut a `v0.1.6` glyph/font release: Victor Mono default body font (size 20,
      bundled, JetBrains Mono retained/selectable), bundled symbol fallback on by
      default, expanded geometric coverage, and the OSC 133 narrow-resize prompt
      repaint fix.
- [ ] Evaluate an AppImage artifact after the source package and desktop
      integration are proven.

## Deferred Until After the First Prototype

- [x] Tabs and multiple local shell sessions: landed as the first multi-context
      slice. Splits/panes within a window have since landed too (see Stage 8);
      detachable sessions, profiles, and cross-session multiplexing remain
      deferred.
- [x] Shell integration beyond basic PTY behavior — landed: OSC 133 semantic
      prompt marks and command-aware UX (see Stage 7). Further shell-integration
      surface (click-to-position) is tracked there.
- [x] Command-palette headless substrate: dependency-free fuzzy scorer, stable
      action catalog, bounded source composer, and read-only bounded
      shell-history / recent-directory data provider. History detection covers
      bash `.bash_history`, zsh `.zsh_history` extended history, and Fish
      `fish_history`; tests use synthetic fixtures only and never read real
      user history.
- [x] In-window command-palette overlay: keyboard-driven fuzzy picker over local
      actions, bounded shell history, and recent OSC 7 directories. Exposed as
      the opt-in `command-palette` keybind action (unbound by default; suggested
      `ctrl+alt+p=command-palette`). Selecting history/directories types text
      into the active pane without pressing Enter; selecting actions dispatches
      the local action after the overlay closes.
- [x] SSH config parser substrate: pure, bounded parser over caller-supplied
      OpenSSH config bytes/path for the connection manager. It surfaces
      concrete `Host` aliases plus optional `HostName`/`User`/`Port`, skips
      `Include`, ignores runtime-dependent `Match` blocks until the next `Host`,
      treats wildcard/negated patterns as non-quick-connect entries, and never
      exposes key directives such as `IdentityFile`. Tests use synthetic
      fixtures only.
- [x] Connection hosts data layer: OdyTTY-owned `$XDG_CONFIG_HOME/odytty/hosts.conf`
      / `~/.config/odytty/hosts.conf` source with `Host <alias>` blocks and
      optional per-host profile fields (`Theme`, `Font`, `Title`) for the future
      overlay. `ssh_config_hosts` / `ODYTTY_SSH_CONFIG_HOSTS` is default-off;
      while off, the SSH config loader is never invoked. When explicitly on,
      caller-supplied OpenSSH config entries merge name-only after OdyTTY-owned
      hosts, with owned duplicates winning. Tests use synthetic fixtures only.
- [x] SSH connect action: resolved connection entries spawn the system `ssh`
      binary in a new tab/session using argv built only from name fields
      (`ssh [-p PORT] -- [USER@]HOST`). OdyTTY never reads, stores, prompts for,
      or passes credentials, private keys, or passphrases; authentication stays
      with system `ssh` and its agent. The same argv can back a detached
      session-host command, so SSH sessions can use the resumable attach path.
- [x] Connection-manager overlay UI: keyboard-driven, type-to-filter list of the
      merged saved hosts (OdyTTY-owned first, opt-in OpenSSH-config names only
      when `ssh_config_hosts` is on), fuzzy-ranked over alias/host/user via the
      shared scorer. Exposed as the opt-in `connection-manager` keybind action
      (unbound by default; suggested `ctrl+alt+h=connection-manager`), so the
      unset input path stays byte-identical. `↑`/`↓` select, Enter quick-connects
      via the SSH connect action, Esc dismisses; per-host profile fields show in
      the row. Presentation-only — the overlay never mutates live terminal state
      (isolation test proves the live frame is byte-identical when active). With
      the opt-in off it shows OdyTTY-owned hosts only and never references
      `~/.ssh` (proven by test). Tests use synthetic fixtures only.
  - [x] Connection-manager build-out: ad-hoc **Connect to: …** for an unsaved
        `[user@]host[:port]` (Enter connects, Shift+Enter connects and appends a
        `hosts.conf` block); an in-app **Add / Edit** form that writes a single
        block with a byte-span splice (every other block, comment, and unknown
        field left byte-for-byte untouched) and a per-host `IdentityFile`
        (`ssh -i`, never a stored secret); a **Test connection** tri-state probe
        that carries no password; and a saved-host right-click menu (Open in New
        Tab / Open in New Workspace / Bind Current Workspace, plus Edit / Remove
        for OdyTTY-owned rows). A `Protocol` field is reserved (`ssh` only).
- [ ] Plugin systems, AI features, dashboards, or rich nonstandard workflows.
- [ ] Heavy animation or effects that can compromise readability or latency.
- [ ] Broad cross-platform support beyond Linux-first validation.
- [ ] Windows on-device hardening. Interactive Windows behaviour has now been
      validated on-device across several passes for the 0.7.0 cycle (local
      shells, tabs, splits, selection/copy-paste, minimize/restore, wheel
      routing, shell integration, and clickable paths incl. inline images);
      CI additionally proves compile + unit tests on every push. It remains a
      newer target with a lower polish bar than Linux. Known follow-up: a
      Windows shell that exits on its own does not yet close its pseudoconsole,
      so its tab will not auto-close until a dedicated child-process waiter
      thread is added — an architectural change best validated on a real
      Windows machine.
- [ ] Windows default-terminal handoff. OdyTTY can be launched directly and
      hosts ConPTY shells, but it does not implement the Windows
      default-terminal handoff protocol (DelegationConsole/DelegationTerminal
      registration), so it cannot be selected as the system default terminal
      that Explorer/other apps hand consoles to. Future work; needs COM/registry
      registration and on-device validation.
- [ ] Daily-driver claims against Ghostty/Konsole before compatibility and
      performance are proven.
