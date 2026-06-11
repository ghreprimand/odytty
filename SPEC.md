# OdyTTY — Spec

## The Spark

In the spirit of OdysseyOS, I feel like I need my own terminal: Odyssey Terminal. Ghostty is the mark right now, but I want to explore whether we can make something more visually interesting, flashy, and pretty while still being reliable and solid.

The open questions are part of the idea: what would make Odyssey Terminal stand out from terminals like Ghostty or Konsole? Is it richer visual effects, better themes and color schemes, a stronger sense of identity, or features that make the terminal feel more alive without getting in the way?

The project should pursue genuinely original terminal work rather than forking or skinning an existing terminal. If that path cannot produce something interesting, useful, and reliable enough to justify itself, the project should be scrapped or rethought instead of becoming a themed version of another terminal.

## Concept

Odyssey Terminal is a reliable terminal emulator with an OdysseyOS visual identity, exploring how motion, themes, effects, and interface details can make command-line work feel more alive without weakening core terminal behavior. Its central question is whether a terminal can add useful, nonstandard features and a richer experience while staying fast, solid, and practical for daily use.

## The Case

Odyssey Terminal is worth exploring because the terminal is a daily operating surface, not just a utility, and OdysseyOS needs one that carries its own visual identity without compromising trust. It is for the operator who wants command-line work to feel more expressive, polished, and alive while remaining dependable enough for real use. The friction it removes is the gap between solid existing terminals and a more personal, visually distinctive environment: instead of accepting either reliability with generic presentation or flashiness that risks distraction, the project tests whether both can coexist. Scope should stop before novelty damages terminal fundamentals; speed, compatibility, input correctness, readable text, stable rendering, and predictable behavior matter more than effects, themes, or nonstandard features.

## Build Direction

The project owns its full byte path from PTY to glyph quad. Shell process and
PTY handling, escape-sequence parsing, input mapping, text layout, renderer
geometry, and shaders are OdyTTY-originated code. The Odyssey experience layer
(themes, visual effects, and identity treatments) sits above that core; visual
experiments must not destabilize terminal correctness and must be
off-switch-able at all times.

The owned byte path is now real: `src/pty.rs` owns the Linux PTY
(openpt/grantpt/unlockpt/TIOCGPTPEER via `rustix`); `src/parser/` holds the
clean-room OdyParser (two-layer DEC ANSI pipeline built from primary
specifications — vt100.net diagram, ECMA-48, xterm `ctlseqs`); `src/core/`
holds the terminal screen model; `src/grid.rs` builds renderer geometry; the
shaders live in `src/native/gpu.rs`. External crates remain intentional
below-product-line tools: `ab_glyph` for font rasterization, `wgpu` for GPU API
access, `winit` for windowing, `arboard` for clipboard transport, and
`unicode-width` for character cell widths. They do not own terminal semantics.

Ghostty and other mature terminals are compatibility references, not
implementation sources. Visual ambition stays open, but every effect and
workflow layer must be isolated from terminal correctness and bounded by
readability and performance.

## Ownership Boundary

The ownership boundary is drawn at the same line the strongest independent
terminals draw. OdyTTY owns:

- Linux PTY allocation, child spawn, resize, and I/O (`src/pty.rs`)
- The VT escape-sequence parser (`src/parser/`)
- The terminal state machine: screen grid, scrollback, alternate screen, scroll
  regions, resize/reflow, all escape-sequence semantics (`src/core/`)
- Renderer geometry and vertex layout (`src/grid.rs`)
- The GPU shader pipeline (`src/native/gpu.rs`)
- The graphics protocol decode and placement pipeline (`src/graphics/`,
  `src/core/graphics_routing.rs`)

OdyTTY deliberately does not own:

- Font file parsing and glyph rasterization (`ab_glyph`)
- GPU API and device management (`wgpu`)
- Window creation and event loop (`winit`)
- Clipboard transport (`arboard`)
- Unicode character-width tables (`unicode-width`)

This boundary is deliberate, not a trade-off pending revisitation. These crates
sit below the product line; re-owning them would add maintenance without adding
identity or capability.

## Scope

v0 is complete. The prototype proved the core loop: a native window opens a real
local shell, renders GPU-backed monospaced text, handles keyboard input, resize,
paste, mouse selection/copy, scrollback navigation, and cursor rendering.

Stages 1 through 4.5 are substantially complete. The parity half of Stage 6 is
substantially complete. The current focus is completing the Kitty graphics
protocol MVP and moving toward Stage 5 (file-based configuration).

**In scope and delivered:**
- Owned Linux PTY layer and owned VT parser (clean-room from primary specs)
- Broad escape-sequence compatibility (SGR, alternate screen, mouse modes,
  wide characters, combining marks, and more)
- Sixel graphics: full decoder, terminal integration, GPU image rendering
- Kitty APC routing seam in place; direct still-image MVP in progress
- HiDPI-correct text rasterization across scale factors
- Wide-glyph 2-cell atlas slots; bearing-aware glyph quad geometry
- Optional subpixel anti-aliasing and tunable text gamma/contrast
- Configurable font family and bold/italic style faces
- Full text attribute rendering: bold, dim, italic, underline, strikethrough,
  inverse, hidden
- Scrollback search with match navigation and highlights
- Refined selection: double-click word, triple-click line, drag-scroll,
  scrollback-aware anchors
- Clipboard hardening: chunked paste, bracketed-paste sanitization, PRIMARY
  selection
- Right-edge scroll position indicator
- Configurable cursor shapes and blink policy (DECSCUSR + settings)
- Configurable terminal-local key bindings
- Window title from OSC 0/2; DECSET 1004 focus reporting
- Keyboard mode-awareness: DECCKM, keypad modes, modified named keys
- Lazy scrollback re-wrap and resize fast paths
- Theme system (plain baseline, Odyssey presets); optional ambient visual effect

**Out of scope until foundations are stronger:**
- Ligature/stylistic-set shaping (strategy decided; implementation deferred
  until a specific trigger condition is met)
- File-based configuration (Stage 5)
- Tabs, panes, sessions, profiles, and multiplexing (Stage 7)
- Shell integration beyond basic PTY behavior
- Plugin systems, AI features, command palettes, rich dashboards, or nonstandard
  terminal semantics
- Heavy animation or effects that compromise readability or latency
- Broad cross-platform support beyond Linux-first validation
- Packaging, CI, release builds (Stage 8)

## Stack

The stack is: Rust, Linux-first, `winit` (windowing), `wgpu` (GPU/Vulkan),
`ab_glyph` (font rasterization), `unicode-width` (cell widths), `arboard`
(clipboard), `rustix` (PTY/termios).

The terminal core is a distinct boundary from the native app. The `core` module
never imports windowing, GPU, or rendering code; it consumes VT bytes via the
owned parser and exposes a `Snapshot` for the renderer to consume. The native
module owns the `winit` event loop, `wgpu` surface, glyph atlas, grid vertex
builder, and image layer, consuming core snapshots through a narrow seam.

Text is cell-based: each codepoint occupies one or two columns (`unicode-width`
consistent with core), and all coordinate systems are per-cell. The glyph atlas
uses one monospace face for regular text, with bold/italic/bold-italic faces
loaded when discovered by filename convention. Ligatures and complex shaping are
not implemented; each atlas entry is a single character rasterized into its cell
or two-cell slot.
