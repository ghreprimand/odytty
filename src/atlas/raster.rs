// SPDX-License-Identifier: GPL-3.0-only
//! Slot geometry and coverage rasterization policy.

use super::*;

pub(super) fn overflow_margin(cell: CellSize) -> u32 {
    (cell.height / 4).max(2)
}

/// Total border each side of a slot's cell: the transparent bleed gutter
/// ([`ATLAS_PAD`]) plus the drawable [`overflow_margin`]. The cell's inner
/// top-left within a slot is `(ox + slot_border, oy + slot_border)`.
pub(super) fn slot_border(cell: CellSize) -> u32 {
    ATLAS_PAD + overflow_margin(cell)
}

/// Full slot width in pixels: the glyph cell plus its border on both sides.
pub(super) fn slot_w(cell: CellSize) -> u32 {
    cell.width + 2 * slot_border(cell)
}

/// Full slot height in pixels: the glyph cell plus its border on both sides.
pub(super) fn slot_h(cell: CellSize) -> u32 {
    cell.height + 2 * slot_border(cell)
}

/// Pixel offset `(ox, oy)` of an atlas slot's **outer** top-left within the
/// bitmap (the border corner). The cell's inner origin is
/// `(ox + slot_border, oy + slot_border)`.
pub(super) fn slot_offset(slot: u32, cols: u32, cell: CellSize) -> (u32, u32) {
    ((slot % cols) * slot_w(cell), (slot / cols) * slot_h(cell))
}

/// Rasterize one glyph's coverage into the slot whose **outer** top-left is
/// `origin`, positioning it on the shared integer `baseline`, and return its
/// inked pixel extent relative to the cell's inner top-left.
///
/// Returns `None` if the font has no outline for `ch`, or an outline that inks
/// no pixels (e.g. a space). The returned [`GlyphInk`] offsets may be negative
/// (ink left of / above the cell) and its size may exceed the cell (ink right of
/// / below it), which is what lets the renderer draw overflow uncropped.
///
/// The glyph's pen is placed at the cell's inner origin `(ox + slot_border,
/// oy + slot_border)` and on `baseline`, then each coverage sample is placed at
/// the **nearest** atlas pixel (rounding, not truncation, for stable sub-pixel
/// placement). Coverage may land anywhere in the drawable region — the cell plus
/// its overflow margin — so ink genuinely past the cell box (powerline glyphs,
/// box-drawing joins, descenders, italic side bearing) is preserved; only the
/// outermost [`ATLAS_PAD`] bleed ring is kept transparent, and the clip keeps a
/// glyph strictly out of its neighbors. The strongest value wins on any overlap.
///
/// When `synth` is non-identity (no real face for the requested style), the
/// upright Regular outline is transformed at coverage-write time: a per-sample
/// horizontal **shear** for synthetic italic and a rightward **double-strike**
/// for synthetic bold (see [`SynthTransform`]). Both effects are tracked by the
/// same `min/max` ink bounds, so the returned [`GlyphInk`] reports the real
/// (sheared/widened) extent and the renderer draws it uncropped; the existing
/// drawable-region clip keeps the synthesis inside the slot. Synthesis never
/// changes the cell advance.
///
/// Maximum exponent gain for stem-darkening at strength `1.0`. The applied
/// exponent is `1.0 / (1.0 + strength * STEM_DARKEN_GAIN)`; with strength in
/// `0.0..=1.0` this yields an exponent in `1.0 ..= 1/(1+GAIN)`. At `0.6` the
/// strongest setting boosts a 50%-coverage edge sample by roughly +30%, which
/// thickens stems noticeably without flooding glyph counters.
const STEM_DARKEN_GAIN: f32 = 0.6;

/// Active stem-darkening strength, bit-cast `f32` in an atomic so raster reads
/// stay lock-free (mirrors the runtime color-override seams in
/// [`crate::text`]). `0.0` (the default) is a true no-op: coverage is written
/// byte-identically to the pre-feature atlas. Presentation-only — never affects
/// terminal cell contents or metrics.
///
/// **RV5 prototype.** This is a *global* coverage boost applied to every
/// rasterized glyph (it cannot see per-cell fg/bg at raster time). The
/// luminance-conditioned "light-on-dark only" variant is a documented in-shader
/// follow-up; see the audit findings.
static STEM_DARKEN: AtomicU32 = AtomicU32::new(0); // 0.0_f32.to_bits()

/// Set the global stem-darkening strength used when rasterizing glyphs.
///
/// Called once at native startup (and on the atlas-rebuild seam) by the
/// settings layer with the parsed `ODYTTY_STEM_DARKEN` value. The strength is
/// clamped to `0.0..=1.0`; `0.0` disables the boost and is pixel-identical to
/// the pre-feature renderer. Only glyphs rasterized *after* this call observe
/// the new value, which on the live path is the entire atlas (it is rebuilt
/// when the setting changes).
pub fn set_stem_darken(strength: f32) {
    let clamped = if strength.is_finite() {
        strength.clamp(0.0, 1.0)
    } else {
        0.0
    };
    STEM_DARKEN.store(clamped.to_bits(), Ordering::Relaxed);
}

/// The active stem-darkening strength (`0.0` when disabled).
pub(super) fn stem_darken_strength() -> f32 {
    f32::from_bits(STEM_DARKEN.load(Ordering::Relaxed))
}

/// Test-only read of the process-global stem-darkening strength, so the shared
/// render-globals guard can snapshot and restore it. Restoration goes through
/// the public [`set_stem_darken`] setter; the snapshot is already clamped, so
/// the write-back is exact.
#[cfg(test)]
pub(crate) fn stem_darken_strength_for_test() -> f32 {
    stem_darken_strength()
}

/// Apply stem-darkening to one 8-bit coverage sample.
///
/// Compensates for the irradiation illusion (light text on a dark field appears
/// thinner than it is) by raising partial coverage toward full, so stems hold
/// weight at small sizes. The mapping is `c^(1/(1+strength*GAIN))` on the
/// normalized coverage `c = value/255`.
///
/// **Pixel-identity guarantee:** at `strength <= 0.0` this returns `value`
/// unchanged, and the fully-uncovered (`0`) and fully-covered (`255`) endpoints
/// are always returned exactly — only intermediate (anti-aliased edge / thin
/// stem) coverage is boosted. So a disabled or absent setting reproduces the
/// historical atlas byte-for-byte.
pub(super) fn apply_stem_darken(value: u8, strength: f32) -> u8 {
    if strength <= 0.0 || value == 0 || value == 255 {
        return value;
    }
    let c = value as f32 / 255.0;
    let exponent = 1.0 / (1.0 + strength * STEM_DARKEN_GAIN);
    let boosted = c.powf(exponent);
    (boosted * 255.0).round().clamp(0.0, 255.0) as u8
}

/// **Primary size knob.** Fraction of cell HEIGHT a fitted symbol/icon glyph
/// fills. The glyph is scaled so its ink height is `SYMBOL_CELL_FILL *
/// cell.height` (then width-capped so it can never clip), and centered on the
/// cell. Tuned against ghostty on a dev build: ~0.82 matches; full
/// height (~0.95) reads too big, the old width-fit (~0.6 em cell width) too
/// small. Tune here during the dev-build eyeball.
pub(super) const SYMBOL_CELL_FILL: f32 = 0.82;

/// Inset padding, as a fraction of the smaller cell dimension, used **only** to
/// narrow the width safety cap for a fitted icon (so a wide glyph scaled to
/// height still leaves a gutter inside the slot's drawable region and never
/// kisses a neighbour). In the height-fraction model the inset no longer drives
/// the size target — [`SYMBOL_CELL_FILL`] does — it just trims `max_draw_w`.
pub(super) const SYMBOL_CELL_INSET: f32 = 0.10;

/// Maximum upscale applied when fitting a *sub-cell* symbol/icon glyph. The
/// height-fraction model needs more headroom than the old width-fit so small
/// icons reach the target fill; downscaling (oversized glyphs, the common
/// Nerd-Font case) is uncapped, upscaling is capped here so a 1-pixel dot does
/// not balloon to flood the cell. Tunable in the dev build.
const SYMBOL_MAX_UPSCALE: f32 = 2.0;

/// Opt-in directive to **fit-and-center** a glyph inside its cell box instead of
/// placing it at the font's natural left bearing on the text baseline. Passed
/// `Some` only for symbol-fallback / SYMMAP-override (icon) faces, whose em-size
/// ink does not match OdyTTY's cell aspect and would otherwise overflow/clip and
/// render as a ragged column. Every text path (primary font, ASCII build,
/// synthetic styles) passes `None`, leaving placement byte-identical.
#[derive(Clone, Copy)]
pub(super) struct CellFit {
    /// Drawable cell-box width in pixels (`span * cell.width`).
    pub(super) box_w: f32,
    /// Drawable cell-box height in pixels (`cell.height`).
    pub(super) box_h: f32,
    /// Inset padding in pixels per side. In the height-fraction model this only
    /// narrows the width safety cap (`max_draw_w`); it no longer drives size.
    pub(super) pad: f32,
}

/// Height-fraction fit scale for a symbol/icon glyph (aspect-preserving).
///
/// The glyph is scaled so its natural ink height (`nat_h`) reaches `target_h`
/// (= [`SYMBOL_CELL_FILL`] × cell height), then bounded by a width safety cap so
/// a wide glyph scaled to height still cannot exceed `max_draw_w` (the slot's
/// drawable region minus the inset gutter) and clip. Upscaling is capped at
/// `max_upscale`; downscaling is uncapped. Inputs are clamped strictly
/// positive; a non-finite or non-positive result degrades to `1.0` (no scaling).
pub(super) fn symbol_fit_scale_v2(
    nat_w: f32,
    nat_h: f32,
    target_h: f32,
    max_draw_w: f32,
    max_upscale: f32,
) -> f32 {
    let s_h = target_h.max(1.0) / nat_h.max(1.0);
    let s_w = max_draw_w.max(1.0) / nat_w.max(1.0);
    let s = s_h.min(s_w).min(max_upscale);
    if s.is_finite() && s > 0.0 { s } else { 1.0 }
}

/// Resolved placement for a fitted symbol glyph: the scaled pen size and the
/// pre-biased atlas-pixel origin. The origin already subtracts the measured ink
/// bbox min, so in the draw closure `origin + bounds.min + (gx, gy)` lands the
/// centered ink top-left exactly (with the per-channel subpixel shift preserved
/// through `bounds.min.x`).
#[derive(Clone, Copy)]
struct FitPlacement {
    scale: PxScale,
    origin_x: f32,
    origin_y: f32,
}

/// Destination slot geometry for [`rasterize_glyph`].
pub(super) struct SlotRegion {
    /// Outer top-left of the (lead) slot in atlas pixels.
    pub(super) origin: (u32, u32),
    /// Shared per-cell metrics.
    pub(super) cell: CellSize,
    /// Total horizontal extent in pixels — `slot_w(cell)` for a normal glyph,
    /// `span * slot_w(cell)` for a wide one — i.e. the right clip edge.
    pub(super) outer_w: u32,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rasterize_glyph(
    font: &FontVec,
    pen: Pen,
    ch: char,
    anchor_x: f32,
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    region: SlotRegion,
    synth: SynthTransform,
    fit: Option<CellFit>,
) -> Option<GlyphInk> {
    if !font_has_glyph(font, ch) {
        return None;
    }
    rasterize_glyph_id(
        font,
        pen,
        font.glyph_id(ch),
        anchor_x,
        data,
        width,
        subpixel,
        region,
        synth,
        fit,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rasterize_glyph_id(
    font: &FontVec,
    pen: Pen,
    glyph_id: GlyphId,
    anchor_x: f32,
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    region: SlotRegion,
    synth: SynthTransform,
    fit: Option<CellFit>,
) -> Option<GlyphInk> {
    let SlotRegion {
        origin,
        cell,
        outer_w,
    } = region;
    let (ox, oy) = origin;
    let scale = PxScale::from(pen.px);
    if glyph_id.0 == 0 {
        return None;
    }
    // Stem-darkening strength is read once per glyph; `0.0` (the default) makes
    // `apply_stem_darken` an identity so coverage is byte-identical to before.
    let stem = stem_darken_strength();
    // Cell inner origin, and the drawable region (cell + overflow margin) that
    // coverage may occupy, leaving the outer ATLAS_PAD bleed ring transparent.
    // `outer_w` is the slot's total horizontal extent in pixels — `slot_w(cell)`
    // for a normal glyph, `span * slot_w(cell)` for a wide (multi-cell) glyph —
    // so the right clip extends across every reserved cell of a wide slot.
    let border = slot_border(cell) as i32;
    let inner_x = ox as i32 + border;
    let inner_y = oy as i32 + border;
    // Symbol/icon fit (RV6+): when `fit` is `Some`, measure the glyph's natural
    // ink box at the body em-size, scale it so its ink height fills
    // `SYMBOL_CELL_FILL` of the cell (width-capped so a wide glyph can't clip),
    // and CENTER it on the cell box — ignoring the font's bearing and text
    // baseline, since icons are not baseline-aligned text. `None` (every text
    // path) leaves placement at the natural bearing on `pen.baseline`,
    // byte-identical to the pre-fit renderer.
    let fit_placement: Option<FitPlacement> = fit.and_then(|f| {
        let measure = |sc: PxScale| {
            font.outline_glyph(glyph_id.with_scale_and_position(sc, point(0.0, 0.0)))
                .map(|o| o.px_bounds())
        };
        let nb = measure(PxScale::from(pen.px))?;
        // Height-fraction target: fill SYMBOL_CELL_FILL of the cell height.
        let target_h = (f.box_h * SYMBOL_CELL_FILL).max(1.0);
        // Width safety cap: the slot's drawable region (cell + overflow margin)
        // inside the ATLAS_PAD bleed gutter, minus the inset pad on each side, so
        // a wide glyph scaled to height still cannot clip or kiss a neighbour.
        let max_draw_w = (outer_w as f32 - 2.0 * ATLAS_PAD as f32 - 2.0 * f.pad).max(1.0);
        let s = symbol_fit_scale_v2(
            nb.width(),
            nb.height(),
            target_h,
            max_draw_w,
            SYMBOL_MAX_UPSCALE,
        );
        let scaled = PxScale::from(pen.px * s);
        let sb = measure(scaled)?;
        // Centered ink top-left on the full cell box, both axes.
        let left = inner_x as f32 + (f.box_w - sb.width()) / 2.0;
        let top = inner_y as f32 + (f.box_h - sb.height()) / 2.0;
        Some(FitPlacement {
            scale: scaled,
            // Pre-subtract the measured bbox min so that a glyph re-outlined at
            // position `(shift_x, 0)` in the draw closure lands its ink top-left
            // exactly at `(left, top)`: `origin + bounds.min == (left, top)`
            // plus the per-channel subpixel `shift_x` carried in `bounds.min.x`.
            origin_x: left - sb.min.x,
            origin_y: top - sb.min.y,
        })
    });
    let x_lo = ox as i32 + ATLAS_PAD as i32;
    let x_hi = (ox + outer_w) as i32 - ATLAS_PAD as i32;
    let y_lo = oy as i32 + ATLAS_PAD as i32;
    let y_hi = (oy + slot_h(cell)) as i32 - ATLAS_PAD as i32;
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let mut draw_sample = |shift_x: f32, channel: Option<usize>| {
        // Fitted symbol glyphs re-outline at the scaled pen size on baseline 0
        // (the fit origin supplies absolute placement); text glyphs use the body
        // scale on `pen.baseline` exactly as before.
        let (use_scale, pos_y) = match fit_placement {
            Some(fp) => (fp.scale, 0.0),
            None => (scale, pen.baseline),
        };
        let glyph = glyph_id.with_scale_and_position(use_scale, point(anchor_x + shift_x, pos_y));
        let Some(outline) = font.outline_glyph(glyph) else {
            return;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let value = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
            if value == 0 {
                return; // uninked sample contributes no ink and no bounds
            }
            // Stem-darkening (RV5): boost partial coverage so light-on-dark
            // stems hold weight. Identity at the default strength of 0.0.
            let value = apply_stem_darken(value, stem);
            // Round to the nearest atlas pixel (truncation drifts edges and can
            // drop a glyph's final row/column). Fitted glyphs center on the cell
            // box via `fit_placement`; text glyphs sit on the cell baseline.
            let ay = match fit_placement {
                Some(fp) => (fp.origin_y + bounds.min.y + gy as f32).round() as i32,
                None => inner_y + (bounds.min.y + gy as f32).round() as i32,
            };
            if ay < y_lo || ay >= y_hi {
                return; // clip vertically (rows are unaffected by synthesis)
            }
            // Synthetic italic: shift this sample horizontally in proportion to
            // its height above the baseline. `pen.baseline` is the baseline in
            // the same absolute pixel space as `bounds`, so the unrounded glyph
            // y gives a smooth oblique. Rounding to whole atlas pixels introduces
            // minor stair-stepping along near-horizontal edges — acceptable for a
            // fallback face that exists only when no real italic is installed.
            // Synthesis is never combined with fit (icon faces use `synth`
            // identity), so the fitted branch needs no shear.
            let base_ax = match fit_placement {
                Some(fp) => (fp.origin_x + bounds.min.x + gx as f32).round() as i32,
                None => {
                    let shear_dx = if synth.shear != 0.0 {
                        let gy_abs = bounds.min.y + gy as f32;
                        (synth.shear * (pen.baseline - gy_abs)).round() as i32
                    } else {
                        0
                    };
                    inner_x + (bounds.min.x + gx as f32).round() as i32 + shear_dx
                }
            };
            // Synthetic bold: strike a second time shifted right by `embolden_px`
            // (max-combined by `write_coverage`), thickening horizontal weight
            // without touching verticals, the baseline, or the cell advance.
            let strikes = if synth.embolden_px > 0 { 2 } else { 1 };
            for s in 0..strikes {
                let ax = base_ax + s * synth.embolden_px as i32;
                if ax < x_lo || ax >= x_hi {
                    continue; // clip horizontally to the drawable region
                }
                write_coverage(data, width, subpixel, ax as u32, ay as u32, channel, value);
                min_x = min_x.min(ax);
                min_y = min_y.min(ay);
                max_x = max_x.max(ax);
                max_y = max_y.max(ay);
            }
        });
    };
    match subpixel {
        SubpixelMode::Off => draw_sample(0.0, None),
        SubpixelMode::Rgb => {
            draw_sample(-1.0 / 3.0, Some(0));
            draw_sample(0.0, Some(1));
            draw_sample(1.0 / 3.0, Some(2));
        }
        SubpixelMode::Bgr => {
            draw_sample(-1.0 / 3.0, Some(2));
            draw_sample(0.0, Some(1));
            draw_sample(1.0 / 3.0, Some(0));
        }
    }

    if subpixel.enabled() {
        lcd_filter_subpixel_region(
            data,
            width,
            subpixel,
            x_lo as u32,
            y_lo as u32,
            (x_hi - x_lo) as u32,
            (y_hi - y_lo) as u32,
        );
    }

    let Some((filtered_min_x, filtered_min_y, filtered_max_x, filtered_max_y)) =
        scan_coverage_bounds(data, width, subpixel, x_lo, y_lo, x_hi, y_hi)
    else {
        return None; // outline produced no inked pixels in the drawable region
    };
    min_x = filtered_min_x;
    min_y = filtered_min_y;
    max_x = filtered_max_x;
    max_y = filtered_max_y;

    Some(GlyphInk {
        offset_x: min_x - inner_x,
        offset_y: min_y - inner_y,
        width: (max_x - min_x + 1) as u32,
        height: (max_y - min_y + 1) as u32,
    })
}

fn scan_coverage_bounds(
    data: &[u8],
    width: u32,
    subpixel: SubpixelMode,
    x_lo: i32,
    y_lo: i32,
    x_hi: i32,
    y_hi: i32,
) -> Option<(i32, i32, i32, i32)> {
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for ay in y_lo..y_hi {
        for ax in x_lo..x_hi {
            let base =
                (ay as u32 * width + ax as u32) as usize * subpixel.bytes_per_pixel() as usize;
            let inked = match subpixel {
                SubpixelMode::Off => data[base] > 0,
                SubpixelMode::Rgb | SubpixelMode::Bgr => {
                    data[base..base + 3].iter().any(|&v| v > 0)
                }
            };
            if inked {
                min_x = min_x.min(ax);
                min_y = min_y.min(ay);
                max_x = max_x.max(ax);
                max_y = max_y.max(ay);
            }
        }
    }
    (max_x >= min_x).then_some((min_x, min_y, max_x, max_y))
}

const LCD_FILTER_TAPS: [u16; 5] = [1, 2, 3, 2, 1];
const LCD_FILTER_SUM: u16 = 9;

pub(super) fn lcd_filter_subpixel_region(
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    x0: u32,
    y0: u32,
    region_w: u32,
    region_h: u32,
) {
    if !subpixel.enabled() || region_w == 0 || region_h == 0 {
        return;
    }

    let samples_per_row = region_w as usize * 3;
    let mut src = vec![0u8; samples_per_row];
    let mut dst = vec![0u8; samples_per_row];
    for row in 0..region_h {
        let y = y0 + row;
        for dx in 0..region_w {
            let x = x0 + dx;
            let base = ((y * width + x) * 4) as usize;
            for physical in 0..3 {
                let channel = physical_channel(subpixel, physical);
                src[dx as usize * 3 + physical] = data[base + channel];
            }
        }

        for (i, dst_sample) in dst.iter_mut().enumerate().take(samples_per_row) {
            let mut weighted = 0u16;
            for (tap, weight) in LCD_FILTER_TAPS.iter().enumerate() {
                let source_i = i as isize + tap as isize - 2;
                if (0..samples_per_row as isize).contains(&source_i) {
                    weighted += src[source_i as usize] as u16 * weight;
                }
            }
            *dst_sample = ((weighted + LCD_FILTER_SUM / 2) / LCD_FILTER_SUM) as u8;
        }

        for dx in 0..region_w {
            let x = x0 + dx;
            let base = ((y * width + x) * 4) as usize;
            let mut any = false;
            for physical in 0..3 {
                let channel = physical_channel(subpixel, physical);
                let value = dst[dx as usize * 3 + physical];
                data[base + channel] = value;
                any |= value > 0;
            }
            data[base + 3] = if any { 255 } else { 0 };
        }
    }
}

fn physical_channel(subpixel: SubpixelMode, physical: usize) -> usize {
    match subpixel {
        SubpixelMode::Off => 0,
        SubpixelMode::Rgb => physical,
        SubpixelMode::Bgr => 2 - physical,
    }
}

/// Rasterize a geometric box-drawing / block / Powerline glyph into the slot
/// whose outer top-left is `region.origin` (RV2).
///
/// The coverage bitmap comes from [`crate::boxdraw::coverage`], computed at the
/// exact cell pixel size, and is written into the slot's inner cell region (no
/// overflow, no synthesis, no stem-darkening — the geometry is already crisp).
/// Achromatic coverage is replicated across all channels for subpixel atlases.
/// Returns the full-cell ink extent, or `None` if the codepoint is uncovered or
/// produced an empty bitmap (the caller then falls back to the cell box).
pub(super) fn rasterize_geometric(
    ch: char,
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    region: SlotRegion,
) -> Option<GlyphInk> {
    let SlotRegion { origin, cell, .. } = region;
    let (ox, oy) = origin;
    let border = slot_border(cell);
    let inner_x = ox + border;
    let inner_y = oy + border;
    let cov = crate::boxdraw::coverage(ch, cell.width, cell.height)?;
    let mut any = false;
    for cy in 0..cell.height {
        for cx in 0..cell.width {
            let value = cov[(cy * cell.width + cx) as usize];
            if value == 0 {
                continue;
            }
            any = true;
            let ax = inner_x + cx;
            let ay = inner_y + cy;
            match subpixel {
                SubpixelMode::Off => write_coverage(data, width, subpixel, ax, ay, None, value),
                SubpixelMode::Rgb | SubpixelMode::Bgr => {
                    for channel in 0..3 {
                        write_coverage(data, width, subpixel, ax, ay, Some(channel), value);
                    }
                }
            }
        }
    }
    if !any {
        return None;
    }
    Some(GlyphInk {
        offset_x: 0,
        offset_y: 0,
        width: cell.width,
        height: cell.height,
    })
}

fn write_coverage(
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    ax: u32,
    ay: u32,
    channel: Option<usize>,
    value: u8,
) {
    let base = (ay * width + ax) as usize * subpixel.bytes_per_pixel() as usize;
    match subpixel {
        SubpixelMode::Off => {
            if value > data[base] {
                data[base] = value;
            }
        }
        SubpixelMode::Rgb | SubpixelMode::Bgr => {
            let channel = channel.unwrap_or(1).min(2);
            if value > data[base + channel] {
                data[base + channel] = value;
            }
            data[base + 3] = 255;
        }
    }
}

/// Draw the synthesized missing-glyph fallback — a hollow rectangle inset from
/// the cell edges — into the atlas cell at `(ox, oy)`. Font-independent so the
/// fallback looks the same regardless of which font is loaded.
pub(super) fn draw_fallback_box(
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    ox: u32,
    oy: u32,
    cell: CellSize,
) {
    let cw = cell.width;
    let ch = cell.height;
    let mut set = |x: u32, y: u32| {
        if x < cw && y < ch {
            match subpixel {
                SubpixelMode::Off => {
                    data[((oy + y) * width + ox + x) as usize] = 255;
                }
                SubpixelMode::Rgb | SubpixelMode::Bgr => {
                    let idx = ((oy + y) * width + ox + x) as usize * 4;
                    data[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    };
    if cw < 3 || ch < 3 {
        // Degenerate cell: a single inked pixel is the best "visible" marker.
        set(cw / 2, ch / 2);
        return;
    }
    let inset_x = (cw / 6).max(1);
    let inset_y = (ch / 6).max(1);
    let thick = (cw / 12).max(1);
    let x0 = inset_x;
    let x1 = cw - inset_x - 1;
    let y0 = inset_y;
    let y1 = ch - inset_y - 1;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let on_v = x < x0 + thick || x + thick > x1;
            let on_h = y < y0 + thick || y + thick > y1;
            if on_v || on_h {
                set(x, y);
            }
        }
    }
}
