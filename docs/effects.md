# OdyTTY — Visual Effects

This guide covers OdyTTY's visual effects: what they do, how to tune or disable
them, and what to expect on different hardware. For the architecture and
readability invariants behind the effects system, see
[docs/visual-architecture.md](visual-architecture.md).

## Contents

- [The model](#the-model)
- [Bloom](#bloom)
- [Retro preset](#retro-preset)
- [CRT / retro profile](#crt--retro-profile)
- [The ambient visual setting](#the-ambient-visual-setting)
- [Cursor animations](#cursor-animations)
- [New-output fade](#new-output-fade)
- [Background treatments](#background-treatments)
- [Window transparency](#window-transparency)
- [Pixel-precise scrolling](#pixel-precise-scrolling)
- [Accessibility](#accessibility)
- [Plain / fast mode](#plain--fast-mode)

---

## The model

OdyTTY's visual effects follow four hard rules:

**Always disable-able.** The no-config startup path uses OdyTTY's current
ambient visual baseline. Every effect has an explicit off switch, and
`render_quality = plain` forces the direct renderer for benchmarking,
accessibility validation, and weak adapters.

**Readability-gated.** The terminal cell colors pass through the CPU
minimum-contrast floor (`min_contrast` / `ODYTTY_MIN_CONTRAST`) before GPU
post-processing. Post effects cannot feed their output back into that CPU
resolver, so any effect that changes brightness must be structurally bounded in
the shader. Bloom adds light only; CRT scanlines and vignette use capped
multiplicative dimming with a soft-knee brightness floor (no hard clamp step)
plus an 8×8 ordered dither so the gradient never posterizes into banding.

**Adapter-gated.** Effects that require GPU features OdyTTY cannot guarantee
(for example, a filterable HDR render target) silently fall back to the plain
path when the adapter cannot support them. OdyTTY prints one notice to stderr
and continues normally.

**Pixel-identical plain path.** When all effects are off, the renderer is
byte-identical to the pre-effects codebase. You can verify this with the
`pixel_smoke` test suite, which asserts exact structural equivalence between the
direct and offscreen-passthrough paths.

---

## Bloom

Bloom adds an optional HDR phosphor glow around bright cells — glyphs whose
linear luminance exceeds a configurable knee. The effect is achieved by
rendering the terminal into a linear `Rgba16Float` HDR offscreen target,
extracting bright pixels in a threshold pass, blurring them at half resolution
with a separable Gaussian, and compositing the result additively back onto the
scene.

Bloom is on by default in the OdyTTY ambient baseline and pixel-identical to
the plain renderer when disabled.

### Settings

All four settings are live-reloadable: changes in `odytty.conf` or the settings
overlay take effect on the next frame without restarting. The full knob
reference (all settings, types, defaults, and reload behaviour) is in
[`docs/runtime-knobs.md`](runtime-knobs.md).

| Setting | Env | Type | Default | Range |
|---------|-----|------|---------|-------|
| `bloom` | `ODYTTY_BLOOM` | `on` / `off` | `on` | — |
| `bloom_threshold` | `ODYTTY_BLOOM_THRESHOLD` | float or `auto` | `0.70` | `0.70–1.25` |
| `bloom_intensity` | `ODYTTY_BLOOM_INTENSITY` | float | `0.7` | `0.0–1.0` |
| `bloom_radius` | `ODYTTY_BLOOM_RADIUS` | float | `8.0` | `0.5–8.0` |

**`bloom`** — master switch. `on` enables the effect; `off` returns to the
direct scene path when no other post effect is active.

**`bloom_threshold`** — linear luminance knee for the bright pass; pixels above
it glow, pixels below do not.

- `auto` (and an empty value) resolves to the static built-in default `0.70`; it
  is not theme-derived today.
- A theme-foreground-seeded knee (`relative_luminance(foreground) + 0.12`,
  clamped to `0.70–1.25`) is reserved in the code but not yet wired into config
  resolution.
- The default keeps normal body text below the knee, so only genuinely bright
  elements — bold highlights, status indicators, and glyphs against a bright
  background — participate. Specify a fixed float to override.

**`bloom_intensity`** — additive glow strength. `0.0` produces no glow even
when enabled; `0.7` is the default ambient glow strength; `1.0` is the cap.
Values above the cap are clamped.

**`bloom_radius`** — blur spread in half-resolution pixels. Smaller values
(`0.5–1.5`) keep the glow tight around individual glyphs; larger values
(`5.0–8.0`) produce a wide phosphor wash across the screen. `8.0` is the
ambient baseline.

### Enabling via odytty.conf

```
bloom = on
bloom_threshold = 0.70
bloom_intensity = 0.7
bloom_radius = 8.0
```

### Enabling via environment

```sh
ODYTTY_BLOOM=on ODYTTY_BLOOM_THRESHOLD=0.70 ODYTTY_BLOOM_INTENSITY=0.7 ODYTTY_BLOOM_RADIUS=8.0 odytty
```

### Enabling via the settings overlay

Open the settings overlay with `Ctrl+Shift+,` (see
[`docs/keybindings.md`](keybindings.md) for the full chord reference), navigate
to the `bloom` row, and toggle it with `Space` or `Enter`. Bloom activates immediately on the next
frame. Adjust `bloom_threshold`, `bloom_intensity`, and `bloom_radius` in the
same panel; each change applies live. Press `Ctrl+S` in the overlay to persist
the settings to `odytty.conf`.

### Requirements and when bloom won't show

Bloom requires a GPU adapter where `Rgba16Float` render targets are renderable,
texture-bindable, and filterable (needed for the separable blur pass). Most
discrete GPUs on Linux meet this requirement. Older integrated GPUs and some
virtual machine graphics adapters may not.

When the adapter probe fails, OdyTTY prints a single line to stderr:

```
odytty: GPU adapter lacks filterable Rgba16Float render targets; post-process effects disabled
```

The terminal continues normally on the direct sRGB path. No setting is needed
to trigger the fallback; it is automatic.

---

## Retro preset

The retro preset is a one-switch stronger phosphor profile. It does not persist
over the individual bloom and CRT knobs; it only changes their effective runtime
values while `retro = on`.

| Setting | Env | Type | Default |
|---------|-----|------|---------|
| `retro` | `ODYTTY_RETRO` | `on` / `off` | `off` |

When enabled, the preset uses:

```conf
retro = on
# effective runtime values:
# bloom_threshold = 0.70
# bloom_intensity = 1.0
# bloom_radius = 8.0
# crt_scanline_intensity = 0.35
# crt_vignette_strength = 0.35
# crt_curvature = 0.025
```

`render_quality = plain` still bypasses the preset and renders through the direct
path.

---

## CRT / retro profile

The CRT profile adds refined scanlines and a subtle vignette over the same HDR
offscreen target used by bloom. It also has an optional subtle curvature pass.
Chromatic aberration is deferred because it carries a higher readability risk.

CRT is on by default in the OdyTTY ambient baseline and pixel-identical to the
plain renderer when disabled.
When CRT and bloom are both enabled they share one offscreen scene render and
one final composite pass.

### Settings

All CRT settings are live-reloadable: changes in `odytty.conf` or the settings
overlay take effect on the next frame without restarting.

| Setting | Env | Type | Default | Range |
|---------|-----|------|---------|-------|
| `crt` | `ODYTTY_CRT` | `on` / `off` | `on` | — |
| `crt_scanline_intensity` | `ODYTTY_CRT_SCANLINE_INTENSITY` | float | `0.17` | `0.0–0.35` |
| `crt_scanline_period` | `ODYTTY_CRT_SCANLINE_PERIOD` | float | `7.0` | `2.0–12.0` |
| `crt_vignette_strength` | `ODYTTY_CRT_VIGNETTE_STRENGTH` | float | `0.45` | `0.0–0.45` |
| `crt_curvature` | `ODYTTY_CRT_CURVATURE` | float | `0.0` | `0.0–0.12` |

**`crt`** — master switch. `on` enables the scanline/vignette profile; `off`
returns to the direct scene path when no other post effect is active.

**`crt_scanline_intensity`** — dark-band strength. Values are clamped to
`0.0–0.35`. The shader keeps a separate brightness floor so stronger scanlines
remain bounded rather than becoming an opaque overlay.

**`crt_scanline_period`** — vertical distance between scanline bands in
physical pixels. `7.0` is the default.

**`crt_vignette_strength`** — edge dimming strength. Values are clamped to
`0.0–0.45`. The shader approaches its brightness floor through a soft knee
rather than a hard clamp, so the edge gradient stays smooth instead of forming a
visible banding ring, and the 8×8 ordered dither in the composite pass — applied
whenever post-process is active (bloom or CRT), not CRT-only — breaks up any
residual 8-bit posterization. Lit cells are never zeroed by the vignette.

**`crt_curvature`** — subtle barrel-distortion screen curvature. `0.0` is flat
and pixel-identical to the no-curvature path. The cap is intentionally low
(`0.12`) and the retro preset uses a light `0.025` curve.

### Enabling via odytty.conf

```
crt = on
crt_scanline_intensity = 0.17
crt_scanline_period = 7.0
crt_vignette_strength = 0.45
crt_curvature = 0.0
```

### Enabling via environment

```sh
ODYTTY_CRT=on ODYTTY_CRT_SCANLINE_INTENSITY=0.17 odytty
```

### Requirements and fallback

CRT uses the same `Rgba16Float` post-process target as bloom. If the adapter
cannot render, bind, and filter that format, OdyTTY uses the plain direct path.
With both bloom and CRT disabled, no offscreen texture is allocated.

---

## The ambient visual setting

`visual` is the back-compat selector for OdyTTY's scanline look.

| Setting | Env | Values | Default |
|---------|-----|--------|---------|
| `visual` | `ODYTTY_VISUAL` | `off` / `ambient` (alias `scanlines`) | `ambient` |

The scanline look is produced by the unified CRT post-process described above —
the legacy per-cell ambient wash was retired and folded into it. `visual = ambient`
(the default) and `visual = scanlines` are aliases that request that look: when
no explicit `crt` value is set, an ambient `visual` turns the CRT pass on, while
an explicit `crt` setting always wins, so the two never stack. `off`, `none`, and
`plain` opt out of the alias. In practice you tune the scanline appearance with
the `crt_*` knobs above; `visual` exists so older configs keep working.

---

## Cursor animations

OdyTTY has three optional cursor animations; all are purely visual and never
affect the logical cursor position or terminal state. Since v0.6.0 the shipped
defaults enable **cursor easing** and the **cursor trail** (`cursor_easing = on`,
`cursor_trail = on`) as part of the OdysseyOS identity, while **cursor slide**
(`cursor_motion`) stays off by default. Set any of them to `off` to disable.

**Cursor slide** (`cursor_motion = on`): the cursor glides between adjacent
positions (55 ms ease-out-cubic) instead of jumping. Large jumps, resizes,
scrollback navigation, and the first frame always snap instantly.

**Cursor trail** (`cursor_trail = on`): a short fading after-image trails the
cursor as it glides, drawn behind the cursor block in the theme cursor color.
Only visible while cursor slide is also on; fully decays as the glide settles.

**Cursor glow** (`cursor_glow = on`): three faint concentric rings in the theme
foreground color behind the cursor block. Faint enough to keep nearby text
readable.

**Cursor blink fade** (`cursor_easing = on`): the cursor eases its opacity in
and out across each blink instead of switching hard on and off. Only active
while the cursor is blinking and the window is focused.

## New-output fade

`new_output_fade = on` fades freshly arrived output rows in over a short ramp
at the live tail instead of appearing instantly. The fade obscures then reveals
each new row, so the text is always fully rendered and readable. Scrollback and
resize snap. Off by default; only at the live tail.

---

## Background treatments

`background_treatment` controls depth behind the terminal grid. Since v0.6.0
OdyTTY ships with a bundled "Dark Waves" background **on by default**; turn it
off with `background_treatment = color` or `background_image = none`.

| Setting | Env | Type | Default |
|---------|-----|------|---------|
| `background_treatment` | `ODYTTY_BACKGROUND_TREATMENT` | `off`/`color`, `gradient`, `vignette`, `image` | `image` |
| `background_image` | `ODYTTY_BACKGROUND_IMAGE` | PNG/JPEG/WebP path, `default` (bundled), or `none` | `default` (bundled) |
| `cell_bg_opacity` | `ODYTTY_CELL_BG_OPACITY` | float `0.0–1.0` | `0.8` |
| `background_blur_radius` | `ODYTTY_BACKGROUND_BLUR_RADIUS` | integer px `0–256` | `0` |
| `background_image_scrim` | `ODYTTY_BACKGROUND_IMAGE_SCRIM` | `auto`, empty, or float `0.0–1.0` | `0.5` |

`gradient` darkens toward the bottom of the grid. `vignette` darkens toward the
edges and corners. Both are applied before the minimum-contrast floor, so the
foreground is re-lifted over the treated background cell by cell.

`image` draws a PNG, JPEG, or WebP behind the grid:

- With `cell_bg_opacity = 1.0`, cell backgrounds stay opaque and the image is
  hidden behind the cells; values below `1.0` let it show through behind text.
- OdyTTY computes a readability scrim automatically unless
  `background_image_scrim` is set explicitly.
- Missing, unreadable, undecodable, or oversized inputs degrade safely with a
  warning.
- The settings panel's `Background image` row opens an inline path picker that
  enumerates directories off the UI path, so navigation stays responsive while
  large folders load.

The settings panel shows `cell_bg_opacity` as **Wallpaper visibility**, the
inverse of the config value: `0.0` hides the wallpaper behind solid cells, while
higher values reveal more of the image. It shows `background_image_scrim` as
**Wallpaper readability**: lower explicit values keep the image clearer, while
higher values make text safer over busy images.

Example:

```conf
background_treatment = image
background_image = /tmp/background.jpg
cell_bg_opacity = 0.85
background_blur_radius = 8
```

---

## Window transparency

Separately from the per-cell wallpaper opacity above, OdyTTY can make the whole
window translucent so the desktop shows through behind the terminal.

| Setting | Env | Type | Default |
|---------|-----|------|---------|
| `window_transparency` | `ODYTTY_WINDOW_TRANSPARENCY` | `on`/`off` | `off` |
| `window_opacity` | `ODYTTY_WINDOW_OPACITY` | percent `20`–`100`, step `5` | `85` |

`window_transparency` is **off by default**, and with it off the render path is
the unchanged opaque one. When on, only the terminal background and the chrome
bands scale toward `window_opacity` — text, cursor, selection, and every overlay
(menus, pickers, settings, prompts) stay fully opaque, so readability never
depends on the opacity value. `window_opacity` is a percent: `100` is fully
opaque and lower values let more of the desktop through.

Transparency needs a compositing window manager: Wayland composites natively,
X11 needs a running compositor, and Windows uses DWM. Where no alpha compositing
is available the toggle has no visible effect and the window stays opaque.
Blur/acrylic behind the window is not offered.

```conf
window_transparency = on
window_opacity = 85
```

---

## Pixel-precise scrolling

High-resolution wheels and touchpads emit pixel-precise deltas that report
physical finger travel. With `pixel_scroll = on` (the default), that input
scrolls the viewport by a continuous sub-row amount tracking travel 1:1, instead
of quantizing to whole notches. Continuous pixel input is tracked directly
rather than eased, which avoids the sawtoothing an easing catch-up produces on
high-resolution devices.

Classic detented wheels emit line deltas and are unaffected — they keep using
`scroll_wheel_lines` as the per-notch multiplier. Pixel-precise scrolling is
single-pane only for now; inside a split, pixel input falls back to the notch
path.

### Settings

| Setting | Env | Type | Default | Range |
|---------|-----|------|---------|-------|
| `pixel_scroll` | `ODYTTY_PIXEL_SCROLL` | `on` / `off` | `on` | — |
| `scroll_pixel_speed` | `ODYTTY_SCROLL_PIXEL_SPEED` | float | `1.0` | `0.25–4.0` |
| `scroll_glide` | `ODYTTY_SCROLL_GLIDE` | `on` / `off` | `on` | — |

**`pixel_scroll`** — master switch for the continuous lane. `on` (default)
tracks pixel-precise devices 1:1; `off` routes them through the same discrete
notch path as detented wheels.

**`scroll_pixel_speed`** — sensitivity multiplier for the continuous lane. `1.0`
tracks finger travel exactly; higher scrolls faster than the finger, lower
slower. Applies only to pixel-precise input.

**`scroll_glide`** — animate scrollback between *discrete* wheel notches (on by
default; primary screen only).

- Detented wheels carry no sub-step data, so the viewport offset jumps instantly
  per notch and the rendered view eases toward it with a forward-chase follower
  that only moves in the scroll direction, so a notch stream cannot sawtooth.
- In a split, each pane glides independently as an eased follower: the pane under
  the pointer moves on its own, its overflowing partial row clipped to the pane
  so it never smears across the divider into a neighbour.

### Configuring via odytty.conf

```
pixel_scroll = on
scroll_pixel_speed = 1.0
```

### Configuring via environment

```sh
ODYTTY_SCROLL_PIXEL_SPEED=1.5 odytty
```

---

## Accessibility

OdyTTY's accessibility effects share the same readability and pixel-identical-when-off
contracts as the rest of this guide. The full reference — including the
minimum-contrast floor and the bell — lives in
[`docs/accessibility.md`](accessibility.md); the visual knobs are summarized here.

| Setting | Env | Type | Default | Range |
|---------|-----|------|---------|-------|
| `cvd_mode` | `ODYTTY_CVD_MODE` | `off` / `protan` / `deutan` / `tritan` | `off` | — |
| `cvd_strength` | `ODYTTY_CVD_STRENGTH` | float | `1.0` | `0.0–1.0` |
| `focus_dim` | `ODYTTY_FOCUS_DIM` | float | `0.0` | `0.0–1.0` |
| `inactive_pane_dim` | `ODYTTY_INACTIVE_PANE_DIM` | float | `0.0` | `0.0–1.0` |
| `window_border` | `ODYTTY_WINDOW_BORDER` | `on` / `off` | `off` | — |

**`cvd_mode`** — colour-vision-deficiency adaptation. `off` (default) publishes
the authored palette unchanged. `protan` and `deutan` target red–green
confusion; `tritan` targets blue–yellow. The adaptation is an OKLab
daltonization scoped to the palette only — the 16 ANSI colours plus the
cursor/selection/search roles — and is re-floored for readability. Application
truecolour and indexed-256 output are not remapped.

**`cvd_strength`** — how strongly the palette is daltonised toward separability
for the selected mode. `1.0` (default) is the full correction; `0.0` is an exact
passthrough. Inert while `cvd_mode = off`.

**`focus_dim`** — how much the whole grid recedes while the window is unfocused.
The dim runs at color-resolution time before the minimum-contrast floor, so
legibility is preserved by construction. `0.0` (default) disables it and is
pixel-identical to the plain renderer; the focused window is never dimmed.

**`inactive_pane_dim`** — a subtle OKLab dim applied to the non-focused panes of
a multi-pane tab so the focused pane stands out. `0.0` (default) disables it; the
focused pane and single-pane tabs are never affected.

**`window_border`** — draws a thin border in the theme `border` role color into
the existing window padding band, framing the grid. Off by default; while off no
border quads are emitted and the render path is byte-identical. It never eats
cell area.

---

## Plain / fast mode

Set `render_quality = plain`, `bloom = off`, `crt = off`, and
`min_contrast = 1.0` for the plain, fast renderer. This path is:

- Pixel-identical to the pre-effects codebase (verified by `pixel_smoke` tests)
- No offscreen allocation (the `Rgba16Float` intermediate is never created)
- No extra draw pass
- The correct choice for benchmarking, accessibility validation, and any
  environment where predictability matters more than aesthetics

The ambient path is the default. The plain path remains the explicit fast-mode
escape hatch.
