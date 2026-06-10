# OdyTTY Full Build Roadmap

This document captures the larger build direction after the first meaningful
prototype. It is not a commitment to build every idea immediately; it is the
map of what a serious OdyTTY product would need if the prototype continues to
justify itself.

The core rule stays the same: terminal correctness, readable text, predictable
input, and stable performance outrank visual novelty. Odyssey-specific visuals
should make the terminal feel more intentional and alive without weakening trust.

## Current Baseline

The first prototype is complete. OdyTTY can open a native Wayland window, run a
real local shell, render GPU-backed monospaced text, handle keyboard input,
resize, cursor rendering, scrollback navigation, paste, mouse selection/copy,
and one disableable ambient visual treatment. The terminal core is owned by
OdyTTY and uses `vte` as a parser rather than embedding another terminal
emulator core.

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
Ghostty sets a strong modern baseline: text feels sharp, stable, well-spaced,
and pleasant for long sessions. OdyTTY should aim to meet that baseline first
and then explore whether it can feel even better within its own visual identity.

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

## Stage 5: Settings And Profiles

Move from prototype environment variables to a stable user configuration model.

Focus:

- Config file format and path.
- Defaults, validation, and diagnostics.
- Theme, font, cursor, shell, shortcut, window, and effect settings.
- Profile support once the basic config model is reliable.
- Precedence rules: built-in defaults, config file, environment, command-line
  overrides.

Acceptance target:

- Users can configure OdyTTY without recompiling or relying on ad hoc env vars.
- Bad config is recoverable and clearly reported.

## Stage 6: Odyssey Visual Layer

Develop the visual identity after the terminal is trustworthy. Effects must stay
optional, measurable, and isolated from correctness.

Focus:

- Theme presets that work with terminal colors rather than fighting them.
- Better baseline and Odyssey palettes.
- Optional background treatments and subtle motion only after frame timing is
  measured.
- Strict off switches for every effect.
- Performance and readability budgets for all visual work.

Acceptance target:

- OdyTTY has a recognizable visual identity while remaining readable and fast.
- A plain baseline remains available and tested.

## Stage 7: Product Shell Features

Only start these after the core terminal and daily interaction loop are solid.

Focus:

- Tabs.
- Panes.
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

## Near-Term Recommendation

The next active plan should cover stages 1 through 4:

- Prototype stabilization.
- Terminal correctness hardening.
- High-quality text and rendering.
- Daily-driver interaction.

Tabs, panes, profiles, plugins, AI features, heavy effects, packaging, and broad
cross-platform work should remain deferred until this foundation is stronger.
