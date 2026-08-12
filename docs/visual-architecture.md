# OdyTTY Visual Architecture

This document describes the current renderer pipeline, color resolution model,
and visual-enhancement boundaries. It is a companion to the user-facing
settings guide in [`docs/runtime-knobs.md`](runtime-knobs.md).

## Contents

- [Current renderer pipeline](#current-renderer-pipeline)
  - [Draw order within the scene pass](#draw-order-within-the-scene-pass)
  - [Cell pipeline (`cell.wgsl` / `cell_subpixel.wgsl`)](#cell-pipeline-cellwgsl--cell_subpixelwgsl)
  - [CRT scanline effect and the legacy ambient setting](#crt-scanline-effect-and-the-legacy-ambient-setting)
  - [Coverage atlas (`GlyphAtlas`)](#coverage-atlas-glyphatlas)
  - [Color-glyph atlas (`ColorGlyphAtlas`)](#color-glyph-atlas-colorglyphatlas)
- [Current color model](#current-color-model)
  - [Theme system (landed)](#theme-system-landed)
  - [Dynamic color overrides (OSC 10/11/12, OSC 4)](#dynamic-color-overrides-osc-101112-osc-4)
  - [Color resolution at render time](#color-resolution-at-render-time)
  - [Color-vision-deficiency adaptation](#color-vision-deficiency-adaptation)
- [Visual-enhancement direction](#visual-enhancement-direction)
  - [Hard rule (enforced across all tiers)](#hard-rule-enforced-across-all-tiers)
  - [Tier 1 — Readability-first enhancements](#tier-1--readability-first-enhancements)
  - [Tier 2 — Identity and depth](#tier-2--identity-and-depth)
  - [Tier 3 — Atmospheric effects](#tier-3--atmospheric-effects)
  - [Theme and appearance system](#theme-and-appearance-system)
  - [In-app configuration UX](#in-app-configuration-ux)
- [Modularity boundary](#modularity-boundary)

---

## Current renderer pipeline

*Source: `src/native/gpu.rs` (facade) and
`src/native/gpu/{frame,resources,pipelines,pipeline_policy,scene,post}.rs`,
`src/shaders/cell.wgsl`, `src/shaders/cell_subpixel.wgsl`, `src/grid.rs`,
`src/atlas/mod.rs`, `src/emoji/color_atlas.rs`.*

OdyTTY uses a forward cell/image renderer with a lazy post-process branch. When
`post_active()` is false, the scene writes directly to the swapchain. When bloom,
CRT, retro, or another post-process effect is active and the adapter supports
the required HDR format, the same scene is rendered to a linear `Rgba16Float`
offscreen target and composited back through a fullscreen pass. The
`render_quality = plain` profile forces the direct path.

### Draw order within the scene pass

The canonical scene order is:

1. **Scene clear** — the selected scene attachment is cleared to the theme's
   `clear` color: the HDR offscreen target while post-processing is active, or
   the swapchain on the direct path.
2. **Background image** — the configured wallpaper, when active.
3. **Background cell quads** — solid color quads covering every cell background
   (pass 1 of the cell pipeline, instance range `0..background_count`).
4. **Below-zero images** — Kitty/Sixel placements with `z < 0` drawn by the
   image layer (`image_layer.draw_below`).
5. **Cursor aura and large-jump follower** — the focused pane's one shape-aware
   analytic cursor aura (`cursor_glow`, `src/shaders/cursor_glow.wgsl`) and, when
   one is animating, its elastic large-jump follower
   (`src/shaders/cursor_streak.wgsl`), each drawn in its own pipeline *behind*
   both glyph lanes so text pixels are preserved exactly. Both are emitted only
   for the focused, live-tail pane and clipped to that pane's rect.
6. **Coverage glyphs and decorations** — glyph coverage quads, underlines, and
   strikethroughs (pass 2 of the cell pipeline, instance range
   `background_count..cell_count`).
7. **Color-glyph quads** — premultiplied-RGBA color emoji bitmaps drawn by the
   dedicated color-glyph pipeline (instance range
   `0..color_glyph_vertex_count`).
8. **Cursor and overlays** — the remaining cell-pipeline range,
   `cell_count..vertex_count`.
9. **Above/non-negative-z images** — Kitty/Sixel placements with `z >= 0`
   drawn by the image layer (`image_layer.draw_above`).

Steps 2–9 are the scene pass — the sequence that the post-process branch
re-targets to the offscreen `Rgba16Float` buffer when an effect is active. The
in-app **image lightbox** (the C4 viewer overlay; `src/native/image_layer.rs`,
`OverlayImage`) is the exception:

- It is composited **after** the CRT/bloom post pass, directly onto the
  swapchain, so the presented photo is never run through scanlines, vignette, or
  bloom.
- It draws a full-viewport dimming scrim (`SCRIM_ALPHA`) and then the fitted
  image (`OVERLAY_FIT_FRACTION` of the viewport, never upscaled) using a
  dedicated **`Linear`** sampler — distinct from the `Nearest` sampler used for
  inline terminal-graphics placements — so a scaled-down image is smoothly
  interpolated.
- With `interactive_paths` enabled, it is opened by Ctrl+click on Linux/Windows
  or Cmd+click on macOS over a resolved image path (see
  [`docs/keybindings.md`](keybindings.md)) and dismissed with `Esc` or a click
  outside.

### Cell pipeline (`cell.wgsl` / `cell_subpixel.wgsl`)

Both shaders share one compact per-quad instance layout (`pos_px`,
`end_pos_px`, `uv`, `end_uv`, `color`, `is_glyph`) and the same `Viewport`
uniform block. `@builtin(vertex_index)` selects the fixed
`[tl, bl, tr, tr, bl, br]` corner order, so one 64-byte instance replaces six
48-byte expanded CPU vertices. The color-glyph shader uses the same expansion
contract with its own compact position/UV/alpha instance. The buffers are
grow-only and content changes refill reusable CPU storage; retained frames skip
geometry uploads, while cursor-only frames rewrite only their bounded tail.

**Grayscale path** (`cell.wgsl`, used when `SubpixelMode::Off`):

- Atlas texture format: `R8Unorm` (single coverage channel).
- Glyph fragment: samples `atlas_tex.r`, applies coverage gamma
  (`pow(coverage, 1/gamma)`, passthrough at `gamma == 1.0`), outputs
  `vec4(color.rgb, color.a * corrected_coverage)`.
- Background fragment: outputs `vec4(color.rgb, color.a)`. (The legacy inline
  cell-shader scanline wash is retired; see the CRT/scanline section below.)

**Subpixel path** (`cell_subpixel.wgsl`, used when `SubpixelMode::Rgb` or
`SubpixelMode::Bgr`; requires `wgpu` dual-source blending):

- Atlas texture format: `Rgba8Unorm` (RGB = per-channel coverage, A unused).
- Coverage filter: a 5-tap `[1,2,3,2,1]/9` energy-conserving LCD filter runs
  over the physical left-to-right subpixel axis at raster time
  (`src/atlas/mod.rs`: `lcd_filter_subpixel_region`), collapsing vertical-stem
  color fringing toward neutral while preserving per-row luminance. It runs only
  for `SubpixelMode::Rgb`/`Bgr`; `Off` coverage is never filtered.
- Glyph fragment: applies gamma per channel, emits two blend sources — a
  weighted color and a per-channel weight — for hardware dual-source blending.
- Background fragment: same as the grayscale path (inline scanline wash retired; CRT post-process handles it).
- Fallback: if the adapter lacks `DUAL_SOURCE_BLENDING`, OdyTTY falls back to
  `SubpixelMode::Off` with a stderr notice; startup never fails because of it
  (`src/native/gpu/pipeline_policy.rs`: `effective_subpixel_mode`).

### CRT scanline effect and the legacy ambient setting

The unified CRT post-process is the single scanline implementation. The legacy
cell-shader scanline wash (the old `VisualEffect::Ambient` path) has been
retired: the cell shaders no longer modulate background brightness inline.

`visual=ambient` and `visual=scanlines` are back-compat aliases: when either is
set and no explicit `crt` key is present, OdyTTY enables the CRT scanline
effect as if `crt=on` were specified. An explicit `crt` setting always wins —
the `visual` key never overrides it. `visual=off`/`none`/`plain` suppress only
the legacy alias; because CRT and bloom both default on, use `crt=off` and
`bloom=off` (or `render_quality=plain`) for a plain renderer.

The CRT path requires a GPU adapter with filterable 16-bit float support; it
silently no-ops on adapters that lack it. Unlike the old cell-shader wash
(which only dimmed backgrounds), the CRT post-process dims the full scene
including glyph coverage, and its scanline strength, period, vignette, and
screen curvature are independently configurable via their own settings.

### Coverage atlas (`GlyphAtlas`)

*Source: `src/atlas/mod.rs`.*

`GlyphAtlas` is a CPU-rasterized glyph coverage atlas. ASCII printables are
baked at atlas build time; non-ASCII codepoints are rasterized on demand. Each
glyph slot is one or two cells wide (two cells for wide CJK/emoji). Bearing-
aware glyph quads let ink overflow the nominal cell bounds for box-drawing joins
and wide glyphs. The atlas is grow-only: it appends pages of rows when capacity
is exhausted.

- Grayscale (`SubpixelMode::Off`): `width * height` bytes of R8 coverage.
- Subpixel (`Rgb` or `Bgr`): `width * height * 4` bytes (RGBA8, per-channel
  coverage in RGB); roughly 4x the grayscale memory for the same slot count.

Synthetic bold (double-strike) and italic (12° shear) are baked into atlas
slots at rasterization time when no real bold/italic face is loaded.

### Color-glyph atlas (`ColorGlyphAtlas`)

*Source: `src/emoji/color_atlas.rs`, `src/native/gpu/pipelines.rs`
(`create_color_glyph_pipeline`), `src/native/gpu/scene.rs`
(`rebuild_color_glyph_segment`).*

`ColorGlyphAtlas` is a separate `Rgba8Unorm` atlas for premultiplied-RGBA
color emoji bitmaps. Entries are keyed by `(font_id, glyph_or_cluster_id,
px_size, scale)`, not by Unicode scalar. The color-glyph pipeline uses a
dedicated WGSL shader (inlined in `gpu/pipelines.rs`) and a premultiplied-alpha
blend state (`gpu/pipeline_policy.rs`: `blend_state_for_color_glyphs`). The
shader expands one compact quad instance per glyph, matching the mono cell
pipeline's corner and UV interpolation contract. The atlas grows in 4-row
increments up to a cap of 4096 slots
(`MAX_COLOR_GLYPH_SLOTS`).

---

## Current color model

*Source: `src/theme/mod.rs` (`Theme`), `src/core/types.rs` (`DynamicColors`),
`src/grid.rs` (`foreground_linear`, `background_linear`), `src/text.rs`
(`indexed_srgb`, `srgb_to_linear`).*

### Theme system (landed)

*Source: `src/theme/mod.rs`, `src/theme/builtins.rs`, `src/theme/spec.rs`.*

`Theme` now carries the full 16-color ANSI palette (indices 0–7 normal, 8–15
bright), semantic-role colors (cursor, selection, search highlight, border,
inactive), and the three original sRGB triples (foreground, background,
clear). At startup the native layer calls `text::set_ansi_palette` to publish
the theme's 16-color palette alongside the default fg/bg, and passes `clear` to
`src/native/gpu/resources.rs` as the wgpu surface clear color.

The built-in library contains 142 contrast-validated themes. `plain`
reproduces the historical xterm table byte-for-byte, while `odyssey-default` is
the fresh-install default. The settings loader warns and falls back to
`odyssey-default` for unknown names; `Theme::from_name_or_default` uses `plain`.

The indexed-color resolution chain is now theme-driven for indices 0–15:
`text::indexed_srgb(0..=15)` reads the published theme palette override before
falling back to the built-in constant table; cube (16–231) and grayscale
(232–255) entries are unchanged.

### Dynamic color overrides (OSC 10/11/12, OSC 4)

`DynamicColors` (`src/core/types.rs`) is snapshotted alongside the cell grid
on every frame. It carries:

- `foreground` / `background` / `cursor` — per-session overrides from OSC
  10/11/12; initialized to the theme defaults; reset to `base_colors` on OSC
  reset.
- `palette[256]` — per-index overrides from OSC 4; `None` entries fall
  through to the xterm-256 built-in table (`text::indexed_srgb`).

### Color resolution at render time

`grid.rs`'s `foreground_linear` and `background_linear` resolve:

- `Color::Default` → `DynamicColors.{foreground,background}`
- `Color::Indexed(i)` → `palette[i]` if overridden, else `indexed_srgb(i)`
- `Color::Rgb(r,g,b)` → passthrough

The result is linearized (`srgb_to_linear`) before upload as a vertex
attribute; the sRGB swapchain surface applies the linear→sRGB transfer on
write, so no explicit gamma correction is needed in the output path (only the
glyph-coverage gamma matters for text rendering).

**The theme update closed the fg/bg/clear-only limitation.** The palette and semantic roles
are live in the theme. The full theme epic is now complete — see the
Theme and appearance system section below and `docs/themes.md` for details.

### Color-vision-deficiency adaptation

*Source: `src/cvd.rs`, `src/native/cvd_theme.rs`.*

When `cvd_mode` is set (`protan`, `deutan`, or `tritan`; default `off`), the
theme palette is daltonized in OKLab before it reaches the render path:

- `cvd_theme::effective_theme` calls `cvd::adapt_palette`, which separates the
  confusable opponent axis (red–green `a` for protan/deutan, blue–yellow `b` for
  tritan) and re-floors the result so it stays readable.
- `cvd_strength` (default `1.0`) scales the adaptation; the `off` short-circuit
  leaves the theme byte-for-byte untouched.
- The scope is **palette-only** — the 16 ANSI colors plus the structural
  foreground/background/chrome roles. Indexed-256 cube/grayscale colors and
  application truecolor are **not** remapped.

This sits in the same accessibility band as the minimum-contrast floor and focus
dimming; see [`docs/accessibility.md`](accessibility.md).

---

## Visual-enhancement direction

The enhancement work is organized into three tiers. Tier 1 readability
foundations are shipped. Tier 2 identity/depth work now includes themed roles,
focus dimming, background treatments, padding, border, and window decoration
controls. Tier 3 atmospheric work now includes cursor motion, glow, trail, and
blink fade alongside the HDR post-process branch, bloom, CRT/retro, curvature,
and new-output fade.

### Hard rule (enforced across all tiers)

Every atmospheric or decorative effect must:

- Have an explicit off switch and stay behind a normal setting.
- Be **perf-gated**: an adapter that lacks the required capability (filterable
  HDR format, dual-source blending, and similar) auto-downgrades to the plain
  path without visual corruption. A live per-frame budget detector that
  downgrades a still-capable adapter after an over-budget frame is deferred
  design, not implemented.
- Be **readability-gated**: no effect may make text less legible at the user's
  configured settings (contrast floor).
- Have a **plain/fast bypass** that is **pixel-identical** to the minimal
  renderer (grayscale cell pipeline, no post-process). The bypass must be
  tested as such.

The plain/fast bypass today is `render_quality = plain`, which forces the
direct path and disables post-process effects and visual treatments for
benchmarking and compatibility checks.

### Tier 1 — Readability-first enhancements

Features in this tier help reading as well as looking better; they are the
highest-priority additions.

- **Perceptual color pipeline (landed):** linear-space blending is active
  in the render path; OKLab/OKLCH helpers (`dim_perceptual`, `mix_oklab`,
  `src/color.rs`) are used throughout — by the minimum-contrast lift, by SGR
  dim-text, and by the focus-dim step. Honest note: `dim_perceptual` applies
  a uniform OKLab scale that reduces algebraically to a uniform linear-RGB scale,
  so for the uniform-dim case it is output-identical to naive per-channel halving
  (both preserve hue). The perceptual pipeline's payoff is in the non-uniform
  paths: `mix_oklab` for blends and the OKLab bisect for the contrast floor.
- **Minimum-contrast guarantee (landed):** configurable perceptual fg/bg
  contrast floor applied at render time (`ODYTTY_MIN_CONTRAST`, `min_contrast`).
  Value `1.0` = exact passthrough; the fresh-install default is `17.0`.
  The floor is measured via WCAG
  relative luminance; lift is applied by bisecting OKLab lightness while
  preserving hue and chroma (`src/color.rs:enforce_min_contrast`).
- **Geometric box-drawing / Powerline rendering (landed):** U+2500–257F,
  U+2580–259F, Braille, and Powerline separators rendered as pixel-perfect
  geometry at exact cell size rather than font glyphs. Controlled by
  `ODYTTY_GEOMETRIC_BOXDRAW` / `geometric_boxdraw`; default on (since v0.6.0).
- **Scroll glide and pixel scrolling (landed, default-on):** detented wheels ease
  the rendered view toward each notch over a few frames (`scroll_glide`);
  high-resolution wheels and touchpads track physical travel 1:1 on a continuous
  sub-row lane (`pixel_scroll`, `scroll_pixel_speed`). The scroll target snaps
  instantly, so neither adds input latency, and both move only in the scroll
  direction so they cannot overshoot.
- **Stem darkening for light-on-dark text (landed, default-on):** a coverage
  boost that keeps glyph stroke weight on light-on-dark displays.
  `ODYTTY_STEM_DARKEN` / `stem_darken`, range `0.0`–`1.0`, default `0.7`.
  Applied at rasterization time (`src/atlas/mod.rs`); `0.0` is the
  byte-identical opt-out to the classic raster.
- **Nerd-font / symbol fallback (landed):** automatic PUA glyph fallback
  for modern prompt icons (starship, powerlevel10k, eza). The `symbol_fallback`
  setting / `ODYTTY_SYMBOL_FALLBACK` env var enables the secondary symbol-font
  face and is **default on**; `symbol_font` / `ODYTTY_SYMBOL_FONT` specifies an
  explicit face path and is unset by default (the automatic font search supplies
  the bundled Nerd Fonts Symbols faces when no path is given).

### Tier 2 — Identity and depth

Distinctive treatments that direct attention without harming legibility.

- **Cursor and selection treatments (landed):** themed
  cursor/selection/search semantic roles are delivered: when
  `themed_ui_roles` is on (default), the cursor uses the theme cursor color,
  selections use the theme selection color, and search highlights use the theme
  search color rather than raw cell inversion.
  `ODYTTY_THEMED_UI_ROLES=off`
  restores the classic inversion behavior.
- **Focus dimming (landed):** dims the whole grid — both text foreground
  and background — perceptually in OKLab while the window is unfocused, so it
  recedes visually without color shifts. `ODYTTY_FOCUS_DIM` / `focus_dim`,
  range `0.0`–`1.0`, default `0.0` (off); recommended range `0.15`–`0.30` for
  a subtle recede. Applied in the grid resolve closure *after* SGR-dim and
  *before* the contrast floor, so legibility is preserved by construction — the
  contrast floor sees the dimmed background and re-lifts text if needed. Focused frames
  are never dimmed: the effective amount is always `0.0` when focused, keeping
  focused frames byte-identical to the unfocused-off path.
- **Background treatments (landed):** optional gradient, vignette, and
  PNG/JPEG/WebP image backgrounds. Image mode uses `cell_bg_opacity`, a one-time
  optional CPU blur (`background_blur_radius`, default `0` = off), and a
  readability scrim (`background_image_scrim`, fixed `0.5` by default; `auto`
  computes a floor-safe value).
- **Window chrome / padding identity (landed):** themed padding, optional thin
  semantic-role border, and a live window-decoration toggle.

### Tier 3 — Atmospheric effects

These require a post-process pipeline (offscreen render target + composite
pass), which now exists (VE1) and carries the first effect (VE2). For user-facing
settings and how to enable effects, see [`docs/effects.md`](effects.md).

- **Post-process pipeline architecture (VE1) (landed):** linear `Rgba16Float`
  offscreen render target, composite pass, filterable-format probe with
  weak-adapter auto-downgrade. The pipeline is a no-op when no effect is enabled;
  the shipped defaults enable bloom and CRT, so the offscreen path is normally
  active. Scene pipelines re-target the offscreen format only while a pass is
  active.
- **Bloom / phosphor glow (VE2) (landed):** bright-text/bright-cell glow via a
  bright-pass threshold + half-res separable blur + additive composite. Enabled
  in the fresh-install ambient baseline behind the `bloom` setting and
  adapter-gated.
- **CRT / retro profile (landed):** refined scanlines and vignette, plus a
  separate `retro=on` preset for a stronger phosphor reference look. Optional
  curvature comes only from the config/environment `crt_curvature` knob and is
  flat by default; the retro preset does not force it. Chromatic aberration
  remains deferred.
- **Subtle motion (landed):** optional cursor glow, blink fade, slide, trail,
  and fade-in of new output; bounded and disable-able.
- **GPU quality + effect settings:** per-effect toggles in the settings
  panel; hard plain/fast mode bypasses all post-process.

### Theme and appearance system

**The full theme system has landed:**

- A 16-color ANSI palette with semantic roles (cursor, selection, search,
  border, inactive), carried by `Theme`.
- A dependency-free `.theme` file format with built-in themes and live reload
  through the settings/config seam.
- 142 built-in themes across OdyTTY original, community, and retro/phosphor
  families.
- An in-app theme builder with live preview, saved to a user theme file.

The indexed-color render path is theme-driven; OSC-4 / dynamic-color overrides
layer on top with correct precedence. See `docs/themes.md` for the full roster
and attribution.

### In-app configuration UX

**Delivered.** The cell-rendered overlay framework (keyboard-driven, never
mutates terminal state), the full settings panel (`Ctrl+Shift+,`; every setting
editable with help text, live-applied, written back to `odytty.conf` on confirm
via atomic rename), the live theme picker (`Ctrl+Shift+H`; arrow-to-preview,
`Enter` to persist, `Esc` to restore), and the in-app theme builder with live
preview are all shipped.

---

## Modularity boundary

The renderer is designed to keep the visual layer entirely separate from the
terminal core:

- `src/core/` never imports windowing, GPU, or rendering code.
- All visual settings are in `src/settings.rs`; they flow to the renderer
  through the `Settings` struct and the config-reload seam.
- The post-process pipeline is an additional render branch that reuses the same
  scene draw sequence and does not change terminal state.
- The plain/fast bypass is `render_quality = plain`, which is tested as the
  compatibility/performance escape hatch.
