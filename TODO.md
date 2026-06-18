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
        OSC 6 accepted-and-ignored. Native consumer is a follow-up packet.
- [x] Add mouse reporting modes required by real TUIs.
  - [x] Core: DECSET/DECRST tracking (9/1000/1002/1003) and encoding
        (1005/1006/1015) state plus pure report encoders.
  - [x] MS1: SGR-pixel mode 1016 core half — DECSET/DECRST 1016 selects the
        `SgrPixel` encoding on the single-active axis, DECRQM reports it
        set/reset, and a pure `encode_mouse_event_pixel` seam emits
        `CSI < Cb ; Px ; Py M|m` from caller-owned 1-based pixel coordinates
        (core never derives pixels from cells; the native pixel seam is a
        follow-up packet).
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
        Post-process, Cursor, Input, Clipboard, Development) and cryptic keys
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
  - [x] `--list-themes`: enumerate the 100 built-in themes as
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
        VS15/VS16 + degradation in the first implementation packet, and
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
        cursor/overlays. Real color font decoding is a follow-up packet.
  - [x] Noto Color Emoji CBDT/CBLC rendering with VS15/VS16 policy:
        `EmojiRasterizer` shapes eligible cell graphemes with `swash`, renders
        color bitmaps into the premultiplied atlas, drives live color-glyph
        runs, keeps VS15 on coverage, sends VS16/default emoji to color when
        resident, and degrades to coverage/fallback when the color path cannot
        resolve a bitmap.
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
      bounded performance observations on `decode_sixel` routed to the director
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
      `13.0` is the fresh-install readability floor; `1.0` is the exact
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
        a user-specified font path cover common Nerd-font installs.
  - [x] First-class `symbol_fallback` / `symbol_font` settings knob with
        in-panel control, config round-trip, and help text; `ODYTTY_SYMBOL_FALLBACK`
        / `ODYTTY_SYMBOL_FONT` remain as env overrides.
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
  - [x] Click-to-position cursor via OSC 133 click events (`sh_click`, off by
        default): the click slice only, not a shell-input takeover, and inert
        unless a cooperating shell advertises click support.
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
        settings group.
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
- [ ] Theme library and config UX.
  - [x] Built-in theme library expanded to 100 contrast-validated themes
        (data-only, ongoing).
  - [x] Mouse-driven settings overlay with sliders and click-to-type numeric
        entry.
  - [x] Surface font-load failures in the overlay instead of failing silently.
  - [x] In-app keybinding editor: the settings panel's Keybindings row opens a
        dedicated editor where the 12 core non-tab actions are listed; pressing
        a row captures a new chord, `Backspace` resets a row to its default,
        `R` resets all bindings, and conflicts prompt before replacing. Changes
        are written to `odytty.conf` via the preservation-first writeback path;
        all 16 bindable actions, including tab actions, remain configurable
        through `ODYTTY_KEYBINDS` / `keybinds`.
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
- [ ] Offset in-band image placements correctly while the tab bar is visible.
- [ ] Custom tab renaming if shell titles prove insufficient.
- [ ] Panes/splits, profiles, detachable sessions, and session persistence
      remain future work.

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
  - [x] `scroll_wheel_lines` (1-10, default 3): configurable local wheel scroll
        step, driven by the overlay slider. Only the local viewport path is
        scaled; reported wheel events (TUI mouse mode on), overlay free-scroll,
        and touchpad pixel deltas are unaffected. Default 3 is byte-identical to
        the prior fixed step.
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
- [ ] Add `odytty -e command args...` plus `--working-directory` and `--title`
      so OdyTTY can advertise `X-TerminalArgExec` and work with
      `xdg-terminal-exec`/default-terminal integrations.
- [ ] Decide whether to keep `TERM=xterm-256color` for compatibility or ship an
      OdyTTY-specific terminfo entry before changing `TERM`.
- [x] Document GitHub Release/tag/checksum expectations and Odyssey-Mon
      upstream tracking.
- [ ] Cut a `v0.1.2` source release with checksums and an Odyssey PKGBUILD.
- [ ] Evaluate an AppImage artifact after the source package and desktop
      integration are proven.

## Deferred Until After the First Prototype

- [x] Tabs and multiple local shell sessions: landed as the first multi-context
      slice. Panes, detachable sessions, profiles, and multiplexing remain
      deferred.
- [x] Shell integration beyond basic PTY behavior — landed: OSC 133 semantic
      prompt marks and command-aware UX (see Stage 7). Further shell-integration
      surface (click-to-position) is tracked there.
- [ ] Plugins, AI features, command palettes, dashboards, or rich nonstandard
      workflows.
- [ ] Heavy animation or effects that can compromise readability or latency.
- [ ] Broad cross-platform support beyond Linux-first validation.
- [ ] Daily-driver claims against Ghostty/Konsole before compatibility and
      performance are proven.
