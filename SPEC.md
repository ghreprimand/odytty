# OdyTTY — Spec

This specification defines OdyTTY's product boundaries, owned architecture,
shipped scope, platform model, and rendering stack.

## Contents

- [The Spark](#the-spark)
- [Concept](#concept)
- [The Case](#the-case)
- [Privacy And Data Posture](#privacy-and-data-posture)
- [Build Direction](#build-direction)
- [Ownership Boundary](#ownership-boundary)
- [Graphics Architecture](#graphics-architecture)
- [Configuration Architecture](#configuration-architecture)
- [Interaction Architecture](#interaction-architecture)
- [Scope](#scope)
- [Cross-Platform Architecture](#cross-platform-architecture)
- [Post-Process Pipeline Architecture](#post-process-pipeline-architecture)
- [Stack](#stack)

## The Spark

OdyTTY began as an answer to a Linux From Scratch system called
OdysseyOS: if a system has a distinct working environment, what would its
terminal feel like if it were built from the ground up rather than skinned from
something else? OdysseyOS remains the naming and visual inspiration, but OdyTTY
is a standalone public Linux terminal emulator, not an OdysseyOS-only tool.

The open questions that drive the project: can a terminal emulator add richer
visual effects, better themes, a stronger sense of identity, and features that
make command-line work feel more alive — while staying fast, correct, and
trustworthy for real daily use?

The project pursues genuinely original terminal work. If that path cannot
produce something interesting, useful, and reliable enough to justify itself,
it should be rethought rather than becoming a re-skinned version of another
terminal.

## Concept

OdyTTY is a reliable terminal emulator with its own Odyssey-inspired visual
identity, exploring how motion, themes, effects, and interface details can make
command-line work feel more alive without weakening core terminal behavior. Its
central question is whether a terminal can add useful, nonstandard features and
a richer experience while staying fast, solid, and practical for daily use.

## The Case

### Why Build It

OdyTTY is worth exploring because the terminal is a daily operating surface, not
just a utility, and there is room for one with a more personal, visually
distinctive identity that does not compromise trust. It is for people who want
command-line work to feel more expressive, polished, and alive while remaining
dependable enough for daily use.

The project tests whether reliable terminal behavior and a personal visual
environment can coexist.

### Protect Terminal Fundamentals

Scope stops before novelty damages terminal fundamentals. Speed,
compatibility, input correctness, readable text, stable rendering, and
predictable behavior matter more than effects, themes, or nonstandard features.

## Privacy And Data Posture

The privacy guarantees are structural:

- **Local by default.** OdyTTY runs on the local machine and has no telemetry,
  analytics, crash-reporting, update-check, or product-improvement data path.

- **No account layer.** There is no sign-in, cloud sync, or server-side
  component.

- **User-owned state.** Settings, themes, and scrollback stay on the local
  filesystem, and `odytty.conf` is plain text the user can inspect.

- **Verifiable behavior.** GPL-3.0 source makes the absence of data collection
  auditable from the implementation.

Connection-manager data follows the same local boundary:

- OdyTTY-owned hosts live in the user's OdyTTY config directory.

- OpenSSH host-name import is opt-in, read-only, name-only, and bounded.

- Imported data excludes identity files, key material, and credentials.

- Authentication stays with the system `ssh` client and its credential helper.

- OdyTTY never reads, stores, requests, or passes passwords, private keys, or
  passphrases.

This stance is durable. Any future feature that transmits data without explicit
user action is out of scope.

Network-capable actions are explicit and user-initiated. Ctrl+click on
Linux/Windows or Cmd+click on macOS sends an allowlisted hyperlink to the
platform default opener, while connection entries delegate to system `ssh`;
links never open automatically from terminal output.

## Build Direction

The project owns its full byte path from PTY to glyph quad. Shell process and
PTY handling, escape-sequence parsing, input mapping, text layout, renderer
geometry, and shaders are OdyTTY-originated code. The visual experience layer
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

**PTY layer** (`src/pty/`). A platform-neutral contract in `src/pty/mod.rs`
selects a per-OS backend by `#[cfg]`; every consumer imports `crate::pty::PtySession`
and is unaware of which backend is live. All PTY semantics are OdyTTY's own
code; `portable-pty` and `crossterm` are gone from the dependency tree.

- **Unix backend** (`src/pty/unix.rs`, `#[cfg(unix)]`). PTY allocation via
  `openpt`/`grantpt`/`unlockpt`/`TIOCGPTPEER` through `rustix`. Child spawn as a
  new session leader with a controlling terminal. `TIOCSWINSZ` resize.

  Nonblocking reader and writer via file-descriptor clones. Foreground-job
  detection via the controlling-terminal process group. Child reaping on drop.

- **Windows backend** (`src/pty/windows.rs`, `#[cfg(windows)]`). A pseudoconsole
  (ConPTY) via `CreatePseudoConsole` fed by a `CreatePipe` pair; the child is
  launched with `CreateProcessW` attaching the pseudoconsole through
  `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` on a `STARTUPINFOEXW` attribute list.
  `ResizePseudoConsole` for resize; `TerminateJobObject` on a kill-on-close Job
  Object for whole-tree kill, with root-only `TerminateProcess` as the degraded
  fallback if job assignment failed. Closing the pseudoconsole then lets the
  output reader hit EOF, mirroring the Unix `EIO`→EOF teardown. There is no
  POSIX process group, so `foreground_job` returns the contract's
  `ForegroundJob::Unknown` indeterminate default.

  The
  default interactive shell prefers PowerShell to match Windows Terminal's
  command surface — `pwsh.exe` (PowerShell 7) if present, else Windows
  PowerShell 5.1 (resolved by its fixed `%SystemRoot%\System32\WindowsPowerShell\v1.0\`
  path), else `%ComSpec%` (cmd.exe) as a last resort. `spawn_shell_command`
  selects the one-shot flag by shell family — `cmd /C <command>` for cmd,
  `powershell -NoProfile -Command <command>` for PowerShell — the Windows
  analogue of the Unix `$SHELL -lc` split, not a `-lc` login shell. The
  pseudoconsole handle is owned by an RAII guard so it cannot leak.

The `PtySession` surface (`spawn_default_shell{,_in}`, `spawn_shell_command`,
`spawn_exec`, `spawn_command`, `resize`, `try_clone_reader`, `take_writer`,
`foreground_job`, `try_wait`, `wait`, `kill`, `read_to_end`) is identical across
backends; reader/writer are erased to `Box<dyn Read + Send>` / `Box<dyn Write +
Send>` so the PTY pump and all native consumers are platform-agnostic. The
`open_pty_pair` helper returns a POSIX `(File, File)` master/slave and therefore
stays `#[cfg(unix)]`-only (a ConPTY has no termios-capable slave `File`); its
sole caller is a Unix-only termios test.

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
approximation confined within the cell.

Underline color is taken from the cell's
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
unknown names receive the xterm invalid response.

DECRQSS reports live SGR
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
DECRARA calls.

The `rect_attr_extent` field lives on each `Screen`; it is carried
across alternate-screen entry and exit (same extent restored), and both RIS and
DECSTR reset it to `Stream` (the default). The `blink` field on `Attrs`
follows the same `Debug` omission policy as `protected`: `Attrs::fmt`
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
new image fits.

The store is renderer-independent — decoded pixel data lives in
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

**Multi-pane graphics.** Inline graphics composite into the per-pane render
path: each pane collects its own visible placements (namespaced by session so
two panes' independent image id spaces never collide) and draws them relative to
the pane's origin, clipped by a per-pane scissor rect that bounds both axes so a
placement cannot bleed across a vertical or horizontal divider into a neighbour.
Text, cursor, selection focus, search highlighting, dividers, resize, smooth
scroll, and Kitty/Sixel images all work in splits.

**Kitty graphics protocol.** Actions `a=t` (transmit), `a=T` (transmit and
display), `a=p` (display existing by id), `a=d` (delete), and `a=q` (query)
are supported. Formats `f=24` (raw RGB), `f=32` (raw RGBA), and `f=100` (PNG
still image) are supported. Transports `t=d` (direct), `t=f` (file), and `t=t`
(temp file) are supported on all platforms; `t=s` (POSIX shared memory) is
Unix-only and rejected as unsupported on Windows. File transports carry
security restrictions documented in `docs/graphics.md`. Chunked transfer
(`m=1`/`m=0`) is supported under a 96 MiB encoded-payload cap.

Placement ids,
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

2. **Config file** — on Unix, `$XDG_CONFIG_HOME/odytty/odytty.conf` (falling
   back to `~/.config/odytty/odytty.conf`); on Windows, `%APPDATA%\odytty\odytty.conf`.
   The same resolved path is used for live reload, in-app writeback, theme files,
   and the first-run marker, so persistence works identically on every platform.
   A missing or unreadable file is silently skipped; malformed lines and unknown
   keys warn to stderr and are skipped.

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

All settings in `docs/runtime-knobs.md` except `native_autoclose_ms` are
live-reloadable when their environment variable was not set at startup. Font
path, family, size, weight, line height, subpixel, synthetic style, stem
darkening, symbol fallback, and geometric box-drawing changes rebuild or
re-rasterize the glyph atlas as needed, recompute cell metrics when those
metrics change, and push PTY `TIOCSWINSZ` through the same path used for HiDPI
scale changes. Presentation-only knobs apply on the next frame/event.
`window_decorations` is immediate on Wayland and best-effort on X11.

A bad
rewrite is a no-op; a deleted config file keeps the current settings; reload
never panics.

**Startup-only setting.** `native_autoclose_ms` is not reloadable. Changing a
lifecycle smoke timer mid-session would make manual and automated test behavior
ambiguous.

**CLI introspection.** Startup flags (`src/cli.rs`) print information and exit
without opening a window: `--list-themes` prints built-in themes,
`--list-fonts` inventories discoverable system font files, and `--show-config`
prints the current stable effective-config dump. `docs/runtime-knobs.md` remains
the full settings authority.

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
render snapshot — no change to the terminal model state. On explicit Ctrl+click
on Linux/Windows or Cmd+click on macOS, the native app calls the platform
default opener with the URI as a direct argument after verifying its scheme
against an allowlist (`http`, `https`, `file`, `mailto`). Mouse reporting does
not add a Shift requirement. No shell interpolation occurs.

Links are never followed automatically; the allowlist check and platform-opener
call only happen on deliberate user action.

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

#### Track Negotiated Flags

OdyTTY implements the Kitty keyboard protocol as a progressive enhancement on
top of its existing DEC/xterm key encoder. The terminal core tracks active
keyboard protocol flags per screen buffer: `CSI > flags u` pushes the current
flags and applies a new set, `CSI < n u` pops saved states, `CSI = flags ;
mode u` sets/adds/removes flags, and `CSI ? u` replies with `CSI ? flags u` on
the host-output path.

The stack is bounded at 16 entries with oldest-entry
eviction, and `RIS`/`DECSTR` reset it. Primary and alternate screen maintain
separate Kitty keyboard flag stacks so full-screen TUIs cannot leak negotiated
keyboard behavior back to the shell prompt.

#### Encode Native Input

The native layer still resolves terminal-local key bindings before encoding a
key for the PTY. With no Kitty flags active, OdyTTY emits the exact legacy
bytes. With disambiguation active, ambiguous control/Alt text and named keys
use CSI-u forms with the Kitty `+1` modifier encoding; report-all uses the same
encoder for ordinary text and recovery keys. Event-type reporting uses the
modifier subfield for repeat (`:2`) and release (`:3`) events, with release
bytes emitted only when negotiated.

Alternate-key reporting adds shifted and
base-layout key-code subfields for character CSI-u events where OdyTTY can
derive them from the logical key. Associated-text reporting appends printable
generated text code points as the third CSI-u parameter when combined with
report-all.

### Synchronized Output (DEC Private Mode 2026)

#### Track The Core Mode

DEC private mode 2026 (`DECSET ?2026h` / `DECRST ?2026l`) is tracked as a
boolean field on the terminal screen; `RIS` and `DECSTR` both reset it to off.
`DECRQM` (`CSI ? 2026 $ p`) reports the current mode status through the normal
mode-query path in `src/core/screen/ops.rs`.

#### Hold Native Presentation

The native layer owns the presentation-hold policy. `SynchronizedOutputHold`
(`src/native/app/`) monitors the core mode flag and, while it is set, defers
GPU content uploads — the terminal model continues to advance and process PTY
bytes without interruption, but grid snapshots are not uploaded or rendered.
After `SYNCHRONIZED_OUTPUT_TIMEOUT` (150 ms in `src/native/app/mod.rs`), the hold
is released unconditionally and will not re-engage until the application resets
the mode and sets it again. The timeout deadline is registered with the event
loop so the release fires promptly at the deadline without additional polling.

A crashed application that never sends the DECRST therefore cannot freeze the
display for longer than 150 ms. Cursor blink remains live during the hold: the
hold path calls `update_held_cursor_frame`, which re-renders the cursor blink
delta against the last presented snapshot without touching grid content.

### Semantic Prompt Marking (OSC 133)

#### Parse Prompt Phases

OSC 133 (`ESC ] 133 ; <letter> [; k=v ...] ST`) marks four shell phases:

| Mark | Meaning |
| --- | --- |
| `A` | Prompt start |
| `B` | Prompt end and command-input start |
| `C` | Command-output start |
| `D[;exit]` | Command end with an optional exit status |

The parser uses the same `dispatch_osc` seam as OSC 7, 8, and 52.
`src/core/prompt_marks.rs` owns `PromptKind` and the `handle_osc133` setters.

Auxiliary `k=v` fields are accepted and ignored. The `D` status is parsed
digits-only into `Option<i32>`; absent, non-numeric, or overflowing values
become `None`, while malformed payloads are consumed without a reply.

#### Inject Shell Hooks

The supported way to enable marks for local shells is `shell_integration = on`.
The setting is off by default. For a one-off Bash hook, run:

```console
odytty shell-integration bash
```

Substitute `zsh` or `fish` for another supported Unix shell. When enabled, new
local default-shell spawns receive wrapper hooks that source the user's normal
config first, then emit the same OSC 133 A/B/C/D marks. On Windows, a
`powershell`/`pwsh` default-shell spawn is injected with a PowerShell profile via
`-NoExit -Command`: a wrapped `prompt` emits `D` (carrying `$LASTEXITCODE`), `A`,
the user's original prompt, then `B`, while a PSReadLine Enter hook emits `C`.

The Windows snippet builds its ESC/BEL bytes from `[char]27`/`[char]7` for
Windows PowerShell 5.1 compatibility and guards against double-wrapping with a
set-once `ODYTTY_SHELL_INTEGRATION` export. `cmd.exe` has no equivalent OSC 133
hook surface and is deliberately unsupported; unknown shells degrade to normal
startup. Manual snippets remain useful for SSH and login-shell setups.

#### Store Logical-Line Marks

Marks are **logical-line-anchored**: a mark is stored as `Option<PromptKind>`
on the cursor's logical line (carried on the first physical row), so it survives
scroll-out into scrollback and width-changing reflow. `RIS`, `ED 2/3`, `EL 2`,
resize, and alternate-screen transitions clear or re-anchor marks as the rows
they sit on change. A read-only poll API (`prompt_mark_at`,
`take_prompt_marks_changed`) exposes the marks; the change flag is conservative
(fires on any stamp, clear, or reposition).

#### Consume Marks Without Changing Rendering

The core mark model stays render-neutral: `prompt_mark` is never read by the
render path and is deliberately absent from `Snapshot`, so the plain renderer is
byte-identical with or without OSC 133 in the stream. The command-aware UX that
consumes these marks reads them through the poll API instead. Jump-to-prompt is
wired — `Ctrl+Shift+Up` / `Ctrl+Shift+Down` (and the matching command-palette
actions) move the viewport between prompt marks — and a `command_status_gutter`
setting marks command success/failure. Per-command output selection remains
downstream work.

#### Gate Prompt-Aware Editing

Two features consume the
input boundary (`B` mark): selecting prompt text and pressing Delete/Backspace
deletes only the selected editable input through shell-edit bytes, and (with the
`sh_click` setting, on by default) clicking in the typed command line repositions
the shell cursor to the nearest character boundary (a click in a glyph's right
half lands the caret after it, the left half before it) — including across
soft-wrapped lines. Both require shell integration, because OdyTTY must know the prompt
boundary before it can safely edit or position within shell input — it never
guesses with a no-OSC heuristic that could corrupt the command line. When the
boundary is unknown (integration off, or no prompt mark yet), a selection-delete
does **not** send blind edit bytes: it clears the stale visual selection and
raises a hint pointing at shell integration, so the UX is honest about why the
action did not run. Click-to-position additionally requires the shell snippet to
advertise `click_events=1` on its `A` mark — the bundled bash/zsh/fish and
PowerShell snippets all do, so the capability is live out of the box with shell
integration on.

These are deliberately fail-safe: no boundary means no prompt-aware
deletion, no advertised click support means no click-to-position, and stale local
state is cleared rather than acted on speculatively.

### Cursor Keys & Paste Pass-Through

Cursor-key and paste handling is intentionally a thin pass-through, so the shell
line editor — not OdyTTY — owns multiline navigation within an editable command.
Unmodified arrows encode as CSI (`ESC[A`) in normal cursor-key mode and SS3
(`ESC O A`) under DECCKM application cursor mode; modified arrows use xterm-style
CSI-with-modifier encoding first, so e.g. Ctrl+Right stays CSI-with-modifier
regardless of cursor-key mode. Bare arrows are **not** bound in the flat global
keymap; they reach the PTY.

They are bound only as the pane-multiplexer prefix's
second key (default `Ctrl-B`, then an arrow, focuses the adjacent pane), and copy
mode or an open overlay consumes arrows only while explicitly active. Bracketed
paste is emitted as a single envelope — one `ESC[200~`, the whole payload (chunked
for transport but never split into per-line paste events), one `ESC[201~` — with
embedded end markers stripped so a paste cannot self-terminate early. Because the
correct cursor-key bytes and a single paste envelope reach the PTY, Up/Down
navigation inside a pasted multiline buffer is owned by readline/zle/PSReadLine/
fish, which is working as designed rather than a terminal defect.

### Bell (BEL)

BEL (`0x07`) is split across the ownership boundary the same way clipboard and
prompt marks are. The terminal core (`src/core/screen/mod.rs`) does nothing
audible or visual: `dispatch_execute` sets a one-shot `bell_pending` latch, and
`Terminal::take_bell()` drains it edge-not-level (coalesced, cleared on read).
BEL never touches the grid or moves the cursor — a regression suite
(`src/core/tests/bell.rs`) pins that invariant.

The native layer decides presentation, gated by the `bell` setting
(`BellMode`): `off` (drain and ignore), `visual` (a brief full-viewport flash
that decays to transparent over 150 ms on an ease-out curve, painted as a single
`SolidQuad` overlay so the cells beneath stay at full opacity — the RV1
readability floor is preserved by construction), `urgent` (the **default** —
`Window::request_user_attention` when unfocused, no pixels change while focused,
so a foreground shell never flashes on tab-completion bells), or `all`. OdyTTY
has no audio backend, so there is no audible mode. The flash joins the existing
overlay-registry animation infrastructure: a `BellFlash { epoch }` render-cache
fragment and an `animation_deadline` contributor, both `Inert`/`None` on the off
and urgent-only paths so the default render path is byte-identical.

### IME Composition

IME input is enabled at window creation (`Window::set_ime_allowed(true)`) so
`winit` delivers the four `Ime` events. `src/native/app/ime.rs` routes them:
`Enabled`/`Disabled` clear any stale pre-edit; `Preedit(text, _)` stores the
in-progress composition and positions the IME candidate area at the cursor;
`Commit(text)` writes the finalized UTF-8 to the active PTY exactly like typed
`Character` input and clears the pre-edit. The pre-edit is rendered inline at the
cursor cell with a straight underline (an `ImePreedit` overlay fragment forces a
full repaint per composition keystroke); it is never sent to the shell until
commit. This makes CJK input methods and compose-key/dead-key accents work.

With no composition in progress the pre-edit is empty and the render path is
unchanged.

## Scope

v0 is complete. Stages 1 through 4.5 are substantially complete. The parity
half of Stage 6 (graphics protocols, wide glyphs, subpixel AA, text quality) is
substantially complete. Stage 5 (file-based configuration with live reload) has
its first stable layer.

### Parser And Protocols

- Owned platform PTY layer (Unix PTY and Windows ConPTY backends) and owned VT
  parser (clean-room from primary specs)

- File-based configuration with live reload; env always wins

- Broad escape-sequence compatibility (SGR including 256-color, truecolor
  semicolon and colon forms (`38:2::r:g:b`, `48:2::r:g:b`), alternate screen,
  mouse modes, wide characters, combining marks, and more)

### Graphics Protocols

- Kitty graphics protocol: `a=t/T/p/d/q`, `f=24/32/100`, `t=d/f/t/s`,
  chunked transfer, placement ids, z-index, source crop, cell scaling, pixel
  offset, delete specifiers

- Sixel graphics: full decoder, terminal integration, DECSDM, GPU image rendering

### Text And Rendering

- HiDPI-correct text rasterization across scale factors

- Wide-glyph 2-cell atlas slots; bearing-aware glyph quad geometry

- Optional subpixel anti-aliasing and tunable text gamma/contrast

- Configurable font family and bold/italic style faces with synthetic fallback
  (double-strike bold, 12° shear italic) when real faces are absent

- Full text attribute rendering: bold, dim, italic, extended underline styles
  (`SGR 4:0`–`4:5`, straight/double/curly/dotted/dashed), underline color
  (`SGR 58`/`59`, colon and semicolon forms), strikethrough, inverse, hidden

### Input And Interaction

- Scrollback search with match navigation and highlights

- Refined selection: double-click word, triple-click line, drag-scroll,
  scrollback-aware anchors

- Clipboard hardening: chunked paste, bracketed-paste sanitization, PRIMARY
  selection, OSC 52 write support, and default-deny OSC 52 read policy

- OSC 8 hyperlinks: hover underline, Ctrl+click on Linux/Windows or Cmd+click on
  macOS through the platform opener, scheme allowlist
  (`http`/`https`/`file`/`mailto`), never auto-opened from input

- Dynamic colors: OSC 10/11/12, OSC 4 palette entries, and reset/query support

- Right-edge scroll position indicator

- Configurable cursor shapes and blink policy (DECSCUSR + settings)

- Configurable terminal-local key bindings; `keybinds` / `ODYTTY_KEYBINDS`
  supports all bindable local, tab, palette, and pane actions. The in-app
  key-remap editor in the settings panel covers every bindable action (all 40)
  (select a row and press `Enter` to capture a new chord, `Backspace` resets to
  default, `R` resets all, conflict prompt on clash, writes to `odytty.conf` via
  the preservation-first writeback path). See
  [`docs/keybindings.md`](docs/keybindings.md) for the full keyboard reference.

- Keyboard copy mode (`copy-mode` action, `Ctrl+Shift+Space` by default): a
  keyboard-driven scrollback selection mode. `h/j/k/l`, `w/b/e`, `0/^/$`,
  `gg/G` move the
  caret; `v` and `V` start character and line selection; `y` / Enter yanks the
  selected text to the clipboard; `Esc`/`q` cancel. Arrow keys, PageUp/Down,
  Home/End, and `Ctrl-u/d/b/f` paging are also bound. Terminal state is never
  modified while copy mode is active.

- Mouse reporting: tracking modes 9 (X10), 1000 (normal), 1002 (button-event),
  1003 (any-event), focus reporting (1004); encodings 1005 (UTF-8 coordinate
  extension), 1006 (SGR decimal), 1015 (urxvt decimal); legacy byte protocol
  as default. Only one tracking mode and one encoding mode are active at a time;
  `DECRST` clears back to the default. SGR-pixel encoding (mode 1016) is
  supported core-side: `DECSET`/`DECRST`/`DECRQM` are wired, and a
  pure pixel encoder emits `CSI < Cb ; Px ; Py M|m` from caller-owned 1-based
  pixel coordinates. The native pixel seam is closed end-to-end: when
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

- Bounded scrollback (`scrollback_lines` / `ODYTTY_SCROLLBACK_LINES`, default
  10,000 logical lines): oldest history is evicted past the cap so a process
  streaming unbounded output cannot grow memory until the OS OOM-kills it. `0`
  means unbounded. A defensive per-line cell ceiling bounds the pathological
  no-terminator stream (`cat /dev/zero`). Live-reloadable; lowering the cap
  trims existing history immediately, and the cap applies to every session
  (including background tabs), not just the focused one.

- IRM (insert/replace mode, ANSI mode 4): `CSI 4 h` makes a printed glyph shift
  the cells at and right of the cursor toward the right edge instead of
  overwriting in place; `CSI 4 l` (the default) overwrites. Reset by RIS and
  DECSTR; DECRQM reports the live set/reset state. Required by line editors such
  as Apple's `pico`/`nano` that lean on IRM for incremental line redraw.

### Native UI And Workspaces

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
  `Ctrl+Shift+H` lists built-ins, previews each theme on arrow
  navigation, persists the selected built-in with `Enter`, and restores the
  originally active theme with `Esc`. The custom theme builder has landed:
  clone/tweak/author with live preview, saved to a user `.theme` file.

- Multi-session tabs: the native app runs multiple PTY/terminal sessions in a
  `WorkspaceSet`, routes PTY output by session id, and shows a one-row tab bar
  once two or more sessions exist. `Ctrl+Shift+T` opens a tab,
  `Ctrl+Shift+W` closes one, and `Ctrl+PageDown` / `Ctrl+PageUp` switch. The
  single-session view stays visually identical to the original full-grid view.
  Inline graphics use the same reserved top-row offset as cell geometry, so
  Kitty/Sixel placements remain aligned with text while the tab bar is visible.

  The tab context menu can assign a session-lifetime custom tab name; while set,
  shell title updates refresh the underlying title but do not replace the
  displayed custom label. Submitting an empty rename clears the override.
  Tabs live inside **workspaces**: a `WorkspaceSet` holds one or more
  workspaces, each an independent list of tabs with its own active-tab focus,
  while every PTY session stays in one flat arena keyed by session id (so
  pump-thread lookup never walks the hierarchy). A single-workspace set is
  behaviourally identical to the prior single tab-list model.

  Lifecycle
  invariants: a workspace is never empty — closing a workspace's last tab closes
  that workspace; the last tab of the last workspace exits the app; a new
  workspace opens with exactly one single-pane tab. This typed-exit escalation
  is configurable through `shell_exit_closes`: with `app`, a shell exit (typed
  `exit` or Ctrl-D) that would close any workspace quits OdyTTY instead, without
  reaping that workspace first, so a `restore_workspaces` snapshot still
  captures it.

- Workspace-shape persistence (`restore_workspaces`, default off): OdyTTY can
  snapshot the window's **shape** — workspace names, tab titles and order, the
  pane split tree and ratios, each pane's working directory, and the remote
  host a remote pane was connected to — to an atomic
  temp-and-rename file in the platform state dir (`%LOCALAPPDATA%` on Windows).
  The snapshot is a strict privacy boundary: it records structure only and
  **never** grid content, scrollback, environment, or the commands that were
  running, so a restore can never replay a command. Restore fires only for the
  primary instance (elected via a state-dir lock file) on a bare `odytty`
  launch; any CLI argument suppresses it, and a fresh shell is always spawned
  per pane. Named layouts persist the same shape under a chosen name.

  Opening a
  layout onto a window that already holds real state prompts for how it lands —
  **Replace** the current workspaces with the saved set, **Add** the saved
  workspace(s) beside them, or **Cancel** — while a fresh window holding a single
  untouched default workspace skips the prompt and lets the layout consume that
  workspace. On Unix a pane may
  carry a detached session-host id and reattach on restore when that host is
  still alive (falling back to a fresh shell silently); Windows stores no ids
  and always restores fresh. A pane opened on a remote host records
  the connection — the saved-profile alias, or the literal
  `[user@]host[:port]` for an ad-hoc destination — and restore reconnects it
  through the `ssh` path as a fresh remote login shell. The remote cwd is not
  restored (the shell lands at the host's own default), and no command is ever
  re-run.

  A host that no longer resolves opens a local shell instead, and a
  local pane whose captured directory exists but denies a shell retries at home
  rather than aborting the whole restore — a layout comes back in full or
  degrades per pane, never wholesale.

- Readability pipeline: visual enhancements are explicit settings with
  individual opt-outs, and `render_quality=plain` preserves the pixel-identical
  plain/fast path that bypasses extras. Three delivered knobs:
  - **Perceptual color pipeline** (`src/color.rs`): linear-space blending is
    active in the render path, and OKLab / OKLCH dim/fade/mix helpers
    (`dim_perceptual`, `mix_oklab`) are in place so equal numeric steps can
    produce equal perceived steps. These back the minimum-contrast lift below,
    and the live SGR dim/faint text path dims through `dim_perceptual`
    (OKLab, hue-preserving). Honest note: `dim_perceptual` applies a *uniform*
    OKLab scale, which reduces algebraically to a uniform linear-RGB scale
    (`(1-amount)^3 * rgb`) — so for the uniform-dim case it is output-identical
    to naive per-channel halving (both preserve hue).

  The perceptual pipeline's
    payoff is in the *non-uniform* fade/mix paths, not uniform dim; a test pins
    this equivalence so the claim cannot silently drift.
  - **Minimum-contrast floor** (`ODYTTY_MIN_CONTRAST`, `min_contrast`): a
    configurable WCAG contrast ratio floor between foreground and background,
    applied at render time. Default `17.0` is the fresh-install readability
    floor (range `1.0`–`21.0`); `1.0` disables the floor and is the exact
    passthrough opt-out. Higher
    values lift underpowered foregrounds toward legibility.

  The floor is measured
    via WCAG relative luminance; the lift is applied by bisecting OKLab lightness
    while preserving hue and chroma (`src/color.rs:enforce_min_contrast`).
  - **Stem darkening** (`ODYTTY_STEM_DARKEN`, `stem_darken`): a coverage boost
    that keeps glyph stroke weight on light-on-dark displays. Default `0.7`;
    range `0.0`–`1.0`, where `0.0` is the
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
  replaced lossily; payloads are bounded by the parser's 128 KiB OSC cap.

  No
  response is emitted and no filesystem access occurs. RIS leaves the stored
  path untouched (it reflects the foreground process's state, not resettable
  terminal state — mirroring the title decision). The native layer now consumes
  the tracked cwd: Detach & switch spawns the fresh session in the focused
  pane's working directory, and the command palette feeds recent OSC 7
  directories into its picker. OSC 6 is accepted-and-ignored.

- Post-process pipeline foundation: lazy offscreen render target +
  fullscreen-triangle passthrough composite; default path stays
  direct-to-swapchain (byte-identical); GPU readback smoke guards the seam

- Cursor presentation effects (purely visual, never move the logical cursor).
  The v0.9 defaults enable cursor blink fade (`cursor_easing`), cursor slide
  (`cursor_motion`, snapping on large jumps, resize, and scrollback), the
  cursor trail (`cursor_trail`, a short fading after-image in the theme cursor
  color), and cursor glow (`cursor_glow`, one shape-aware analytic aura behind
  the Block, Bar, or Underline glyph). Each is independently disableable with
  `= off`; reduced motion makes the presentation static without overwriting
  saved settings.

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

- Pixel-precise scrolling (`pixel_scroll`, on by default): high-resolution
  wheels and touchpads that emit pixel deltas scroll the viewport by a
  continuous sub-row amount tracking physical finger travel 1:1, instead of
  quantizing to whole notches. Continuous pixel input is tracked directly rather
  than eased, avoiding sawtoothing on high-resolution devices. `scroll_pixel_speed`
  (default `1.0`, range `0.25..=4.0`) tunes the sensitivity. Classic detented
  wheels emit line deltas and are unaffected, keeping `scroll_wheel_lines` as the
  per-notch multiplier.

  The continuous direct-tracking pixel lane is per-pane: in
  a split it drives the pane under the pointer (without stealing focus), the
  overflowing partial row clipped to that pane so the sub-cell shift never smears
  across the divider.

- Animated scroll glide (`scroll_glide`, on by default): discrete wheels emit
  whole notches with no sub-step data, so smoothness between notches can only
  come from animating them. When on, a notch moves the integer viewport offset
  instantly (governing selection, scrollbar, and return-to-live as always) while
  the rendered view eases toward it with a forward-chase follower that only ever
  moves in the scroll direction, so continuous input cannot sawtooth. On by
  default; primary screen only. In a split each pane glides independently as an
  eased follower with pixel-precise sub-cell smoothness — the pane under the
  pointer, without stealing focus — its overflowing partial row clipped to the
  pane so it never smears across the divider into a neighbour.

  High-resolution
  direct-tracking input uses `pixel_scroll`, which is likewise per-pane in a split.

- Wheel scroll amount (`scroll_wheel_lines`, default `6` rows per notch): sets
  how far one wheel notch moves local scrollback, and the same amount drives
  alternate-scroll (DECSET 1007) arrow emulation, so pagers like `less`, `man`,
  and `git log` scroll at the same rows-per-notch as the viewport. Full
  mouse-reporting applications own the wheel (their report carries direction,
  not magnitude), and continuous pixel input is never multiplied.

- Font weight control (`font_weight`, empty = regular by default): selects a
  named base weight face (e.g. `Light`, `Medium`, `SemiBold`) for normal text,
  independently of the SGR bold attribute; bold SGR still contrasts against your
  chosen base. Uses real font weight faces only — an unresolvable weight name
  falls back to the regular face. Changes rebuild the glyph atlas through the
  same path as `font_family`.

- Window decorations toggle (`window_decorations`, on by default): shows or
  hides the native window titlebar and borders. On Wayland, client-side
  decoration negotiation removes decorations reliably. On X11, this is a hint
  to the window manager; whether borderless takes effect depends on the WM and
  compositor — borderless is not guaranteed on X11. Purely a window chrome
  preference; never affects terminal model state or PTY behavior.

- Basic native tabs: each tab owns an independent local PTY session, terminal
  model, scrollback, and title. The default bindings are `Ctrl+Shift+T` for a
  new tab, `Ctrl+Shift+W` to close, and `Ctrl+PageUp` / `Ctrl+PageDown` to
  switch.

- Native splits / panes: a tab owns a binary layout tree whose leaves are
  independent local PTY sessions, so one tab can hold several panes side-by-side
  or stacked, each with its own PTY, terminal model, scrollback, viewport,
  selection, search, and cursor. Panes are driven by a tmux-style prefix
  (default `Ctrl+b`, configurable via `pane_prefix`; press it twice to send a
  literal prefix to a nested multiplexer once a tab is split): `%`/`"` split
  columns/rows, arrows or `o` move focus, `x` close, `z` zoom (toggle the
  focused pane full-bleed while preserving the layout underneath),
  `Space`/`=` equalize. Dividers are drag-resizable. The prefix is captured only
  when the active tab has more than one pane; on a single-pane tab, `Ctrl+b` and
  ordinary input pass through byte-identically, and a single-pane tab is
  byte-identical to the pre-panes render path.

  Optional inactive-pane dimming is implemented by `inactive_pane_dim`, defaults
  off, is forced off in plain render quality, and leaves the no-dim path
  byte-identical. Interactive overlays (selection / search) render per pane, so
  a selection or a search match shows in the correct pane regardless of focus
  (the interactive search query bar stays on the focused pane). Kitty and Sixel
  inline graphics also composite within each pane's origin and clip rectangle.

### Sessions And SSH

- Unix resumable-session substrate: a hidden root-level `session_host` module
  owns the first detached-host foundation outside `src/native/`. The internal
  `odytty session-host` mode hosts one PTY + terminal model, exposes only a
  per-user Unix-domain socket under `$XDG_RUNTIME_DIR/odytty/`, requires a
  `0700` current-user runtime directory, rejects incompatible protocol/snapshot
  versions, sends a current `SnapshotEnvelope` on every attach, streams
  output/invalidation frames, and reaps the child process. Runtime-dir
  resolution: an explicitly-set `XDG_RUNTIME_DIR` always wins on Unix
  (Linux uses its standard `/run/user/<uid>`, byte-identical). On macOS, which
  has no `XDG_RUNTIME_DIR`, the host falls back to the per-user Darwin temp
  directory (`confstr(_CS_DARWIN_USER_TEMP_DIR)`, for example
  `/var/folders/.../T/`);
  the `odytty/` socket subdirectory is still created `0700` and validated
  owner-private, so the macOS runtime directory upholds the same local-only,
  owner-private privacy charter — the socket never touches the network and
  nothing leaves the machine.

  Because `AF_UNIX` socket paths are bounded
  (`sun_path` is 104 bytes on macOS, 108 on Linux), the host rejects a runtime
  base that would overflow the limit with a clear error rather than an opaque
  `bind()` failure. Client detach or
  socket close removes only that client; the hosted PTY and bounded terminal
  model continue until the child exits or the detached idle timeout kills and
  reaps it. Public CLI commands now cover `odytty new --detached`,
  `odytty list`, and `odytty attach [--diagnostic] ID`.

  `list` prints
  metadata-only rows and never scrollback/command output. `odytty attach <id>`
  opens a live native window, boots the normal initial local session, then
  reattaches the hosted session as a focused tab repainted from the host
  snapshot. On a missing or dead id, the window still opens and stderr reports
  `odytty: attach session <id> failed: <err>`. `odytty attach --diagnostic <id>`
  is the headless script/CI variant: it prints the one-line status dump and
  exits without opening a window.

- Command-palette substrate: pure modules outside `src/native/` provide a
  dependency-free fuzzy scorer, stable action catalog, source composer, and
  read-only candidate sources. Shell history reads are bounded to a 1 MiB tail
  window, 20,000 physical lines scanned, 5,000 returned entries, and 4,096
  characters per entry by default. Supported history formats are bash/plain
  line history, zsh extended history, and Fish `- cmd:` rows including simple
  block commands. Recent directories are fed from already-parsed OSC 7 cwd
  values; the source layer never writes history files, never logs history
  contents, and never sends candidates over the network.

- Native command-palette overlay: exposed as the `command-palette` bindable
  action, which ships a default `Ctrl+Shift+P` chord (a `Ctrl+Shift+<letter>`
  chord a full-screen TUI cannot itself receive) and remains rebindable. It
  presents a
  keyboard-driven fuzzy picker over local actions, bounded shell history, and
  recent OSC 7 directories. Accepting a history or directory row types that
  text into the active pane without appending a newline; accepting an action
  dispatches the local action after the overlay closes.

- Output replay overlay: opt-in per-session output recording (`session_replay`,
  off by default) keeps a bounded in-memory ring of recent screen frames —
  capped by both 600 frames and 24 MiB, whichever binds first, with the oldest
  evicted — recorded by the PTY pump off the render path. The `session-replay`
  bindable action (default chord `Ctrl+Shift+R`, rebindable) opens a
  keyboard-scrubbable overlay over a frozen, fully decoupled clone of the ring:
  `←`/`→` step, `PgUp`/`PgDn` jump ten, `Home`/`End` go to the ring ends. Replay
  is presentation-only — it never mutates live core terminal state, so the live
  frame is byte-identical whether or not the overlay is active. Recording is
  local-only: frames live only in memory, never written to disk or sent over the
  network, and are dropped when the session closes or recording is turned off.

- Connection-manager overlay: exposed as the `connection-manager` bindable
  action, which ships a default `Ctrl+Shift+S` chord (also reachable from the
  right-click menu) and remains rebindable. It presents
  a keyboard-driven, type-to-filter list of saved hosts merged from the
  OdyTTY-owned `hosts.conf` and, only when `ssh_config_hosts` is enabled, the
  name-only OpenSSH-config import; fuzzy matching ranks over alias, host name,
  and user. Selecting a host hands the connect action a name-only target to
  spawn (system `ssh`); the overlay itself is presentation-only and never
  mutates live core terminal state, so the live frame is byte-identical whether
  or not it is active. With the opt-in off, the overlay shows OdyTTY-owned hosts
  only and OdyTTY never reads or references `~/.ssh`.

  Beyond quick-connect, the
  overlay reaches saved hosts several ways: typing a `[user@]host[:port]` that
  matches no saved host offers an ad-hoc **Connect to: …** row (Enter connects,
  Shift+Enter connects and appends a `hosts.conf` block); an in-app **Add / Edit
  connection** form writes or edits a single block with a byte-span splice that
  leaves every other block, comment, and unknown field byte-for-byte untouched,
  carries an optional `IdentityFile` path (adds `ssh -i`, never a stored secret)
  and a reserved `Protocol` field, and offers a **Test connection** probe that
  reports an honest tri-state result without ever handling a password. A host-row
  right-click menu opens the selected host in a new tab or a fresh host-bound
  workspace, binds the current workspace to it, or (for OdyTTY-owned rows) edits
  or removes it.

- Remote shell integration (`remote_integration`, default on; `remote_reuse`,
  default on; `remote_tmux`, default off): the connect path builds the remote
  `ssh` argv through a single owned builder (`src/ssh_connect.rs`). With
  integration on it injects a bash-only bootstrap — an inline base64 rcfile
  materialized to a temp file on the remote and exec'd as an interactive shell,
  carrying OdyTTY's OSC 133 boundaries onto the remote with nothing persisted
  there; a non-bash shell or any failure degrades to a byte-identical plain
  `ssh`. Reuse layers `ControlMaster=auto`/`ControlPersist` over an
  OdyTTY-owned control socket so repeat tabs to a host skip the handshake;
  tmux wraps the remote shell in `tmux new-session -A -s odytty` for
  reconnect-survivable persistence. Each is globally configurable and
  per-host-overridable in `hosts.conf` (`Integration`/`Reuse`/`Tmux`).

  A
  dropped remote tab is held open with an in-pane reconnect prompt
  (Enter re-establishes in the same tab, Esc/Ctrl+D closes). Pasting a
  clipboard image into an integrated remote tab arms a confirm-first upload
  (`remote_image_paste`, default `ask`): the image is streamed over the existing
  `ssh` connection to a `0600` private temp file on the remote, then a one-line
  notice reports the path and copies it to the local clipboard — the path is not
  typed into the shell, so no stray command runs. A size cap and best-effort
  cleanup apply, and reconnected/restored remote tabs support it. A workspace can be bound
  to a saved host so **New Tab** connects there, with a **New Local Tab**
  escape.

  ControlMaster reuse is compiled out on a Windows client (OpenSSH
  there has no connection multiplexing); integration, tmux, reconnect, image
  paste-through, and workspace binding are cross-platform.

### Platform Integration

- Interactive / clickable file paths (`interactive_paths`, master gate, default
  off): when enabled, OdyTTY syntactically detects file and directory spans in
  the visible row under the pointer and stat-gates them against the filesystem
  (`std::fs::symlink_metadata`, no symlink traversal), so only real paths arm.
  Ctrl+click (Cmd+click on macOS) opens a resolved path through the platform
  default opener; a
  `:line[:col]` suffix routes to an editor by an editor-argument matrix
  (`vim`/`nvim`/`vi`, `code`, `emacs`/`emacsclient`, `hx`/`helix`/`subl`/
  `micro`, `nano`, with an open-and-drop-position fallback) chosen by
  `interactive_paths_editor` (empty by default → `$EDITOR` → `$VISUAL`). Three
  sub-keys default on but are inert until the master gate is on:
  `interactive_paths_barewords` (also detect basename-plus-extension tokens),
  `interactive_paths_click_hint` (a transient platform-specific Ctrl/Cmd-click
  teaching chip after repeated plain mis-clicks on a resolved path), and
  `interactive_paths_image_inline` (route image paths to the in-app viewer). The
  right-click menu adds **Open**, **Open in OdyTTY**, **Open With…**, **Copy
  Path**, **Copy File** (`file://` URI), and **Reveal in File Manager**. Open
  With uses freedesktop MIME/desktop enumeration on Linux and `NSWorkspace` on
  macOS; Windows currently opens an empty picker because application enumeration
  is not implemented there.

  Every open routes through a single argv-only detached-spawn point — never
  `sh -c` — so a filename containing `;`, `$()`, backticks, or spaces is an inert
  argv element.

- In-app image lightbox: Ctrl+clicking a resolved `png`/`jpg`/`jpeg`/`webp` path
  on Linux/Windows, Cmd+clicking it on macOS, or choosing **Open in OdyTTY** opens
  a decoded image in a free-floating
  overlay drawn after the post-process pass directly onto the swapchain, so CRT
  and bloom never touch it. The image is aspect-fit to a fraction of the
  viewport and never upscaled past source, behind a dark scrim. Dismiss with
  `Esc` or a click outside the image rect. Decode is bounded
  (`12_000` px per axis, `256 MiB` allocation) and content-sniffed, so a lying
  `.png` extension cannot mislead the decoder.

- Keyboard hints / quick-select (`hints` bindable action, default `Ctrl+Shift+L`,
  rebindable): a pattern scanner labels URLs, paths, and SHA hashes in the
  focused pane's visible viewport (joining soft-wrapped rows) with home-row
  letter labels; completing a label copies the exact matched text to the
  clipboard and closes. Presentation-only — terminal state is never modified, and
  a zero-match activation consumes the chord without leaking it to the PTY.

- Manage Sessions overlay (`session-attach` bindable action, default
  `Ctrl+Shift+A`, rebindable; also reachable from the right-click menu): an
  in-window, type-to-filter list of live detached sessions, the GUI counterpart
  to the `odytty list` / `odytty attach` CLI verbs. Selecting a session attaches
  it; an already-open session is de-duplicated to its existing tab instead of
  opening a second copy, and otherwise a New-tab / Replace-current dialog
  chooses placement (Replace attaches the new session, then cleanly detaches the
  old hosted tab so it stays reattachable). Right-clicking a row requests a kill
  with a "Terminate session" confirm; on confirm OdyTTY kills the host (a stale
  socket is treated as already-gone) and reopens the manager so the dead row
  disappears. On Windows the overlay opens empty and attach is unavailable.

- Detach & switch (Unix-only right-click menu item): spawns a **fresh**
  managed session — honestly a spawn, not live-process migration, because the
  focused pane's shell is this window's own child and cannot be handed off — in
  the focused pane's OSC 7 working directory, attaches it in a new tab, and
  switches. A three-way dialog chooses Swap (close the original pane once the
  managed session is live), Keep both, or Cancel. The order is always
  spawn → attach → (Swap only) close-original, so the original pane is never
  closed before the new session confirms live; spawn or attach failure raises a
  transient notice and leaves the pane untouched.

### Accessibility

- Color-vision-deficiency accessibility (`cvd_mode`, default off, values
  `off`/`protan`/`deutan`/`tritan`; `cvd_strength`, default `1.0`): OKLab
  daltonization of the theme palette (the 16 ANSI slots plus cursor/selection/
  search roles), neutralizing the lost opponent axis and steering the cue onto a
  retained one, then re-flooring against a WCAG-AA authoring gate. Palette-only:
  indexed-256 and application truecolor are not remapped. `cvd_mode = off` or
  `cvd_strength = 0.0` is a pixel-identical passthrough. See
  [`docs/accessibility.md`](docs/accessibility.md) for the accessibility knobs
  (CVD modes, `min_contrast`, `focus_dim`, bell).

### Platform And Packaging

Linux, macOS, and Windows behavior is defined in
[Cross-Platform Architecture](#cross-platform-architecture). Published package
formats and install channels are defined in the
[Install Guide](docs/install.md).

### Out Of Scope

- Kitty animation (`a=f`, `a=a`) and Unicode placeholder rendering

- iTerm2 graphics protocol

- Ligature/stylistic-set shaping (strategy remains undecided; implementation deferred
  until a specific trigger condition is met)

- Profiles and cross-session multiplexing (panes/splits within a window are now
  supported — see above)

- Shell integration beyond OSC 7 cwd tracking and OSC 133 prompt/command marks
  plus the current command-aware native actions

- Plugin systems, AI features, rich dashboards, or nonstandard terminal
  semantics

- Heavy animation or effects that compromise readability or latency

- Flatpak packaging; the AppImage is the portable single-file Linux option

## Cross-Platform Architecture

Linux is the primary target. macOS and Windows are additional build targets,
each exercised on its own CI runner; all three legs (`ubuntu-latest`,
`macos-latest`, `windows-latest`) are blocking regression gates. The
platform-divergent surface is small, localized, and `#[cfg]`-gated, so Windows
code is physically absent from a Linux/macOS build and cannot regress it — the
Linux/macOS byte path is unchanged by the port.

### Platform Summary

- **Linux:** The primary target uses the Unix PTY backend, XDG paths, and the
  complete detachable-session feature set.

- **macOS:** The Unix PTY backend remains in use, with Darwin-native runtime
  directory handling and a dedicated blocking CI runner.

- **Windows:** ConPTY, native paths, and Windows-specific open and reveal
  behavior support the v1 terminal surface. Unix socket features remain gated
  off.

### Select The PTY Backend

The PTY layer is the largest platform divergence and is documented under the
Ownership Boundary above. `src/pty/mod.rs` defines a neutral `PtySession`
contract and `#[cfg]`-selects `unix.rs` (rustix/termios PTY) or `windows.rs`
(ConPTY `CreatePseudoConsole`). Every consumer is backend-agnostic.

### Resolve Windows Runtime Resources

Where Unix uses XDG/POSIX paths, the Windows build substitutes platform-native
equivalents, all behind `#[cfg]`:

- **Config & themes** resolve under `%APPDATA%\odytty\` (see Configuration
  Architecture); persistence, live reload, writeback, and the first-run marker
  all function.

- **Kitty image temp transports** (`t=f`/`t=t`) consult `std::env::temp_dir()`
  (`%TEMP%`) instead of `/tmp` + `/dev/shm`, so file-based image transport works;
  the POSIX shared-memory transport (`t=s`) has no Windows analogue and its
  backend body is `#[cfg(unix)]`-only, returning a transport error elsewhere.

- **Host font discovery** scans `%WINDIR%\Fonts` (machine) and
  `%LOCALAPPDATA%\Microsoft\Windows\Fonts` (per-user) in addition to the always-
  present bundled fonts, so host families (Consolas, Cascadia Code, …) are
  selectable.

- **Clickable paths** recognize Windows path shapes — drive-letter absolute
  (`C:\…`, `C:/…`), UNC (`\\server\share`), and backslash separators — with a
  drive-letter-aware `:line:col` suffix split so `C:\src\main.rs:10:5` peels the
  position without consuming the drive colon.

- **Default-open & reveal** route through `cmd /C start` (open) and
  `explorer /select,` (reveal) instead of `xdg-open`.

### Gate Unix-Only Subsystems

Detachable and resumable sessions and the attach UI rest on Unix-domain sockets
and POSIX runtime-directory semantics. The `session_host` transport
(`socket.rs`/`host.rs`/`client.rs`/`registry.rs`),
`src/native/attach.rs`, the attach overlays, the
`SessionSource::Attached` Session variant and its match arms, and the
`--interactive` headless raw-mode path (`src/app.rs`, a wholly POSIX module) are
all `#[cfg(unix)]`-gated.

The pure pieces stay cross-platform on purpose: the `session_host` wire
`protocol.rs` (neutral core types), the `SessionCliCommand` parser (so
`--help`/usage strings stay byte-identical), and the
`BindableAction::SessionAttach` keybind variant. On Windows that action opens an
empty presentation overlay and any attach attempt is rejected cleanly, keeping
the shared keybind catalog and its test identical.

The CLI execution boundary prints a clean "not supported on Windows yet" rather
than panicking. Non-detached SSH-in-a-tab still works on Windows because it uses
the local ConPTY path, not `session_host`.

### Windows v1 Scope

- **Supported:** Local terminal behavior including tabs, splits, rendering,
  themes, effects, and per-pane images; persistent config via `%APPDATA%`;
  host-font scanning of `%WINDIR%\Fonts` and
  `%LOCALAPPDATA%\Microsoft\Windows\Fonts`; Windows clickable-path detection;
  default-open and reveal; and non-detached SSH-in-a-tab.

- **Deferred:** Detachable sessions, detached or resumable SSH,
  `--interactive` headless mode, the full "Open With…" application list
  (desktop-entry enumeration is freedesktop-only), registry font display-name
  parsing, and PowerShell/PSReadLine history in the command palette.

- **Silent fallbacks:** Command-palette shell history is empty; panic and
  application logs land in `%LOCALAPPDATA%\odytty`. Hostname resolution uses
  `GetComputerNameExW`, so OSC 7 foreign-host checks remain available.

### Windows Natural-Exit Teardown

Every successful ConPTY spawn starts a child-process waiter on a duplicated
process handle. A natural child exit closes the pseudoconsole, wakes the output
reader through EOF, and lets the tab follow the normal session teardown path.

## Post-Process Pipeline Architecture

*Source: `src/native/gpu.rs` (`PostProcessResources`, `post_active`,
`draw_scene`, `encode_scene_pass`, post composite encode path),
`src/shaders/bloom.wgsl`, `tests/gpu_composite_smoke.rs`.*

### Preserve The Plain Bypass

The renderer carries a **lazy post-process scaffold**: an offscreen render
target, a nearest-clamp sampler, and a fullscreen-triangle composite pipeline.
In the plain/fast profile the pipeline is **dormant**: `post_active()` returns `false`, the
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

### Sequence The Tier-3 Pipeline

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
   swapchain surface stays `Rgba8UnormSrgb`.

   When a post-process pass is active
   the scene pipelines (cell, color-glyph, image) are rebuilt to render into
   the HDR offscreen format and rebuilt back when it is disabled, so the default
   path stays on the swapchain format and byte-identical.

3. **Bloom / phosphor glow (landed):** bright-pass threshold + half-res
   separable blur + additive composite, enabled in the fresh-install ambient
   baseline behind the `bloom` setting and gated on adapter HDR support. The
   built-in threshold is fixed at `0.70`; `auto` resolves to that fixed default.

4. **CRT / retro profile core (landed):** bounded scanlines + vignette, enabled
   in the fresh-install ambient baseline behind `crt` and sharing the same
   offscreen scene render and final composite pass as bloom. Because
   post-composite dimming cannot feed back into the CPU minimum-contrast
   resolver, the shader clamps scanline/vignette strength and enforces a
   brightness floor so lit cells are never zeroed.
   `retro=on` is a stronger preset over the same bloom/CRT path and applies a
   subtle `0.025` curvature. Configurable curvature ships as `crt_curvature`
   (`0.0`–`0.12`, default `0.0`); chromatic aberration is deferred.

Cursor motion trail (`cursor_trail`, on by default since v0.6.0): a short fading
after-image that trails the cursor as it glides between cells, drawn behind the cursor block
in the theme cursor color. Only visible while cursor slide (`cursor_motion`) is
also on; fully decays as the glide settles. New-output fade (`new_output_fade`,
off by default): rows of freshly arrived output fade in over a short ramp at the
live tail. Both effects landed in the Tier-3 sequencing after bloom and CRT
profile were proven.

GPU quality / per-effect settings panel controls follow.

### Gate Visual Effects For Readability

The **minimum-contrast floor** (`enforce_min_contrast`,
`src/color.rs:enforce_min_contrast`) runs at **CPU color-resolve time** — the
last step of the per-cell resolve closure inside `build_cell_vertices_with_focus_dim_into`
— before the vertex buffer is written and long before any GPU scene or
post-process pass executes. There is no within-frame feedback path from the GPU
composite back to the CPU resolve step.

**Consequence (binding design rule):** post-process effects **cannot** rely on
the minimum-contrast floor to clean up legibility after the fact. Every Tier-3 effect must
be **structurally unable** to harm body-text legibility by construction:

- **Bloom / additive glow:** the threshold floor is fixed at `0.70`, above the
  luminance of normal body text, so body text is never in the bright set that
  glow acts on. Composition is additive (never replace), so background regions
  brighten but existing foreground coverage is only increased, not reduced.

- **CRT scanlines / vignette:** modulate brightness uniformly; paired with
  an intensity cap that keeps the worst-case dimming above the body-text
  legibility floor. The user-configured `min_contrast` floor is the explicit
  safety net at the CPU level; effects must not require it.

- **Background treatments** (`background_treatment`, `off`/`color`/`gradient`/`vignette`,
  `image`, **default `image`** since v0.6.0): position-based per-cell background
  darkening (gradient toward the bottom; vignette toward the edges/corners) and
  static PNG/JPEG/WebP background images behind the grid. OdyTTY ships an original
  "Dark Waves" background **embedded into the binary** (`assets/backgrounds/`,
  selected by the `background_image = default` sentinel) and shown by default, so
  the OdysseyOS identity is the out-of-the-box look on every install target with
  no external asset to resolve. The opt-out is `background_treatment = color`
  (theme background only) or `background_image = none`. Legibility is
  safe-by-construction: gradient/vignette darken is applied to the per-cell
  background **before** the minimum-contrast floor resolves, and image backgrounds
  use a readability scrim plus `cell_bg_opacity` so the floor sees a bounded
  background.

  The knob is forced off under the plain renderer profile. Whole-window
  transparency ships separately (`window_transparency` / `window_opacity`,
  default off): only background layers scale toward the desktop while text,
  cursor, selection, and overlays stay fully opaque. Blur-behind (acrylic)
  compositing remains a planned future extension.

- Any new Tier-3 effect must document its structural legibility guarantee
  before landing.

## Stack

### Core Crates

OdyTTY is a Linux-first Rust application built around these primary crates:

| Crate | Role |
| --- | --- |
| `winit` | Windowing |
| `wgpu` | GPU rendering through Vulkan and other platform backends |
| `ab_glyph` | Normal-text font rasterization |
| `swash` | Emoji discovery, shaping, and color-font probing |
| `unicode-width` | Terminal cell widths |
| `arboard` | Clipboard integration |
| `rustix` | Unix PTY and termios access |
| `png` | PNG decoding for Kitty `f=100` |
| `image` | PNG, JPEG, and WebP wallpaper decoding |

Normal text remains on `ab_glyph`; `swash` is limited to the emoji and
color-font path. Platform syscall crates are split by target:
`rustix`/`libc` sit under `[target.'cfg(unix)'.dependencies]`, while the
`windows` crate supports the ConPTY backend under
`[target.'cfg(windows)'.dependencies]`.

The `windows` crate is pinned to the `0.62` line already locked by `wgpu`, so
the dependency graph does not gain a second copy.

### Keep Core And Native Boundaries Separate

The terminal core is a distinct boundary from the native app. The `core` module
never imports windowing, GPU, or rendering code; it consumes VT bytes via the
owned parser and exposes a `Snapshot` for the renderer to consume. The native
module owns the `winit` event loop, `wgpu` surface, glyph atlas, grid vertex
builder, and image layer, consuming core snapshots through a narrow seam.

### Render Cell-Based Text

Text is cell-based: each codepoint occupies one or two columns (`unicode-width`
consistent with core), and all coordinate systems are per-cell. The default
body font is bundled **Victor Mono** (SIL OFL 1.1) at 20 logical pixels;
**JetBrains Mono** is also bundled and remains selectable. SGR italic maps to
Victor Mono's roman-slant Oblique faces. The font picker lists families in two
subgroups — **Bundled Fonts** (Victor Mono, JetBrains Mono — always present,
loaded from compiled-in bytes) and **System Fonts** (host monospace families); a
host copy of a bundled family is shown once, in the bundled group.

Either group
resolves with zero further config. Symbol/Nerd-font icons — both the Private Use
Area sets and the standard symbol blocks body fonts lack (arrows, power symbols,
Dingbats such as the prompt `❯`, …) — resolve through a bundled **Symbols Nerd
Font fallback chain** (enabled by default). The chain order is **explicit >
bundled > host**, where *bundled* is **two** version-pinned faces — Nerd Fonts
**v3.4.0** then **v2.3.3** — so the glyph pack covers both codepoint eras out of
the box (v3 relocated PUA icons such as the archway `U+F557` and python `U+F81F`
into new slots; the v2 face fills the ones it emptied). The atlas walks the chain
per glyph and rasterizes from the first face that has it, so coverage is the
union of every face: an explicit `ODYTTY_SYMBOL_FONT` / `symbol_font` override
leads, the bundled faces guarantee the out-of-the-box path never depends on
host-installed Nerd fonts, and a host-discovered face can extend coverage at the
tail.

`symbol_map` per-range overrides and the settings toggle remain in effect.
`--show-config` reports the live `symbol_fallback` state and the resolved
`symbol_font_source` as the full chain, joined with ` > ` (e.g.
`bundled > bundled > host:<path>`, or `disabled`). The
glyph atlas uses one monospace face for regular text, with bold/italic/bold-italic faces
loaded when discovered by filename convention.

When a style face is absent,
`StyleFonts::synthetic_mask()` derives a per-face synthesis flag by comparing
loaded `Arc` identities; `GlyphAtlas::set_synthetic_styles` receives those bits
and applies a `SynthTransform` during rasterization — italic via horizontal
shear (tan 12° ≈ 0.2126), bold via double-strike at a sub-pixel embolden offset,
bold-italic by composing both. Real faces always take precedence; synthesis
activates only for genuinely absent slots. Ligatures and complex shaping are not
implemented; each atlas entry is a single character rasterized into its cell
or two-cell slot.

### Store Cell Attributes Compactly

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

### Render Color Emoji

The accepted direction is a separate
premultiplied-RGBA color-glyph path, distinct from the current monochrome
coverage shader. `swash` is chosen for emoji shaping and rasterization: it
covers CBDT/CBLC bitmap strikes (Noto Color Emoji's format on Linux),
COLR/CPAL, and sbix, while providing full cluster shaping — VS15/VS16
selectors, modifier sequences, ZWJ sequences, flags, and keycaps. Font
rasterization remains external per the project boundary; atlas management,
placement, blending policy, fallback routing, and terminal-cell behavior are
OdyTTY-owned. A dedicated `ColorGlyphAtlas` stores premultiplied-RGBA source
pixels keyed by shaped cluster, font identity, and physical cell size alongside
the existing coverage atlas.

Emoji cells sample source pixels directly and are
never tinted by SGR foreground color. Linux font discovery probes fontconfig
only for Noto Color Emoji; directory scanning also finds Noto Color Emoji on
Linux and Apple Color Emoji on macOS. Stock Windows Segoe UI Emoji is not
discovered or rasterized, so Windows falls back to monochrome glyphs. An
explicit per-session setting is planned as a follow-up. VS15 (`U+FE0E`) forces
the text path; VS16 (`U+FE0F`) forces the emoji path; characters with
Unicode `Emoji_Presentation=Yes` default to emoji; others default to text. The
predicate must not claim whole symbol blocks: text-default Dingbats/geometric
markers stay on the monochrome coverage/symbol fallback path.

RGI clusters are
treated as atomic if `swash` shapes them to a single color glyph; unsupported
clusters degrade per-codepoint to the existing fallback path.
Draw order: cell backgrounds → below-text images → coverage glyphs and line
decorations → color emoji glyphs → cursor and overlays. COLR v1 and SVG-in-OT
are deferred but architecturally permitted; the boundary rule (rasterization
external, placement owned) applies to those paths as well. The delivery ladder
is tracked in `TODO.md`.

### First Emoji Increment

The first `src/emoji/` increment was a renderer-free probe
module: no atlas, GPU, shader, or core terminal code. Discovery runs in two
stages. First, `fc-match -f '%{file}\n%{family}' 'Noto Color Emoji'` is invoked
directly; the returned path and family string are checked against a strict
identity predicate (normalized filename or family must contain `notocoloremoji`),
so generic fontconfig substitution fonts are rejected. If fontconfig is
unavailable or returns a non-matching result, a bounded directory scan covers
the standard Linux directories, the macOS system/user font directories, or the
Windows machine/per-user font directories at maximum depth 6 and a 20 000-file
cap, matching only Noto Color Emoji or Apple Color Emoji by normalized filename
stem. The Windows scan therefore finds no supported stock color face.

When no Noto Color Emoji is found, the module returns `None` and
all downstream code skips the emoji path without error. On a successful find,
the module loads the face as a borrowed `swash::FontRef` and probes: detected
color-table set (CBDT/CBLC, sbix, COLR/CPAL, SVG), OpenType family name string,
and per representative-sequence records: shaped glyph ids, cluster structure
(source byte range, advance, ligature/complex flags), and per-sequence fallback
outcome (`Resolved` when any shaped glyph id is non-zero, `MissingGlyph`
otherwise). Default tests are hermetic (temp-dir filename discovery, fixed
sequence list, non-color format detection for a monospace outline font).

The host-dependent full probe is `#[ignore]`-gated. Run it with:

```console
cargo test emoji -- --ignored
```

The test exits cleanly when the font is absent.

### Second Emoji Increment

`src/emoji/color_atlas.rs` adds the OdyTTY-owned
`ColorGlyphAtlas`: a grow-only `Rgba8Unorm` atlas for premultiplied source
pixels, keyed by `(font identity, glyph-or-cluster id, physical px size,
scale)` rather than by character. Slots span one or two terminal cells; wide
color glyphs draw once from the lead cell and continuation cells emit nothing.
The native renderer owns a dedicated color-glyph texture, vertex buffer, WGSL
shader, and premultiplied-alpha blend state. At this increment the segment
received no live runs because the real decoder had not yet supplied decoded
swash glyphs, but synthetic tests pinned the atlas bookkeeping, UVs, dirty
revision, pass ordering, and wide-cell contract. The third increment below
activates the live path.

Selection/search backgrounds render under color glyphs; OdyTTY does not tint or
recolor source emoji pixels with SGR foreground colors.

### Third Emoji Increment

`src/emoji/render.rs` activates the first live color emoji
path for Linux Noto Color Emoji CBDT/CBLC bitmaps. `EmojiRasterizer` discovers
the Noto face, shapes each eligible terminal-cell grapheme with `swash`, renders
single-glyph color bitmaps with best-fit strike selection, scales/centers them
into the one- or two-cell atlas slot, and premultiplies RGBA before insertion.
VS15 (`U+FE0E`) forces the text/coverage path; VS16 (`U+FE0F`) and default
emoji-presentation codepoints request color. The default presentation gate uses
the Unicode property ranges rather than whole Dingbats/Misc Symbols blocks, so
text-default markers such as `U+2731` and `U+25CF` remain eligible for normal
monochrome font fallback.

The native renderer computes runs
from the snapshot before coverage-atlas insertion, skips normal monochrome
foreground quads only for resident color runs, uploads dirty color-atlas pixels,
and draws the dedicated color segment in the established draw order. If
discovery, shaping, bitmap rendering, color-face coverage, or atlas insertion
fails, no color run is emitted and the existing coverage/fallback path remains
visible.

### Fourth Emoji Increment

The live color path reconstructs bounded multi-codepoint
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

### Bound Emoji Atlas Capacity

`ColorGlyphAtlas` capacity is bounded and
corruption-safe as implemented. The atlas starts at 16 columns by four rows,
grows in four-row chunks, and caps at 4096 resident color glyph/cluster slots.
At the cap, a new insertion returns `ColorGlyphAtlasError::Full`; existing
slots stay lookupable, no slot is overwritten, `revision` is unchanged, and a
failed insert does not mark the atlas dirty. The renderer therefore degrades by
omitting the new color run and leaving fallback rendering visible.

No eviction
policy is added until observed workloads prove 4096 resident color glyphs is too
small.
