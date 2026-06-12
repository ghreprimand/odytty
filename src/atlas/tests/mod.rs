//! Glyph-atlas behavioral tests (M5 mechanical split from atlas.rs).
//! Shared helpers live here; tests are grouped by theme into sibling
//! files (metrics, geometry, glyph_quad, scaling).

use super::*;
use crate::text::load_font;

mod geometry;
mod glyph_quad;
mod metrics;
mod scaling;
mod synthetic;

pub(super) fn test_font() -> Option<FontVec> {
    load_font().ok()
}

/// The inner top-left pixel `(x, y)` of the cell a UV rect points at. The
/// inner origin is an integer pixel, so reconstructing it from the
/// normalized UV round-trips exactly.
pub(super) fn inner_origin(atlas: &GlyphAtlas, uv: [f32; 4]) -> (u32, u32) {
    (
        (uv[0] * atlas.width as f32).round() as u32,
        (uv[1] * atlas.height as f32).round() as u32,
    )
}

/// Sum the coverage bytes of the inner atlas cell a UV rect points at, in
/// the atlas's current pixel space.
pub(super) fn cell_ink(atlas: &GlyphAtlas, uv: [f32; 4]) -> u64 {
    let (cx, cy) = inner_origin(atlas, uv);
    let mut sum = 0u64;
    for y in cy..cy + atlas.cell.height {
        for x in cx..cx + atlas.cell.width {
            sum += atlas.data[(y * atlas.width + x) as usize] as u64;
        }
    }
    sum
}

pub(super) fn subpixel_cell_channels(atlas: &GlyphAtlas, uv: [f32; 4]) -> [u64; 4] {
    let (cx, cy) = inner_origin(atlas, uv);
    let mut sum = [0u64; 4];
    for y in cy..cy + atlas.cell.height {
        for x in cx..cx + atlas.cell.width {
            let idx = ((y * atlas.width + x) * 4) as usize;
            for c in 0..4 {
                sum[c] += atlas.data[idx + c] as u64;
            }
        }
    }
    sum
}

/// A non-ASCII codepoint the loaded font actually has an outline for, used
/// to exercise the dynamic region. `None` if none is found (unusual).
pub(super) fn glyph_bearing_non_ascii(font: &FontVec) -> Option<char> {
    (0x00A1u32..=0x05FF)
        .filter_map(char::from_u32)
        .find(|&ch| font_has_glyph(font, ch))
}

/// Scan a slot's full drawable region for the tight bounding box of inked
/// pixels, returning `(min_x, min_y, max_x, max_y)` in absolute atlas pixels.
pub(super) fn scan_slot_ink(atlas: &GlyphAtlas, slot: u32) -> Option<(i32, i32, i32, i32)> {
    let (ox, oy) = slot_offset(slot, atlas.cols, atlas.cell);
    let (sw, sh) = (slot_w(atlas.cell), slot_h(atlas.cell));
    let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in oy..oy + sh {
        for x in ox..ox + sw {
            if atlas.data[(y * atlas.width + x) as usize] > 0 {
                minx = minx.min(x as i32);
                miny = miny.min(y as i32);
                maxx = maxx.max(x as i32);
                maxy = maxy.max(y as i32);
            }
        }
    }
    (maxx >= minx).then_some((minx, miny, maxx, maxy))
}

// ----- W1: wide-glyph (East Asian width-2) atlas support -----

/// A width-2 codepoint the loaded font actually has an outline for. `None`
/// on hosts without a CJK/fullwidth-capable font (the common case here), so
/// dependent tests skip rather than fail.
pub(super) fn wide_glyph_supported(font: &FontVec) -> Option<char> {
    // CJK ideographs, hiragana/katakana, and fullwidth ASCII forms.
    let ranges = [
        0x4E00u32..=0x4F00, // CJK unified
        0x3040..=0x30FF,    // kana
        0xFF01..=0xFF60,    // fullwidth forms
    ];
    ranges
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
        .find(|&ch| glyph_cells(ch) == 2 && font_has_glyph(font, ch))
}

// ----- H3: fractional-scale UV/quad bookkeeping -----

/// Inline replica of `gpu::physical_font_px` — the real function is
/// `pub(super)` inside `native::gpu` and not re-exported. The atlas tests
/// need it purely for constructing the scale matrix; keeping it duplicated
/// here avoids widening module visibility for test-only use.
pub(super) fn physical_font_px(font_size_px: f32, scale: f32) -> f32 {
    (font_size_px * scale.max(1.0)).max(1.0)
}
