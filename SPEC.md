# OdyTTY — Spec

## The Spark

OdysseyOS needs its own terminal — one that carries its own visual identity
and has features worth having, built from the ground up rather than skinned
from something else. The open questions that drive the project: can a terminal
emulator add richer visual effects, better themes, a stronger sense of identity,
and features that make command-line work feel more alive — while staying fast,
correct, and trustworthy for real daily use?

The project pursues genuinely original terminal work. If that path cannot
produce something interesting, useful, and reliable enough to justify itself,
it should be rethought rather than becoming a re-skinned version of another
terminal.

## Concept

Odyssey Terminal is a reliable terminal emulator with an OdysseyOS visual identity, exploring how motion, themes, effects, and interface details can make command-line work feel more alive without weakening core terminal behavior. Its central question is whether a terminal can add useful, nonstandard features and a richer experience while staying fast, solid, and practical for daily use.

## The Case

Odyssey Terminal is worth exploring because the terminal is a daily operating surface, not just a utility, and OdysseyOS needs one that carries its own visual identity without compromising trust. It is for the operator who wants command-line work to feel more expressive, polished, and alive while remaining dependable enough for real use. The friction it removes is the gap between solid existing terminals and a more personal, visually distinctive environment: instead of accepting either reliability with generic presentation or flashiness that risks distraction, the project tests whether both can coexist. Scope should stop before novelty damages terminal fundamentals; speed, compatibility, input correctness, readable text, stable rendering, and predictable behavior matter more than effects, themes, or nonstandard features.

## Privacy & Data Posture

OdyTTY runs entirely on the local machine. It collects no telemetry and has no
analytics, crash-reporting, update-check, or "product improvement" data path of
any kind — there is no network client in the terminal to disable, because none
is built. There is no account, no sign-in, no cloud sync, and no server-side
component; settings, themes, and scrollback never leave the local filesystem.
Configuration is a plain local `odytty.conf` the user owns and can read in full.
The source is open under the GPL-3.0, so the absence of any data collection is
verifiable rather than merely promised.

This is a durable product stance, not a default to be flipped: any future
feature that would transmit data off the machine is out of scope by charter.
The one network-capable action — Ctrl+click to open a hyperlink — is explicit,
user-initiated, routed through `xdg-open`, and gated by a scheme allowlist;
links are never opened from terminal output automatically.

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
bounds, underline/strikethrough/decoration quads, cursor quads, search/selection
highlights. Wide-glyph (width-2) quads span two cell columns. Extended underline
decoration is rendered per-cell from the `UnderlineStyle` enum (`None`,
`Straight`, `Double`, `Curly`, `Dotted`, `Dashed`): double emits two parallel
quads offset by a fraction of cell height; curly emits a stepped square-wave
approximation confined within the cell. Underline color is taken from the cell's
`underline_color` attribute when set, or falls back to the effective foreground;
this is resolved in the vertex builder, not in the shader.

**GPU shader pipeline** (`src/native/gpu.rs`). The `wgpu` render pass, pipeline
descriptor, vertex/uniform layout, and WGSL shader source. Text coverage
correction (gamma uniform) and optional dual-source blending for subpixel AA
live here.

**DCS query surface** (`src/core/screen/query.rs`). XTGETTCAP (`DCS +q`)
and DECRQSS (`DCS $q`) capture ride the same parser hook/put/unhook seam used
for graphics DCS payloads — no parser changes required. `dcs_query_hook`
dispatches on the intermediate byte (`+` vs `$`) and returns a typed
`DcsQueryCapture`; `dcs_query_put` buffers bytes up to 4 KiB; the screen
dispatches the result via `dispatch_dcs_query`. XTGETTCAP answers only the
conservative truth set the terminal can currently claim (`TN`, `Co`, `RGB`);
unknown names receive the xterm invalid response. DECRQSS reports live SGR
(including extended underline styles and underline color), DECSCUSR, and
DECSTBM and DECSCA protection state (`"q`); unimplemented selectors respond
invalid per xterm convention.

**Rectangle operations** (`src/core/screen/rect.rs`). DECCRA, DECFRA, DECERA,
and DECSERA are implemented. DECCRA uses a snapshot-copy strategy: the source
cells are copied into a temporary buffer before the destination write, so
overlapping regions produce correct results without requiring a scratch page.
Rectangle coordinates are 1-based, inclusive, and clamp to the visible page.
With DECOM active, row coordinates are relative to the active vertical scroll
margins; columns remain screen-relative (horizontal margins are not implemented).
After every rectangle write, affected rows are sanitized via `sanitize_wide_row`:
any wide glyph whose pair is severed at a rectangle boundary has both cells
replaced with the current blank, preventing orphan continuation cells. The `Cell`
protection bit is set by DECSCA (`CSI Ps " q`): Ps=1 protects, Ps=0/2 clears.
The bit is omitted from `Cell`'s `Debug` output when `false` so existing oracle
golden fixtures remain stable across code changes that add new cells.

DECCARA and DECRARA apply attribute changes to a rectangle (or stream extent
per DECSACE). `RectAttrMask` collects the requested SGR codes: `change_rect_attr_mask`
builds a set/clear mask for DECCARA (0 resets all four, 1/4/5/7 set bold/
underline/blink/inverse, 22/24/25/27 clear them); `reverse_rect_attr_mask`
builds a toggle mask for DECRARA (0 toggles all, 1/4/5/7 each flip their bit).
Both operate on bold, plain underline, blink, and inverse only; `4:x` extended
underline subparameters are silently ignored per xterm; the DECSCA protection
bit is never touched by either op per xterm convention. DECSACE (`CSI Ps * x`)
selects stream (Ps=0/1) or exact-rectangle (Ps=2) extent for subsequent DECCARA/
DECRARA calls. The `rect_attr_extent` field lives on each `Screen`; it is carried
across alternate-screen entry and exit (same extent restored), and both RIS and
DECSTR reset it to `Stream` (the default). The `blink` field added to `Attrs`
by RC2 follows the same `Debug` omission policy as `protected`: `Attrs::fmt`
omits `blink: false` so oracle golden fixtures remain stable.

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

**In-app writeback.** The settings panel is a presentation-only overlay until
the user explicitly saves. `Ctrl+S` writes only changed rows back to the
resolved config file, preserving comments, blank lines, key order, and
unknown/future keys. Missing changed keys are appended under an OdyTTY settings
section. Saves use a same-directory temporary file followed by rename; OdyTTY
does not truncate the config file in place.

**Live reload.** The native app polls the resolved config path at a one-second
cadence from the existing event-loop wake path, without a watcher thread or
`inotify` dependency. When the file changes, new settings are applied
immediately. Env-pinned keys are preserved: any setting that was supplied via
`ODYTTY_*` at startup is held at that value for the session lifetime; live
reload cannot override it.

Reloadable settings: `theme`, `visual`, `font`, `font_family`, `font_size`,
`text_gamma`, `subpixel`, `cursor_style`, `cursor_blink`, `keybinds`,
`osc52_read`, `stem_darken`, `min_contrast`. Font path, family, size, and subpixel changes rebuild the glyph
atlas and cell metrics, recompute the terminal grid, and push PTY `TIOCSWINSZ`
through the same path used for HiDPI scale changes. A bad rewrite is a no-op; a
deleted config file keeps the current settings; reload never panics.

**Startup-only setting.** `native_autoclose_ms` is not reloadable. Changing a
lifecycle smoke timer mid-session would make manual and automated test behavior
ambiguous.

**CLI introspection.** Two startup flags (`src/cli.rs`) print information and
exit without opening a window: `--list-themes` prints the names of all
available built-in themes; `--show-config` prints the active settings resolved
from defaults, config file, and environment, together with per-setting
descriptions. Both are driven by the same `SettingInfo` table used by the
in-app settings panel, so they stay in sync with the runtime knob surface
automatically.

**Settings search.** Typing `/` while the in-app settings panel is open
filters the displayed roster by name, config key, description, or group label.
`Esc` once clears the filter; a second `Esc` closes the panel. Theme-picker
search is a separate future slice.

**First-run onboarding.** On first launch — detected by the absence of a
config file, or overridden with `ODYTTY_ONBOARDING=1` — OdyTTY shows a
welcome overlay with the core keyboard shortcuts before the shell starts. All
shortcut labels are read live from the active bindings at display time, so the
card reflects any prior customization correctly. The overlay is dismissed with
`Enter`, `Esc`, or `Space`. First-run state is stored as the config file's
existence — there is no separate flag file, no telemetry, and no account
requirement; the first-run state is therefore controlled entirely by a plain
local file the user owns.

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

### OSC 52 Clipboard

OSC 52 clipboard writes (`ESC ] 52 ; selector ; base64 ST`) decode bounded
UTF-8 text in the terminal core and surface a native clipboard request through
an explicit queue. Selectors `c` and `p` target the regular clipboard and
PRIMARY selection; an empty selector defaults to the regular clipboard. Decoded
payloads are capped at 64 KiB and invalid base64 or non-UTF-8 payloads are
dropped without grid leakage or a host reply.

OSC 52 reads (`... ; ? ST`) are disabled by default because replying with
clipboard contents lets a remote program exfiltrate local data. With the
default `osc52_read = off`, the core queues no request and sends no reply.
Only an explicit `osc52_read = on` / `ODYTTY_OSC52_READ=on` opt-in lets native
clipboard reads produce an OSC 52 reply.

### Dynamic Colors

OdyTTY supports xterm-style runtime color controls: OSC 10/11/12 set and query
default foreground, background, and cursor colors; OSC 4 sets and queries
palette entries; OSC 104/110/111/112 reset palette/default overrides. Runtime
colors live in terminal state and are included in render snapshots. The active
theme remains the base presentation; resets return to that theme rather than
rewriting the theme itself.

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
encoder for ordinary text and recovery keys. Event-type reporting uses the
modifier subfield for repeat (`:2`) and release (`:3`) events, with release
bytes emitted only when negotiated. Alternate-key reporting adds shifted and
base-layout key-code subfields for character CSI-u events where OdyTTY can
derive them from the logical key. Associated-text reporting appends printable
generated text code points as the third CSI-u parameter when combined with
report-all.

### Synchronized Output (DEC Private Mode 2026)

DEC private mode 2026 (`DECSET ?2026h` / `DECRST ?2026l`) is tracked as a
boolean field on the terminal screen; `RIS` and `DECSTR` both reset it to off.
`DECRQM` (`CSI ? 2026 $ p`) reports the current mode status through the normal
mode-query path in `src/core/screen/ops.rs`.

The native layer owns the presentation-hold policy. `SynchronizedOutputHold`
(`src/native/app.rs`) monitors the core mode flag and, while it is set, defers
GPU content uploads — the terminal model continues to advance and process PTY
bytes without interruption, but grid snapshots are not uploaded or rendered.
After `SYNCHRONIZED_OUTPUT_TIMEOUT` (150 ms, `src/native/app.rs:56`), the hold
is released unconditionally and will not re-engage until the application resets
the mode and sets it again. The timeout deadline is registered with the event
loop so the release fires promptly at the deadline without additional polling.
A crashed application that never sends the DECRST therefore cannot freeze the
display for longer than 150 ms. Cursor blink remains live during the hold: the
hold path calls `update_held_cursor_frame`, which re-renders the cursor blink
delta against the last presented snapshot without touching grid content.

### Semantic Prompt Marking (OSC 133) — foundation

OSC 133 (`ESC ] 133 ; <letter> [; k=v ...] ST`) marks the prompt/command/output
boundaries a shell-integration script emits: `A` prompt start, `B` prompt end /
command-input start, `C` command-output start, `D[;exit]` command end. The
parser surfaces this through the same `dispatch_osc` ident seam as OSC 7/8/52;
`src/core/prompt_marks.rs` owns the `PromptKind` model and the `handle_osc133`
setters. Aux `k=v` keys are accepted-and-ignored; the `D` exit code is parsed
digits-only into `Option<i32>` (absent / non-numeric / overflow → `None`), and
malformed payloads are consumed without panic or host reply.

Marks are **logical-line-anchored**: a mark is stored as `Option<PromptKind>`
on the cursor's logical line (carried on the first physical row), so it survives
scroll-out into scrollback and width-changing reflow. `RIS`, `ED 2/3`, `EL 2`,
resize, and alternate-screen transitions clear or re-anchor marks as the rows
they sit on change. A read-only poll API (`prompt_mark_at`,
`take_prompt_marks_changed`) exposes the marks; the change flag is conservative
(fires on any stamp, clear, or reposition).

This is a **foundation slice with no render consumer**: `prompt_mark` is never
read by the render path and is deliberately absent from `Snapshot`, so the
plain renderer is byte-identical with or without OSC 133 in the stream. The
command-aware UX that consumes these marks (jump-to-prompt, per-command output
selection, success/fail gutter) is separate downstream work.

## Scope

v0 is complete. Stages 1 through 4.5 are substantially complete. The parity
half of Stage 6 (graphics protocols, wide glyphs, subpixel AA, text quality) is
substantially complete. Stage 5 (file-based configuration with live reload) has
its first stable layer.

**In scope and delivered:**
- Owned Linux PTY layer and owned VT parser (clean-room from primary specs)
- File-based configuration with live reload; env always wins
- Broad escape-sequence compatibility (SGR including 256-color, truecolor
  semicolon and colon forms (`38:2::r:g:b`, `48:2::r:g:b`), alternate screen,
  mouse modes, wide characters, combining marks, and more)
- Kitty graphics protocol: `a=t/T/p/d/q`, `f=24/32/100`, `t=d/f/t/s`,
  chunked transfer, placement ids, z-index, source crop, cell scaling, pixel
  offset, delete specifiers
- Sixel graphics: full decoder, terminal integration, DECSDM, GPU image rendering
- HiDPI-correct text rasterization across scale factors
- Wide-glyph 2-cell atlas slots; bearing-aware glyph quad geometry
- Optional subpixel anti-aliasing and tunable text gamma/contrast
- Configurable font family and bold/italic style faces with synthetic fallback
  (double-strike bold, 12° shear italic) when real faces are absent
- Full text attribute rendering: bold, dim, italic, extended underline styles
  (`SGR 4:0`–`4:5`, straight/double/curly/dotted/dashed), underline color
  (`SGR 58`/`59`, colon and semicolon forms), strikethrough, inverse, hidden
- Scrollback search with match navigation and highlights
- Refined selection: double-click word, triple-click line, drag-scroll,
  scrollback-aware anchors
- Clipboard hardening: chunked paste, bracketed-paste sanitization, PRIMARY
  selection, OSC 52 write support, and default-deny OSC 52 read policy
- OSC 8 hyperlinks: hover underline, Ctrl+click open via `xdg-open`, scheme
  allowlist (`http`/`https`/`file`/`mailto`), never auto-opened from input
- Dynamic colors: OSC 10/11/12, OSC 4 palette entries, and reset/query support
- Right-edge scroll position indicator
- Configurable cursor shapes and blink policy (DECSCUSR + settings)
- Configurable terminal-local key bindings; in-app keybinding editor in the settings panel (browse all 12 bindable actions, capture a new chord by pressing a row, `Backspace` resets to default, `R` resets all, conflict prompt on clash, writes to `odytty.conf` via the preservation-first writeback path; `ODYTTY_KEYBINDS` hand-editing is byte-identical)
- Keyboard copy mode (`copy_mode` action, off by default): a keyboard-driven
  scrollback selection mode. `h/j/k/l`, `w/b/e`, `0/^/$`, `gg/G` move the
  caret; `v` and `V` start character and line selection; `y` / Enter yanks the
  selected text to the clipboard; `Esc`/`q` cancel. Arrow keys, PageUp/Down,
  Home/End, and `Ctrl-u/d/b/f` paging are also bound. Off by default — nothing
  changes until the action is bound via `ODYTTY_KEYBINDS` and invoked.
- Mouse reporting: tracking modes 9 (X10), 1000 (normal), 1002 (button-event),
  1003 (any-event), focus reporting (1004); encodings 1005 (UTF-8 coordinate
  extension), 1006 (SGR decimal), 1015 (urxvt decimal); legacy byte protocol
  as default. Only one tracking mode and one encoding mode are active at a time;
  `DECRST` clears back to the default. SGR-pixel encoding (mode 1016) is
  supported core-side as of MS1: `DECSET`/`DECRST`/`DECRQM` are wired, and a
  pure pixel encoder emits `CSI < Cb ; Px ; Py M|m` from caller-owned 1-based
  pixel coordinates. As of MS2 the native pixel seam is closed end-to-end: when
  1016 is active the native mouse handler routes true 1-based physical pixel
  coordinates (floored from the winit cursor position, clamped to the grid
  pixel extent) to the core pixel encoder, while every other encoding keeps the
  cell path.
- Window title from OSC 0/2; DECSET 1004 focus reporting
- Synchronized output (DEC private mode 2026): presentation hold with 150 ms
  safety timeout; cursor blink live during hold
- Keyboard mode-awareness: DECCKM, keypad modes, modified named keys, Kitty
  keyboard protocol disambiguation, event-type, alternate-key, report-all, and
  associated-text flags
- Terminal capability queries: XTGETTCAP (conservative truth set: `TN`,
  `Co=256`, `RGB=1`; unknown names → xterm invalid response) and DECRQSS (live
  SGR including extended underlines + underline color, DECSCUSR `" q`, DECSCA
  `"q`, DECSTBM; unimplemented selectors → invalid per xterm)
- Rectangle operations: DECCRA (snapshot-copy, overlap-safe), DECFRA, DECERA,
  DECSERA; DECCARA/DECRARA attribute rectangle ops (bold, underline, blink,
  inverse; stream and exact extents via DECSACE); DECSCA character protection;
  DECSED/DECSEL selective erase; wide-pair edge sanitization
- Lazy scrollback re-wrap and resize fast paths
- Theme system: full 16-color ANSI palette + semantic roles (cursor, selection,
  search highlight, reserved border/inactive) per theme; a curated,
  contrast-validated built-in library plus user `.theme` files through one
  shared dependency-free parse path (see [`docs/themes.md`](docs/themes.md) for
  the current roster and file format); `ODYTTY_THEME` accepts a built-in name,
  directory-relative name, or file path; OSC-4 / OSC-10/11/12 dynamic overrides
  layer on top with correct precedence; optional CRT scanline visual effect (`visual=ambient`/`scanlines` are back-compat aliases for the CRT path when no explicit `crt` setting is present; explicit `crt` always wins)
- In-window overlay framework (`src/native/overlay.rs`): a native multi-row
  panel layer rendered through the existing cell path — text fields, lists,
  toggles, keyboard-driven navigation; presentation-only, never mutates terminal
  state
- In-app settings panel: `Ctrl+Shift+,` opens a keyboard-driven editor
  covering font, theme, cursor, keybinds, and all runtime knobs; edits apply
  live through the existing reload seam; `Ctrl+S` writes changed rows back to
  `odytty.conf` with preservation-first writeback (comments, blank lines, and
  unknown keys untouched; same-directory atomic rename). Live theme picker:
  `Ctrl+Shift+T` lists built-ins, previews each theme on arrow
  navigation, persists the selected built-in with `Enter`, and restores the
  originally active theme with `Esc`. The custom theme builder has landed:
  clone/tweak/author with live preview, saved to a user `.theme` file.
- Readability pipeline: all visual enhancements are off by default, behind
  explicit settings, with a pixel-identical plain/fast path that bypasses
  extras. Three delivered knobs:
  - **Perceptual color pipeline** (`src/color.rs`): linear-space blending is
    active in the render path, and OKLab / OKLCH dim/fade/mix helpers
    (`dim_perceptual`, `mix_oklab`) are in place so equal numeric steps can
    produce equal perceived steps. These back the minimum-contrast lift below,
    and the live SGR dim/faint text path dims through `dim_perceptual`
    (OKLab, hue-preserving). Honest note: `dim_perceptual` applies a *uniform*
    OKLab scale, which reduces algebraically to a uniform linear-RGB scale
    (`(1-amount)^3 * rgb`) — so for the uniform-dim case it is output-identical
    to naive per-channel halving (both preserve hue). The perceptual pipeline's
    payoff is in the *non-uniform* fade/mix paths, not uniform dim; a test pins
    this equivalence so the claim cannot silently drift.
  - **Minimum-contrast floor** (`ODYTTY_MIN_CONTRAST`, `min_contrast`): a
    configurable WCAG contrast ratio floor between foreground and background,
    applied at render time. Default `1.0` is exact passthrough (no lift); higher
    values lift underpowered foregrounds toward legibility. The floor is measured
    via WCAG relative luminance; the lift is applied by bisecting OKLab lightness
    while preserving hue and chroma (`src/color.rs:enforce_min_contrast`).
  - **Stem darkening** (`ODYTTY_STEM_DARKEN`, `stem_darken`): a coverage boost
    that keeps glyph stroke weight on light-on-dark displays. Default `0.2` (on,
    a conservative boost for crisper text); range `0.0`–`1.0`, where `0.0` is the
    byte-identical opt-out to the classic raster. Applied at rasterization time
    (`src/atlas/mod.rs`).
- Shell working-directory tracking: OSC 7 (`file://host/path`) is parsed and
  stored as advisory string state on the terminal core (`Screen::current_working_directory`,
  `Screen::take_working_directory_changed`). The parser requires the `file://`
  scheme (case-insensitive), splits the authority, and percent-decodes the path.
  Only an empty host or `localhost` (case-insensitive) is accepted; foreign hosts
  are ignored rather than stored as misleading local paths — resolving a real
  hostname would require `gethostname`, a syscall the core deliberately avoids
  to stay deterministic and filesystem-free. Robustness: non-`file://` URLs,
  missing path, malformed or truncated percent-escapes, and decoded NUL (`%00`)
  all ignore the OSC and leave the stored path unchanged; non-UTF-8 bytes are
  replaced lossily; payloads are bounded by the parser's 128 KiB OSC cap. No
  response is emitted and no filesystem access occurs. RIS leaves the stored
  path untouched (it reflects the foreground process's state, not resettable
  terminal state — mirroring the title decision). The native consumer
  (e.g. open-new-tab-in-same-directory) is a deliberate follow-up packet (SI2).
  OSC 6 is accepted-and-ignored.

- Post-process pipeline foundation: lazy offscreen render target +
  fullscreen-triangle passthrough composite; default path stays
  direct-to-swapchain (byte-identical); GPU readback smoke guards the seam
- Opt-in cursor animations (all off by default, purely visual, never move the
  logical cursor): cursor blink fade (`cursor_easing`, 180 ms ease); cursor
  slide (`cursor_motion`, 55 ms ease-out-cubic, snaps on large jumps/resize/
  scrollback); cursor glow (`cursor_glow`, three faint concentric rings in the
  theme foreground color behind the cursor block); cursor trail (`cursor_trail`,
  short fading after-image trailing the gliding cursor in the theme cursor color,
  only visible while cursor slide is on).
- New-output fade (`new_output_fade`, off by default): rows of freshly arrived
  output fade in over a short ramp at the live tail; scrollback and resize snap.
- Themed window border (`window_border`, off by default): an optional thin
  border around the terminal grid in the theme's `border` role color, drawn
  inside the window padding band, DPI-scaled, purely visual.
- Follow-OS dark/light theme (`follow_os_theme`, off by default): switches
  between `os_theme_dark` and `os_theme_light` based on the desktop
  color-scheme preference. Live on Wayland via the compositor
  `org.freedesktop.portal.Settings` `color-scheme` property. On X11 there is no
  live signal — seed direction at launch with `ODYTTY_APPEARANCE=dark|light`.
  When either theme name is unset the authored `theme` value is kept unchanged
  for that direction.
- Close confirmation (`confirm_close`, default on): shows a brief in-window
  prompt before closing if a foreground program is still running. An idle shell
  closes without prompting. Off disables the guard unconditionally.

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

## Post-Process Pipeline Architecture

*Source: `src/native/gpu.rs` (`PostProcessResources`, `post_active`,
`draw_scene`, `encode_scene_pass`, post composite encode path),
`src/shaders/bloom.wgsl`, `tests/gpu_composite_smoke.rs`.*

### Design invariant — dormant by default

The renderer carries a **lazy post-process scaffold**: an offscreen render
target, a nearest-clamp sampler, and a fullscreen-triangle composite pipeline.
By default the pipeline is **dormant**: `post_active()` returns `false`, the
`PostProcessResources` are `None`, no offscreen texture is allocated, and
rendering writes directly to the swapchain surface. The direct path and the
offscreen path share one scene-draw sequence (`draw_scene()` /
`encode_scene_pass()`); the branch point is a single `if post_active()` in the
`render()` method. When dormant, the renderer is byte-identical to the
pre-pipeline renderer.

A GPU readback smoke (`tests/gpu_composite_smoke.rs`,
`passthrough_composite_matches_direct_render_bytes`) renders the same checker
scene both direct-to-swapchain and through offscreen→composite and asserts
exact byte equality. This test is adapter-gated and runs in the default suite;
it guards the passthrough seam against regressions as effects land.

### Tier-3 sequencing decision

Tier-3 atmospheric effects land in this order:

1. **Post-process scaffold (landed):** offscreen scaffold + passthrough
   composite, sRGB-8 intermediate. Default dormant; zero visible change.
   Foundation for all subsequent effects.
2. **Linear HDR intermediate (landed):** the offscreen intermediate is a linear
   `Rgba16Float` format with a filterable-format probe and graceful auto-disable
   on adapters that cannot support it. This was the **hard prerequisite for
   bloom**: an sRGB-8 intermediate clamps all values at 1.0 and quantizes,
   destroying the HDR overshoot (>1.0 linear) that additive glow needs. The
   composite pass performs the final linear→sRGB encode at store time; the
   swapchain surface stays `Rgba8UnormSrgb`. When a post-process pass is active
   the scene pipelines (cell, color-glyph, image) are rebuilt to render into
   the HDR offscreen format and rebuilt back when it is disabled, so the default
   path stays on the swapchain format and byte-identical.
3. **Bloom / phosphor glow (landed):** bright-pass threshold + half-res
   separable blur + additive composite, off by default behind the `bloom`
   setting and gated on adapter HDR support. The bright-pass threshold is
   auto-derived from the active theme's foreground luminance so normal body text
   never glows.
4. **CRT / retro profile core (landed):** bounded scanlines + vignette, off by
   default behind `crt` and sharing the same offscreen scene render and final
   composite pass as bloom. Because post-composite dimming cannot feed back into
   the CPU minimum-contrast resolver, the shader clamps scanline/vignette
   strength and enforces a brightness floor so lit cells are never zeroed.
   Curvature and chromatic aberration are deferred.

Cursor motion trail (`cursor_trail`, off by default): a short fading after-image
that trails the cursor as it glides between cells, drawn behind the cursor block
in the theme cursor color. Only visible while cursor slide (`cursor_motion`) is
also on; fully decays as the glide settles. New-output fade (`new_output_fade`,
off by default): rows of freshly arrived output fade in over a short ramp at the
live tail. Both effects landed in the Tier-3 sequencing after bloom and CRT
profile were proven. GPU quality / per-effect settings panel controls follow.

### Readability-gate architecture — durable design rule

The **minimum-contrast floor** (`enforce_min_contrast`,
`src/color.rs:enforce_min_contrast`) runs at **CPU color-resolve time** — the
last step of the per-cell resolve closure inside `build_cell_vertices_with_focus_dim_into`
— before the vertex buffer is written and long before any GPU scene or
post-process pass executes. There is no within-frame feedback path from the GPU
composite back to the CPU resolve step.

**Consequence (binding design rule):** post-process effects **cannot** rely on
the minimum-contrast floor to clean up legibility after the fact. Every Tier-3 effect must
be **structurally unable** to harm body-text legibility by construction:

- **Bloom / additive glow:** threshold is auto-derived to lie strictly above
  the luminance of normal body text, so body text is never in the bright set
  that glow acts on. Composition is additive (never replace), so background
  regions brighten but existing foreground coverage is only increased, not
  reduced.
- **CRT scanlines / vignette:** modulate brightness uniformly; paired with
  an intensity cap that keeps the worst-case dimming above the body-text
  legibility floor. The user-configured `min_contrast` floor is the explicit
  safety net at the CPU level; effects must not require it.
- **Background treatments** (`background_treatment`, `off`/`gradient`/`vignette`,
  default `off`): position-based per-cell background darkening (gradient toward
  the bottom; vignette toward the edges/corners). Legibility is
  safe-by-construction: the darken is applied to the per-cell background
  **before** the minimum-contrast floor resolves, so the floor sees the treated
  background and re-lifts the foreground as needed. A `MAX_BG_TREATMENT_DARKEN`
  cap keeps the worst-case dimming bounded; the knob is forced off under the
  plain renderer profile. Image/blur-behind is a planned future extension.
- Any new Tier-3 effect must document its structural legibility guarantee
  before landing.

## Stack

The stack is: Rust, Linux-first, `winit` (windowing), `wgpu` (GPU/Vulkan),
`ab_glyph` (font rasterization for normal text), `swash` (emoji font discovery,
shaping, and color-font probe — emoji/color-font path only; normal text stays on
`ab_glyph`), `unicode-width` (cell widths), `arboard` (clipboard), `rustix`
(PTY/termios), `png` (PNG decode for Kitty `f=100`).

The terminal core is a distinct boundary from the native app. The `core` module
never imports windowing, GPU, or rendering code; it consumes VT bytes via the
owned parser and exposes a `Snapshot` for the renderer to consume. The native
module owns the `winit` event loop, `wgpu` surface, glyph atlas, grid vertex
builder, and image layer, consuming core snapshots through a narrow seam.

Text is cell-based: each codepoint occupies one or two columns (`unicode-width`
consistent with core), and all coordinate systems are per-cell. The glyph atlas
uses one monospace face for regular text, with bold/italic/bold-italic faces
loaded when discovered by filename convention. When a style face is absent,
`StyleFonts::synthetic_mask()` derives a per-face synthesis flag by comparing
loaded `Arc` identities; `GlyphAtlas::set_synthetic_styles` receives those bits
and applies a `SynthTransform` during rasterization — italic via horizontal
shear (tan 12° ≈ 0.2126), bold via double-strike at a sub-pixel embolden offset,
bold-italic by composing both. Real faces always take precedence; synthesis
activates only for genuinely absent slots. Ligatures and complex shaping are not
implemented; each atlas entry is a single character rasterized into its cell
or two-cell slot.

`Attrs` stores its eight boolean display flags (bold, dim, italic, underline,
blink, strikethrough, inverse, hidden) in a single private `flags: u16`
bitfield. The public API is `&self` getters (`bold()` … `hidden()`) and `&mut
self` setters (`set_bold()` … `set_hidden()`). `protected` and
`wide_continuation` remain public `bool` fields on `Cell` because they do not
benefit from the same packing (`Cell` is 36 B with or without them). The
hand-written `Debug` impl reads through the getters and emits the same field
names and values as the previous `#[derive(Debug)]` output, so parser-oracle
golden fixtures do not need to change when the representation does — the same
rationale that governs the `protected`-omit and `blink:false`-omit golden
decisions elsewhere.

**Color emoji — decision record.** The accepted direction is a separate
premultiplied-RGBA color-glyph path, distinct from the current monochrome
coverage shader. `swash` is chosen for emoji shaping and rasterization: it
covers CBDT/CBLC bitmap strikes (Noto Color Emoji's format on Linux),
COLR/CPAL, and sbix, while providing full cluster shaping — VS15/VS16
selectors, modifier sequences, ZWJ sequences, flags, and keycaps. Font
rasterization remains external per the project boundary; atlas management,
placement, blending policy, fallback routing, and terminal-cell behavior are
OdyTTY-owned. A dedicated `ColorGlyphAtlas` stores premultiplied-RGBA source
pixels keyed by shaped cluster, font identity, and physical cell size alongside
the existing coverage atlas. Emoji cells sample source pixels directly and are
never tinted by SGR foreground color. Font discovery probes fontconfig for
Noto Color Emoji, Noto Emoji, or the `emoji` generic family; an explicit
per-session setting is planned as a follow-up. VS15 (`U+FE0E`) forces the text
path; VS16 (`U+FE0F`) forces the emoji path; characters with
`Emoji_Presentation` default to emoji; others default to text. RGI clusters
are treated as atomic if `swash` shapes them to a single color glyph;
unsupported clusters degrade per-codepoint to the existing fallback path.
Draw order: cell backgrounds → below-text images → coverage glyphs and line
decorations → color emoji glyphs → cursor and overlays. COLR v1 and SVG-in-OT
are deferred but architecturally permitted; the boundary rule (rasterization
external, placement owned) applies to those paths as well. Implementation
the delivery ladder is tracked in `TODO.md`.

**First increment (delivered).** The first `src/emoji/` packet was a renderer-free probe
module: no atlas, GPU, shader, or core terminal code. Discovery runs in two
stages. First, `fc-match -f '%{file}\n%{family}' 'Noto Color Emoji'` is invoked
directly; the returned path and family string are checked against a strict
identity predicate (normalized filename or family must contain `notocoloremoji`),
so generic fontconfig substitution fonts are rejected. If fontconfig is
unavailable or returns a non-matching result, a bounded directory scan covers
`/usr/share/fonts`, `/usr/local/share/fonts`, `~/.local/share/fonts`, and
`~/.fonts` at maximum depth 6 and a 20 000-file cap, matching by normalized
filename stem. When no Noto Color Emoji is found, the module returns `None` and
all downstream code skips the emoji path without error. On a successful find,
the module loads the face as a borrowed `swash::FontRef` and probes: detected
color-table set (CBDT/CBLC, sbix, COLR/CPAL, SVG), OpenType family name string,
and per representative-sequence records: shaped glyph ids, cluster structure
(source byte range, advance, ligature/complex flags), and per-sequence fallback
outcome (`Resolved` when any shaped glyph id is non-zero, `MissingGlyph`
otherwise). Default tests are hermetic (temp-dir filename discovery, fixed
sequence list, non-color format detection for a monospace outline font). The
host-dependent full probe is `#[ignore]`-gated and runs via
`cargo test emoji -- --ignored`; it exits cleanly when the font is absent.

**Second increment (delivered).** `src/emoji/color_atlas.rs` adds the OdyTTY-owned
`ColorGlyphAtlas`: a grow-only `Rgba8Unorm` atlas for premultiplied source
pixels, keyed by `(font identity, glyph-or-cluster id, physical px size,
scale)` rather than by character. Slots span one or two terminal cells; wide
color glyphs draw once from the lead cell and continuation cells emit nothing.
The native renderer owns a dedicated color-glyph texture, vertex buffer, WGSL
shader, and premultiplied-alpha blend state. The segment currently receives no
live runs until the real decoder supplied decoded swash glyphs, but synthetic tests pin the
atlas bookkeeping, UVs, dirty revision, pass ordering, and wide-cell contract.
Selection/search backgrounds render under color glyphs; OdyTTY does not tint or
recolor source emoji pixels with SGR foreground colors.

**Third increment (delivered).** `src/emoji/render.rs` activates the first live color emoji
path for Linux Noto Color Emoji CBDT/CBLC bitmaps. `EmojiRasterizer` discovers
the Noto face, shapes each eligible terminal-cell grapheme with `swash`, renders
single-glyph color bitmaps with best-fit strike selection, scales/centers them
into the one- or two-cell atlas slot, and premultiplies RGBA before insertion.
VS15 (`U+FE0E`) forces the text/coverage path; VS16 (`U+FE0F`) and default
emoji-presentation codepoints request color. The native renderer computes runs
from the snapshot before coverage-atlas insertion, skips normal monochrome
foreground quads only for resident color runs, uploads dirty color-atlas pixels,
and draws the dedicated color segment in the established draw order. If discovery, shaping,
bitmap rendering, or atlas insertion fails, no color run is emitted and the
existing coverage/fallback path remains visible. Multi-codepoint RGI stitching
such as flags, keycaps, skin-tone modifiers, and ZWJ families remains for a future increment.

**Fourth increment (delivered).** The live color path reconstructs bounded multi-codepoint
emoji clusters from the snapshot before shaping. Flags are assembled from
adjacent regional indicators, skin-tone sequences from the base emoji plus
modifier cell, keycaps from the base cell's VS16/keycap combining marks, and
ZWJ chains from successive cells whose graphemes carry trailing ZWJ marks. A
cluster that shapes to one nonzero Noto color bitmap glyph is inserted into the
atlas with a `ColorGlyphId::Cluster` key and draws once from the owning cell,
using a one- or two-cell atlas slot while recording every source column whose
foreground should be suppressed. If shaping or color bitmap rasterization does
not resolve, no cluster run is emitted; the existing per-cell coverage/color
fallback remains visible.

**Capacity audit (delivered).** `ColorGlyphAtlas` capacity is bounded and
corruption-safe as implemented. The atlas starts at 16 columns by four rows,
grows in four-row chunks, and caps at 4096 resident color glyph/cluster slots.
At the cap, a new insertion returns `ColorGlyphAtlasError::Full`; existing
slots stay lookupable, no slot is overwritten, `revision` is unchanged, and a
failed insert does not mark the atlas dirty. The renderer therefore degrades by
omitting the new color run and leaving fallback rendering visible. No eviction
policy is added until observed workloads prove 4096 resident color glyphs is too
small.
