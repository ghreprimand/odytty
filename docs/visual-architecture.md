# OdyTTY Visual Architecture

This document describes the current renderer pipeline, the color resolution
model, and the planned visual-enhancement direction. Claims in the **Current
state** sections are grounded in source; claims in the **Planned** sections are
design intent, not yet built.

---

## Current renderer pipeline

*Source: `src/native/gpu.rs`, `src/shaders/cell.wgsl`,
`src/shaders/cell_subpixel.wgsl`, `src/grid.rs`, `src/atlas/mod.rs`,
`src/emoji/color_atlas.rs`.*

OdyTTY uses a **single-pass forward renderer**: one render pass writes directly
to the swapchain surface; there is no offscreen render target, no post-process
composite step, and no multi-sample resolve in the default path.

### Draw order within the single pass

The canonical order (`src/native/gpu.rs`, `fn render`, the `pass.draw` calls)
is:

1. **Surface clear** — the swapchain attachment is cleared to the theme's
   `clear` color before any draw calls.
2. **Background cell quads** — solid color quads covering every cell background
   (pass 1 of the cell pipeline, vertex range `0..background_count`).
3. **Below-zero images** — Kitty/Sixel placements with `z < 0` drawn by the
   image layer (`image_layer.draw_below`).
4. **Coverage glyphs and decorations** — glyph coverage quads, underlines,
   strikethroughs, and cursor/overlay quads (pass 2 of the cell pipeline,
   vertex range `background_count..cell_count`, then cursor/overlays at
   `cell_count..vertex_count`).
5. **Color-glyph quads** — premultiplied-RGBA color emoji bitmaps drawn by the
   dedicated color-glyph pipeline (vertex range
   `0..color_glyph_vertex_count`).
6. **Above/non-negative-z images** — Kitty/Sixel placements with `z >= 0`
   drawn by the image layer (`image_layer.draw_above`).

### Cell pipeline (`cell.wgsl` / `cell_subpixel.wgsl`)

Both shaders share the same vertex layout (`pos_px`, `uv`, `color`,
`is_glyph`) and the same `Viewport` uniform block.

**Grayscale path** (`cell.wgsl`, used when `SubpixelMode::Off`):

- Atlas texture format: `R8Unorm` (single coverage channel).
- Glyph fragment: samples `atlas_tex.r`, applies coverage gamma
  (`pow(coverage, 1/gamma)`, passthrough at `gamma == 1.0`), outputs
  `vec4(color.rgb, color.a * corrected_coverage)`.
- Background fragment: optionally applies the ambient scanline wash (see
  below); outputs `vec4(color.rgb * factor, color.a)`.

**Subpixel path** (`cell_subpixel.wgsl`, used when `SubpixelMode::Rgb` or
`SubpixelMode::Bgr`; requires `wgpu` dual-source blending):

- Atlas texture format: `Rgba8Unorm` (RGB = per-channel coverage, A unused).
- Glyph fragment: applies gamma per channel, emits two blend sources — a
  weighted color and a per-channel weight — for hardware dual-source blending.
- Background fragment: same scanline wash as the grayscale path.
- Fallback: if the adapter lacks `DUAL_SOURCE_BLENDING`, OdyTTY falls back to
  `SubpixelMode::Off` with a stderr notice; startup never fails because of it
  (`src/native/gpu.rs`: `fn actual_subpixel`).

### Ambient scanline wash

*Source: `src/theme.rs` (`VisualEffect`), `src/native/gpu.rs`
(`effect_params`, `fn set_visual`).*

`VisualEffect::Ambient` packs `[scanline_strength, scanline_period_px]` into
the `viewport.effect` uniform (`effect_params` in `gpu.rs`). The fragment
shader modulates background brightness with `1 - strength * trough`, where
`trough = 0.5 - 0.5 * cos(TAU * y / period)`. When `strength == 0.0` (the
`VisualEffect::Off` path) the factor is exactly `1.0` — pixel-identical to no
effect. **Glyphs are never touched by the wash** (the shader branches on
`is_glyph`).

`VisualEffect::Off` is the default; `VisualEffect::Ambient` (also `scanlines`)
is the only other value. Both shaders implement the wash identically.

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

*Source: `src/emoji/color_atlas.rs`, `src/native/gpu.rs`
(`create_color_glyph_pipeline`, `rebuild_color_glyph_segment`).*

`ColorGlyphAtlas` is a separate `Rgba8Unorm` atlas for premultiplied-RGBA
color emoji bitmaps. Entries are keyed by `(font_id, glyph_or_cluster_id,
px_size, scale)`, not by Unicode scalar. The color-glyph pipeline uses a
dedicated WGSL shader (inlined in `gpu.rs`) and a premultiplied-alpha blend
state (`blend_state_for_color_glyphs`). The atlas grows in 4-row increments
up to a cap of 4096 slots (`MAX_COLOR_GLYPH_SLOTS`).

---

## Current color model

*Source: `src/theme.rs` (`Theme`), `src/core/types.rs` (`DynamicColors`),
`src/grid.rs` (`foreground_linear`, `background_linear`), `src/text.rs`
(`indexed_srgb`, `srgb_to_linear`).*

### Theme (fg / bg / clear)

`Theme` carries three sRGB triples: `foreground`, `background`, and `clear`.
At startup the native layer calls `text::set_default_colors(fg, bg)` to
publish the theme defaults as process-global atomics, and passes `clear` to
`gpu.rs` as the wgpu surface clear color.

Three built-in themes exist today: `plain` (`#CCCCCC` / `#0B0C10`), `odyssey`,
and `odyssey-noir`. Unknown names fall back to `plain`.

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

**The Theme struct today carries only fg/bg/clear.** Indexed-color defaults,
semantic roles (cursor, selection, search highlight, border), and visual-effect
profiles are not yet part of the theme. This is the gap the palette-foundation
work addresses.

---

## Planned visual-enhancement direction

> **Status: planned / not yet built.** The sections below describe design
> intent. None of this exists in source yet unless explicitly noted.

The enhancement work is organized into three tiers, ordered by risk and
default-on policy.

### Hard rule (enforced across all tiers)

Every atmospheric or decorative effect must:

- Be **off by default** and reachable only through an explicit setting.
- Be **perf-gated**: a weak adapter or a budget-exceeded frame must
  auto-downgrade to the plain path without visual corruption.
- Be **readability-gated**: no effect may make text less legible at the user's
  configured settings (contrast floor).
- Have a **plain/fast bypass** that is **pixel-identical** to the minimal
  renderer (grayscale cell pipeline, no post-process). The bypass must be
  tested as such.

The plain/fast bypass today is the absence of a post-process pass and the
`VisualEffect::Off` shader branch — both of which are the current defaults and
are always tested.

### Tier 1 — Readability-first enhancements

Features in this tier help reading as well as looking better; they are the
highest-priority additions.

- **Perceptual color pipeline (RV3):** linear-space blending and OKLab/OKLCH
  interpolation for SGR dim, selection blends, and theme transitions.
  Foundational for subsequent effect compositing.
- **Minimum-contrast guarantee (RV1):** a configurable perceptual fg/bg
  contrast floor; value `1.0` = exact passthrough (default safe).
- **Geometric box-drawing / Powerline rendering (RV2):** U+2500–257F,
  U+2580–259F, Braille, and Powerline separators rendered as pixel-perfect
  geometry at exact cell size rather than font glyphs.
- **Smooth scrolling (RV4):** interpolated viewport movement within a strict
  bounded latency budget; instant/off mode preserved and default-safe.
- **Stem darkening for light-on-dark text (RV5):** compensate for irradiation
  thinning at small sizes; tunable, default-on with off switch.
- **Nerd-font / symbol fallback (RV6):** automatic PUA glyph fallback for
  modern prompt icons.

### Tier 2 — Identity and depth

Distinctive treatments that direct attention without harming legibility.

- **Cursor and selection treatments (ID1):** themed smooth cursor with optional
  soft glow, theme-colored selection vs raw inverse, emphasized current search
  match, cursor-position easing. Depends on full theme semantic roles.
- **Focus dimming (ID2):** subtle window dim when unfocused; bounded, never
  harms legibility.
- **Background treatments (ID3):** optional gradient/vignette/image background
  and blur-behind transparency, each with automatic readability-preserving dim.
- **Window chrome / padding identity (ID4):** themed padding and optional thin
  semantic-role border using the theme clear color.

### Tier 3 — Atmospheric effects (opt-in post-process)

These require a post-process pipeline (offscreen render target + composite
pass) that does not exist yet.

- **Post-process pipeline architecture (VE1):** offscreen render target,
  composite pass, perf-budget seam, weak-adapter auto-downgrade. Zero visible
  change at default settings.
- **Bloom / phosphor glow (VE2):** bright-text/bright-cell glow via threshold
  + separable blur + additive composite. Default subtle-or-off.
- **CRT / retro profile (VE3):** refined scanlines, vignette, optional
  curvature/chromatic aberration; selectable as a theme visual profile.
- **Subtle motion (VE4):** optional cursor glow/trail and fade-in of new
  output; bounded, disable-able, strict latency budget.
- **GPU quality + effect settings (VE5):** per-effect toggles in the settings
  panel; hard plain/fast mode bypasses all post-process.

### Theme and appearance system

The current `Theme` struct holds only `fg/bg/clear`. The planned extension adds
the full 16 + bright ANSI palette plus semantic roles (cursor, selection, search
highlight, border, inactive) as first-class theme fields. The renderer will
resolve indexed and default colors from the active theme rather than the fixed
xterm-256 table; OSC-4 / dynamic-color overrides layer on top with correct
precedence. Theme files, a curated built-in library, and a live in-app editor
are downstream of this foundation.

### In-app configuration UX

An overlay framework (cell-rendered, keyboard-driven, never mutates terminal
state) will expose all settings and a live theme picker without requiring
manual config-file edits.

---

## Modularity boundary

The renderer is designed to keep the visual layer entirely separate from the
terminal core:

- `src/core/` never imports windowing, GPU, or rendering code.
- All visual settings are in `src/settings.rs`; they flow to the renderer
  through the `Settings` struct and the config-reload seam.
- The post-process pipeline (Tier 3) will slot in as an additional render pass
  without changing the existing cell or color-glyph pipelines.
- The plain/fast bypass is the absence of a post-process pass and the
  `VisualEffect::Off` shader branch — both of which are the current defaults
  and are always tested.
