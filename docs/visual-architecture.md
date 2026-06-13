# OdyTTY Visual Architecture

This document describes the current renderer pipeline, the color resolution
model, and the visual-enhancement direction. Claims marked **(landed)** are
grounded in source; all other enhancement items are design intent, not yet
built.

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

### Theme (TH1 landed)

*Source: `src/theme.rs` (TH1, commit `fa857f0`).*

`Theme` now carries the full 16-color ANSI palette (indices 0–7 normal, 8–15
bright), semantic-role colors (cursor, selection, search highlight, reserved
border/inactive), and the three original sRGB triples (foreground, background,
clear). At startup the native layer calls `text::set_ansi_palette` to publish
the theme's 16-color palette alongside the default fg/bg, and passes `clear` to
`gpu.rs` as the wgpu surface clear color.

Three built-in themes: `plain` (`#CCCCCC` / `#0B0C10`, palette reproduces the
historical xterm table byte-for-byte), `odyssey`, and `odyssey-noir`. Unknown
names fall back to `plain`.

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

**TH1 closed the fg/bg/clear-only limitation.** The palette and semantic roles
are live in the theme. The full theme epic (TH1–TH4) is now complete — see the
Theme and appearance system section below and `docs/themes.md` for details.

---

## Visual-enhancement direction

> **Tier 1 (Readability-first) is substantially delivered** as of the current
> HEAD — RV3, RV1, and RV2 are live. Tiers 2 and 3 remain planned / in
> progress. Sub-sections marked **(landed)** are grounded in source; all other
> items are design intent, not yet built.

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

- **Perceptual color pipeline (RV3, landed):** linear-space blending is active
  in the render path; OKLab/OKLCH helpers (`dim_perceptual`, `mix_oklab`) are
  in place and back the minimum-contrast lift. Equal numeric steps produce equal
  perceived steps for color selection blends and the contrast floor. The SGR
  dim-text path currently applies a linear-space scale; adopting the perceptual
  dim there is a tracked follow-up.
- **Minimum-contrast guarantee (RV1, landed):** configurable perceptual fg/bg
  contrast floor applied at render time (`ODYTTY_MIN_CONTRAST`, `min_contrast`).
  Value `1.0` = exact passthrough (default). The floor is measured via WCAG
  relative luminance; lift is applied by bisecting OKLab lightness while
  preserving hue and chroma (`src/color.rs:enforce_min_contrast`).
- **Geometric box-drawing / Powerline rendering (RV2, landed):** U+2500–257F,
  U+2580–259F, Braille, and Powerline separators rendered as pixel-perfect
  geometry at exact cell size rather than font glyphs. Controlled by
  `ODYTTY_GEOMETRIC_BOXDRAW` / `geometric_boxdraw`; default on.
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

**The full theme epic has landed.** TH1 (full 16-color ANSI palette + semantic
roles), TH2 (dependency-free `.theme` file format, built-ins authored in it,
live reload via `SIGHUP` or settings panel), TH3 (53 built-in themes across
three families: Odyssey identity, Community, Retro/phosphor), and TH4 (in-app
theme builder — clone/tweak/author with live preview, saved to user theme file)
are all shipped. `Theme` carries the full 16-color ANSI palette plus semantic
roles (cursor, selection, search, reserved border/inactive). The indexed-color
render path is theme-driven; OSC-4 / dynamic-color overrides layer on top with
correct precedence. See `docs/themes.md` for the full roster and attribution.

### In-app configuration UX

**Delivered.** The overlay framework (UX1, cell-rendered, keyboard-driven,
never mutates terminal state), the full settings panel (UX2, `Ctrl+Shift+,`;
every setting editable with help text, live-applied, written back to
`odytty.conf` on confirm via atomic rename), the live theme picker (UX3,
`Ctrl+Shift+T`; arrow-to-preview, `Enter` to persist, `Esc` to restore), and
the in-app theme builder (TH4; clone/tweak/author with live preview) are all
shipped.

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
