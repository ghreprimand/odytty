# OdyTTY — Visual Effects

This guide covers OdyTTY's optional visual effects: what they do, how to enable
them, and what to expect on different hardware. For the architecture and
readability invariants behind the effects system, see
[docs/visual-architecture.md](visual-architecture.md).

---

## The model

OdyTTY's visual effects follow three hard rules:

**Off by default.** Every effect is disabled at startup. No visual treatment is
active unless you enable it explicitly with a setting. The out-of-the-box
renderer is the plain, unadorned fast path.

**Readability-gated.** The terminal cell colors pass through the CPU
minimum-contrast floor (`min_contrast` / `ODYTTY_MIN_CONTRAST`, RV1) before GPU
post-processing. Post effects cannot feed their output back into that CPU
resolver, so any effect that changes brightness must be structurally bounded in
the shader. Bloom adds light only; CRT scanlines and vignette use capped
multiplicative dimming plus a hard brightness floor.

**Adapter-gated.** Effects that require GPU features OdyTTY cannot guarantee
(for example, a filterable HDR render target) silently fall back to the plain
path when the adapter cannot support them. OdyTTY prints one notice to stderr
and continues normally.

**Pixel-identical plain path.** When all effects are off, the renderer is
byte-identical to the pre-effects codebase. You can verify this with the
`pixel_smoke` test suite, which asserts exact structural equivalence between the
direct and offscreen-passthrough paths.

---

## Bloom (VE2)

Bloom adds an optional HDR phosphor glow around bright cells — glyphs whose
linear luminance exceeds a configurable knee. The effect is achieved by
rendering the terminal into a linear `Rgba16Float` HDR offscreen target,
extracting bright pixels in a threshold pass, blurring them at half resolution
with a separable Gaussian, and compositing the result additively back onto the
scene.

Bloom is off by default and pixel-identical to the plain renderer when disabled.

### Settings

All four settings are live-reloadable: changes in `odytty.conf` or the settings
overlay take effect on the next frame without restarting.

| Setting | Env | Type | Default | Range |
|---------|-----|------|---------|-------|
| `bloom` | `ODYTTY_BLOOM` | `on` / `off` | `off` | — |
| `bloom_threshold` | `ODYTTY_BLOOM_THRESHOLD` | float or `auto` | `auto` | `0.70–1.25` |
| `bloom_intensity` | `ODYTTY_BLOOM_INTENSITY` | float | `0.4` | `0.0–1.0` |
| `bloom_radius` | `ODYTTY_BLOOM_RADIUS` | float | `3.0` | `0.5–8.0` |

**`bloom`** — master switch. `on` enables the effect; `off` (default) returns to
the direct scene path with no offscreen allocation.

**`bloom_threshold`** — linear luminance knee for the bright-pass. Pixels
brighter than this value are eligible to glow; pixels below it are not. The
`auto` default derives the threshold from the active theme's foreground
luminance plus a safety margin (`relative_luminance(foreground) + 0.12`),
clamped to `0.70–1.25`. This keeps normal body text below the knee so it does
not glow — only genuinely bright elements (bold highlights, status indicators,
and glyphs rendered against a bright background) participate. Specify a fixed
float to override the theme-derived value.

**`bloom_intensity`** — additive glow strength. `0.0` produces no glow even
when enabled; `0.4` is the conservative default for a subtle phosphor warmth;
`1.0` is the cap. Values above the cap are clamped.

**`bloom_radius`** — blur spread in half-resolution pixels. Smaller values
(`0.5–1.5`) keep the glow tight around individual glyphs; larger values
(`5.0–8.0`) produce a wide phosphor wash across the screen. `3.0` is a soft
default.

### Enabling via odytty.conf

```
bloom = on
# bloom_threshold = auto   # leave unset for theme-derived default
bloom_intensity = 0.4
bloom_radius = 3.0
```

### Enabling via environment

```sh
ODYTTY_BLOOM=on ODYTTY_BLOOM_INTENSITY=0.4 ODYTTY_BLOOM_RADIUS=3.0 odytty
```

### Enabling via the settings overlay

Open the settings overlay with `Ctrl+Shift+,`, navigate to the `bloom` row, and
toggle it with `Space` or `Enter`. Bloom activates immediately on the next
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

## CRT / retro profile (VE3-a)

The CRT profile adds refined scanlines and a subtle vignette over the same HDR
offscreen target used by bloom. Curvature and chromatic aberration are deferred:
they are more likely to affect readability, so the first shipped profile keeps
to brightness-only treatments.

CRT is off by default and pixel-identical to the plain renderer when disabled.
When CRT and bloom are both enabled they share one offscreen scene render and
one final composite pass.

### Settings

All four settings are live-reloadable: changes in `odytty.conf` or the settings
overlay take effect on the next frame without restarting.

| Setting | Env | Type | Default | Range |
|---------|-----|------|---------|-------|
| `crt` | `ODYTTY_CRT` | `on` / `off` | `off` | — |
| `crt_scanline_intensity` | `ODYTTY_CRT_SCANLINE_INTENSITY` | float | `0.08` | `0.0–0.18` |
| `crt_scanline_period` | `ODYTTY_CRT_SCANLINE_PERIOD` | float | `3.0` | `2.0–12.0` |
| `crt_vignette_strength` | `ODYTTY_CRT_VIGNETTE_STRENGTH` | float | `0.10` | `0.0–0.16` |

**`crt`** — master switch. `on` enables the scanline/vignette profile; `off`
(default) returns to the direct scene path when no other post effect is active.

**`crt_scanline_intensity`** — dark-band strength. Values are clamped to
`0.0–0.18` so scanlines remain a subtle brightness modulation rather than an
opaque overlay.

**`crt_scanline_period`** — vertical distance between scanline bands in
physical pixels. `3.0` is the default.

**`crt_vignette_strength`** — edge dimming strength. Values are clamped to
`0.0–0.16`, and the shader enforces a brightness floor so lit cells are never
zeroed by the vignette.

### Enabling via odytty.conf

```
crt = on
crt_scanline_intensity = 0.08
crt_scanline_period = 3.0
crt_vignette_strength = 0.10
```

### Enabling via environment

```sh
ODYTTY_CRT=on ODYTTY_CRT_SCANLINE_INTENSITY=0.08 odytty
```

### Requirements and fallback

CRT uses the same `Rgba16Float` post-process target as bloom. If the adapter
cannot render, bind, and filter that format, OdyTTY uses the plain direct path.
With both bloom and CRT disabled, no offscreen texture is allocated.

---

## Motion (VE4) — coming

Cursor glow/trail and fade-in of new output. Bounded and disable-able. Not yet
shipped.

---

## Plain / fast mode

Setting all effects to their defaults (`bloom = off`) and leaving `min_contrast`
at `1.0` (exact passthrough) gives you the plain, fast renderer. This path is:

- Pixel-identical to the pre-effects codebase (verified by `pixel_smoke` tests)
- No offscreen allocation (the `Rgba16Float` intermediate is never created)
- No extra draw pass
- The correct choice for benchmarking, accessibility validation, and any
  environment where predictability matters more than aesthetics

The plain path is the default. You do not need to do anything to keep it.
