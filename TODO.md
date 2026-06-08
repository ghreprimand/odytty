# OdyTTY — TODO

First steps, drawn from the build direction. Refine before starting.

- [ ] Confirm the stack and scope boundaries
- [ ] Stand up the minimal runnable skeleton
- [ ] Build the smallest end-to-end slice of the core concept

## Build-direction notes

Start with a narrow terminal-emulator prototype that proves the core rendering and interaction loop before committing to a full product direction. The first slice should open a real shell, handle common terminal I/O correctly, render readable text at speed, support copy/paste and resizing, and expose a small Odyssey-themed visual layer such as theme presets, subtle motion, or optional background/effect treatments. Keep effects strictly behind performance and readability boundaries so the project can test visual identity without masking terminal correctness.

Architecture should separate the terminal core from the Odyssey experience layer: shell process and PTY handling, escape-sequence parsing, input mapping, text layout, rendering, theme/effects, and settings should be distinct enough that visual experiments can change without destabilizing core behavior. The build should include a compatibility test path early, using existing terminal behavior as the baseline rather than inventing semantics.

The main open decision is whether to build from scratch or fork an existing terminal. A from-scratch spike is useful for learning and identity exploration, but the project should set an early checkpoint: if shell compatibility, text rendering, input correctness, or performance become the dominant work, evaluate forking or extending an existing terminal instead. Other open decisions include the implementation language, rendering backend, how ambitious visual effects should be, and which nonstandard features are valuable enough to justify their maintenance cost.
