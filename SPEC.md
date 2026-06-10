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

Start with a narrow terminal-emulator prototype that proves the core rendering and interaction loop before committing to a full product direction. The first slice should open a real shell, handle common terminal I/O correctly, render readable text at speed, support copy/paste and resizing, and expose a small Odyssey-themed visual layer such as theme presets, subtle motion, or optional background/effect treatments. Keep effects strictly behind performance and readability boundaries so the project can test visual identity without masking terminal correctness.

Architecture should separate the terminal core from the Odyssey experience layer: shell process and PTY handling, escape-sequence parsing, input mapping, text layout, rendering, theme/effects, and settings should be distinct enough that visual experiments can change without destabilizing core behavior. The build should include a compatibility test path early, using existing terminal behavior as the baseline rather than inventing semantics.

The project should start from scratch as a Linux-first Rust terminal with a GPU-backed renderer, an OdyTTY-owned terminal model, and an OdyTTY-owned Linux PTY layer. Use narrow, purpose-built crates for parsing, windowing, text shaping, and rendering where they help, but do not delegate the product's terminal core to another terminal emulator. The current parser dependency is a staged replacement target; the owned VT parser should land before graphics protocols and richer byte-path features. Use Ghostty and other mature terminals as behavior references, not implementation bases. Visual ambition should stay open, but every effect and workflow layer must be isolated from terminal correctness and remain bounded by readability and performance.

## Scope

v0 should be a narrow, from-scratch terminal prototype that can open a real local shell and prove the basic daily loop: launch, type commands, render output clearly, scroll, copy/paste, resize, and apply one small Odyssey visual layer without breaking readability or speed.

In scope:
- Local PTY-backed shell session
- Basic keyboard input and command output rendering
- Common ANSI escape handling sufficient for ordinary shell use
- Readable text rendering with stable cursor, selection, scrolling, and resize behavior
- Copy/paste support
- A small theme system with 2-3 Odyssey-style presets
- One optional visual experiment, such as subtle background treatment, cursor motion, or restrained panel glow
- Simple settings for theme/effect enablement
- Early compatibility checks against known terminal behavior

Out of scope for v0:
- Tabs, panes, sessions, profiles, remote connections, terminal multiplexing, and shell integration features
- Plugin systems, AI features, command palettes, rich dashboards, or nonstandard terminal semantics
- Heavy animation, effects that reduce legibility, or visual features tied into terminal correctness
- Full cross-platform polish beyond the initial target environment
- Replacing Ghostty/Konsole as a daily driver before compatibility and performance are proven

Smallest useful end-to-end slice: a single-window Odyssey Terminal opens the user’s shell, runs common commands reliably, supports text selection/copy/paste and resizing, renders output fast and legibly, and lets the user toggle between a plain baseline theme and one Odyssey visual treatment to judge whether the identity layer adds value without getting in the way.

## Stack

Start from scratch for the first spike, with a Linux-first Rust core and a GPU-backed rendering path. A strong early direction is Rust for the terminal model, owned Linux PTY handling, input state, and settings; `vte` temporarily for escape-sequence parsing while the owned parser is built; `winit` for windowing; `wgpu` for rendering; and a CPU-shaped glyph atlas for sharp, predictable terminal text. Treat visual effects as a separate Odyssey layer on top of the core, not as part of terminal semantics.

Use existing terminal standards and behavior as compatibility references rather than as implementation bases. Ghostty should be the primary daily-driver behavior reference, with xterm/ECMA-style behavior as the lower-level compatibility baseline. Do not fork a terminal or embed another terminal emulator's core as the product path.

The native app is a distinct boundary from the terminal core. Windowing (`winit`), GPU surface/rendering (`wgpu`), the glyph atlas/text renderer, and grid presentation live behind a `native` module seam that consumes the owned core's snapshot and never holds terminal semantics. The renderer and any later Odyssey visual layer must be replaceable without touching core correctness. The native window and renderer are built incrementally: the `winit`/`wgpu` dependencies are added with the packet that implements the window, so the dependency tree only carries code that is actually exercised. The first prototype's text path is a single monospace font with a CPU-rasterized glyph atlas and no complex shaping (no ligatures or BiDi); per-character cell width comes from `unicode-width`, consistent with the core.
