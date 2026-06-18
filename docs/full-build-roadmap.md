# OdyTTY Full Build Roadmap

This document is the durable map of where OdyTTY is going. It captures the full
build direction after the first meaningful prototype — not a promise to build
every idea immediately, but the complete picture of what a serious OdyTTY would
need if the project keeps justifying itself. Nothing here is forgotten just
because it is not being built this week; this is where the long tail lives.

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
rasterization, GPU API, windowing, clipboard transport, and Unicode character
data. This ownership boundary is real and in production: the owned PTY layer and
the clean-room VT parser ship today; `vte`, `portable-pty`, and `crossterm` have
all been removed from the dependency tree.

A third pillar, which shapes much of the roadmap below, is **configuration you
never hand-edit**. The defining UX goal is that everything is discoverable and
adjustable from in-app overlays that write the config file for you. Hand-editing
a config file is a fallback, never a requirement.

---

## How to read this roadmap

The forward work is organized by **theme** (the tracks below), and each item
carries a **horizon tag** so the relative sequencing is clear:

- **Now** — actively in progress or the immediate next packet.
- **Next** — near-term; queued and shovel-ready.
- **Later** — wanted, but waiting on a foundation or an evidence baseline.
- **Someday** — acknowledged demand, deliberately deferred; recorded so it is
  never lost, promoted only on an explicit decision.

Two rules govern every feature in every track:

1. **Behind a setting, with an off switch.** Anything beyond a plain terminal
   must be configurable. The plain, fast path stays byte-for-byte identical to a
   renderer without the feature, and is tested as such.
2. **Readability is a floor, not a preference.** A minimum-contrast guarantee is
   the safety net that every visual feature validates against; no effect may
   push text below the legibility floor.

---

## What's shipped today

The foundation is broad and solid. The following are complete and in production.

**Owned byte path.** A native Wayland window runs a real local shell and renders
GPU-backed monospaced text via `wgpu`/Vulkan. The Linux PTY layer uses `rustix`
directly; the VT parser is a clean-room two-layer DEC ANSI pipeline built from
primary specifications; the terminal model, renderer geometry, and shaders are
OdyTTY-originated. The owned path is the only path.

**Terminal correctness.** Printing, cursor movement, SGR, erase, scrollback,
alternate screen, save/restore, scroll regions, bracketed paste, insert/delete
lines and characters, scroll up/down, origin mode, soft/hard reset, repeat, tab
stops, and device-attribute replies. Alternate-screen behavior is hardened
against editors, pagers, and full-screen apps with a deterministic fixture
matrix. Reporting probes (mode queries, size reports, version) are in place.

**Unicode & text.** Mid-stream UTF-8 decoding, wide-character (CJK/width-2)
write/erase coherence with 2-cell atlas slots, zero-width combining-mark
attachment, and a defined ambiguous-width policy.

**Input & interaction.** The full daily-driver loop: scrollback search; refined
selection (double-click word, triple-click line, drag-scroll past the viewport,
scrollback-aware anchors); clipboard hardening (chunked large paste, bracketed-
paste sanitization, PRIMARY selection, middle-click paste); a right-edge scroll
indicator; configurable cursor shapes and blink policy; configurable key
bindings; window title and focus reporting; mode-aware keyboard encoding; the
Kitty keyboard protocol; and the full mouse-reporting matrix including SGR-pixel
mode.

**First-class pointer use.** Beyond the base loop, pointer work is now a polished
surface: extend an existing selection (shift-click to the point, double-click-
then-drag by whole words, triple-click-then-drag by whole lines); rectangular /
block (column) selection by modifier-drag; velocity-proportional drag-autoscroll
(the further past the edge you drag, the faster scrollback advances, bounded);
optional copy-on-select; a draggable scroll-thumb that scrubs scrollback; and
configurable wheel speed with modifier-wheel font zoom. Local selection never
disturbs an application's own mouse reporting.

**Shell integration.** Semantic prompt marking (OSC 133) records prompt,
command, and output boundaries per row, with reflow-stable marks — the
foundation the command-aware navigation builds on.

**Graphics & media.** A complete Sixel decoder and terminal integration; the
Kitty graphics protocol (direct RGB/RGBA and PNG transmit, file/shared-memory
transports with security hardening, placements with z-order/crop/scale/offset,
delete and query operations); a GPU image layer; and color emoji (ZWJ families,
flags, keycaps, skin-tone modifiers, variation selectors) via a dedicated RGBA
color-glyph atlas.

**Text rendering quality.** Bearing-aware glyph quads, bold/italic style faces
with synthetic fallback, the full attribute set (underline, strikethrough, dim,
inverse, hidden), optional subpixel anti-aliasing with an energy-conserving LCD
fringe filter, tunable text gamma, stem darkening, HiDPI scale-factor tracking
with debounced rebuild, and a headless CPU compositor for structural pixel-level
assertions.

**Performance.** Lazy scrollback re-wrap on width change (~2300× faster deep
resize), a width-unchanged fast path (~293× faster height-only resize), reusable
vertex storage with a grow-only GPU buffer, resize debounce, and a render
invalidation/retained-frame system.

**Configuration & themes.** File-based configuration with live reload and a
clear precedence model (defaults < config file < environment); an in-window
overlay framework; an in-app settings panel where every setting is editable,
live-applied, and written back to the config file; a live theme picker; an
in-app custom theme builder (clone, tweak, live preview, save); and CLI config
introspection. A dependency-free `.theme` format, a full 16-color + bright ANSI
palette plus semantic roles, and a curated 100-theme built-in library (dark and
light, all contrast-validated).

**The visual engine.** A perceptual color pipeline (OKLab/OKLCH) with linear-
space blending; a configurable minimum-contrast readability floor; geometric
box-drawing, block, and Powerline rendering at exact cell geometry; symbol /
Nerd-font fallback for prompt icons; themed cursor/selection/search roles; focus
dimming; background gradient/vignette/image treatments; a post-process pipeline
on an HDR offscreen target; bloom / phosphor glow; a CRT/retro profile
(scanlines, soft-knee vignette, subtle curvature); and a render-quality master
control with a hard plain/fast bypass. Effects are configurable with explicit
opt-outs, and the plain renderer remains pixel-identical when selected.

**Readability & accessibility.** The perceptual pipeline now carries four
readability flagships, each pure readability or accessibility: a universal
legibility guarantee that extends the minimum-contrast floor to all application
text (256-color and truecolor), nudging the foreground in perceptual color space
to clear the floor while preserving hue; a perceptual-safe theme builder with
mouse-driven OKLCH (Lightness/Chroma/Hue) sliders, a live contrast readout, and
snap-to-floor so it cannot author an unreadable theme (raw-hex entry stays as the
expert fallback); contrast-aware palette generation that turns a seed color into
a readability-validated theme starting point; and colorblind palette adaptation
that remaps the ANSI palette in perceptual space for color-vision deficiencies.

**Window & identity.** Adjustable window padding with a fully aligned
pixel↔cell coordinate seam, optional themed border, decoration toggle, OS
dark/light following, and a visible tab bar once multiple sessions exist.

**Privacy posture.** No telemetry, no cloud, no account — fully local. The
absence of any phone-home path is a deliberate, stated feature.

**Licensing & project identity.** GPL-3.0-only with a Developer Certificate of
Origin contribution flow, SPDX headers throughout, and a name/branding notice.

---

## Track 1 — Configuration & in-app UX (the no-hand-edit north star)

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
  panel, so features are discoverable without a separate command palette.
- **Shipped — Customizable keybinding remap UI.** The settings panel can remap
  the 12 core non-tab actions and writes back to the config. The `keybinds`
  config surface supports all 16 bindable actions, including tabs.
- **Shipped — CLI introspection: list available fonts**, completing the
  existing introspection helpers.
- **Someday — Profiles.** Named configuration profiles once the base config
  model has settled.

## Track 2 — Text & rendering quality

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
- **Shipped — Smooth scrolling** on a bounded latency budget, with instant
  scroll preserved as the default-safe path.
- **Shipped — Stem-darkening default activation.** The rasterization machinery
  ships default-on at `0.5`, with `0.0` as the byte-identical opt-out.
- **Someday — Legibility font features.** A narrow, charter-clean subset (such
  as a slashed or dotted zero) is the near-term slice; broader ligatures and
  arbitrary font features remain deferred pending an explicit shaping decision.
- **Someday — Scalable color-font expansion** (COLR/CPAL, then SVG-in-OT only
  from real evidence) beyond the current emoji rendering.

## Track 3 — Readability & perceptual color

This is where OdyTTY invests its differentiation budget, leaning on the
perceptual color pipeline and the contrast floor. Every item here is pure
readability or accessibility.

The four flagships of this track now ship (see *What's shipped today*): the
universal legibility guarantee across 256-color and truecolor text, the
perceptual-safe theme builder with OKLCH sliders and snap-to-floor, contrast-
aware palette generation from a seed, and colorblind palette adaptation. The
readability foundation is in place and is the safety net the visual-identity
work in Track 4 validates against.

- **Shipped — Readability scrim primitive.** A computed-bound dim that lets a
  background treatment (Track 4) keep the contrast floor valid by construction,
  bounding the effective luminance behind text to the theme background the floor
  already references. The pure core of the safe-by-construction background work.

## Track 4 — Visual identity & depth

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
- **Shipped — Subtle motion.** Cursor glow, trail, slide, blink fade, and
  fade-in of new output —
  bounded, and fully disable-able.
- **Shipped — Cohesive opt-in retro mode.** A single switch raises bloom, scanlines,
  and vignette into a stronger phosphor reference look. Subtle screen curvature
  is delivered as a setting; chromatic aberration remains deferred.

## Track 5 — Shell & prompt integration

The terminal cooperating with the shell and prompt. This is the highest-leverage
gap to close and unlocks the most downstream value. Semantic prompt marking
(OSC 133) ships today as the foundation.

- **Shipped — Command-aware UX.** Built on prompt marking: jump to the previous or
  next prompt, select and copy a single command's output, and show a per-command
  success/failure indicator in the gutter.
- **Shipped — Click to position the cursor** at a prompt, using the prompt-marking
  click events. The click slice only — not a takeover of shell input editing.
- **Someday — Remote shell integration.** Automatically carrying terminal info
  and shell integration to a remote host over SSH; depends on shell-integration
  maturity first.

## Track 6 — Interaction & productivity

Mostly small, independent ergonomic wins, all overlay-configured.

### Mouse & pointer excellence

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

- **Shipped — Right-click context menu** (copy, paste, selection/input actions,
  settings, and tab actions).
- (See also: click-to-position-cursor in Track 5, and the mouse-driven settings
  panel in Track 1.)

### Other ergonomics

- **Shipped — Keyboard pattern-select / quick-select.** Label on-screen URLs,
  paths, and hashes for keyboard selection and copy.
- **Shipped — Copy mode.** Vim-key keyboard selection of scrollback —
  standalone, no multiplexer required.
- **Shipped — Close-confirmation prompt** when a child process or job is still
  running.
- **Shipped — Window-decoration control.** Toggle client-side vs server-side
  decorations or borderless mode (compositor-dependent on Linux).
- **Shipped — Bindable clear-input action** (low priority; the standard key
  combinations already cover the common case).

## Track 7 — Theming & palettes

- **Later — Theme-naming standard.** A two-tier approach: keep the original
  `odyssey`-named family as the primary OdyTTY identity, and optionally ship
  popular community palettes under their real names only where the upstream license
  permits redistribution and brand guidelines are followed and attributed,
  without implying endorsement.
- **Ongoing — Theme-library expansion** past the current 100 (data-only).

## Track 8 — Positioning & performance posture

Privacy as a stated feature — no telemetry, no cloud, no account, fully local
and open — ships today (see *What's shipped today*).

- **Later — Performance-tuning knob** (repaint cadence / input delay) only after
  a measured latency baseline exists. No unbacked performance claims.

## Track 9 — Multiple contexts: tabs, panes, sessions

The first slice has shipped: OdyTTY can run multiple shell sessions in one
window and shows a one-row tab bar when two or more sessions exist. Remaining
work is the heavier session-management surface.

- **Shipped — Tabs.** Multiple PTY/terminal sessions, tab switching, tab close,
  tab rename, new-tab affordance, and conventional tab keybindings.
- **Next — Tab polish.** Offset in-band image placements correctly while the
  tab bar is visible; continue tightening tab interaction details as evidence
  appears.
- **Someday — A detachable-capable core.** If persistent sessions are promoted,
  architect detaching from day one rather than retrofitting it later.
- **Someday — Panes / splits.** Split position, ratio, and direction;
  table-stakes once session persistence and tab polish justify the complexity.
- **Someday — Broadcast input** to multiple panes at once.
- **Someday — Persistent / detachable sessions** that survive disconnect. This
  is the loudest real-user demand and the headline when this epic is promoted —
  deferred by design, not abandoned.
- **Someday — Window-state persistence** (reopen where you left off) — a lighter
  cousin of session persistence.
- **Someday — Multi-window** management.

## Track 10 — Packaging, release & platform

Making OdyTTY installable and maintainable outside the source tree.

- **Someday — Release builds, desktop entry and icon, Linux packaging, CI
  checks, versioning and a changelog, and a crash/logging story**, so a user can
  install, launch, and update OdyTTY without building from source.
- **Someday — Broader platform work.** Confirm behavior under both Wayland and
  X11 where relevant; consider macOS or Windows only after the Linux app is
  solid and the architecture earns it. Avoid portability abstractions until real
  platform pressure exists. Linux-first remains the right constraint.

## Track 11 — Exploratory / far future

Ideas worth recording, only sensible once OdyTTY is already a reliable terminal,
and only if they never compromise terminal trust.

- **Someday — Command palette** beyond the settings overlay. For now the project
  borrows only search-within-overlay rather than a full palette.
- **Someday — Full block-reflow rendering model.** Prompt marking and the
  command-aware UX deliver most of the value without this large scrollback
  departure; far future, if ever.
- **Someday — Workflows / notebooks / saved snippets.** Scope-creepy; deferred.

---

## Non-goals

Recorded so the boundary is deliberate and not relitigated by default.

**Out of scope by charter:**

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

**External by design (below the product line):**

- Font rasterization, the GPU API, the windowing toolkit, clipboard transport,
  and Unicode width tables stay external. Re-owning them would add maintenance
  without adding identity or capability.

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

The honest current ordering, now that the friction-bug close-out, the in-app
configuration UX, the readability flagships, and the pointer-excellence work all
ship:

1. **Command-aware UX on the prompt-marking foundation** — jump to previous/next
   prompt, select a command's output, and a per-command success/failure gutter.
   The highest-leverage integration work, now in progress.
2. **Native wiring for the banked ergonomic cores** — activation and overlays for
   keyboard pattern-select (quick-select) and copy mode, whose pure cores ship.
3. **Readability-safe background treatments** — gradient, vignette, and image
   backgrounds whose readability dimming is bounded to the contrast floor by
   construction; the pure scrim primitive is the active first step.
4. **The cheaper ergonomic and visual knobs** — line height, box-drawing
   thickness, per-codepoint overrides, follow-OS theme, and the rest — in
   parallel as they fit a settings-owner slot.

Everything beyond a plain terminal stays measured, opt-out-able, and — above all
— never something you are forced to hand-edit a config file to reach.
