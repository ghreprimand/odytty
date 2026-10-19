// SPDX-License-Identifier: GPL-3.0-only
//! SYMMAP per-codepoint-range font-override behavior at the atlas seam.
//!
//! The gate-critical guarantee is **off-path identity**: with no override map
//! installed (the default), `symbol_map_font_for` returns `None`, the chosen
//! raster face stays the primary font, and every glyph path is byte-for-byte
//! the no-SYMMAP renderer. When an override range is installed it takes
//! priority over geometric box-drawing and the symbol fallback, with
//! first-match-wins precedence matching `text::SymbolMap`.

use super::fallback::{SYMBOL_ICON, marker_blank_font, symbol_bar_font, symbol_block_font};
use super::*;
use std::sync::Arc;

#[test]
fn empty_symbol_map_font_for_returns_none() {
    let font = test_font();
    let atlas = GlyphAtlas::build(&font, 24.0);
    // Default state: no override map → every lookup is the identity (None).
    assert!(atlas.symbol_map_font_for('A').is_none());
    assert!(atlas.symbol_map_font_for('\u{2500}').is_none());
    assert!(atlas.symbol_map_font_for('\u{E000}').is_none());
}

#[test]
fn explicit_empty_map_is_identity() {
    let font = test_font();
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas.set_symbol_map_fonts(Vec::new());
    assert!(atlas.symbol_map_font_for('\u{2500}').is_none());
}

#[test]
fn symbol_map_font_for_matches_range_and_first_wins() {
    let font = test_font();
    // Reload distinct instances so Arc identity tells the two override faces
    // apart (each Arc wraps a separately loaded handle).
    let (face_a, face_b, face_c) = (test_font(), test_font(), test_font());
    let a = Arc::new(face_a);
    let b = Arc::new(face_b);
    let c = Arc::new(face_c);
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas.set_symbol_map_fonts(vec![
        (0x2500, 0x257F, Arc::clone(&a)),
        (0xE000, 0xF8FF, Arc::clone(&b)),
        // Overlaps the first range; first-match-wins means it never shadows it.
        (0x2500, 0x2500, Arc::clone(&c)),
    ]);

    // In-range lookups resolve to the first matching face.
    let got = atlas
        .symbol_map_font_for('\u{2500}')
        .expect("range start matches");
    assert!(
        Arc::ptr_eq(&got, &a),
        "first matching rule wins over a later overlap"
    );
    let got = atlas
        .symbol_map_font_for('\u{257F}')
        .expect("range end matches (inclusive)");
    assert!(Arc::ptr_eq(&got, &a));
    let got = atlas
        .symbol_map_font_for('\u{E000}')
        .expect("second range matches");
    assert!(Arc::ptr_eq(&got, &b));

    // Out-of-range codepoints are identity (None).
    assert!(atlas.symbol_map_font_for('\u{2480}').is_none());
    assert!(atlas.symbol_map_font_for('A').is_none());
    assert!(atlas.symbol_map_font_for('\u{F900}').is_none());
}

#[test]
fn symbol_map_font_for_wins_over_installed_fallback() {
    let font = test_font();
    let (fallback_face, override_face) = (test_font(), test_font());
    let fallback = Arc::new(fallback_face);
    let override_font = Arc::new(override_face);
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas.set_fallback_fonts(vec![Arc::clone(&fallback)]);
    atlas.set_symbol_map_fonts(vec![(0xE000, 0xE000, Arc::clone(&override_font))]);

    let got = atlas
        .symbol_map_font_for('\u{E000}')
        .expect("SYMMAP range should match");
    assert!(
        Arc::ptr_eq(&got, &override_font),
        "SYMMAP must remain the first glyph-source decision when fallback is installed"
    );
}

/// The face that draws a codepoint when both an installed fallback and a
/// SYMMAP range cover it is the override, whichever face is which. The two
/// fixture faces map the same private-use codepoint with different ink, so a
/// precedence change in `ensure_styled` changes the ink and fails here; the
/// lookup-only test above cannot see it.
#[test]
fn symbol_map_override_draws_ahead_of_the_installed_fallback_glyph() {
    // Coverage magnitude is compared across atlases rasterized at different
    // moments, so hold the guard for the process-global stem-darkening gain.
    let _guard = crate::test_lock::render_globals_lock();
    let primary = marker_blank_font();
    let ink = |fallback: Option<FontHandle>, override_face: Option<FontHandle>| {
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        if let Some(face) = fallback {
            atlas.set_fallback_fonts(vec![Arc::new(face)]);
        }
        if let Some(face) = override_face {
            let code = SYMBOL_ICON as u32;
            atlas.set_symbol_map_fonts(vec![(code, code, Arc::new(face))]);
        }
        let uv = atlas.ensure(&primary, SYMBOL_ICON).expect("icon uv");
        assert_ne!(
            uv,
            atlas.slot_uv(FALLBACK_SLOT),
            "the icon must draw a glyph"
        );
        cell_ink(&atlas, uv)
    };
    let block_alone = ink(Some(symbol_block_font()), None);
    let bar_alone = ink(Some(symbol_bar_font()), None);
    assert!(
        block_alone > bar_alone * 2,
        "the fixture faces must leave clearly different ink ({block_alone} vs {bar_alone})"
    );
    assert_eq!(
        ink(Some(symbol_block_font()), Some(symbol_bar_font())),
        bar_alone,
        "the SYMMAP face must draw, not the installed fallback"
    );
    assert_eq!(
        ink(Some(symbol_bar_font()), Some(symbol_block_font())),
        block_alone,
        "the SYMMAP face must draw whichever face the fallback holds"
    );
}

#[test]
fn empty_map_ensure_styled_is_byte_identical() {
    // Coverage MAGNITUDE, not presence: the two atlases below are rasterized at
    // different moments and their ink sums are compared for equality.
    // Rasterization reads the process-global stem-darkening gain, so a gain
    // change landing between the two passes diverges the sums. Hold the shared
    // render-globals guard across both passes.
    let _guard = crate::test_lock::render_globals_lock();
    let font = test_font();
    let ch =
        glyph_bearing_non_ascii(&font).expect("bundled face: font has no usable non-ASCII glyph");
    // Baseline atlas with no override map.
    let mut base = GlyphAtlas::build(&font, 24.0);
    let base_uv = base.ensure(&font, ch).expect("baseline glyph");
    let base_slots = base.slot_count();
    let base_ink = cell_ink(&base, base_uv);

    // Atlas with an explicitly-installed empty override map: must match exactly.
    let mut empty = GlyphAtlas::build(&font, 24.0);
    empty.set_symbol_map_fonts(Vec::new());
    let empty_uv = empty.ensure(&font, ch).expect("identity glyph");
    assert_eq!(empty_uv, base_uv, "empty map must not move the slot");
    assert_eq!(
        empty.slot_count(),
        base_slots,
        "empty map must not change slot use"
    );
    assert_eq!(
        cell_ink(&empty, empty_uv),
        base_ink,
        "empty map must not change ink"
    );
}

#[test]
fn override_renders_a_real_glyph_from_the_override_face() {
    let font = test_font();
    let ch =
        glyph_bearing_non_ascii(&font).expect("bundled face: font has no usable non-ASCII glyph");
    let override_face = test_font();
    let cp = ch as u32;
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas.set_symbol_map_fonts(vec![(cp, cp, Arc::new(override_face))]);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let uv = atlas.ensure(&font, ch).expect("override glyph uv");
    // The override face has the glyph (it is the same family), so the atlas
    // rasterizes a real glyph into a fresh slot rather than the hollow box.
    assert_ne!(
        uv, box_uv,
        "override glyph must not degrade to the hollow box"
    );
    assert!(cell_ink(&atlas, uv) > 0, "override glyph must have ink");
}

#[test]
fn override_suppresses_geometric_for_a_box_drawing_codepoint() {
    let font = test_font();
    // A box-drawing codepoint that the geometric path recognizes.
    let box_ch = '\u{2500}'; // BOX DRAWINGS LIGHT HORIZONTAL
    assert!(crate::boxdraw::covers(box_ch));
    let override_face = test_font();
    let cp = box_ch as u32;
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas.set_geometric_boxdraw(true);
    atlas.set_symbol_map_fonts(vec![(cp, cp, Arc::new(override_face))]);
    // The override matches the box-drawing codepoint, so geometric rendering is
    // suppressed: the lookup that drives that guard returns Some.
    assert!(
        atlas.symbol_map_font_for(box_ch).is_some(),
        "override must shadow the geometric box-drawing path"
    );
    // The override face draws the glyph, so its pixels differ from the
    // geometric rendering of the same codepoint.
    let _guard = crate::test_lock::render_globals_lock();
    let override_uv = atlas.ensure(&font, box_ch).expect("override box glyph uv");
    let mut geometric = GlyphAtlas::build(&font, 24.0);
    geometric.set_geometric_boxdraw(true);
    let geometric_uv = geometric.ensure(&font, box_ch).expect("geometric uv");
    assert_ne!(
        cell_bytes(&atlas, override_uv),
        cell_bytes(&geometric, geometric_uv),
        "the override glyph must replace the geometric drawing"
    );
}

/// The inner cell bytes a UV rect points at (grayscale atlas).
fn cell_bytes(atlas: &GlyphAtlas, uv: [f32; 4]) -> Vec<u8> {
    let (cx, cy) = inner_origin(atlas, uv);
    let mut out = Vec::new();
    for y in cy..cy + atlas.cell.height {
        let row = (y * atlas.width + cx) as usize;
        out.extend_from_slice(&atlas.data[row..row + atlas.cell.width as usize]);
    }
    out
}
