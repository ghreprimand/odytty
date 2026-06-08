# OdyTTY — TODO

First steps, drawn from the build direction. Refine before starting.

- [x] Confirm the stack and scope boundaries
- [x] Stand up the minimal runnable skeleton
- [ ] Build the smallest end-to-end slice of the core concept

## Build-direction notes

Start with a narrow terminal-emulator prototype that proves the core rendering and interaction loop before committing to a full product direction. The first slice should open a real shell, handle common terminal I/O correctly, render readable text at speed, support copy/paste and resizing, and expose a small Odyssey-themed visual layer such as theme presets, subtle motion, or optional background/effect treatments. Keep effects strictly behind performance and readability boundaries so the project can test visual identity without masking terminal correctness.

Architecture should separate the terminal core from the Odyssey experience layer: shell process and PTY handling, escape-sequence parsing, input mapping, text layout, rendering, theme/effects, and settings should be distinct enough that visual experiments can change without destabilizing core behavior. The build should include a compatibility test path early, using existing terminal behavior as the baseline rather than inventing semantics.

The project should pursue genuinely original terminal work rather than forking or skinning an existing terminal. The first spike should be Linux-first, written in Rust, GPU-rendered, and built around an OdyTTY-owned terminal model using `vte` as a parser rather than embedding another terminal emulator's core. Ghostty and other mature terminals should be compatibility references, not implementation bases. Visual ambition should stay open, but effects and workflow layers must remain isolated from terminal correctness and bounded by readability and performance.
