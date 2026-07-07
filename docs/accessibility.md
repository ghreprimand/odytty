# OdyTTY — Accessibility

OdyTTY treats legibility as a hard floor, not a theme setting. This page collects
the accessibility-oriented controls: the minimum-contrast guarantee,
color-vision-deficiency adaptation, dimming, motion, and the bell. Everything
here is local — there is no telemetry, account, or network call involved in any
of it. For the full config-key table see [`runtime-knobs.md`](runtime-knobs.md);
for keyboard control see [`keybindings.md`](keybindings.md).

## Minimum-contrast floor

OdyTTY enforces a minimum text/background contrast at render time, so foreground
text is never illegibly close to its background — including against background
images, treatments, and translucent cell backgrounds.

| Key | Env var | Default | Range |
| --- | --- | --- | --- |
| `min_contrast` | `ODYTTY_MIN_CONTRAST` | `16.0` | `1.0`–`21.0` |

The value is a WCAG 2.x relative-luminance contrast ratio. The default `16.0` is
a deliberately strong readability floor; setting `1.0` disables the floor
entirely (exact passthrough of theme colors). The number shown in the Theme
Builder is the same metric the render floor enforces — what you see previewed is
what is guaranteed on screen.

Note: the `render_quality = plain` fast path turns the floor **off** (it forces
`min_contrast` to `1.0`). If you want a calmer, effect-free look *and* the
contrast guarantee, prefer turning off individual effects (below) over switching
to `plain`.

## Color-vision-deficiency (CVD) modes

OdyTTY can adapt its palette for protanopia, deuteranopia, and tritanopia. The
adaptation (daltonization) runs in the perceptual OKLab color space: it moves
color cues off the axis you cannot distinguish and onto the axes you can, then
re-checks the result against a WCAG-AA `4.5` contrast target.

| Key | Env var | Default | Values |
| --- | --- | --- | --- |
| `cvd_mode` | `ODYTTY_CVD_MODE` | `off` | `off`, `protan`, `deutan`, `tritan` |
| `cvd_strength` | `ODYTTY_CVD_STRENGTH` | `1.0` | `0.0`–`1.0` |

```conf
# odytty.conf
cvd_mode = deutan
cvd_strength = 1.0
```

Scope and behavior:

- Adaptation applies to the **16 ANSI palette colors plus the cursor, selection,
  and search-highlight roles**. Structural colors (background, foreground,
  border, inactive) are held so the overall theme stays recognizable.
- `cvd_strength = 1.0` is full correction; `0.0` is exact bit-for-bit
  passthrough (identical to `cvd_mode = off`). The adaptation is applied once —
  it is not meant to be stacked.
- Each theme's light/dark appearance is re-inferred from its actual background
  luminance during adaptation, so the correction is anchored correctly for both
  light and dark themes.

Current limitations: application-emitted indexed-256 and 24-bit truecolor output
is **not** remapped (only the theme palette is adapted); a per-cell output lens
is future work. With a CVD mode active, the Theme Builder's live preview is
itself adapted.

## Dimming and focus

Two independent dimming controls help direct attention; both are off by default.

| Key | Env var | Default | Range | Effect |
| --- | --- | --- | --- | --- |
| `focus_dim` | `ODYTTY_FOCUS_DIM` | `0.0` | `0.0`–`1.0` | Dims the whole grid while the window is **unfocused**. The focused window is never dimmed. |
| `inactive_pane_dim` | `ODYTTY_INACTIVE_PANE_DIM` | `0.0` | `0.0`–`1.0` | Dims panes other than the focused one, so the active pane stands out in a split. |

Both are disabled on the `render_quality = plain` path.

## Reduced motion and a calm profile

Every motion effect is **off by default**, so a fresh install is already static:
cursor easing, cursor glide/motion, cursor glow, the cursor trail (which also
requires cursor motion), the new-output fade, and smooth scrolling are all off
unless you turn them on. Smooth scrolling, when enabled, never adds input
latency — the scroll target snaps immediately and only the sub-row pixel offset
eases.

Two ambient effects *are* on by default — `bloom` and `crt` (a subtle scanline
and vignette). The default `visual = ambient` is a back-compat alias that folds
into the CRT post-process rather than adding a separate wash (an explicit `crt`
setting wins). For a flat, effect-free terminal while keeping the contrast floor,
turn these off individually:

```conf
# odytty.conf — calm, static, with the readability floor intact
bloom = off
crt = off
visual = off
# (all motion effects are already off by default)
```

The `render_quality = plain` profile disables post-processing, background
treatments, dimming, and per-cell stem darkening in one switch — but it also
turns off the minimum-contrast floor, so reach for it only when you want the
hard fast path rather than an accessibility profile.

## The bell

OdyTTY has **no audible bell** — there is no audio backend at all. The terminal
bell (`BEL`) is handled visually or via the window manager.

| Key | Env var | Default | Values |
| --- | --- | --- | --- |
| `bell` | `ODYTTY_BELL` | `urgent` | `off`, `visual`, `urgent`, `all` |

- `urgent` (default) requests window attention through the window manager, and
  only while the window is **unfocused** — a focused shell never flashes the
  taskbar on a tab-completion bell.
- `visual` paints a brief, readability-safe full-viewport flash (a low-alpha tint
  that decays over ~150 ms; light on dark themes, dark on light).
- `all` does both; `off` ignores the bell entirely.

## Themes and legibility

Theme choice interacts with all of the above. The minimum-contrast floor applies
on top of any theme, and the retro/phosphor themes use deliberately narrow
palettes that still meet the library's contrast floor. `theme = system` follows
the OS dark/light preference automatically. See [`themes.md`](themes.md) for the
theme format, the in-app Theme Picker (`Ctrl+Shift+H`) and Theme Builder
(`Ctrl+Shift+B`), and the full built-in library.

## Window transparency and the contrast floor

`window_transparency` (off by default) lets the desktop show through the window
background, but it is designed to leave legibility untouched: only backgrounds
and chrome bands scale toward `window_opacity`, while text, cursor, selection,
and every overlay stay fully opaque. The minimum-contrast floor is computed
against the terminal's own background color, not the blended desktop behind it,
so lowering the opacity never lifts foreground text off its readability floor.
See [`effects.md`](effects.md#window-transparency) for the settings.

## Privacy

None of these features phone home. OdyTTY has no telemetry, analytics, crash
reporting, account, cloud sync, or update ping; first-run state is just the
presence of your `odytty.conf`. See the Privacy & Data Posture section of
[`../SPEC.md`](../SPEC.md).

## See also

- [`runtime-knobs.md`](runtime-knobs.md) — every config key, env var, and default.
- [`keybindings.md`](keybindings.md) — the full keyboard reference.
- [`effects.md`](effects.md) — bloom, CRT, retro, background, and motion effects.
- [`themes.md`](themes.md) — theme format and the built-in library.
