# OdyTTY Full Build Roadmap

This document captures the larger build direction after the first meaningful
prototype. It is not a commitment to build every idea immediately; it is the
map of what a serious OdyTTY product would need if the prototype continues to
justify itself.

The core rule stays the same: terminal correctness, readable text, predictable
input, and stable performance outrank visual novelty. At the same time, visual
quality is a defining pillar of the product, not decoration. OdyTTY treats the
visible rendering capability of the strongest modern GPU terminals as a floor
to reach and then exceed within its own identity. Odyssey-specific visuals
should make the terminal feel more intentional and alive without weakening
trust.

A second defining pillar is foundation ownership. OdyTTY's standard is that
every byte from the PTY to the glyph quad passes exclusively through
OdyTTY-owned code: the PTY layer, the escape-sequence parser, the terminal
model, the renderer geometry, and the shaders. External crates are acceptable
only below the product line — font rasterization, GPU API, windowing,
clipboard transport, and Unicode character data — which is the same boundary
the strongest independent terminals draw. Where the project currently leans on
a crate inside that line, replacing it is roadmap work, not a settled
trade-off.

## Current Baseline

The first prototype is complete. OdyTTY can open a native Wayland window, run a
real local shell, render GPU-backed monospaced text, handle keyboard input,
resize, cursor rendering, scrollback navigation, paste, mouse selection/copy,
and one disableable ambient visual treatment. The terminal model — grid,
scrollback, modes, attributes, reflow, search, selection, encoders — is owned
by OdyTTY. The byte-level escape-sequence parser currently delegates to the
`vte` crate behind a narrow callback seam; replacing it with an OdyTTY-owned
parser is scheduled work (see Stage 4.5).

The prototype proves the loop. It does not yet prove daily-driver quality.

## Stage 1: Prototype Stabilization

Make the prototype comfortable enough for repeated short sessions before
expanding the product surface.

Focus:

- Add font-size configuration.
- Add a minimal settings path, starting with environment variables or a small
  native options/settings layer before a full config file.
- Keep defaults stable and readable.
- Improve launch/run documentation for the native prototype.
- Keep the public docs aligned with actual validated behavior.

Acceptance target:

- A user can run OdyTTY with a comfortable text size and known settings knobs
  without editing source code.
- Invalid settings fail softly and fall back to safe defaults.
- The prototype remains cleanly testable and easy to launch.

## Stage 2: Terminal Correctness Hardening

Treat real shell and TUI failures as evidence. Every reproducible behavior gap
should become a fixture before more speculative features land.

Focus:

- Expand escape-sequence support from observed failures, not guesswork.
- Improve OSC support, including title handling and common shell/editor
  sequences.
- Add mouse reporting modes needed by real TUIs.
- Harden alternate-screen behavior against editors, pagers, and full-screen
  apps.
- Improve Unicode, wide-character, combining-mark, and ambiguous-width behavior.
- Add more PTY-backed smoke cases and deterministic transcript fixtures.
- Continue comparing behavior against xterm/Ghostty/Konsole as references, not
  as implementation sources.

Acceptance target:

- Common shells, pagers, editors, and lightweight TUIs behave predictably.
- New compatibility fixes come with deterministic coverage.
- The terminal core remains independent from rendering and visual layers.

## Stage 3: High-Quality Text And Rendering

Text quality should be a major product pillar, not a minor renderer detail.
Ghostty sets the strongest modern baseline: text feels sharp, stable,
well-spaced, and pleasant for long sessions. For OdyTTY that baseline is the
floor, not the finish line. Meet it first, then keep pushing wherever
measurable rendering quality allows.

The goal is not to copy Ghostty's implementation. The goal is to hold OdyTTY to
the same user-visible standard: text should look professionally rendered at
normal terminal sizes, on HiDPI displays, during scrolling, under color themes,
and inside dense TUI screens.

Focus:

- Configurable font size.
- Configurable font family once the settings path exists.
- HiDPI correctness across window scales and monitor changes.
- Better glyph atlas management, including cache growth and invalidation.
- Font fallback for missing glyphs.
- Correct handling for wide glyphs, combining marks, emoji policy, and
  ambiguous-width characters.
- Evaluate shaping strategy: stay cell-based for terminal correctness, but
  decide explicitly whether ligatures, stylistic sets, or HarfBuzz-style shaping
  belong behind settings.
- Improve rasterization quality: pixel alignment, baseline consistency, glyph
  padding, gamma handling, subpixel strategy, and color blending.
- Ensure cursor, selection, inverse video, bold, dim, italic, underline, and
  strikethrough render cleanly at multiple sizes.
- Profile redraw, scrolling, resize, and large-output performance.
- Add visual regression screenshots or pixel-level smoke checks when practical.

Acceptance target:

- Text is sharp and comfortable at the default size and at several configured
  sizes.
- Side-by-side comparison against reference terminals shows no visible text
  quality deficit at common sizes and scale factors.
- Dense colored shell output and basic TUIs remain readable.
- Renderer performance remains stable under large output and scrollback.
- Visual effects never reduce glyph contrast or text clarity unless explicitly
  enabled by a setting and bounded by readability tests.

## Stage 4: Daily Driver Interaction

Make OdyTTY feel normal and efficient for repeated use before larger product
features such as tabs or panes.

Focus:

- Refine selection: double-click word, line selection, drag beyond viewport,
  scrollback-aware selection, and clear selection semantics.
- Improve clipboard behavior: primary selection if appropriate, paste policy,
  large paste behavior, and clear diagnostics.
- Add search in scrollback.
- Add viewport affordances such as a scroll indicator or scrollbar.
- Add configurable key bindings.
- Add cursor styles and blink policy.
- Add window title/focus behavior.
- Improve mouse and keyboard interaction in TUI apps.

Acceptance target:

- OdyTTY supports the everyday terminal gestures users expect.
- Short real sessions do not reveal obvious interaction friction.
- Interaction features remain separated from terminal semantics.

## Stage 4.5: Foundation Ownership

Own the full byte path before building the features that sit on top of it.
This stage exists because two pressures point at the same work: the project's
ground-up identity, and concrete engineering needs. The Kitty graphics
protocol is APC-based and the current parser dependency never surfaces APC
sequences; Sixel is DCS-based and the current DCS handling is an unimplemented
pass-through. Synchronized output, richer OSC support, and a deliberate
malformed-input recovery policy all benefit from owning the byte-level layer.

The replacement seam already exists: the terminal model consumes parser
callbacks behind a single narrow trait boundary, so the parser is a
replaceable part by design. The migration method is differential: the existing
parser is kept as a development-only oracle, identical byte streams are fed to
both parsers against cloned terminal models, and state is asserted identical
across the full fixture and transcript corpus before the dependency is
removed.

Focus:

- An OdyTTY-owned VT parser implementing the canonical DEC ANSI state machine:
  ground/escape/CSI/OSC/DCS states, mid-stream UTF-8 decoding, C1 handling,
  parameter limits, cancel/abort semantics, and OSC terminator variants.
- Real DCS and APC support designed in from the start, so graphics protocols
  land on an owned byte path rather than being bolted around a dependency.
- A differential test harness against the outgoing parser plus a fuzzing
  harness, retained as permanent fixtures after the swap.
- An OdyTTY-owned Linux PTY layer (openpty, spawn, resize) replacing the
  cross-platform PTY abstraction.
- Retire the remaining terminal-adjacent convenience dependencies from the
  input path so key handling uses the windowing layer's native types.
- Update SPEC and README to state the ownership boundary plainly once it is
  real.

Explicit non-goals, recorded so the boundary is deliberate: font parsing and
rasterization, GPU API, windowing, clipboard transport, and Unicode width
tables stay external. These sit below the product line; re-owning them adds
maintenance without adding identity or capability.

Acceptance target:

- Every byte from the PTY to the glyph quad passes exclusively through
  OdyTTY-owned code.
- The owned parser matches or exceeds the outgoing parser's behavior across
  the full fixture corpus, with divergences documented as deliberate.
- Graphics-protocol work can begin against owned APC/DCS plumbing.

## Stage 5: Settings And Profiles

Move from prototype environment variables to a stable user configuration model.

Focus:

- Config file format and path.
- Defaults, validation, and diagnostics.
- Theme, font, cursor, shell, shortcut, window, and effect settings.
- Live reload of config changes where the renderer already has rebuild seams
  (the scale-agnostic atlas rebuild path was built to support a live font
  change, for example); settings that cannot reload live should say so clearly.
- CLI introspection helpers such as listing themes, fonts, and the effective
  config, once there is enough surface to introspect.
- Profile support once the basic config model is reliable.
- Precedence rules: built-in defaults, config file, environment, command-line
  overrides.

Acceptance target:

- Users can configure OdyTTY without recompiling or relying on ad hoc env vars.
- Bad config is recoverable and clearly reported.

## Stage 6: Visual Capability Parity And The Odyssey Layer

This stage is the project's thesis test. Stages 1 through 5 build a competent
terminal; Stage 6 is where OdyTTY must become visually distinctive without
weakening what was built. The work has two halves, in order: capability parity,
then identity. Parity means OdyTTY can render what the leading GPU terminals
render, at the same visible quality. Identity means using that capability to
look and feel like OdyTTY rather than a generic terminal.

Focus, parity half:

- Close remaining text-rendering gaps against reference terminals found by
  side-by-side comparison.
- Ligatures and stylistic sets behind settings, following the recorded shaping
  decision and its trigger conditions.
- Subpixel anti-aliasing strategy where the display stack benefits from it.
- Image and graphics protocol support (Kitty graphics protocol, Sixel) so
  modern TUI media workflows render natively.
- Extend visual regression coverage to every parity feature as it lands.

Focus, identity half:

- Theme presets that work with terminal colors rather than fighting them.
- Better baseline and Odyssey palettes.
- Cursor, selection, and window chrome treatments distinctive to OdysseyOS.
- Optional background treatments and subtle motion only after frame timing is
  measured.
- Strict off switches for every effect.
- Performance and readability budgets for all visual work.

Acceptance target:

- A user comparing OdyTTY side by side with the leading GPU terminals finds no
  visual capability it lacks, and at least one respect in which it clearly
  looks or feels better.
- OdyTTY has a recognizable visual identity while remaining readable and fast.
- A plain baseline remains available and tested.

## Stage 7: Product Shell Features

Only start these after the core terminal and daily interaction loop are solid.

Focus:

- Tabs.
- Panes.
- Multi-window.
- Sessions.
- Profiles.
- Session restore.
- Command palette, if it solves real workflows.

Acceptance target:

- OdyTTY starts to become a full terminal application, not just a terminal
  emulator window.

## Stage 8: Packaging And Release

Make OdyTTY installable and maintainable outside the repository.

Focus:

- Release builds.
- Desktop entry and icon.
- Packaging for the target Linux environment.
- CI checks.
- Versioning and changelog.
- Crash/logging story.

Acceptance target:

- A user can install, launch, and update OdyTTY without running from the source
  tree.

## Stage 9: Broader Platform Work

Linux-first remains the right constraint until the Linux app is solid.

Focus:

- Confirm behavior under Wayland and X11 where relevant.
- Consider macOS or Windows only after the architecture earns it.
- Avoid portability abstractions until real platform pressure exists.

Acceptance target:

- Platform support expands deliberately rather than weakening the core Linux
  target.

## Stage 10: Future Experimental Layer

Plugins, AI features, dashboards, shell integration, and richer workflows belong
after OdyTTY is already a reliable terminal.

Focus:

- Shell integration that improves real workflows.
- Plugin or extension experiments only with strict safety boundaries.
- AI or rich UI features only if they do not compromise terminal trust.

Acceptance target:

- Experimental features build on a reliable terminal instead of compensating for
  an unreliable one.

## Open Architectural Questions

Named here so they get decided deliberately rather than by default:

- Embeddable core: whether the terminal core should eventually be exposed as a
  reusable library with a stable embedding interface (in the spirit of
  libghostty), or remain application-internal. The existing core/render
  separation keeps the option open; there is no commitment in either direction
  yet, and no current work should depend on one.

## Near-Term Recommendation

Stages 1 through 4 are substantially complete: settings, correctness
hardening, text/rendering quality, and daily-driver interaction have all
landed, with a small set of manual-validation and evidence-gated items
remaining open. The next active plan should cover, in order of emphasis:

- Stage 4.5 foundation ownership: the owned parser, the owned PTY layer, and
  the input-path dependency retirement.
- The remaining Stage 1–4 items: manual friction sessions, HiDPI manual
  validation, alternate-screen and fixture work as evidence appears, and TUI
  interaction polish.
- Early Stage 6 parity-half work where it does not depend on the parser
  (wide-glyph quality, subpixel anti-aliasing), and graphics-protocol work
  once it can land on owned APC/DCS plumbing.

Tabs, panes, profiles, plugins, AI features, heavy effects, packaging, and broad
cross-platform work should remain deferred until this foundation is stronger.

Stages 1 through 4.5 are the table stakes that make the rest safe to attempt.
Stage 6 is where the project's central question gets answered, and the
foundation work should be judged by how well it sets that stage up.
