# HiDPI Scale Validation — Manual Matrix

Operator-runnable manual test matrix for HiDPI/fractional-scale behavior.
Covers initial-launch correctness, live scale-factor transitions, and
interaction with font-size settings.

Status: turnkey — run each cell, record pass/fail in the rightmost column.

---

## Prerequisites

- A Linux desktop with either:
  - **Multi-monitor**: one monitor at 1.0× and another at 2.0× (or different
    scales), OR
  - **GNOME/KDE fractional scaling**: ability to change the display scale live
    from 100% through 200%.
- `cargo build --release` in the OdyTTY tree.
- The `WINIT_X11_SCALE_FACTOR` environment variable override for X11 sessions
  (Wayland uses `WAYLAND_DISPLAY` native scale or `wp-fractional-scale`).

## Environment variable overrides

| Variable                   | Purpose                                      |
|----------------------------|----------------------------------------------|
| `WINIT_X11_SCALE_FACTOR=N` | Force X11 scale to N (e.g. 1.25, 2.0)       |
| `ODYTTY_FONT_SIZE=N`       | Override logical font size (default 14)      |
| `ODYTTY_THEME=name`        | Theme selection (default `odyssey`)          |

---

## Matrix cells

### A. Initial-launch correctness

Launch OdyTTY at each scale; verify text is crisp from frame one.

| # | Scale | Font | Command | Check | Result |
|---|-------|------|---------|-------|--------|
| A1 | 1.0 | default | `cargo run --release` | Crisp glyphs, correct grid size (`tput cols; tput lines`) | |
| A2 | 1.25 | default | `WINIT_X11_SCALE_FACTOR=1.25 cargo run --release` | Crisp, no blur, grid fits window | |
| A3 | 1.5 | default | `WINIT_X11_SCALE_FACTOR=1.5 cargo run --release` | Crisp, no seams between cells | |
| A4 | 1.75 | default | `WINIT_X11_SCALE_FACTOR=1.75 cargo run --release` | Crisp, baseline consistency across glyphs | |
| A5 | 2.0 | default | `WINIT_X11_SCALE_FACTOR=2 cargo run --release` | Crisp HiDPI, no oversized or undersized glyphs | |
| A6 | 2.0 | 18px | `WINIT_X11_SCALE_FACTOR=2 ODYTTY_FONT_SIZE=18 cargo run --release` | Crisp at larger font, atlas memory sane (no OOM) | |

### B. Live scale-factor transitions (multi-monitor drag)

Requires two monitors at different scales, or Wayland live-rescale.

| # | Transition | Check | Result |
|---|-----------|-------|--------|
| B1 | 1.0 → 2.0 (drag window) | Text re-rasterizes crisp, grid resizes, `tput cols/lines` updates | |
| B2 | 2.0 → 1.0 (drag back) | No blur, no oversized glyphs, grid resizes back | |
| B3 | 1.0 → 1.25 (GNOME/KDE slider) | Seam-free grid at 1.25, no torn frame on transition | |
| B4 | 1.25 → 1.5 → 1.75 → 2.0 (step through) | Crisp at each stop, grid adjusts, no stale density | |

### C. Fractional-scale detail checks

At each scale, inspect these specific rendering details.

| # | Scale | Check | How to verify | Result |
|---|-------|-------|---------------|--------|
| C1 | 1.25 | Box-drawing joins | `printf '\u2500\u253c\u2500'` — horizontal line through the cross, no gap at the cell boundary | |
| C2 | 1.5 | Baseline consistency | Type `EFghij` — cap tops of E/F align, descenders of g/j extend below baseline | |
| C3 | 1.75 | Cursor alignment | Move cursor with arrows — block cursor covers exactly one cell, no pixel offset | |
| C4 | 2.0 | Selection highlight | Select text with mouse — highlight tracks cell boundaries exactly | |
| C5 | any | Scroll indicator | Scroll up in history — indicator bar is at right edge, 3px wide, no artifacts | |

### D. Shell / TUI interaction at non-default scales

| # | Scale | Font | App | Check | Result |
|---|-------|------|-----|-------|--------|
| D1 | 2.0 | default | `htop` | Full-screen TUI renders, resize responds, alt-screen restores | |
| D2 | 1.5 | 18px | `vim file.txt` | Editor renders, cursor moves, quit restores primary | |
| D3 | 1.25 | default | `ls --color` | Colored output readable, grid correct after resize | |
| D4 | 2.0 | default | `man ls` | Pager enters alt-screen, scroll works, quit restores | |

### E. Edge cases

| # | Scenario | Check | Result |
|---|----------|-------|--------|
| E1 | Minimize → restore at 2.0 | No crash, grid correct on restore | |
| E2 | Hot-plug monitor at different scale while focused | No crash, grid stays correct on active output | |
| E3 | Very small window (< 5 cells) at 2.0 | 1×1 minimum grid, no divide-by-zero panic | |

---

## Recording results

Fill in the **Result** column with:
- ✅ Pass
- ❌ Fail — brief description of misbehavior
- ⏭️ Skip — reason (e.g. "single-monitor setup")

Any failure should be captured as a deterministic fixture where the failure mode
is headless-expressible (cell math, debounce), or documented as a known-gap
with the exact visual description and screenshot if possible.
