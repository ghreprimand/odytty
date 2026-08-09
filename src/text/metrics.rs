// SPDX-License-Identifier: GPL-3.0-only
//! Raster-policy and metric probes applied to an already-loaded face.
//!
//! Both questions here are answered by rasterizing or measuring through
//! `ab_glyph` rather than by reading metadata, because what matters to the
//! atlas is what the face will actually draw and advance.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};

/// Whether `font` provides a usable **monochrome outline** for `ch`: it has the
/// codepoint in its cmap (`glyph_id != 0`) and an inked vector outline. This is
/// the symbol-fallback face filter: color/bitmap-only faces and blank
/// placeholder outlines both render nothing useful in the coverage atlas, so
/// they must not block a later fallback face.
pub fn font_provides_outline_glyph(font: &FontVec, ch: char) -> bool {
    let id = font.glyph_id(ch);
    id.0 != 0
        && font.outline(id).is_some_and(|outline| {
            !outline.curves.is_empty()
                && outline.bounds.min.x != outline.bounds.max.x
                && outline.bounds.min.y != outline.bounds.max.y
        })
}

/// Whether a font's representative glyphs share one advance width (monospace).
///
/// Compares the horizontal advance of several probe glyphs at a fixed scale; a
/// proportional font (where, e.g., `i` is narrower than `M`) is rejected. Glyphs
/// the font lacks are skipped; at least one probe must resolve.
pub fn is_monospace(font: &FontVec) -> bool {
    let scaled = font.as_scaled(PxScale::from(64.0));
    let probe = ['i', 'l', '.', 'M', 'W', 'm', 'x', '@'];
    let mut advance: Option<f32> = None;
    for ch in probe {
        let id = font.glyph_id(ch);
        if id.0 == 0 {
            continue; // font lacks this probe glyph
        }
        let a = scaled.h_advance(id);
        if a <= 0.0 {
            return false;
        }
        match advance {
            None => advance = Some(a),
            // Allow a sub-pixel tolerance for hinting/rounding noise.
            Some(prev) if (prev - a).abs() > 0.5 => return false,
            Some(_) => {}
        }
    }
    advance.is_some()
}
