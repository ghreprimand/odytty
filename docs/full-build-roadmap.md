# OdyTTY Full Build Roadmap

This roadmap records OdyTTY's shipped foundation, forward tracks, deliberate
deferrals, and durable product boundaries.

## Contents

- [Product Direction](#product-direction)
- [How To Use This Roadmap](#how-to-use-this-roadmap)
- [Shipped Foundations](#shipped-foundations)
- [Track 1: Configuration And In-App UX](#track-1--configuration-and-in-app-ux)
- [Track 2: Text And Rendering Quality](#track-2--text-and-rendering-quality)
- [Track 3: Readability And Perceptual Color](#track-3--readability-and-perceptual-color)
- [Track 4: Visual Identity And Depth](#track-4--visual-identity-and-depth)
- [Track 5: Shell And Prompt Integration](#track-5--shell-and-prompt-integration)
- [Track 6: Interaction And Productivity](#track-6--interaction-and-productivity)
- [Track 7: Theming And Palettes](#track-7--theming-and-palettes)
- [Track 8: Positioning And Performance](#track-8--positioning-and-performance-posture)
- [Track 9: Multiple Contexts](#track-9--multiple-contexts-tabs-panes-and-sessions)
- [Track 10: Packaging, Release, And Platform](#track-10--packaging-release-and-platform)
- [Track 11: Exploratory Work](#track-11--exploratory-and-far-future)
- [Non-Goals](#non-goals)
- [Open Architectural Questions](#open-architectural-questions)
- [Near-Term Focus](#near-term-focus)

## Product Direction

This document captures the full build direction after the first meaningful
prototype. It is not a promise to build every idea immediately; it records what
a serious OdyTTY would need if the project keeps justifying itself. The long
tail remains visible even when it is not current work.

The core rule never changes: terminal correctness, readable text, predictable
input, and stable performance outrank visual novelty. At the same time, visual
quality and a distinctive identity are defining pillars of the product, not
decoration. OdyTTY aims to be a distinctive, well-crafted terminal that stands
on its own merits — judged against its own quality bar, not framed as a contest
with anything else. Mature terminals (xterm, Konsole, and others) serve only as
compatibility references for correctness, never as implementation sources.

A second defining pillar is **foundation ownership**. Every byte from the PTY to
the glyph quad passes exclusively through OdyTTY-owned code: the PTY layer, the
escape-sequence parser, the terminal model, the renderer geometry, and the
shaders. External crates are acceptable only below the product line — font
rasterization, GPU API, windowing, clipboard transport, image-format decoding
(PNG/JPEG/WebP), and Unicode character data. This ownership boundary is real
and in production: the owned PTY layer and the clean-room VT parser ship today;
`vte`, `portable-pty`, and `crossterm` have all been removed from the dependency
tree.

A third pillar, which shapes much of the roadmap below, is **configuration you
never hand-edit**. The defining UX goal is that everything is discoverable and
adjustable from in-app overlays that write the config file for you. Hand-editing
a config file is a fallback, never a requirement.

---

## How To Use This Roadmap

The forward work is organized by theme, and each item carries a horizon tag:

| Horizon | Meaning |
| --- | --- |
| **Now** | Actively in progress or the immediate next increment |
| **Next** | Near-term, queued, and ready to begin |
| **Later** | Wanted, but waiting on a foundation or evidence baseline |
| **Someday** | Deliberately deferred and promoted only by an explicit decision |

Two rules govern every feature in every track:

1. **Behind a setting, with an off switch.** Anything beyond a plain terminal
   must be configurable. The plain, fast path stays byte-for-byte identical to a
   renderer without the feature, and is tested as such.
2. **Readability is a floor, not a preference.** A minimum-contrast guarantee is
   the safety net that every visual feature validates against; no effect may
   push text below the legibility floor.

---

## Shipped Foundations

The foundation is broad and solid. The following are complete and in production.

### Owned Byte Path

A native window runs a real local shell and renders GPU-backed monospaced text
through `wgpu`. Linux and macOS use the owned Unix PTY backend; Windows uses the
owned ConPTY backend. The VT parser is a clean-room two-layer DEC ANSI pipeline
built from primary specifications; the terminal model, renderer geometry, and
shaders are OdyTTY-originated. The owned path is the only path.

### Terminal Correctness

Printing, cursor movement, SGR, erase, scrollback,
alternate screen, save/restore, scroll regions, bracketed paste, insert/delete
lines and characters, scroll up/down, origin mode, soft/hard reset, repeat, tab
stops, and device-attribute replies. Alternate-screen behavior is hardened
against editors, pagers, and full-screen apps with a deterministic fixture
matrix. Reporting probes (mode queries, size reports, version) are in place.

### Unicode And Text

Mid-stream UTF-8 decoding, wide-character (CJK/width-2)
write/erase coherence with 2-cell atlas slots, zero-width combining-mark
attachment, and a defined ambiguous-width policy.

### Input And Interaction

The full daily-driver loop: scrollback search; refined
selection (double-click word, triple-click line, drag-scroll past the viewport,
scrollback-aware anchors); clipboard hardening (chunked large paste, bracketed-
paste sanitization, PRIMARY selection, middle-click paste); a right-edge scroll
indicator; configurable cursor shapes and blink policy; configurable key
bindings; window title and focus reporting; mode-aware keyboard encoding; the
Kitty keyboard protocol; and the full mouse-reporting matrix including SGR-pixel
mode.

### First-Class Pointer Use

Beyond the base loop, pointer work is now a polished
surface: extend an existing selection (shift-click to the point, double-click-
then-drag by whole words, triple-click-then-drag by whole lines); rectangular /
block (column) selection by modifier-drag; velocity-proportional drag-autoscroll
(the further past the edge you drag, the faster scrollback advances, bounded);
optional copy-on-select; a draggable scroll-thumb that scrubs scrollback; and
configurable wheel speed with modifier-wheel font zoom. Local selection never
disturbs an application's own mouse reporting.

### Shell Integration

Semantic prompt marking (OSC 133) records prompt,
command, and output boundaries per row, with reflow-stable marks — the
foundation the command-aware navigation builds on.

### Graphics And Media

A complete Sixel decoder wired to live DCS. DA1 does not yet advertise Sixel,
and XTSMGRAPHICS queries are not implemented, so applications that require a
capability probe may not emit it. The graphics stack also includes the
Kitty graphics protocol (direct RGB/RGBA and PNG transmit, file transports on
all platforms and shared-memory transport on Unix, with security hardening,
placements with z-order/crop/scale/offset, Unicode placeholders (`U=1`),
animation frame/control/composition actions, delete and query operations);
iTerm2 inline images (`OSC 1337 ; File=`, bounded by the OSC payload cap); a
GPU image layer; and color emoji on all three platforms (ZWJ families, flags,
keycaps, skin-tone modifiers, variation selectors) via a dedicated RGBA
color-glyph atlas covering bitmap strikes, COLR/CPAL v0 layers, and COLR v1
Paint graphs, including stock Windows Segoe UI Emoji, whose cluster coverage
is bounded by the host font (no regional-indicator flag glyphs on stock
Windows; see `docs/features.md`).

### Text Rendering Quality

Bearing-aware glyph quads, bold/italic style faces
with synthetic fallback, the full attribute set (underline, strikethrough, dim,
inverse, hidden), optional subpixel anti-aliasing with an energy-conserving LCD
fringe filter, tunable text gamma, stem darkening, HiDPI scale-factor tracking
with debounced rebuild, and a headless CPU compositor for structural pixel-level
assertions.

### Performance

Lazy scrollback re-wrap on width change and a width-unchanged resize fast path
([dated internal measurements](../DEVLOG.md), not cross-terminal comparisons),
instanced cell geometry (one compact per-quad instance expanded in the vertex
shader, over reusable CPU storage and grow-only GPU buffers), resize debounce,
and a render invalidation/retained-frame system. A protocol-governed idle
resource comparison against other terminals is published in
[docs/benchmark-results.md](./benchmark-results.md).

### Configuration And Themes

File-based configuration with live reload and a
clear precedence model (defaults < config file < environment); an in-window
overlay framework; an in-app settings panel where every setting is editable,
live-applied, and written back to the config file; a live theme picker; an
in-app custom theme builder (clone, tweak, live preview, save, and capture of
the pane's current dynamic colors as a new draft); and CLI config
introspection. A dependency-free `.theme` format, a full 16-color + bright ANSI
palette plus semantic roles, and a curated 142-theme built-in library (dark and
light, all contrast-validated).

### The Visual Engine

A perceptual color pipeline (OKLab/OKLCH) with linear-
space blending; a configurable minimum-contrast readability floor; geometric
box-drawing, block, and Powerline rendering at exact cell geometry; symbol /
Nerd-font fallback for prompt icons; themed cursor/selection/search roles; focus
dimming; background gradient/vignette/image treatments; a post-process pipeline
on an HDR offscreen target; bloom / phosphor glow; a CRT/retro profile
(scanlines, soft-knee vignette, subtle curvature); and a render-quality master
control with a hard plain/fast bypass. Effects are configurable with explicit
opt-outs, and the plain renderer remains pixel-identical when selected.

### Readability And Accessibility

The perceptual pipeline now carries four
readability flagships, each pure readability or accessibility: a universal
legibility guarantee that extends the minimum-contrast floor to all application
text (256-color and truecolor), nudging the foreground in perceptual color space
to clear the floor while preserving hue; a perceptual-safe theme builder with
mouse-driven OKLCH (Lightness/Chroma/Hue) sliders, a live contrast readout, and
snap-to-floor so it cannot author an unreadable theme (raw-hex entry stays as the
expert fallback); contrast-aware palette generation that turns a seed color into
a readability-validated theme starting point; and colorblind palette adaptation
that remaps the ANSI palette in perceptual space for color-vision deficiencies.

### Window And Identity

Adjustable window padding with a fully aligned
pixel↔cell coordinate seam, optional themed border, decoration toggle, OS
dark/light following, and a visible tab bar once multiple sessions exist. Linux
windows keep `io.unfinished_works.odytty` as the default Wayland `app_id` and
X11 `WM_CLASS` class, with `odytty` as the X11 instance. Launchers can override
the per-window identity with the equivalent `--app-id` and `--class` space or
equals forms without changing installed desktop metadata.

### Multiple Contexts: Panes, Sessions, Palette, And Connections

OdyTTY runs many shells in one window across three composition layers.

#### Splits And Panes

A tab owns a binary layout tree where each leaf is a session and each node is a
split with a ratio. Every pane keeps its own scrollback, selection, viewport,
search, and cursor, with a 1px themed divider. Single-pane tabs stay
byte-identical to a single session.

Splits are reachable by tmux-compatible prefix bindings, direct GUI chords, and
a right-click context-menu section with live accelerator labels. Dividers are
drag-resizable, and panes support directional focus-move, close, zoom, and
equalize.

#### Persistent And Detachable Sessions (Unix only)

On Unix, a detached session-host process owns the PTYs and terminal models, so a
window can close and a new one can reattach by id. The restored snapshot
includes full scrollback through an OdyTTY-owned, versioned format.

The host socket is a per-user, filesystem-permission-scoped, local-only Unix
socket. It never touches the network, preserving the privacy posture, and
scrollback persistence stays on the local filesystem. Opt-in per-session output
recording with a bounded ring buffer feeds a scrubbable, presentation-only
replay overlay.

The Unix CLI surface adds `odytty list`, `odytty attach [<id>]`, and
`odytty new`. An attach without an id opens the sole session or lists the
available sessions when several exist. New sessions are always detached;
`--detached` is a parsed no-op alias. `odytty new` also parses the application
identity aliases for launcher compatibility, but does not persist them into
host metadata or apply them to a later attach window.

#### Command Palette

The in-window keyboard-driven fuzzy finder covers terminal-local actions and
settings, shell history, and recent directories. It is presentation-only, with
an owned scorer and read-only, bounded history access.

#### Connection Manager

The overlay lists saved hosts and quick-connects by spawning the system `ssh` in
a new pane or session. OdyTTY never handles credentials or private keys itself.

Reading host *names* from `~/.ssh/config` is read-only, opt-in, and parses names
only. An OdyTTY-owned hosts list lets the feature work without touching
`~/.ssh` at all.

### Discoverability

The command palette, connection manager, session replay, and
theme builder each ship with both a default keybinding and a discoverable menu
entry (a right-click launcher section, and a Themes-section entry for the theme
builder), so the in-app surfaces are reachable without hand-editing config. All
defaults are `Ctrl+Shift`+letter chords that a TUI cannot receive as input, so
the application input path is unperturbed.

### Privacy Posture

No telemetry, no cloud, no account — fully local. The
absence of any phone-home path is a deliberate, stated feature.

### Licensing And Project Identity

GPL-3.0-only with a Developer Certificate of
Origin contribution flow, SPDX headers throughout, and a name/branding notice.

---

## Track 1 — Configuration And In-App UX

The defining experience: discoverable overlays that write the config for you.
The mouse-driven settings panel (click to toggle and cycle, scroll, click-to-
focus, drag-a-slider, click-to-type numeric entry), effect grouping with clearer
labels, and visible font-load failure reporting all ship today.

- **Shipped — In-panel help clarity.** Setting labels and descriptions have
  been swept, grouped, and exposed through the settings panel.
- **Shipped — Consolidated the legacy ambient-scanline path** into the unified
  CRT effects model. `visual=ambient`/`scanlines` are now back-compat aliases
  that route to the CRT scanline effect when no explicit `crt` setting is
  present; the old cell-shader scanline wash is retired.
- **Shipped — First-run onboarding overlay** plus search within the settings
  panel, so features are discoverable from inside the app.
- **Shipped — Customizable keybinding remap UI.** The settings panel can remap
  the core actions and writes back to the config. The `keybinds` config surface
  supports the full set of bindable actions, including tabs.
- **Shipped — Discoverability defaults.** The command palette, connection
  manager, session replay, and theme builder each gained a default keybinding and
  a discoverable menu entry, so the in-app surfaces are reachable without
  hand-editing config.
- **Shipped — CLI introspection: list available fonts**, completing the
  existing introspection helpers.
- **Shipped — Settings completeness.** Every configuration group (15 raw groups,
  including the connection and session groups) maps into one of the panel's 10
  display sections, so no shipped knob is unreachable from the panel; a field
  inventory confirmed every user-facing `Settings` field surfaces through a reachable
  `SettingInfo` row (`native_autoclose` included, via the Development → Advanced
  section). The `keybinds` parser and the in-app key-remap editor cover all 40
  bindable actions — including the theme-builder, session-attach, and workspace
  actions — and the panel's keybinds-row option hint now enumerates the same 40
  in `BindableAction::ALL` order. See [keybindings.md](./keybindings.md) for the
  full keyboard reference.
- **Someday — Profiles.** Named configuration profiles once the base config
  model has settled.

## Track 2 — Text And Rendering Quality

Sharp, stable, comfortable text is a primary product pillar.

- **Next — Effect default-tuning pass.** Once a human-eye baseline exists,
  revisit the conservative default strengths of stem darkening, standalone
  scanlines, and bloom.
- **Shipped — Font weight control.** A global weight knob, distinct from the
  bold attribute.
- **Shipped — Line-height / cell-leading knob.** Adjustable vertical spacing
  between lines.
- **Shipped — Box-drawing thickness knob.** Extends the geometric box-drawing
  renderer.
- **Shipped — Per-codepoint font override.** `symbol_map` maps codepoint ranges
  to chosen fallback font families.
- **Shipped — Scroll feel.** Detented wheels ease the rendered view toward each
  notch over a few frames (`scroll_glide`); high-resolution wheels and touchpads
  track physical travel 1:1 on a continuous pixel lane (`pixel_scroll`). Both
  default on, and the scroll target snaps instantly so there is no input latency.
  In a split each pane glides independently as an eased follower with
  pixel-precise sub-cell smoothness — the pane under the pointer, without stealing
  focus.
- **Shipped — Sub-cell scroll smoothness in splits.** A per-pane vertical
  clip-rect in the pane vertex builders (backgrounds, coverage + colour glyphs,
  cursor, and per-pane overlays) bakes each pane's sub-cell glide remainder into
  its render origin and crops the overflowing partial row to the pane's own
  content rect, so the pixel-precise smoothness the single-pane path already had
  now works inside a split without a partial row bleeding across a divider. Inert
  at rest and single-pane, so those frames are byte-identical.
- **Shipped — Per-pane selection and search overlays in splits.** Selection and
  search-match highlighting render for each pane from its own state rather than
  the focused pane only, so a selection or a search match shows in the correct
  pane regardless of which pane holds keyboard focus; the interactive search
  query bar stays on the focused pane. Painter routing only — no new GPU
  plumbing — and inert / byte-identical on a single-pane tab. Cross-platform
  cell paint, no platform-specific surface.
- **Shipped — Multipane pixel_scroll.** The continuous direct-tracking pixel
  lane (`pixel_scroll`, high-resolution wheels and touchpads) now works inside a
  split, not single-pane only. A pixel-delta glide drives the pane under the
  pointer (not the focused pane, fixing the focus/pointer mismatch), and its
  sub-cell remainder is baked into that pane's render origin and clipped to the
  pane's content rect (the same PANE-SUBCELL-CLIP the glide lane uses), so the
  shift never smears across a divider. Reuses the existing per-pane render — no
  new GPU plumbing — and is inert / byte-identical on a single-pane tab.
  Cross-platform pointer math, no platform-specific surface.
- **Shipped — Inline graphics in splits.** Kitty graphics and Sixel placements
  composite into the per-pane render path, closing the last multipane v1 cut.
  Each pane collects its own visible placements under a session-token namespace
  (so two panes' independent per-terminal image id spaces cannot collide in the
  shared texture cache) and draws them relative to the pane's glide-shifted
  origin, clipped by a per-pane scissor rect that bounds BOTH axes — a
  vertical-only clip could not stop an image bleeding horizontally across a
  column divider. Mutually exclusive with the single-pane image path per frame,
  so single-pane frames are byte-identical (they never touch scissor state).
  Cross-platform raster + placement math, ConPTY graphics parity, no
  platform-specific surface.
- **Shipped — Stem-darkening default activation.** The rasterization machinery
  ships default-on at `0.7`, with `0.0` as the byte-identical opt-out.
- **Someday — Legibility font features.** A narrow, charter-clean subset (such
  as a slashed or dotted zero) is the near-term slice. Ligature coverage now
  spans the ASCII contextual path, a curated non-ASCII operator/arrow
  allowlist, Latin `liga` alongside `calt`, and explicit `ss01`/`ss02`
  settings (see `docs/shaping-roadmap.md`); open-ended stylistic sets and
  arbitrary font features remain deferred.
- **Someday - SVG-in-OT color-font expansion**, after the shipped bitmap,
  COLR/CPAL v0, and COLR v1 Paint-graph paths, only from real evidence.

## Track 3 — Readability And Perceptual Color

This is where OdyTTY invests its differentiation budget, leaning on the
perceptual color pipeline and the contrast floor. Every item here is pure
readability or accessibility.

The four flagships of this track now ship (see
[Shipped Foundations](#shipped-foundations)): the
universal legibility guarantee across 256-color and truecolor text, the
perceptual-safe theme builder with OKLCH sliders and snap-to-floor, contrast-
aware palette generation from a seed, and colorblind palette adaptation. The
readability foundation is in place and is the safety net the visual-identity
work in Track 4 validates against. See [accessibility.md](./accessibility.md)
for the CVD modes, the minimum-contrast floor, focus dimming, and bell behavior.

- **Shipped — Readability scrim primitive.** A computed-bound dim that lets a
  background treatment (Track 4) keep the contrast floor valid by construction,
  bounding the effective luminance behind text to the theme background the floor
  already references. The pure core of the safe-by-construction background work.

## Track 4 — Visual Identity And Depth

Tier-2/Tier-3 visual character. Each ships behind a setting, validated against
the readability floor, with a documented performance cost and a pixel-identical
plain bypass.

- **Shipped — Distinctive cursor / selection / search treatments.** Light up the
  themed selection and search roles with distinct colors, with optional soft
  glow and easing.
- **Shipped — Readability-safe background treatments.** Gradient, vignette, and
  static image backgrounds, where readability dimming is tied structurally to
  the contrast floor. Blur-behind remains future.
- **Shipped — Window-chrome identity.** Themed padding and optional thin
  semantic-role border.
- **Shipped — Window transparency.** A translucent window, on by
  default at `window_opacity` 80, draws the terminal background and chrome bands at a configurable
  opacity while text, cursor, and overlays remain fully opaque. Selection has
  an independent strength control and defaults to fully opaque. Platform
  compositing is required; macOS uses its system compositor, Windows uses DWM,
  and X11 without a compositor degrades to opaque. Blur or acrylic behind the
  window remains future.
- **Shipped — Subtle motion.** Cursor glow, trail, slide, blink fade, and
  fade-in of new output —
  bounded, and fully disable-able.
- **Shipped — Cohesive opt-in retro mode.** A single switch raises bloom, scanlines,
  and vignette into a stronger phosphor reference look. Subtle screen curvature
  is a separate config/environment-only setting, flat by default; the retro
  switch does not force it. Chromatic aberration remains deferred.

## Track 5 — Shell And Prompt Integration

The terminal cooperating with the shell and prompt. This is the highest-leverage
gap to close and unlocks the most downstream value. Semantic prompt marking
(OSC 133) ships today as the foundation.

- **Shipped — Command-aware UX.** Built on prompt marking: jump to the previous or
  next prompt and show a per-command success/failure indicator in the gutter.
  Selecting and copying a single command's output has a core range helper
  (`command_output_cell_range`) but is not yet wired to a UI action.
- **Shipped — Click to position the cursor** at a prompt, using the prompt-marking
  click events. The click slice only — not a takeover of shell input editing.
- **Shipped — Remote shell integration.** Connecting to a saved SSH host carries
  OdyTTY's shell integration onto the remote over an inline, Bash-only bootstrap
  with nothing persisted remotely. It defaults on with a per-host opt-out, and
  the tab is titled `user@host`. Unix clients reuse SSH connections across tabs
  through ControlMaster/ControlPersist; dropped connections hold open with a
  reconnect prompt; sessions can persist with tmux; and clipboard images upload
  through the existing connection into a `0600` temporary file before the path
  is copied to the clipboard.

## Track 6 — Interaction And Productivity

Mostly small, independent ergonomic wins, all overlay-configured.

### Mouse And Pointer Excellence

OdyTTY now feels first-class with a mouse, not only the keyboard. The pointer
surface shipped today covers click-drag selection; double-click word and triple-
click line selection; extend-an-existing-selection (shift-click, and double- or
triple-click-then-drag by words or lines); rectangular / block (column)
selection; velocity-proportional drag-autoscroll; copy-from-selection and
optional copy-on-select; middle-click primary-selection paste; mouse-wheel
scrolling with configurable speed and modifier-wheel font zoom; a draggable
scroll-thumb; the full set of TUI mouse-reporting modes (including pixel-precise
reporting); and hyperlink hover with modifier-click to open. Each behavior change
is opt-in or configurable and never disturbs an application's own mouse handling.

- **Shipped — Right-click context menu**, composed per surface — the terminal
  grid (copy, paste, selection/input actions, settings), a tab slot (new, rename,
  close, close others, move to workspace), the empty tab strip, and the workspace
  rail — so each menu offers only what fits where it was invoked.
- (See also: click-to-position-cursor in Track 5, and the mouse-driven settings
  panel in Track 1.)

### Other Ergonomics

- **Shipped — Keyboard pattern-select / quick-select.** Label on-screen URLs,
  paths, and hashes for keyboard selection and copy.
- **Shipped — Copy mode.** Vim-key keyboard selection of scrollback —
  standalone, no multiplexer required.
- **Shipped — Close-confirmation prompt** when a child process or job is still
  running.
- **Shipped — Exit-behavior setting (`shell_exit_closes`).** Choose what typing
  `exit` does when it would close a whole workspace: close that workspace by
  default, or quit OdyTTY so layout restore can reopen the same set. This governs
  only the shell-exit path. The rail close button and the
  close-tab/close-workspace/close-pane keybindings retain their per-surface
  meaning, and App-mode quit honors the running-job close confirmation.
- **Shipped — Window-decoration control.** Toggle client-side vs server-side
  decorations or borderless mode (compositor-dependent on Linux).
- **Shipped — Bindable clear-input action** (low priority; the standard key
  combinations already cover the common case).
- **Shipped — New tab / new window cwd inheritance.** Opening a new tab or a new
  window starts in the active pane's working directory (from the OSC 7 cwd
  already tracked per pane), not the directory OdyTTY was launched from. New tabs
  seed the directory into both the spawned shell and the pane's advisory cwd; new
  windows carry it via the existing `--working-directory` argument. A pane with no
  tracked cwd falls back to the default directory, and opening is never blocked on
  a missing cwd. Cross-platform (ConPTY honors the working directory; drive-letter
  OSC 7 cwds are normalized).
- **Shipped — Duplicate Tab.** A tab context-menu entry (and the bindable
  `duplicate-tab` action, default chord `Ctrl+Shift+D`) opens a new local tab in
  the active pane's working directory. Honest framing: this is a fresh shell in
  the same directory, not a process fork — scrollback and the running program are
  not copied. Rides the new tab / new window cwd inheritance above.
- **Shipped — Duplicate Workspace.** The workspace-level mirror of Duplicate Tab:
  a workspace context-menu entry (and the bindable `duplicate-workspace` action,
  default chord `Ctrl+Shift+Alt+D` — the tab→workspace Alt escalation of Duplicate
  Tab's chord) opens a fresh workspace whose first shell starts in the active
  pane's working directory. Same honest framing: a fresh shell in the same
  directory, not a process fork. Threads the cwd through the same spawn path New
  Tab uses, so it is cross-platform (ConPTY honors the working directory). Brings
  the bindable-action count to 40.
- **Shipped — Adjustable tab bar height.** The top tab bar's height is
  drag-adjustable the same way the workspace rail's width already is: drag the
  bar's bottom edge to make it taller (up to five text rows, with the labels
  centered vertically in the taller band), and double-click that edge to snap
  back to the default single row. Persisted as `tab_bar_height` (`auto` or a row
  count), reflowing the shell grid by the reserved rows. Pure layout + pointer
  math, no platform-specific surface.
- **Shipped — OSC 8 hyperlinks.** Explicit hyperlink escapes render as
  hover-affordanced links that open on Ctrl+click on Linux/Windows or Cmd+click
  on macOS through the same argv-safe dispatch, gated to a
  `http`/`https`/`file`/`mailto` scheme allowlist — never auto-opened, never
  shell-interpolated.
- **Shipped — Clickable bare URLs (`interactive_urls`, on by default).** A URL a
  program printed as plain text (no OSC 8 escape) gets the hand cursor, a
  platform-modifier hover underline and click open — reusing the OSC 8 URL
  scanner (`hints`) and the exact same argv-only, scheme-allowlisted dispatch.
  Explicit OSC 8 hyperlinks win a tie (no double-decoration); the off path never
  scans (byte-identical hover). Independent of `interactive_paths`.
- **Shipped — Smart Ctrl+C (`smart_ctrl_c`, `copy-or-interrupt` by default).**
  Plain `Ctrl+C` copies + clears a local selection
  when one exists and otherwise sends the interrupt (`^C`). The interrupt stays
  reachable (no selection, second press, `Esc`-first, or the always-unambiguous
  `Ctrl+Shift+C`); a full-screen TUI never holds a local selection so its
  `Ctrl+C` keeps interrupting. `smart_ctrl_c=off` restores the pre-v0.6.0
  interrupt-always path. Plain `Ctrl+V` stays
  verbatim-insert (deliberately no smart paste); `keybinds = ctrl+v=paste` is the
  documented opt-in for Windows-style paste.
- **Shipped — Interactive paths.** Detect file paths (and `path:line:col` spans) in
  terminal output and make them actionable: an armed-underline hover affordance and
  modifier (`Ctrl`) click to open a file in the editor (jumping to the line/column
  where present), resolved through an editor matrix with a `$EDITOR`/`$VISUAL`
  fallback and the existing argv-safe, no-shell-interpolation open dispatch.
  Stat-gated so only paths that actually resolve light up, opt-in behind the
  `interactive_paths` master gate with a byte-identical off path (the barewords,
  click-hint, and inline-image sub-keys stay inert until the gate is on), and
  cwd-aware via the OSC 7 tracking already in core.
- **Shipped — In-terminal image viewer.** A resolved image path
  (png/jpg/jpeg/webp) opens in a presentation-only lightbox drawn after the
  post-process pass, so viewing an image never has to leave the terminal.
  Dismiss with `Esc` or a click outside; opt-in behind the inline-image sub-key
  and isolated from live terminal state.

## Track 7 — Theming And Palettes

- **Shipped — Theme-naming standard.** The original `odyssey`-named family remains
  the primary OdyTTY identity, while licensed and attributed community palettes
  ship under their upstream names. `theme_family()` classifies every built-in as
  `baseline`, `odyssey`, or `community`, and `--list-themes` exposes the family.
- **Later — Theme-library expansion** past 100, using data-only additions. The
  roster is now 142 after six more contrast-validated original OdysseyOS
  palettes: four dark palettes (inkwell navy, citadel slate, verdigris teal, and
  wildfire ember) and two light palettes (moonstone and primrose). Earlier
  batches brought the roster to 112, 124, and then 136. Expansion was paused
  during the v0.10.0 scope freeze; resuming it requires a separate scope
  decision.

## Track 8 — Positioning And Performance Posture

Privacy as a stated feature — no telemetry, no cloud, no account, fully local
and open — ships today (see [Shipped Foundations](#shipped-foundations)).

- **Later — Performance-tuning knob** (repaint cadence / input delay) only after
  a measured latency baseline exists. No unbacked performance claims.

## Track 9 — Multiple Contexts: Tabs, Panes, And Sessions

This epic has largely shipped. OdyTTY runs multiple shell sessions in one window
with a tab bar, splits each tab into resizable panes, and keeps sessions alive in
a detached session-host so a window can close and reattach with full scrollback.
The attach launcher and Manage Sessions overlay have shipped; what remains is a
handful of deliberately-deferred niceties.

- **Shipped — Tabs.** Multiple PTY/terminal sessions, tab switching, tab close,
  tab rename, new-tab affordance, conventional tab keybindings, and a bright,
  bold foreground label on the active tab. Centered labels keep
  a physical-pixel descender guard at every configured strip height, and the
  panel wash and seam stop at the content edge when a workspace rail is present.
- **Shipped — Drag-to-reorder tabs.** A top-strip press lifts the grabbed tab
  immediately; motion past the click-jitter threshold turns it into a drag with
  a floating proxy that follows the grabbed point and a live insertion marker.
  Drop targeting excludes the lifted tab, crosses neighbors at their real
  midpoints symmetrically in either direction, and parts the resting tabs around
  a reserved destination slot; appending is marked by a thin caret at the real
  strip edge. Release commits and repaints the new order immediately while the
  active tab follows by identity; a sub-threshold press+release remains a normal
  tab switch and Escape cancels. The reordered strip is captured by
  workspace-shape autosave.

  The pointer path is cross-platform and has no platform-specific surface.

- **Shipped — Workspaces.** Named workspaces each own a tab strip and appear in a
  vertical rail once a second workspace exists; a single-workspace session is
  unchanged. Create, rename, close, or cycle by keyboard or context menu; move a
  tab through a named-destination picker; and bind a workspace to a remote host
  so its new tabs open there. Rail labels retain descender clearance, and the
  active label uses a bright, bold foreground.
- **Shipped — Workspace reorder.** The workspace rail's right-click menu moves a
  slot up or down. The adjacent swap follows the active workspace by identity,
  so focus never changes, and shape autosave captures the order for the next
  launch.
- **Shipped — Drag-to-reorder workspaces in the rail.** A press-drag-drop gesture
  on a rail slot complements the menu reorder. Press feedback lifts the grabbed
  slot immediately, a small movement threshold disambiguates a click from a
  drag, and a floating proxy follows the grabbed point alongside the bright
  insertion rule. Drop targeting excludes the lifted workspace, crosses real
  neighbor midpoints symmetrically upward and downward, and parts the resting
  slots around its reserved destination. Release commits and repaints the new
  order immediately; Escape cancels with the order untouched.

  The commit reuses the shipped `move_workspace` engine, so active-follow by
  identity and shape-autosave persistence match the menu path. The auto-hide
  rail stays open for the whole gesture. The pointer path is cross-platform and
  has no platform-specific surface.

- **Shipped — Tab polish.** In-band image placements offset correctly while the
  tab bar is visible.
- **Shipped — New-slot affordance clarity.** The tab bar and workspace-rail `+`
  rest at a lifted color rather than the dim inactive floor, then brighten on
  hover. A blank, non-interactive spacer row separates the workspace list and
  the rail `+`, so a click past the last workspace does not open one by accident
  or require a horizontal rule through the workspace area.
- **Shipped — A detachable-capable core.** Persistent sessions were architected
  with detaching designed in from the start rather than retrofitted, through an
  OdyTTY-owned, versioned terminal-state snapshot format.
- **Shipped — Panes / splits.** Binary layout tree with split direction and
  ratio; per-pane scrollback/selection/search/cursor; tmux-compatible prefix
  bindings plus direct GUI chords and a context-menu section; drag-resizable
  dividers; directional focus-move, close, zoom, and equalize.
- **Shipped on Unix — Persistent / detachable sessions** that survive a window
  closing.
  The loudest real-user demand: a detached session-host owns the PTYs and
  terminal models over a per-user, local-only socket, with an opt-in, bounded
  output-recording ring buffer and a scrubbable replay overlay.
- **Shipped — Connection manager.** An overlay listing saved hosts that
  quick-connects by spawning the system `ssh` in a new pane/session; opt-in,
  read-only, name-only `~/.ssh/config` parsing, with an OdyTTY-owned hosts list
  as the default so the feature works without touching `~/.ssh`. An SSH pane can
  itself be a persistent session on Unix, so a dropped link can be reattached
  locally.
  An Add/Edit connection form with a Test Connection probe and a per-host
  IdentityFile field manages the OdyTTY-owned hosts; a right-click menu on a host
  row opens it in a new tab or new workspace, binds the current workspace to it,
  or edits/removes it; and an unsaved host can be connected to ad hoc, with an
  offer to save it to the hosts list.
- **Shipped on Unix — Session attach launcher.** The shipped persistence is now
  pleasant to reach: `odytty attach` with no id attaches the sole live session (or lists when
  several exist), and an in-window Manage Sessions overlay (default `Ctrl+Shift+A`)
  filters and reattaches a detached session into a new tab, with New-tab/Replace
  prompts and dedup of already-attached tabs. "Summon, not greet" — opening a
  window stays fast.
- **Shipped on Unix — Manage Sessions overlay management.** Beyond attach, the overlay
  renames a session and kills a session (right-click, with confirmation) directly
  from the manager.
- **Shipped on Unix — Detach & switch.** A context-menu action that spawns a fresh managed
  session in the focused pane's current directory and switches to it, so a window
  can hand off to a new detached session without leaving the keyboard.
- **Someday — Broadcast input** to multiple panes at once.
- **Shipped — Window-state persistence and named layouts.** Opt-in restore,
  off by default, reopens the previous window shape when launched with no
  arguments. The shape includes workspaces, tabs, and pane splits at their
  recorded working directories. The snapshot records structure only, never
  terminal output, scrollback, environment, or the commands that were running.
  Local panes open fresh shells at their captured directories, panes backed by
  still-live Unix session hosts reattach, and SSH panes reconnect to fresh
  remote login shells. Named layouts capture the whole session and reopen with
  a replace-or-add prompt.
- **Someday — Multi-window** management.

## Track 10 — Packaging, Release, And Platform

Making OdyTTY installable and maintainable outside the source tree.

- **Shipped — Release builds and packaging.** Each tag publishes seven artifact
  types: Debian, RPM, Linux binary tarball, Linux AppImage, macOS app zip,
  Windows portable zip, and source archive. Every artifact has an always-latest
  alias and a version-pinned twin, with `SHA256SUMS` and its Minisign
  signature `SHA256SUMS.minisig` completing the 16-asset set.
- **Shipped — Native Linux installation paths.** Debian and RPM packages,
  a checksum-verifying one-line installer, and the standalone binary tarball
  cover package-managed and portable installs. The AppImage remains the
  single-file, no-install fallback.
- **Shipped — Arch User Repository publishing.** The `odytty` AUR package is
  refreshed automatically after every tagged release. If the publishing
  credential is unavailable, the release validates the generated package
  metadata without attempting to publish.
- **Reference — Install and release details.** See the [Install Guide](install.md)
  for platform choices and [Release Guide](release.md) for the publication
  contract.
- **Shipped — Crash & logging story.** A predictable diagnostics path (bounded,
  local, privacy-preserving) for when something does go wrong; shipped in v0.7.5
  (panic hook, freeze watchdog, rotated logging) and documented in
  `docs/diagnostics.md`.
- **Shipped — macOS release artifact.** The release workflow now emits an
  ad-hoc-signed `OdyTTY.app` bundle, zipped as `odytty-macos-arm64.zip`
  (Apple Silicon / arm64), from the macos-latest CI leg — checksummed in
  `SHA256SUMS` with an always-latest alias and a version-pinned twin. Ad-hoc
  signing is free and account-less, so an un-quarantined app launches without a
  Gatekeeper warning; no Apple Developer account or notarization is involved.
- **Shipped — macOS Homebrew tap.** The canonical cask (points at the release
  `.app` zip and its `SHA256SUMS` checksum) and a source-build formula fallback
  live in `dist/homebrew/`. On each tagged release a `homebrew` job stamps their
  version, url, and sha256 from the published `SHA256SUMS` and pushes them to the
  live `ghreprimand/homebrew-odytty` tap, mirroring the Scoop/AUR auto-bump.
  Install from the tap with:

  ```console
  brew tap ghreprimand/odytty
  brew install --cask odytty
  ```

  The tap repository and its deploy key are provisioned. The job stays guarded
  and publishes nothing whenever the key is absent, exactly like the AUR job.
  A signed or notarized `.dmg` stays deferred until the Apple Developer Program
  is adopted.

- **Ongoing — Broader platform work.** Linux, macOS, and Windows build in
  blocking CI and ship release artifacts. Linux remains the primary target;
  Windows uses ConPTY and macOS uses the Unix backend. Confirm behavior under
  both Wayland and X11 where relevant and continue on-device platform hardening.

## Track 11 — Exploratory And Far Future

Ideas worth recording, only sensible once OdyTTY is already a reliable terminal,
and only if they never compromise terminal trust.

- **Shipped — Command palette.** A keyboard-driven in-window fuzzy finder over
  terminal-local actions and settings, shell history, and recent directories,
  beyond the settings-overlay search it grew out of.
- **Someday — Full block-reflow rendering model.** Prompt marking and the
  command-aware UX deliver most of the value without this large scrollback
  departure; far future, if ever.
- **Someday — Workflows / notebooks / saved snippets.** Scope-creepy; deferred.

---

## Non-goals

Recorded so the boundary is deliberate and not relitigated by default.

### Keep These Features Out Of Scope

- **AI / agentic / natural-language-to-shell features.** An explicit non-goal,
  not a deferred feature.
- **Telemetry, cloud sync, accounts, or team features.** Against the private,
  local, no-telemetry direction — their *absence* is a deliberate feature.
- **Scripted / Lua configuration and plugin or extension runtimes.** A
  heavyweight dependency and a hand-editing surface, in direct conflict with the
  no-hand-edit configuration goal.
- **Distinguishing standard output from standard error by color.** Infeasible:
  the PTY merges the two streams before the terminal can see them, so this can
  never be promised.
- **A full input-editor takeover** of the shell line (multi-cursor, undo). Only
  the narrow click-to-position slice is worth doing.
- **Effects for their own sake** — parallax, aggressive dimming of old output,
  decorative piling-on. The visual engine earns its place only through
  readability and restraint.
- **Unbacked "fastest terminal" or daily-driver claims** before compatibility
  and performance are actually proven and measured.

### Keep These Dependencies External

- Font rasterization, the GPU API, the windowing toolkit, clipboard transport,
  image-format decoding (PNG/JPEG/WebP), and Unicode width tables stay external.
  Re-owning them would add maintenance without adding identity or capability.

---

## Open architectural questions

Named here so they get decided deliberately rather than by default:

- **Embeddable core.** Whether the terminal core should eventually be exposed as
  a reusable library with a stable embedding interface, or remain
  application-internal. The existing core/render separation keeps the option
  open; there is no commitment in either direction, and no current work should
  depend on one.

---

## Near-term focus

The honest current ordering, now that the command-aware UX, the ergonomic cores,
the readability-safe background treatments, the interactive paths and image
viewer, the session attach launcher, and the multi-context epic (panes,
persistent sessions, command palette, and connection manager) all ship:

1. **Treat v0.10.0 field reports as regression input.** Address confirmed
   correctness or security problems with narrow patch releases and permanent
   regression coverage.
2. **Keep the release evidence machinery current.** Maintain blocking
   cross-platform CI, bounded dynamic analysis and fuzzing, package channels,
   documentation, and the time-bounded dependency-advisory review.
3. **Choose the next product milestone explicitly before resuming deferred
   feature or effect work.** The conservative defaults remain unchanged unless
   a separately recorded scope authorizes tuning (see Track 2).

Everything beyond a plain terminal stays measured, opt-out-able, and — above all
— never something you are forced to hand-edit a config file to reach.
