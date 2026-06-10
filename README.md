# OdyTTY

## What it is

Odyssey Terminal is a reliable terminal emulator with an OdysseyOS visual identity, exploring how motion, themes, effects, and interface details can make command-line work feel more alive without weakening core terminal behavior. Its central question is whether a terminal can add useful, nonstandard features and a richer experience while staying fast, solid, and practical for daily use.

## Status

First meaningful prototype reached on Linux/Hyprland. In place today: an
OdyTTY-owned terminal core (grid, cursor, scrollback, alternate screen) driven by
`vte` as a parser, a real PTY-backed shell path, broad escape-sequence
compatibility, and a deterministic headless smoke suite. A native `winit` window
opens on Wayland with a live `wgpu` (Vulkan) surface, renders readable
monospaced text, handles keyboard input, resize, paste, mouse selection/copy,
scrollback navigation, cursor rendering, and basic daily shell workflows.

The prototype also includes a small theme system and a disableable ambient
scanline visual treatment selected with `ODYTTY_VISUAL=ambient`; unset,
`off`, `none`, or `plain` keep the baseline renderer. A minimal settings path
loads native runtime knobs such as `ODYTTY_FONT_SIZE` once at startup. Known gaps
remain before daily-driver claims: no profiles or settings UI, basic selection
only, no tabs/panes, and Linux-first validation only. See
[`DEVLOG.md`](DEVLOG.md) for the running record and [`TODO.md`](TODO.md) for the
milestone checklist.

## Why build it

Odyssey Terminal is worth exploring because the terminal is a daily operating surface, not just a utility, and OdysseyOS needs one that carries its own visual identity without compromising trust. It is for the operator who wants command-line work to feel more expressive, polished, and alive while remaining dependable enough for real use. The friction it removes is the gap between solid existing terminals and a more personal, visually distinctive environment: instead of accepting either reliability with generic presentation or flashiness that risks distraction, the project tests whether both can coexist. Scope should stop before novelty damages terminal fundamentals; speed, compatibility, input correctness, readable text, stable rendering, and predictable behavior matter more than effects, themes, or nonstandard features.

## Build direction

Start with a narrow terminal-emulator prototype that proves the core rendering and interaction loop before committing to a full product direction. The first slice should open a real shell, handle common terminal I/O correctly, render readable text at speed, support copy/paste and resizing, and expose a small Odyssey-themed visual layer such as theme presets, subtle motion, or optional background/effect treatments. Keep effects strictly behind performance and readability boundaries so the project can test visual identity without masking terminal correctness.

Architecture should separate the terminal core from the Odyssey experience layer: shell process and PTY handling, escape-sequence parsing, input mapping, text layout, rendering, theme/effects, and settings should be distinct enough that visual experiments can change without destabilizing core behavior. The build should include a compatibility test path early, using existing terminal behavior as the baseline rather than inventing semantics.

The project should pursue genuinely original terminal work rather than forking or skinning an existing terminal. The first spike should be Linux-first, written in Rust, and built around an OdyTTY-owned terminal model: use existing parser and systems crates where they are narrow tools, but do not delegate the product's terminal core to another terminal emulator. Use Ghostty and other mature terminals as behavior references, not implementation bases. Visual ambition should stay open, but every effect and workflow layer must be isolated from terminal correctness and remain bounded by readability and performance.

## Project docs

- [`DEVLOG.md`](DEVLOG.md) — running record of what has landed and current state.
- [`TODO.md`](TODO.md) — milestone checklist toward the first prototype.
- [`SPEC.md`](SPEC.md) — durable product and architecture decisions.
- [`docs/runtime-knobs.md`](docs/runtime-knobs.md) — current native prototype
  settings and launch examples.
- [`docs/full-build-roadmap.md`](docs/full-build-roadmap.md) — staged roadmap
  from prototype stabilization through long-term product work.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — change, commit, and safety conventions.
