// SPDX-License-Identifier: GPL-3.0-only
//! Raster-policy and metric probes applied to an already-loaded face.
//!
//! Both questions here are answered by measuring the loaded face's own glyph
//! outlines and advances rather than by reading metadata, because what matters
//! to the atlas is what the face will actually draw and advance.

use super::FontHandle;
use super::glyph_geom::PxScale;

/// Whether `font` provides a usable **monochrome outline** for `ch`: it has the
/// codepoint in its cmap (`glyph_id != 0`) and an inked vector outline. This is
/// the symbol-fallback face filter: color/bitmap-only faces and blank
/// placeholder outlines both render nothing useful in the coverage atlas, so
/// they must not block a later fallback face.
pub fn font_provides_outline_glyph(font: &FontHandle, ch: char) -> bool {
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
/// proportional font (where, e.g., `i` is narrower than `M`) is rejected. Other
/// probe glyphs the font lacks are skipped, but `M` must resolve: the atlas
/// measures the cell width from `M`, so a face without it (a script-only face
/// whose only probe hit is a period) would size cells from `.notdef` and pass
/// on a single probe while its letters advance at many widths.
pub fn is_monospace(font: &FontHandle) -> bool {
    if font.glyph_id('M').0 == 0 {
        return false;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{FontResolveError, try_resolve_font_family};
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fonts")
            .join(name)
    }

    /// A script-only face whose single probe hit is a period, with letters at
    /// several advances, is proportional. It must not pass the probe on that
    /// one glyph and then size cells from `.notdef`.
    #[test]
    fn latinless_proportional_face_is_not_monospace() {
        let path = fixture("latinless-proportional.ttf");
        let font = FontHandle::try_from_vec(std::fs::read(&path).expect("read fixture"))
            .expect("parse fixture");
        assert_ne!(font.glyph_id('.').0, 0, "fixture maps the period probe");
        assert_eq!(font.glyph_id('M').0, 0, "fixture has no M");
        assert!(!is_monospace(&font), "a face without M is not monospace");
        assert_eq!(
            try_resolve_font_family(path.to_str().expect("utf-8 path"), &[]),
            Err(FontResolveError::NotMonospace),
            "a direct path to the face reports NotMonospace"
        );
    }
}
