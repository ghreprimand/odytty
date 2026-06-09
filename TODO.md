# OdyTTY — TODO

Milestone checklist toward the first meaningful prototype: a single-window,
GPU-rendered terminal that opens a real shell, renders readable text, handles
enough common terminal behavior for basic daily commands, supports resize and
copy/paste basics, and includes one isolated Odyssey visual layer that can be
disabled. See `DEVLOG.md` for current state and `SPEC.md` for durable decisions.

## Core Readiness

- [x] Confirm the stack and scope boundaries.
- [x] Stand up the minimal runnable skeleton (owned core, PTY, render seam).
- [x] Owned terminal model using `vte` as the parser.
- [x] PTY shell command path and host-terminal interactive mode.
- [x] Core compatibility primitives: printing, cursor movement, SGR, erase,
      scrollback, alternate screen, save/restore, scroll regions, bracketed
      paste, RI, IL/DL, SU/SD, DECOM, RIS/DECSTR, ICH/DCH, ECH, REP, tab
      stops, DA reply.
- [x] Headless transcript smoke harness with deterministic default fixtures.
- [x] Add further compatibility sequences as the prototype needs them, decided
      from evidence rather than guesswork (e.g. BCE, SU/SD + DECOM).
- [ ] Convert any reproducible failures into deterministic fixtures.

## Native Window and Rendering

- [x] Document the native app stack: `winit` event loop, `wgpu` renderer,
      font/text shaping approach, and Linux assumptions.
- [x] Scaffold the `native` module boundary and `--native` entry.
- [x] Add a native window that opens and closes cleanly (`winit`, native
      Wayland verified on Linux/Hyprland).
- [x] Bring up a `wgpu` surface that clears the window and survives resize
      (GPU-pipeline half of text rendering; Vulkan on the hardware adapter).
- [x] Render the owned terminal grid with readable monospaced text (glyph atlas).
- [x] Connect PTY output to the rendered grid.
- [x] Connect keyboard input to the PTY using the existing input mapping.
- [x] Render cursor and basic viewport state.
- [x] Handle window resize by resizing both PTY and terminal model.

## Daily Loop Basics

- [x] Paste into the PTY path, respecting bracketed paste mode.
- [x] Basic mouse text selection.
- [x] Copy from selection.
- [ ] Scrollback viewport navigation.
- [ ] Validate basic commands interactively: prompt display, `ls --color`,
      `clear`, simple editor/pager enter-exit behavior, and resize.

## Odyssey Layer

- [ ] Small theme system with a plain baseline and 1–2 Odyssey presets.
- [ ] One optional visual treatment behind a setting, isolated from terminal
      correctness.
- [ ] Verify the visual layer can be disabled and does not affect compatibility
      tests.
- [ ] Check readability and performance boundaries before adding more effects.

## First Prototype Acceptance

- [ ] A native OdyTTY window opens a real local shell.
- [ ] Common shell output is readable and responsive.
- [ ] Resize, paste, selection/copy, cursor, and scrollback work at a basic level.
- [ ] The compatibility test suite and transcript smoke suite pass.
- [ ] One Odyssey visual treatment exists and can be disabled.
- [ ] Public docs and devlog describe what works, what is deferred, and what
      risks remain.

## Deferred Until After the First Prototype

- [ ] Tabs, panes, sessions, profiles, and multiplexing.
- [ ] Shell integration beyond basic PTY behavior.
- [ ] Plugins, AI features, command palettes, dashboards, or rich nonstandard
      workflows.
- [ ] Heavy animation or effects that can compromise readability or latency.
- [ ] Broad cross-platform support beyond Linux-first validation.
- [ ] Daily-driver claims against Ghostty/Konsole before compatibility and
      performance are proven.
