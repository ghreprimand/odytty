# OdyTTY — Devlog

Public running record of how OdyTTY is built, in reverse-chronological order.
Each entry captures what landed, the current state, and the known gaps toward
the first meaningful prototype. See `TODO.md` for the milestone checklist and
`SPEC.md` for durable product/architecture decisions.

---

## 2026-06-08 — Owned terminal core, PTY path, and smoke harness

### Direction

OdyTTY is built as original, from-scratch terminal work — not a fork or skin of
another emulator. The first spike is Linux-first, written in Rust, and built
around an OdyTTY-owned terminal model. It uses `vte` as an escape-sequence
parser, not as a terminal core. Ghostty, xterm, and other mature terminals are
compatibility references only.

### Stack as it stands

- Rust (edition 2024).
- `vte` for escape-sequence parsing into the owned model.
- `portable-pty` for spawning and driving a real local shell.
- `crossterm` for the current host-terminal interactive path.
- `unicode-width` for character-width handling.
- `anyhow` / `thiserror` for errors, `tracing` for diagnostics.

The GPU rendering path (`winit` + `wgpu`) is intentionally **not** wired up yet;
it is a planned prototype milestone, not current state.

### What works today

- **Owned terminal model** (`src/core`): a grid of cells with attributes, cursor
  state, scrollback, and an alternate screen, driven by a `vte` parser feeding an
  OdyTTY-owned state machine. The public surface exposes `Terminal::advance`,
  `screen()`, `plain_text()`, host-reply output, and resize.
- **PTY path** (`src/pty`): `PtySession` spawns the default shell or a one-shot
  shell command and streams bytes into the model.
- **Host-terminal interactive mode** (`src/app`): `run_interactive` connects a
  real shell PTY to the current terminal via `crossterm` (alternate screen, raw
  mode, bracketed paste), as a stepping stone before the native GPU window.
- **Render seam** (`src/render`): a `Renderer` trait with a `NullRenderer` so the
  core can be driven and verified headlessly; the real GPU renderer plugs in here
  later.
- **CLI entry points** (`src/main`): a default skeleton print, `--dump-command`
  to render a command's output through the model, and `--interactive`.

### Compatibility primitives landed

The owned core currently handles, with unit coverage: basic printing and
wrapping, cursor movement, SGR attributes/colors, erase (ED/EL), scrollback,
alternate screen, cursor save/restore, scroll regions, bracketed paste, reverse
index (RI), insert/delete line (IL/DL), reset (RIS/DECSTR), insert/delete
character (ICH/DCH), erase character (ECH), repeat (REP), tab stops (HT/HTS/TBC),
and a primary Device Attributes reply.

### Transcript smoke harness

A headless transcript smoke harness (`tests/transcript_smoke.rs`) feeds synthetic
byte transcripts through the public `Terminal` API and asserts coarse,
host-independent invariants: clear/redraw, `ls --color`-style SGR, alt-screen
restore plus scrollback isolation, tab-stop alignment, carriage-return progress
overwrite, resize coherence, and a DA query round-trip. A single live-PTY test
exists but is `#[ignore]`d so the default suite stays deterministic; run it with
`cargo test -- --ignored`.

### Test status (verified 2026-06-08)

- `cargo test`: 54 lib unit tests + 8 smoke tests pass, 1 live-PTY test ignored.
- `cargo fmt --check`: clean.

### Remaining gaps to the first prototype

- No native window yet: `winit` event loop, `wgpu` renderer, and font/text
  shaping are not implemented.
- The grid is not yet drawn to a GPU surface; PTY output and keyboard input are
  not yet wired to a native window.
- Daily-loop interactions in a native window — mouse selection, copy, scrollback
  navigation, paste honoring bracketed-paste — are not implemented there yet.
- No Odyssey visual layer yet (themes/effects behind a toggle).
- Compatibility coverage is meaningful but not exhaustive; further sequences will
  be added from evidence as the prototype needs them.
