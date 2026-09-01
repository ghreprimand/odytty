# OdyTTY — TODO

Living delivery record and forward backlog for OdyTTY. Historical stage
sections preserve what shipped in each development cycle; the unchecked items
identify current future work. See the [`DEVLOG.md`](DEVLOG.md) index for the
monthly development record, [`SPEC.md`](SPEC.md) for durable decisions, and
[`docs/full-build-roadmap.md`](docs/full-build-roadmap.md) for the full build
roadmap.

The v0.10.0 architecture, compatibility, correctness, security, evidence,
documentation, and release-convergence scope is complete and published, and
the v0.11.0 external-review response scope (documentation accuracy, release
signing, color-font and shaping maturity, graphics protocol completeness,
instanced rendering, theme capture, and the published W6 idle comparison) is
complete and published on top of it. The v0.12.0 memory, measurement,
provenance, graphics-gap, and software-endpoint scope is also complete and
published, including the recorded W6 and SE results and post-release channel
verification. Version 0.12.1 is the narrow security patch that isolates
ControlMaster reuse by OpenSSH's effective connection identity and narrows the
AUR publication workflow to its dedicated secret; publication and bounded
post-publish checks on the shipped macOS, Windows, and Linux package paths are
complete. Version 0.12.2 adds the missing CNL/CPL cursor controls used by
pacman's parallel-download display, with explicit count, margin, pending-wrap,
and multiline redraw coverage in the platform-neutral core. Its same-commit
three-platform CI, signed release, provenance, and Homebrew, Scoop, and AUR
publication gates are complete; a live rerun of the original pacman workload
is not claimed. Version 0.13.0 publishes the safer-paste, verified command-range
action, and bounded completion/progress scope recorded below. Its retained
local gates, exact-commit three-platform CI, signed 16-asset release,
provenance, clean source build, and Scoop, Homebrew, and AUR publication are
complete. Native macOS and Windows on-device runtime checks remain explicitly
unperformed because maintainer hardware was unavailable. A checked item
is delivered at the current head (or at the
historical
milestone its section names). An unchecked item is concrete remaining work or
an unmet evidence gate. Standing policies and explicit non-goals are prose
rather than unchecked boxes, so this file does not present them as
implementation commitments. Longer-range candidates require a separately
recorded milestone before implementation.

## v0.13.0: Safer Command-Aware Work

- [x] Freeze the pre-implementation surface inventory, architecture decisions,
      security/privacy boundaries, four-leg platform acceptance target, and
      existing performance baseline in
      [`docs/v0.13.0-foundation.md`](docs/v0.13.0-foundation.md). This is a
      documentation-only foundation and changes no runtime behavior.
- [x] Add one cross-platform policy for risky non-bracketed text paste while
      preserving ordinary single-line and child-enabled bracketed paste.
- [x] Add select, copy, scoped-search, failed-command navigation, and safe
      plain-text export actions over verified OSC 133 command ranges, including
      same-logical-line soft-wrap boundary collisions without text inference.
- [x] Add bounded command completion, progress, activity, silence, failure, and
      notification presentation without changing BEL semantics or stealing
      focus.
- [x] Close the release only after retained local gates, independent Linux
      Wayland/X11, macOS, and Windows evidence, blocking three-platform CI,
      package checks, documentation convergence, and artifact verification.

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
- [x] Run short manual sessions after stabilization changes and capture new
      friction as concrete packets. Later release, platform, application, and
      package-channel validation records supersede the original prototype pass.

## Stage 2: Terminal Correctness Hardening

Standing compatibility policy: expand behavior only from observed shell/TUI
failures or clearly documented standards gaps, and add a deterministic fixture
for every reproducible terminal-core regression.

Delivered compatibility work under that policy:

- [x] Core reporting probes: DECRQM/DECRPM, XTWINOPS size reports, Secondary
      DA, and XTVERSION.
- [x] IRM (insert/replace mode, ANSI mode 4 `CSI 4 h`/`CSI 4 l`): printing in
      insert mode shifts cells at and right of the cursor toward the right edge
      (dropping cells past the edge), reset by RIS/DECSTR, with DECRQM
      reporting the live set/reset state. Closes the macOS `pico`/`nano`
      incremental-redraw corruption gap.
- [x] Improve the named OSC support needed for titles and common shell/editor
      sequences.
  - [x] Core: OSC 0/2 window-title capture with dirty flag; unknown OSC payloads
        consumed (no grid leakage).
  - [x] Native: apply changed OSC window titles to the `winit` window.
  - [x] OSC 8 hyperlinks: core cell association and id dedup; native hover
        underline plus explicit Ctrl+click open on Linux/Windows or Cmd+click on
        macOS, with a scheme allowlist and no shell interpolation.
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
        OSC 6 accepted-and-ignored. Native consumers subsequently landed for
        recent-directory history, spawn-directory inheritance, duplicate
        tabs/windows, layouts, persistence, and interactive paths.
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
- [ ] Complete the remaining Unicode-width policy work; wide characters,
      combining marks, and supported emoji clusters already render correctly,
      while an ambiguous-width setting remains future work.
  - [x] Core: wide-cell write/erase coherence — overwrite-half clears the pair,
        wide glyph wraps whole at EOL, erase/ICH/DCH/ECH repair pairs. Ambiguous
        width stays narrow (future setting).
  - [x] Core and renderer: up to four zero-width combining marks attach to the
        preceding cell's grapheme, render over the base glyph, survive
        selection/copy, reflow, snapshot, and session-host serialization, and
        move into a per-line side table in scrollback. A mark at line start is
        a safe no-op and excess marks are dropped at the documented bound.
  - [ ] Decide whether to add a user-selectable ambiguous-width policy; the
        current implementation intentionally treats East Asian Ambiguous
        codepoints as narrow.
- [x] Grow PTY-backed smoke coverage without making default tests flaky or slow.

## Stage 3: High-Quality Text And Rendering

Standing quality policy: mature-terminal visible text quality is the baseline,
not a stretch goal.
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
  - [x] H3: headless scale-matrix coverage for CellSize integrality and
        monotonicity, grid dimensions, rebuild invalidation, debounce final
        scale, and fractional-scale UV seams, plus
        [`docs/hidpi-validation.md`](docs/hidpi-validation.md) maintainer-run manual
        matrix (23 cells across 5 sections). All H1/H2 seams confirmed correct.
- [x] Improve glyph atlas management, including cache growth, invalidation, and
      missing-glyph behavior.
  - [x] Atlas extracted to `src/atlas/`; missing-glyph fallback box, dynamic
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
- [ ] Add a general antialiased polygon filler before claiming geometric
      rendering for diagonal-edged Symbols for Legacy Computing
      (`U+1FB3C..U+1FB67`, `U+1FBBD..U+1FBBF`).
- [x] Ship grid-preserving contextual ligatures behind a live setting
      (ASCII graphics plus a curated non-ASCII operator allowlist, with
      `calt`+`liga` on Latin/operator runs). Explicit optional `ss01`/`ss02`
      (off by default); open-ended `ssXX` and full complex-script shaping
      remain deferred; see [`docs/shaping-roadmap.md`](docs/shaping-roadmap.md).
  - [x] Shaping-run infrastructure: grapheme-cluster grouping, byte-to-column
        anchoring, and compatible-run boundary detection (combining marks,
        mixed styles, color-glyph/ZWJ coverage, and wide cells never merge
        into a run).
  - [x] Extended ligature coverage: `SHAPING_OPERATOR_ALLOWLIST` admits a
        curated set of non-ASCII operators and arrows into the same Latin
        overlay path; plain ASCII without allowlisted scalars stays
        byte-identical to the pre-allowlist path.
  - [x] Latin `liga` alongside `calt`, plus optional `ss01`/`ss02` settings
        (off by default; config keys `ss01`/`ss02`, env
        `ODYTTY_LIGATURE_SS01`/`ODYTTY_LIGATURE_SS02`).
  - [x] Arabic contextual joining forms: compatible Arabic runs shaped with
        `Script::Arabic` in logical LTR cell order (not bidi). Overlays cover
        init/medi/fina/isol and length-changing joining ligatures (e.g.
        lam-alef). Harakat-bearing cells still break runs. Active fonts without
        Arabic coverage emit no overlay.
- [x] Improve rasterization quality: pixel alignment, baseline consistency,
      padding, gamma, blending, and contrast.
  - [x] Raster side (`src/atlas/`): single documented baseline for every
        glyph, nearest-pixel rounding, and a per-slot transparent padding gutter
        that blocks UV bleed and preserves box-drawing edge joins + descenders.
  - [x] Shader gamma/contrast side: `ODYTTY_TEXT_GAMMA` drives a glyph coverage
        correction uniform; `1.0` is the exact legacy blend escape hatch and
        `1.2` is the tuned default for light-on-dark text weight.
  - [x] Bearing-aware glyph geometry (`src/atlas/` + `src/grid.rs`, R3): each
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
  - [x] Bold/italic style faces are loaded when discovered; missing faces are
        synthesized by default with double-strike bold and a 12-degree italic
        shear, while real faces always win.
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
        `bool` fields into a private `u16` (Attrs 28->20 B, Cell 44->36 B at
        that revision),
        recovering `seq` +24% with parser-oracle goldens unchanged. Public
        `bold`..`hidden` fields became getters/setters. The later four-slot
        combining-mark array returned live-grid `Cell` to 44 B; current
        scrollback instead stores 28 B `StoredCell` values with marks in a
        per-line side table.
  - [x] Native render-loop mitigation: reusable CPU vertex storage plus a
        grow-only GPU vertex buffer remove steady-state vertex-buffer
        allocation/recreation, and resize debounce coalesces drag bursts before
        core reflow + PTY winsize while still applying the final size exactly.
  - [x] Core resize fast path (`src/core/reflow.rs`): width-unchanged resize
        skips the per-cell reflow. A dated internal microbenchmark measured
        ~16,905 µs → ~58 µs (~293×); this is an implementation before/after
        result, not a cross-terminal product comparison. Differential oracle
        tests prove the fast path byte-identical to full reflow.
  - [x] Lazy scrollback re-wrap on width change (`src/core/scrollback.rs`, P1-b):
        scrollback stored as logical lines with a memoized physical projection;
        resize re-wraps only the trailing lines needed for the new window and
        defers deep history (re-wrap on access). A dated internal
        microbenchmark measured width-changed deep resize at ~46 ms → ~20 µs
        (~2300×) and height-only deep resize at ~58 µs → ~6.6 µs; these are
        implementation before/after results, not cross-terminal product
        comparisons. Proven
        byte/coordinate-identical to eager reflow by a 900-scenario differential
        parity sweep; zero `Snapshot`/`TerminalModel` API change.
  - [x] Render invalidation split (`P2-b`): core exposes a monotonic render
        revision, native keys retained buffers by terminal revision + viewport
        + selection/search + cursor phase + visible graphics + presentation
        epoch, retained frames skip geometry rebuilds, and blink-only frames
        rewrite only the bounded cursor/overlay tail. Region-level row
        granularity remains deferred until evidence justifies the added
        complexity.
  - [x] Instanced cell geometry: the grid and color-glyph builders now emit one
        compact instance per quad; the grayscale, subpixel, and color-glyph
        vertex shaders expand the fixed six corners. For a dense 160x50 grid
        with one background and one glyph quad per cell, the implementation's
        upload payload is 4,608,000 -> 1,024,000 bytes (struct-layout result,
        not a product benchmark). CPU differential tests pin positions, UVs,
        colors, and expansion order to the previous triangle stream, and the
        real-pipeline readback plus pixel-smoke suites pin compositor output.
        The shared wgpu path applies identically to Vulkan, Metal, and DX12.
        Local Vulkan release-profile validation passed on 2026-08-12 at commit
        `a6507f8fb35f304f50da5009992ccd9bc0080ad5` using the clean source-built
        binary with SHA-256
        `df182b0e7a5fedac2cca6ccf17bf118edb5a4f49ed923d02de4121fd00db1e20`
        on an NVIDIA GeForce RTX 5090 Vulkan discrete adapter. The maintainer
        confirmed stable synthetic text/style output, wrapping, sustained row
        output, and resize behavior without geometry corruption. Runtime CJK
        font coverage was unavailable and is not claimed by this check. Metal
        and DX12 passed their named CI build/test legs at the same commit but
        remain manual-unverified; their rows in [`docs/manual-validation.md`](docs/manual-validation.md)
        were not executed.
  - [ ] Row-granular dirty regions: `mark_dirty` still promotes visible changes
        to a full rebuild. Retained and cursor-only frames already avoid the
        full upload. A 2026-08-12 bounded, geometry-only 80x24 quick-profile
        measurement recorded full `build_vertices()` at 208.6 us/op,
        `cursor_tail_only()` at 0.1 us/op, and snapshot plus cursor tail at
        2.2 us/op. These are current implementation measurements, not product
        claims or a controlled pre/post comparison. That sub-millisecond full
        rebuild does not justify dirty-row state across core, snapshot, shaping,
        multi-pane segment assembly, and GPU buffer offsets, so row granularity
        remains deferred pending evidence from a materially hotter workload.
- [x] Add visual regression screenshots or pixel-level smoke checks where
      practical.
  - [x] V1: `tests/pixel_smoke/` — a headless CPU compositor rasterizes the
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
- [x] Improve clipboard behavior, including large paste behavior, diagnostics,
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
  - [x] Risky non-bracketed text (multiline or controls other than tab) is held
        behind a bounded escaped preview with original line/byte counts and
        Paste, reversible Paste as One Line when available, or Cancel. Shortcut,
        palette, context-menu, and Linux PRIMARY routes share the policy; safe
        single-line and child-enabled bracketed paste keep their previous bytes.
        The public Paste Safety reference records the exact trigger matrix and
        explains shell-controlled modes such as Fish; its manual smoke keeps a
        neutral child active so prompt redraw cannot invalidate the test.
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
        (on|off|auto, default on) set the host default policy; DECSCUSR
        overrides at runtime; bad values warn once and fall back.
  - [x] Render the three cursor shapes through the existing quad path; blink is
        focus-aware and busy-redraw-free (solid with no scheduled wake when the
        style does not blink or the window is unfocused).
- [x] Cursor presentation effects (`cursor_easing`, `cursor_trail`,
      `cursor_motion`, and `cursor_glow` on by default).
  - [x] Cursor easing (`cursor_easing`, on by default): opacity fades across each
        blink edge (180 ms ease) instead of a hard toggle; unfocused and steady
        cursors stay fully opaque with no animation overhead.
  - [x] Cursor slide (`cursor_motion`, on by default): cursor glides between
        adjacent cells (55 ms ease-out-cubic) instead of jumping; snaps on first
        frame, large jumps (> 6 cells), resize, scrollback, and unfocus.
  - [x] Cursor trail (`cursor_trail`, on by default): a short fading after-image
        trails the cursor as it glides, drawn behind the cursor block in the
        theme cursor color; only visible while cursor slide is on; fully decays.
  - [x] Cursor glow (`cursor_glow`, on by default): one soft shape-aware analytic
        aura behind the cursor glyph; its restrained alpha keeps nearby text
        readable.
- [x] Add window title and focus behavior.
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
  - [x] Every setting reloads except `native_autoclose_ms`. Font changes rebuild
        the atlas, recompute the grid, and push PTY winsize through the same path
        as HiDPI scale changes.
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
        stable-sorts rows into contiguous groups (Theme, Font, Rendering, Tabs,
        Workspace rail, Panel, Panes, Post-process, Cursor, Input, Connections,
        Sessions, Clipboard, Accessibility, Development) and cryptic keys
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
- [x] Add CLI config introspection.
  - [x] `--list-themes`: enumerate the 144 built-in themes as
        `name`/appearance/family rows.
  - [x] `--list-fonts`: enumerate discoverable font files (path, filename-stem
        name, monospace on/off) from the renderer's bounded search directories.
        Pure introspection; no settings key or render-path change.
  - [x] `--show-config`: print the current stable effective-config dump. The
        full settings authority remains [`docs/runtime-knobs.md`](docs/runtime-knobs.md).
- [ ] Named launch profiles (v0.14.0): schema, precedence, storage, migration,
      Profile Manager CRUD, palette/CLI/`--profile` launch routing, workspace
      `launch_profile` binding, per-pane restoration, saved-layout open, opt-in
      host/directory auto-switch, cached shell discovery, connection-manager
      profile launch rows, the adjacent `+` profile chooser, context-menu
      "with Profile" rows, and the global `default_launch_profile` with a
      workspace override are in-tree (`docs/v0.14.0-profiles-foundation.md`).
- [x] External palette following (v0.14.0): provider-neutral opt-in follow mode,
      content-hash reload, complete-palette fail-closed parsing, last-known-good
      retention, settings + profile appearance fields, and independent
      `colors.toml` / `colors.json` / OdyTTY-Base16 compatibility are in-tree
      (`docs/v0.14.0-external-palette.md`), together with adversarial coverage
      for exact projections, startup isolation, replacement, malformed input,
      and recovery. The retained local landing gate is green; blocking
      three-platform CI remains open.

## Visual Capability Parity (Stage 6 parity half)

Design decision: visual capability parity with the strongest GPU terminals is
a floor; surpassing it is the standing ambition.

- [x] Wide-glyph raster quality: double-width (CJK/wide) atlas slot sizing.
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
  - [x] File transports (`t=f`, `t=t`) on all platforms and shared-memory
        transport (`t=s`) on Unix, with security hardening: temp-dir path
        restriction, Unix O_NOFOLLOW symlink rejection,
        delete-before-decode for t=t, immediate shm_unlink for t=s, size caps,
        and focused integration coverage.
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
        instead of O(N²). A dated internal microbenchmark measured `!9999~` at
        48 ms → 0.19 ms; this is an implementation before/after result, not a
        cross-terminal product comparison. Caps, never-panic, and
        declared-size authority unchanged; +5 sixel fixtures + a relaxed-token
        regression fuzzer; deep tier re-run at 40k clean.
- [x] Kitty placement surface: `p=` placement ids with multiple named
      placements per image and same-`(i,p)` replacement; `a=p` display of a
      previously transmitted image by protocol id; `z=` z-index with the
      canonical bg → negative-z → glyphs → non-negative-z render order in the
      GPU image layer; `x/y/w/h` source-rect crop; `c/r` cell-box scaling via
      live CellMetrics; `X/Y` pixel offset within the anchor cell. Fixed a
      pre-existing `d=i,p=` bug (matched the internal placement id instead of
      the protocol `p=`); 12 fixtures. Animation (`a=f`/`a=a`) and Unicode
      placeholders (`U=1`) were out of scope for that increment and have since
      landed - see the entries below.
- [x] Kitty Unicode placeholders (`U=1`): `U=1` on `a=T`/`a=p` creates a
      *virtual placement* — a prototype that draws nothing, moves no cursor,
      and is addressed by protocol image id. Images then display wherever the
      client prints U+10EEEE placeholder cells, with the image id in the
      foreground color (24-bit or 8-bit palette, optional high byte in a third
      diacritic), the placement id in the underline color, and the tile row /
      column in the first two row/column diacritics; omitted diacritics inherit
      from the cell to the left under the spec's left-to-right rules. Because
      the position lives in the text, placeholders scroll, reflow, erase, and
      are overwritten as text, which is what makes images work inside tmux,
      vim, ratatui-image, and fzf previews. Resolution happens in
      `visible_graphics`, gated on a virtual placement existing so a session
      that never uses the feature does zero extra per-frame work. Deletion
      follows the spec split: `d=i`/`d=I` reach prototypes, the
      location-addressed specifiers do not, and a prototype counts as an image
      reference for GC. Extents from `c=`/`r=` are capped, tiles outside the
      prototype grid are dropped, and the diacritic table is the canonical
      297-entry set. Remaining deviation: tiles split the image uniformly
      across the prototype grid rather than letterboxing to preserve aspect
      ratio.
- [x] iTerm2 inline images (`OSC 1337 ; File=`): landed. The OSC 1337
      dispatch now routes `File=` alongside the existing `Button=` family,
      parsing the argument grammar (`inline`, `size`, `width`, `height`,
      `preserveAspectRatio`, `name`) with cell / `px` / `%` / `auto` dimension
      units and aspect-preserving fit, then decoding the container (PNG, JPEG,
      WebP; content-sniffed, decode bounded against decompression bombs) into
      the existing image store and placement machinery. Parser-limit
      discipline: the payload rides the OSC accumulator, so a single command
      is bounded at 128 KiB (~96 KiB of encoded file bytes) and a command that
      reaches the cap is rejected whole rather than decoded from a truncated
      prefix — the APC rule, applied to OSC. `size=` is cross-checked against
      the decoded length. `inline=0` download requests are parsed and dropped:
      no escape sequence writes files. The cursor advances to column 0 below
      the image, matching iTerm2 and OdyTTY's Sixel default.
- [x] Kitty graphics animation (`a=f`/`a=a`/`a=c`): frame transmission,
      playback control, rectangle composition, and single-frame `d=f`/`d=F`
      deletion with required image addressing and root promotion.
      Fully rendered frames share the image store's decoded-byte quota and have
      a per-image cap of 64. Visible placements in every active split pane
      advance; a session without animated images schedules no animation wake or
      per-frame work. Unicode placeholder placements animate through the same
      visibility path. Animated containers such as APNG and GIF decode as one
      still frame.
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
        16-color default palette, HLS-to-RGB, 40 Mpx total / 10 kpx-per-side
        hard caps, P2 transparency, and robustness coverage for malformed
        input.
  - [x] Integration (`src/core/graphics_routing.rs`): DCS hook/put/unhook routing
        extracted from screen.rs; on DCS q unhook decode payload via the sixel decoder,
        insert RGBA into ImageStore, create cell-anchored placement; cursor
        moves to row below image (DECSDM-off); decode errors counted but
        never disturb terminal state; focused end-to-end coverage pins the
        routing behavior.
- [x] Color emoji - RGBA color-glyph path, `swash` shaping and established
      bitmap/COLR v0 rasterization, Fontations-backed COLR v1 Paint graphs,
      stock Windows Segoe UI Emoji discovery, VS15/VS16 presentation, and
      ZWJ/cluster support; SVG-in-OT remains deferred.
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
  - [x] Static COLR/CPAL v0 layers through swash, including Segoe UI Emoji
        discovery on Windows; synthetic outline and bitmap fixtures pin
        premultiplied RGBA and preserve bitmap-strike output.
  - [x] COLR v1 Paint graphs through Fontations traversal and the existing
        premultiplied atlas: solid fills, linear/radial/sweep gradients,
        transforms, clips, and all standard composite modes; synthetic v1-only
        fixture plus a Windows CI stock-font coverage census.
  - [ ] SVG-in-OT rasterization; SVG-only glyphs use monochrome fallback.
- [x] Perceptual color pipeline: linear-space blending active in the render
      path; OKLab / OKLCH helpers (`dim_perceptual`, `mix_oklab`, `src/color.rs`)
      used by the minimum-contrast lift and the SGR dim-text resolve step.
      SGR dim now uses `dim_perceptual` (hue-preserving, chroma-aware, calibrated
      to match the perceived brightness of the prior linear ×0.5).
- [x] Minimum-contrast floor (`ODYTTY_MIN_CONTRAST`, `min_contrast`):
      configurable WCAG contrast ratio floor applied at render time. Default
      `17.0` is the fresh-install readability floor; `1.0` is the exact
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
        cells) pins `min_contrast = 1.0` and certifies the opt-out no-op; coverage
        units prove each color type is lifted with hue and chroma preserved when
        the floor is up.
- [x] Geometric box-drawing / Powerline rendering (`ODYTTY_GEOMETRIC_BOXDRAW`,
      `geometric_boxdraw`): U+2500–257F, U+2580–259F, Braille, and Powerline
      separators rendered as pixel-perfect geometry at exact cell size rather
      than font glyphs. On by default; `geometric_boxdraw = off` or
      `ODYTTY_GEOMETRIC_BOXDRAW=off` restores font-rasterized glyphs.
- [x] Stem-darkening ships default-on at `0.7` (crisper
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
      4 logical px): an adjustable inset between terminal content and the window,
      chrome, or split-pane divider boundaries. The padding offsets the full
      pixel-cell seam in both directions — forward (glyph/cursor/quad/image
      vertices and the scroll indicator) and inverse (selection hit-test, drag
      autoscroll, SGR-1016 pixel mouse reports) — so mouse and selection stay
      aligned. A pane too narrow after padding keeps a valid one-cell backing
      model but exposes no drawable or input cells until it expands. `0.0`
      restores the historical edge-to-edge layout, byte-identical to the
      pre-feature renderer (guarded by a pixel-smoke).
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
        offscreen scene render and final composite pass as bloom. Subtle barrel
        curvature ships as `crt_curvature` (0.0–0.12, default 0.0); chromatic
        aberration is deferred.
  - [x] Cursor trail (`cursor_trail`, on by default): fading after-image that
        trails the cursor as it glides (rides cursor slide; fully decays on
        settle); themed window border (`window_border`, off by default): a thin
        DPI-scaled frame in the theme border color inside the padding band.
  - [x] New-output fade (`new_output_fade`, on by default): the foreground ink
        of freshly arrived output rows fades in over a 250 ms ramp at the live
        tail; backgrounds render normally from the first frame, and scrollback
        and resize snap.
- [x] Follow-OS dark/light theme (`follow_os_theme`, off by default):
      switches between `os_theme_dark` and `os_theme_light` based on the
      desktop color-scheme signal. Live on Wayland; on X11 seed direction at
      launch with `ODYTTY_APPEARANCE=dark|light` (no live X11 signal).
- [x] Close confirmation (`confirm_close`, default on): brief in-window prompt
      before closing when a foreground program is running; idle shell exits
      without prompting.
- [x] Side-by-side visual comparison against a comparable terminal emulator at
      matched font/size. A matched pass at v0.10.0 covered representative text,
      Unicode, emoji fallback, box drawing, resize feedback, and ordinary
      interaction and found no release-blocking difference in the tested
      surfaces. It is bounded evidence for those conditions, not a conformance
      verdict or a claim about every font, scale, theme, or GPU path.

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
        ends) and an on-by-default success/fail status gutter (a thin left-edge
        bar per finished command and visible pane, green/red from the ANSI
        palette so it adapts under the colorblind remap). A prompt-marks epoch
        folded into the render signature guarantees the bar repaints on a pure
        status transition.
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
- [x] Perceptual color foundation (OKLab/OKLCH pipeline + readability floor).
  - [x] Universal legibility: the contrast floor provably covers every
        text color type (ANSI, 256-color, truecolor, explicit underline color),
        lifting foregrounds in OKLab while preserving hue and chroma. The
        shipped floor is `17.0`; the `1.0` opt-out is exact passthrough.
  - [x] Perceptual-safe theme builder: OKLCH lightness/chroma/hue editing
        with a live contrast readout and snap-to-floor against a dedicated
        authoring floor, so the builder cannot author an unreadable theme;
        clicking a role's displayed value or pressing Enter opens direct hex
        entry as the expert fallback.
  - [x] Contrast-aware palette generation: seed a color and generate a
        readable, floor-validated starting palette to then refine in the
        builder.
  - [x] Theme capture from live dynamic colors: "Create Theme From Current
        Colors" snapshots the focused pane's effective color state — OSC 4
        palette overrides and OSC 10/11/12 foreground/background/cursor, with
        theme-seeded values wherever no override exists — into a builder draft.
        The roles the protocol cannot express (selection, search, border,
        inactive) are derived from the captured colors with documented
        luminance-based heuristics, all editable before saving. Reachable from
        the command palette and from `C` inside the builder; platform-neutral,
        and inert until invoked.
  - [x] Perceptual colorblind palette adaptation: remap the ANSI palette in
        OKLCH for protan/deutan/tritan, adaptive on output, in an Accessibility
        settings group. The contrast floor, CVD modes, focus dim, and bell are
        described for users in [`docs/accessibility.md`](docs/accessibility.md).
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
  - [x] Window transparency (`window_transparency`, default on; `window_opacity`
        percent, default 80): the terminal background and chrome bands draw
        translucent so the desktop shows through, while text, cursor, and every
        overlay stay fully opaque. Selection strength remains independent and
        defaults to fully opaque — the readability boundary. The
        surface alpha mode is chosen explicitly (premultiplied → postmultiplied →
        opaque fallback) and degrades cleanly where the display server offers no
        alpha compositing (X11 with no compositor); the opaque path is
        byte-identical, and wallpaper backgrounds compose under the same window
        alpha. macOS uses the system compositor and Windows uses DWM.
- [x] Deliver the defined pointer-excellence scope without disturbing TUI mouse
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
      Ctrl+click (Cmd+click on macOS) opens them through the OS opener
      (`xdg-open` on Linux, `open` on macOS, `explorer` on Windows) with the
      same scheme-allowlist / argv-only safety as OSC 8.
      Everything below is inert until the master gate is on.
  - [x] Sub-gates default on under the master gate:
        `interactive_paths_barewords` (extension-bearing basenames),
        `interactive_paths_click_hint` (a platform-specific click teaching chip),
        and `interactive_paths_image_inline`; `interactive_paths_editor` is an
        empty editor-command template for `:line[:col]` jump (`{file}`/`{line}`/
        `{col}` argv template, else `$EDITOR`/`$VISUAL`, else the default opener).
  - [x] In-app image lightbox: Ctrl+click (Cmd+click on macOS) a resolved
        `png`/`jpg`/`jpeg`/`webp` path, or use the right-click "Open in OdyTTY"
        item, to open an in-window viewer; `Esc` or click-outside dismisses it.
  - [x] Path right-click menu: Open, Open in OdyTTY (images), Open With… (the
        `xdg-mime` / macOS app-picker overlay), Copy Path, Copy File, and Reveal
        in File Manager. See [`docs/keybindings.md`](docs/keybindings.md) for the chord reference.
- [x] Deliver the current theme-library and configuration UX scope.
  - [x] Built-in theme library expanded to 144 contrast-validated themes
        (data-only, ongoing).
  - [x] Mouse-driven settings overlay with sliders and click-to-type numeric
        entry.
  - [x] Surface font-load failures in the overlay instead of failing silently.
  - [x] In-app keybinding editor: the settings panel's Keybindings row opens a
        dedicated editor where all 48 bindable actions are listed; pressing
        a row captures a new chord, `Backspace` resets a row to its default,
        `R` resets all bindings, and conflicts prompt before replacing. Changes
        are written to `odytty.conf` via the preservation-first writeback path;
        the list is sourced from `BindableAction::ALL` and covers core, overlay,
        tab, workspace, and pane actions.
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
- [x] Unix detached-session CLI surface: `odytty new --detached` starts a local
      session host and prints `id=...`; `odytty list` reports live sessions as
      metadata-only rows (name or id, pane count, humanized age, and the id when
      it differs from the name);
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
      so the app-side input path is identical. `WorkspaceSet::attach_in_new_tab` /
      `App::attach_session_in_new_tab` present a hosted session as a live tab
      (restored mirror + live repaint); `NativeOptions.attach_session` (default
      `None`) is the opt-in startup seam `odytty attach <id>` sets. SessionExit /
      link-drop closes the attached tab exactly like a local shell exit. Guarded by
      `default_session_source_is_local` + `local_session_resize_routes_to_pty_unchanged`
      + the `gpu_composite_smoke` pixel guard; headlessly tested via a fake host.
      Output replay and real-process daemon survival across a full window close
      have since landed below.
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
      inert tests. Opened via the `session-replay` action (`Ctrl+Shift+R` by
      default). Recording is
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
      action set are catalogued in [`docs/keybindings.md`](docs/keybindings.md).
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
## Stage 9: v0.10.0 Release Convergence

Architecture, correctness, security, and evidence work rather than feature
expansion. Published as v0.10.0; the detailed evidence documents named below
stay pinned to the revisions they measured.

- [x] Production Rust file-size rule: every handwritten Rust file compiled into
      a shipping target is under 2,000 physical lines.
  - [x] `scripts/production-file-guard.py` classifies each tracked `src/**/*.rs`
        file by walking the real module graph from the crate's non-test targets
        rather than by filename, so test-only paths cannot hide normal-build
        code and an unclassified file fails closed. Blocking CI runs the
        classifier self-tests and the guard.
  - [x] Eleven production-bearing files were over the limit when the rule
        landed and none are now. The settings model and metadata, terminal
        screen, grid, text and font handling, glyph atlas, snapshot envelope,
        shell integration, and the native context menu, settings panel, and tab
        rail were split along ownership boundaries, each behind a facade that
        owns no logic. [`docs/native-decomposition.md`](docs/native-decomposition.md) records the resulting
        boundaries.
- [x] One audited bounded-reader policy for externally influenced files: check
      the target before opening it so a directory, device, or FIFO is refused
      rather than read, reject an oversized file from its metadata, then stop
      one byte past the ceiling so a file that grew between the check and the
      read is seen as different instead of trusted. Session metadata, font
      files, connection-host records, and the shell-integration wrapper
      comparison all use it.
  - [x] Redirection is handled where it is a threat rather than uniformly:
        session metadata refuses a redirected final component, the graphics file
        transport rejects a Windows reparse point, and font loading still
        follows the symlinks ordinary system font installations depend on.
  - [x] Clipboard images are bounded by dimension and byte count before the
        expensive encode instead of after it.
  - [x] Detached-session output is bounded, and a window that cannot be created
        reports the failure instead of failing quietly.
- [x] Windows key-record correctness: keypad and Ctrl records keep their neutral
      identities, pinned by assertions that run on the blocking Windows CI leg.
- [x] Verification recorded rather than asserted: four retained fuzz targets
      (parser dispatch, terminal state transitions, Kitty graphics, Sixel
      decoding) with a scheduled bounded smoke run, retained corpora, and a
      documented crash-triage path; selective mutation testing across the
      parser, input, and transport surfaces; a promoted Miri subset green on its
      pinned nightly with AddressSanitizer and ThreadSanitizer passing and
      diagnostic probes visible but non-blocking; and region-level coverage
      evidence published with its granularity limits stated.
- [x] The informational `ttf-parser 0.25.1` unmaintained advisory carries a hard
      2026-10-15 expiry rather than an indefinite exception.
- [x] Bounded visual feedback, built from existing state and with no new
      protocol: interactive resize reports the settled `columns × rows` geometry
      for 750 ms, `Ctrl`+wheel zoom reports the effective font size for 1.5
      seconds through the same static surface, and inactive tabs and workspace
      rail rows show a static unseen-activity dot. No animation phase, no wake
      at rest, and no change to terminal state or input routing.
- [ ] Complete the real-application matrix rows in
      [`docs/compatibility/real-application-smoke.md`](docs/compatibility/real-application-smoke.md) with an exact artifact,
      application version, and evidence reference per row. The bounded
      post-release package smoke pass does not fill them.
- [ ] Complete the still-open comparative workloads under the preregistered
      protocol in [`docs/benchmark-protocol.md`](docs/benchmark-protocol.md).
      Protocol 1.5.4 W6 idle and SE1/SE2 software-endpoint comparisons are
      executed and published for OdyTTY, Kitty, Ghostty, and Alacritty
      ([`docs/benchmark-results.md`](docs/benchmark-results.md), raw samples in
      `bench-results/`), with every attempt and oracle passing. W1-W5 remain
      unavailable because their frozen optical endpoints require an external
      stimulus controller and display photosensor on a shared capture clock;
      the protocol forbids substituting software timestamps. W7 remains
      deferred because its frozen run costs about 50 hours of exclusive
      benchmark-machine time. Internal before/after microbenchmarks remain
      implementation measurements, not product comparisons.
      Protocol 1.3.0 additionally records a measured feasibility finding: the
      complete declared calibration search over OdyTTY, Kitty, Ghostty, and
      Alacritty found no common device-pixel cell grid on the measurement
      host, so the protocol controls the grid per implementation — exact
      80x24 on identical font bytes, colors, and profiles, with each
      terminal's own pixel pitch pinned and published — and states the
      remaining pitch difference as a limitation instead of asserting a match
      that no declared configuration produces.
- [ ] The external daily-driver evidence program
      ([`docs/external-daily-driver.md`](docs/external-daily-driver.md)) remains a 1.0 gate and is unstarted.

## Stage 10: v0.11.0 External-Review Response

A response to an independent technical review rather than feature-driven
expansion, published as v0.11.0 with a v0.11.1 patch on top. The detailed
records live in [`DEVLOG.md`](DEVLOG.md) and the documents named below.

- [x] Documentation accuracy: a full-tree comment and docs drift sweep, with
      every confirmed stale claim corrected against the code it describes and
      repeated literal claims converted to intra-doc constant references.
- [x] Release-chain signing: `SHA256SUMS` signed with Minisign from v0.11.0
      onward, the public key committed at `docs/keys/odytty-release.pub`, and
      per-platform verification instructions in [`docs/install.md`](docs/install.md). macOS stays
      ad-hoc signed without notarization and the Windows executable stays
      unsigned; releases before v0.11.0 were not retroactively signed.
- [x] Color-font and shaping maturity: COLR/CPAL v0 layers and COLR v1 Paint
      graphs (stock Windows Segoe UI Emoji included), shaping-run
      infrastructure with grapheme-cluster grouping beyond ASCII, Arabic
      contextual joining forms in logical cell order, and the curated
      non-ASCII ligature allowlist with `liga` and explicit `ss01`/`ss02`.
      [`docs/shaping-roadmap.md`](docs/shaping-roadmap.md) states what remains deferred.
- [x] Graphics protocol completeness: Kitty Unicode placeholders (`U=1`),
      Kitty animation frame/control/composition actions, and iTerm2 inline
      images (`OSC 1337 ; File=`) under the established parser-limit
      discipline. [`docs/graphics.md`](docs/graphics.md) holds the support matrix.
- [x] Instanced cell geometry: one compact per-quad instance expanded in the
      vertex shader, pixel output unchanged, recorded in
      [`docs/visual-architecture.md`](docs/visual-architecture.md).
- [x] Theme capture from live dynamic colors: "Create Theme From Current
      Colors" snapshots the focused pane's effective dynamic-color state into
      a theme-builder draft.
- [x] The published W6 idle comparison ([`docs/benchmark-results.md`](docs/benchmark-results.md)), executed
      under the preregistered protocol against Kitty, Ghostty, and Alacritty
      with results reported exactly as measured.
- [x] v0.11.1 patch: theme-builder back navigation restored to the
      panel-return contract, a persistent usage-help line, and a stable
      layout; bounded post-publish package checks on all three platforms
      (Linux artifact checks, macOS Homebrew channel, Windows Scoop channel).

## Stage 11: v0.12.0 Memory, Measurement, and Provenance

This cycle answers the measured memory regression and the remaining bounded
review gaps without weakening terminal correctness. Detailed measurements and
scope decisions live in [`docs/memory.md`](docs/memory.md), [`docs/benchmark-results.md`](docs/benchmark-results.md),
[`docs/shaping-roadmap.md`](docs/shaping-roadmap.md), and [`DEVLOG.md`](DEVLOG.md).

- [x] Memory attribution and host-side capture distinguish OdyTTY-controlled
      allocations from GPU and driver mappings, with dated v0.11.1 baselines.
- [x] Background images are resampled to the drawable surface, inactive
      post-process targets are released, and glyph-atlas residency is bounded
      and measured.
- [x] Scrollback storage no longer pays the full inline combining-mark cost per
      cell, with differential, serialization, reflow, and index-integrity
      coverage preserving terminal behavior.
- [x] Runtime fallback faces are parsed once and shared across codepoints;
      over-limit collections reconstruct only the selected face instead of
      retaining the full collection. This fixes the reproduced MusicFox CJK
      fallback memory growth.
- [x] Kitty `o=z` payloads and image-number animation addressing are supported,
      Sixel is advertised through DA1, and hostile compressed payloads remain
      bounded.
- [x] Release artifacts gain GitHub OIDC build provenance alongside Minisign,
      with the native-signing cost boundary documented separately.
- [x] The supported shaping boundary is explicit and grounded in the recorded
      ucs-detect run; full BiDi, complex Indic/Brahmic reordering, and
      SVG-in-OpenType remain outside the current cell model.
- [x] Protocol 1.5.0 defines software-endpoint workloads and preserves the
      optical-apparatus boundary for interactive latency claims.
- [x] Freeze the exact v0.12.0 candidate, preregister it, run W6 plus SE1 and
      SE2, and publish the results without pooling evidence classes. The fresh
      W6 result meets the Kitty memory target; the SE result publishes both the
      throughput improvement and the unfavourable SE2 burst retention. W7
      remains explicitly deferred because of its approximately 50-hour
      exclusive-machine cost.
- [x] Publish v0.12.0, verify its provenance from a clean environment, and run
      the documented post-publish package-channel checks.

## Archived First Prototype Checklist

## Core Readiness

- [x] Confirm the stack and scope boundaries.
- [x] Stand up the minimal runnable skeleton (owned core, PTY, render seam).
- [x] Owned terminal model and owned parser.
- [x] PTY shell command path and host-terminal interactive mode.
- [x] Core compatibility primitives: printing, cursor movement, SGR, erase,
      scrollback, alternate screen, save/restore, scroll regions, bracketed
      paste, RI, IL/DL, SU/SD, DECOM, RIS/DECSTR, ICH/DCH, ECH, REP, tab
      stops, CNL/CPL, DA reply.
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
- [x] Add launch-scoped `--app-id` / `--class` aliases for the Wayland
      `app_id` and X11 `WM_CLASS` class while preserving
      `io.unfinished_works.odytty` by default, then advertise
      `X-TerminalArgAppId=--app-id=` in the desktop entry.
- [x] Add `--hold` with explicit true/false forms for the initial local command,
      a truthful in-pane exit-status line, keypress dismissal through the normal
      shell-exit cascade, and `X-TerminalArgHold=--hold` desktop metadata.
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
- [x] AppImage artifact: every tag publishes the smoke-tested
      `odytty-x86_64.AppImage` alias and a version-pinned twin in `SHA256SUMS`.

## Deferred Until After the First Prototype

- [x] Tabs and multiple local shell sessions: landed as the first multi-context
      slice. Splits/panes within a window have since landed too (see Stage 8);
      Unix detachable sessions also landed. Profiles and cross-session
      multiplexing remain deferred.
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
      the `command-palette` action (`Ctrl+Shift+P` by default, rebindable).
      Selecting history/directories types text
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
      shared scorer. Exposed as the `connection-manager` action (`Ctrl+Shift+S`
      by default, rebindable). `↑`/`↓` select, Enter quick-connects
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
Explicit non-goals: plugin systems, AI features, dashboards, rich nonstandard
workflows, heavy effects that compromise readability or latency, and platform
support beyond the shipped Linux, Windows, and macOS targets.

- [ ] Windows on-device hardening. Interactive Windows behaviour has now been
      validated on-device across several passes for the 0.7.0 cycle (local
      shells, tabs, splits, selection/copy-paste, minimize/restore, wheel
      routing, shell integration, and clickable paths incl. inline images);
      CI additionally proves compile + automated tests on every push. A bounded
      v0.10.0 post-release package smoke pass completed without a reported
      blocker; the v0.11.1 and v0.12.0 Scoop upgrade/install paths were
      confirmed on real hardware, and the application launched and ran after
      each update. Broader Windows hardware and application coverage remains
      ongoing. It remains a newer target with a lower polish bar than Linux. A
      child-process waiter now
      closes the pseudoconsole when a shell exits naturally, so the tab follows
      the normal reader-EOF teardown path.
- [ ] Windows default-terminal handoff. OdyTTY can be launched directly and
      hosts ConPTY shells, but it does not implement the Windows
      default-terminal handoff protocol (DelegationConsole/DelegationTerminal
      registration), so it cannot be selected as the system default terminal
      that Explorer/other apps hand consoles to. Future work; needs COM/registry
      registration and on-device validation.
- [ ] Clear the evidence gate for broad daily-driver claims against comparable
      terminal emulators. Published W6 idle and SE1/SE2 results cover memory and
      software-endpoint throughput on one machine; the real-application matrix,
      unavailable optical workloads, W7, and the external daily-driver program
      remain open evidence.
