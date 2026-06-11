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

Ghostty and other mature terminals are compatibility references, not
implementation sources. Visual ambition stays open, but every effect and
workflow layer must be isolated from terminal correctness and bounded by
readability and performance.

## Ownership Boundary

Every byte from the PTY to the glyph quad passes through OdyTTY-owned code.
The owned path is not aspirational — it is in production.

### What OdyTTY owns

**Linux PTY layer** (`src/pty.rs`). PTY allocation via `openpt`/`grantpt`/
`unlockpt`/`TIOCGPTPEER` through `rustix`. Child spawn as a new session leader
with a controlling terminal. `TIOCSWINSZ` resize. Nonblocking reader and writer
via file-descriptor clones. Child reaping on drop. All PTY semantics are
OdyTTY's own code; `portable-pty` and `crossterm` are gone from the dependency
tree.

**VT parser** (`src/parser/`). A clean-room two-layer pipeline built solely
from primary specifications (vt100.net DEC ANSI state diagram, ECMA-48, xterm
`ctlseqs`). Neither `vte` source nor other terminal implementations were
consulted during design or implementation. `vte` is absent from `Cargo.toml`
and `Cargo.lock`; the owned parser is the sole production parser.

- **Layer 1 — segmenter** (`src/parser/segmenter.rs`). Walks input in Ground
  state, splits maximal printable-text runs from single control scalars/bytes,
  and owns all UTF-8 decoding — including partial-codepoint carry across
  arbitrary `advance()` chunk boundaries. C1 scalars arriving via the two-byte
  UTF-8 encoding execute uniformly regardless of chunk splits.
- **Layer 2 — control state machine** (`src/parser/machine.rs`). A byte-only
  automaton driven by `classify(byte) → ByteClass` (~13 classes) and a single
  flat `match (state, class) → Action` discriminator. Not a per-state-method
  decomposition; not a `[state][256]` data table. UTF-8 is absent because
  Layer 1 resolved it.
- **Action vocabulary** (`src/parser/action.rs`). Pure values emitted by the
  state machine, keeping it sink-agnostic.
- **Driver** (`src/parser/driver.rs`). Stitches Layer 1 and Layer 2, buffers
  OSC (128 KiB cap) and APC (1 MiB cap, drop-not-truncate), and adapts the
  action stream to the `VtDispatch` sink. DCS payloads pass through as a
  streaming hook/put/unhook sequence without buffering in the parser.

**Terminal state machine** (`src/core/`). Screen grid (primary + alternate),
lazy scrollback with logical-line storage, scroll regions, resize/reflow, all
VT sequence semantics. The `Screen` type implements `VtDispatch` and is the
parser's sole sink. The core module never imports windowing, GPU, or rendering
code.

**Renderer geometry** (`src/grid.rs`). Builds the CPU vertex buffer consumed by
the GPU pipeline: background quads, per-glyph quads with bearing-aware ink
bounds, underline/strikethrough quads, cursor quads, search/selection highlights.
Wide-glyph (width-2) quads span two cell columns.

**GPU shader pipeline** (`src/native/gpu.rs`). The `wgpu` render pass, pipeline
descriptor, vertex/uniform layout, and WGSL shader source. Text coverage
correction (gamma uniform) and optional dual-source blending for subpixel AA
live here.

**Graphics protocol decode and placement pipeline** (`src/graphics/`,
`src/core/graphics_routing.rs`). See the Graphics Architecture section.

### What OdyTTY deliberately does not own

These sit below the product line. Re-owning them would add maintenance burden
without adding identity or capability. The boundary is a deliberate design
decision, not a trade-off pending revisitation.

| Concern | Crate |
|---------|-------|
| Font file parsing and glyph rasterization | `ab_glyph` |
| GPU API and device management | `wgpu` |
| Window creation and event loop | `winit` |
| Clipboard transport | `arboard` |
| Unicode character-width tables | `unicode-width` |

## Graphics Architecture

Kitty graphics protocol and Sixel both land on OdyTTY-owned APC/DCS plumbing.
Because OdyTTY owns its parser, APC strings (Kitty) and DCS hook/put/unhook
sequences (Sixel) surface to the terminal core as first-class events. No
external parser dependency is needed.

**ImageStore** (`src/graphics/store.rs`). A bounded LRU store for decoded
RGBA8 images keyed by OdyTTY-internal ids. Default limits: 64 MiB decoded
bytes and 1024 images. Insertions evict least-recently-used records until the
new image fits. The store is renderer-independent — decoded pixel data lives in
CPU memory until the GPU image layer uploads it.

**Placement scene** (`src/graphics/placement.rs`). Cell-anchored placement
records associating a stored image with a terminal grid position, source
rectangle, display cell dimensions, anchor pixel offset, z-index, and
generation counter. Placements scroll with terminal content and project into
the scrollback viewport. Primary and alternate screens maintain independent
placement scenes; alternate-screen entry does not disturb primary placements.

**Render order** (canonical, implemented in `src/native/gpu.rs`):
1. Cell background quads (all cells)
2. Negative-z image placements (`z < 0`) — appear behind text
3. Glyph, decoration, cursor, and overlay quads
4. Non-negative-z image placements (`z ≥ 0`) — appear in front of text

This is the order specified by the Kitty graphics protocol. Placements with
equal z-index keep transmission order within each draw segment. The text
pipeline is re-bound between the two image segments so the render pass is
always in a defined state when switching between image and cell geometry.

**Kitty graphics protocol.** Actions `a=t` (transmit), `a=T` (transmit and
display), `a=p` (display existing by id), `a=d` (delete), and `a=q` (query)
are supported. Formats `f=24` (raw RGB), `f=32` (raw RGBA), and `f=100` (PNG
still image) are supported. Transports `t=d` (direct), `t=f` (file), `t=t`
(temp file), and `t=s` (POSIX shared memory) are supported; file transports
carry security restrictions documented in `docs/graphics.md`. Chunked transfer
(`m=1`/`m=0`) is supported under a 96 MiB encoded-payload cap. Placement ids,
z-index, source-rectangle crop, cell-box scaling, and anchor pixel offset are
all wired through. Animation (`a=f`, `a=a`) and Unicode placeholder rendering
(`U=1`) are not supported.

**Sixel.** The complete DCS `q` data language is supported: raster attributes,
RGB and HLS color introducers, repeat introducer, VT340 16-color default
palette, transparent background (`P2=1`). DECSDM (private mode 80) controls
cursor-after-sixel behavior: reset (default) moves cursor to the row below the
image; set keeps the cursor in place. Hard caps: 10,000 × 10,000 pixels or
40 million total pixels.

**iTerm2 protocol.** Deferred indefinitely. No current code handles it.

See `docs/graphics.md` for user-facing protocol detail, security rationale, and
examples.

## Configuration Architecture

Settings follow a three-level precedence chain, lowest to highest:

1. **Built-in defaults** — compiled-in values for every setting.
2. **Config file** — `$XDG_CONFIG_HOME/odytty/odytty.conf` (falling back to
   `~/.config/odytty/odytty.conf`). A missing or unreadable file is silently
   skipped; malformed lines and unknown keys warn to stderr and are skipped.
3. **Environment variables** (`ODYTTY_*`) — always win over both defaults and
   the config file.

The config format is a dependency-free `key = value` text file with `#`
comments, mirroring every runtime knob. See `docs/runtime-knobs.md` for the
full key reference and `docs/odytty.conf.example` for an annotated example.

**Live reload.** The native app polls the resolved config path at a one-second
cadence from the existing event-loop wake path, without a watcher thread or
`inotify` dependency. When the file changes, new settings are applied
immediately. Env-pinned keys are preserved: any setting that was supplied via
`ODYTTY_*` at startup is held at that value for the session lifetime; live
reload cannot override it.

Reloadable settings: `theme`, `visual`, `font`, `font_family`, `font_size`,
`text_gamma`, `subpixel`, `cursor_style`, `cursor_blink`, `keybinds`. Font
path, family, size, and subpixel changes rebuild the glyph atlas and cell
metrics, recompute the terminal grid, and push PTY `TIOCSWINSZ` through the
same path used for HiDPI scale changes. A bad rewrite is a no-op; a deleted
config file keeps the current settings; reload never panics.

**Startup-only setting.** `native_autoclose_ms` is not reloadable. Changing a
lifecycle smoke timer mid-session would make manual and automated test behavior
ambiguous.

## Interaction Architecture

### OSC 8 Hyperlinks

OSC 8 (`ESC ] 8 ; params ; uri ST`) interns each unique `(uri, osc-id)` pair
into a compact `HyperlinkTable` in the terminal core, assigning it a stable
`LinkId` (`NonZeroU32`). Every printed cell records the active `LinkId` (or
none); no URI string is stored per cell. URI payloads are capped at 2083 bytes;
longer URIs are silently dropped so a hostile process cannot grow the table
with arbitrarily large strings.

The native layer tracks the hovered `LinkId` from cursor position. On hover,
all visible cells sharing that id have their underline attribute set in the
render snapshot — no change to the terminal model state. On explicit
Ctrl+click (or Ctrl+Shift+click when mouse reporting is active), the native
app calls `xdg-open` with the URI as a direct argument after verifying its
scheme against an allowlist (`http`, `https`, `file`, `mailto`). No shell
interpolation occurs. Links are never followed automatically; the allowlist
check and the `xdg-open` call only happen on deliberate user action.

### Kitty Keyboard Protocol

OdyTTY implements the Kitty keyboard protocol as a progressive enhancement on
top of its existing DEC/xterm key encoder. The terminal core tracks active
keyboard protocol flags per screen buffer: `CSI > flags u` pushes the current
flags and applies a new set, `CSI < n u` pops saved states, `CSI = flags ;
mode u` sets/adds/removes flags, and `CSI ? u` replies with `CSI ? flags u` on
the host-output path. The stack is bounded at 16 entries with oldest-entry
eviction, and `RIS`/`DECSTR` reset it. Primary and alternate screen maintain
separate Kitty keyboard flag stacks so full-screen TUIs cannot leak negotiated
keyboard behavior back to the shell prompt.

The native layer still resolves terminal-local key bindings before encoding a
key for the PTY. With no Kitty flags active, OdyTTY emits the exact legacy
bytes. With disambiguation active, ambiguous control/Alt text and named keys
use CSI-u forms with the Kitty `+1` modifier encoding; report-all uses the same
encoder for ordinary text and recovery keys. Event-type reporting is a later
extension.

## Scope

v0 is complete. Stages 1 through 4.5 are substantially complete. The parity
half of Stage 6 (graphics protocols, wide glyphs, subpixel AA, text quality) is
substantially complete. Stage 5 (file-based configuration with live reload) has
its first stable layer.

**In scope and delivered:**
- Owned Linux PTY layer and owned VT parser (clean-room from primary specs)
- File-based configuration with live reload; env always wins
- Broad escape-sequence compatibility (SGR, alternate screen, mouse modes,
  wide characters, combining marks, and more)
- Kitty graphics protocol: `a=t/T/p/d/q`, `f=24/32/100`, `t=d/f/t/s`,
  chunked transfer, placement ids, z-index, source crop, cell scaling, pixel
  offset, delete specifiers
- Sixel graphics: full decoder, terminal integration, DECSDM, GPU image rendering
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
- OSC 8 hyperlinks: hover underline, Ctrl+click open via `xdg-open`, scheme
  allowlist (`http`/`https`/`file`/`mailto`), never auto-opened from input
- Right-edge scroll position indicator
- Configurable cursor shapes and blink policy (DECSCUSR + settings)
- Configurable terminal-local key bindings
- Window title from OSC 0/2; DECSET 1004 focus reporting
- Keyboard mode-awareness: DECCKM, keypad modes, modified named keys, Kitty
  keyboard protocol disambiguation/report-all flags
- Lazy scrollback re-wrap and resize fast paths
- Theme system (plain baseline, Odyssey presets); optional ambient visual effect

**Out of scope until foundations are stronger:**
- Kitty animation (`a=f`, `a=a`) and Unicode placeholder rendering
- iTerm2 graphics protocol
- Ligature/stylistic-set shaping (strategy decided; implementation deferred
  until a specific trigger condition is met)
- Tabs, panes, sessions, profiles, and multiplexing
- Shell integration beyond basic PTY behavior
- Plugin systems, AI features, command palettes, rich dashboards, or nonstandard
  terminal semantics
- Heavy animation or effects that compromise readability or latency
- Broad cross-platform support beyond Linux-first validation
- Packaging, CI, release builds

## Stack

The stack is: Rust, Linux-first, `winit` (windowing), `wgpu` (GPU/Vulkan),
`ab_glyph` (font rasterization), `unicode-width` (cell widths), `arboard`
(clipboard), `rustix` (PTY/termios), `png` (PNG decode for Kitty `f=100`).

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
