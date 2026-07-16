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
| `min_contrast` | `ODYTTY_MIN_CONTRAST` | `17.0` | `1.0`–`21.0` |

The value is a WCAG 2.x relative-luminance contrast ratio. The default `17.0` is
a deliberately strong readability floor; setting `1.0` disables the floor
entirely (exact passthrough of theme colors). The number shown in the Theme
Builder uses the same metric as the render floor, so authoring and rendering
agree on what contrast means. The builder authors against WCAG AA 4.5, while
the render floor defaults to 17.0; rendering may therefore lift a role above the
ratio shown by the builder.

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
  and search-highlight roles**. Background, border, inactive, and clear are
  held so the overall theme stays recognizable. Foreground is not daltonized,
  but it is re-floored against the background like other readable roles.
- `cvd_strength = 1.0` is full correction; `0.0` is exact bit-for-bit
  passthrough (identical to `cvd_mode = off`). The adaptation is applied once —
  it is not meant to be stacked.
- Each theme's light/dark appearance is re-inferred from its actual background
  luminance during adaptation, so the correction is anchored correctly for both
  light and dark themes.

Current limitations: application-emitted indexed colors outside the 16 ANSI
slots — the color cube and grayscale ramp at indices 16–255 — and 24-bit
truecolor output are not remapped. Indices 0–15 resolve through the adapted
theme palette. A per-cell output lens is future work. With a CVD mode active,
the Theme Builder's live preview is itself adapted.

## Dimming and focus

Two independent dimming controls help direct attention; both are off by default.

| Key | Env var | Default | Range | Effect |
| --- | --- | --- | --- | --- |
| `focus_dim` | `ODYTTY_FOCUS_DIM` | `0.0` | `0.0`–`1.0` | Dims the whole grid while the window is **unfocused**. The focused window is never dimmed. |
| `inactive_pane_dim` | `ODYTTY_INACTIVE_PANE_DIM` | `0.0` | `0.0`–`1.0` | Dims panes other than the focused one, so the active pane stands out in a split. |

Both are disabled on the `render_quality = plain` path.

## Reduced motion and a calm profile

Cursor slide (`cursor_motion`) is on by default, so a fresh install glides
between nearby cursor positions. The cursor trail (`cursor_trail`) is also on
and follows that slide. Its linked `cursor_trail_strength` profile defaults to
`balanced`: moves of two through six cells keep the restrained nearby echo,
while jumps beyond six cells move one cursor-shaped presentation body on the
same frame. Its leading and trailing edges converge at different rates, so the
body stretches briefly and then compresses into the logical destination.
`subtle` stays nearly rigid and settles fastest; `expressive` allows more
stretch and a longer settle. The logical cursor target and input routing remain
immediate, and the bounded follower schedules no wake after it settles.
Cursor glow (`cursor_glow`) is on by default and follows the same static
override. The new-output fade (`new_output_fade`) remains off unless you turn
it on.

Four additional motion behaviors are on by default, because none adds input
latency:

- **Cursor blink** (`cursor_blink`) periodically hides and restores the cursor.
  Set it to `off` for a cursor that remains continuously visible.

- **Cursor blink fade** (`cursor_easing`) eases the cursor's opacity in and out
  across each blink instead of switching it hard on and off. It only acts while
  the cursor is blinking and the window is focused; it never moves the cursor.
- **Animated scroll glide** (`scroll_glide`) applies to discrete scroll jumps,
  including wheel notches and keyboard page-scroll actions: the viewport moves
  instantly, but the rendered view eases toward the new position over a few
  frames.
- **Continuous pixel scrolling** (`pixel_scroll`) tracks high-resolution wheels
  and touchpads 1:1 on a sub-row lane.

For both scroll features the scroll target snaps immediately — only the visual
position eases, and it moves solely in the scroll direction, so it cannot
overshoot.

`reduced_motion = on` is the master static override for cursor slide, trail,
glow, blink fade, and new-output fade. It forces those effects to snap or stay
static without changing their individual saved settings, so disabling it later
restores the prior choices. The explicit setting behaves the same on Windows,
macOS, and Linux. OdyTTY does not currently read a platform reduced-motion
preference; that integration remains backlog.

```conf
# odytty.conf — disable cursor and output motion while preserving preferences
reduced_motion = on
```

It does not disable cursor blinking, smooth scrolling, or continuous pixel
scrolling. Turn those off explicitly for a fully static terminal:

```conf
# odytty.conf — no cursor or scroll motion at all
cursor_blink = off
reduced_motion = on
scroll_glide = off
pixel_scroll = off
```

Programming ligatures are a static presentation option rather than motion.
`reduced_motion` therefore leaves `ligatures` unchanged. When ligatures are
enabled, logical cells and copied text remain the original characters; set
`ligatures = off` to retain the ordinary one-glyph-per-cell presentation. This
behavior is the same on Windows, macOS, and Linux.

Three ambient treatments are on by default: `bloom`, `crt` (a subtle scanline
and vignette), and the bundled wallpaper selected by
`background_treatment = image`. The default `visual = ambient` is a back-compat
alias that folds into the CRT post-process rather than adding a separate wash
(an explicit `crt` setting wins). For a flat, effect-free terminal while
keeping the contrast floor, turn these off individually:

```conf
# odytty.conf — calm, static, with the readability floor intact
bloom = off
crt = off
visual = off
background_treatment = color
# cursor glow is on by default; the new-output fade is already off.
# reduced_motion makes the default cursor slide, trail, glow, and blink fade static.
# Add cursor_blink = off, scroll_glide = off, and pixel_scroll = off for a
# fully static terminal (see above).
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
palettes that still meet the library's contrast floor. `theme = system` enables
OS dark/light following. Wayland delivers the preference live; X11 has no live
signal, so set `ODYTTY_APPEARANCE=dark|light` to seed it. See
[`runtime-knobs.md`](runtime-knobs.md) for that platform detail and
[`themes.md`](themes.md) for the theme format, the in-app Theme Picker
(`Ctrl+Shift+H`) and Theme Builder
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
