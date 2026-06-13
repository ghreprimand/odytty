//! RV6 symbol / Nerd-font fallback behavior at the atlas seam.
//!
//! The gate-critical guarantee is **default-safe**: with no fallback font
//! installed, a printable codepoint the primary lacks renders the historical
//! hollow box and consumes no slot — byte-for-byte the pre-feature path. When a
//! fallback is installed it is only used for PUA symbol codepoints the fallback
//! actually covers.

use super::*;
use crate::atlas::fallback::is_symbol_codepoint;
use std::sync::Arc;

/// A PUA codepoint the bundled/system test font does *not* map (so it exercises
/// the missing-glyph path), plus one it *does* — discovered at runtime so the
/// tests are not pinned to a single host font.
fn pua_absent(font: &FontVec) -> Option<char> {
    (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| !font_has_glyph(font, ch))
}

fn pua_present(font: &FontVec) -> Option<char> {
    (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| font_has_glyph(font, ch))
}

#[test]
fn pua_missing_glyph_without_fallback_uses_box() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(absent) = pua_absent(&font) else {
        eprintln!("skipping: font covers all of the BMP PUA");
        return;
    };
    assert!(is_symbol_codepoint(absent));
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    // No fallback installed: the missing PUA glyph must take the hollow box and
    // consume no slot — identical to the pre-RV6 renderer.
    let uv = atlas.ensure(&font, absent).expect("fallback uv");
    assert_eq!(uv, box_uv, "missing PUA glyph must use the hollow box");
    assert_eq!(
        atlas.slot_count(),
        count,
        "fallback must not consume a slot"
    );
    assert!(!atlas.take_dirty(), "no pixels changed, so not dirty");
}

#[test]
fn fallback_present_but_lacking_glyph_uses_box() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(absent) = pua_absent(&font) else {
        eprintln!("skipping: font covers all of the BMP PUA");
        return;
    };
    // A second instance of the same font as the "fallback" (FontVec is not
    // Clone, so reload it): it also lacks `absent`, so the fallback must decline
    // and the hollow box is used — proving the atlas verifies fallback coverage
    // rather than blindly drawing.
    let Some(fb) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas.set_fallback_font(Some(Arc::new(fb)));
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    let uv = atlas.ensure(&font, absent).expect("fallback uv");
    assert_eq!(uv, box_uv);
    assert_eq!(
        atlas.slot_count(),
        count,
        "fallback must not consume a slot"
    );
}

#[test]
fn primary_covered_pua_glyph_renders_from_primary() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(present) = pua_present(&font) else {
        eprintln!("skipping: font has no BMP PUA glyph");
        return;
    };
    assert!(is_symbol_codepoint(present));
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    // A PUA codepoint the primary font *does* cover must still render from the
    // primary (classification never hijacks a covered glyph), allocating a real
    // slot distinct from the hollow box.
    let uv = atlas.ensure(&font, present).expect("real glyph uv");
    assert_ne!(uv, box_uv, "covered PUA glyph must not use the hollow box");
    assert!(
        atlas.slot_count() > count,
        "covered PUA glyph must allocate a real slot"
    );
}

#[test]
fn fallback_renders_pua_glyph_when_a_symbol_font_is_available() {
    let Some(primary) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(symbol) = crate::text::resolve_symbol_font() else {
        eprintln!("skipping: no symbol / Nerd font on this host");
        return;
    };
    // A PUA codepoint the symbol font has but the primary lacks: the canonical
    // RV6 case (a prompt icon the body font cannot draw).
    let Some(icon) = (0xE000u32..=0xF8FF).filter_map(char::from_u32).find(|&ch| {
        is_symbol_codepoint(ch) && font_has_glyph(&symbol, ch) && !font_has_glyph(&primary, ch)
    }) else {
        eprintln!("skipping: no PUA codepoint unique to the symbol font");
        return;
    };
    let mut atlas = GlyphAtlas::build(&primary, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    atlas.set_fallback_font(Some(Arc::new(symbol)));
    let uv = atlas.ensure(&primary, icon).expect("fallback glyph uv");
    assert_ne!(uv, box_uv, "fallback glyph must not use the hollow box");
    assert!(
        atlas.slot_count() > count,
        "fallback glyph must allocate a real slot"
    );
    assert!(
        cell_ink(&atlas, uv) > 0,
        "fallback glyph should rasterize visible ink"
    );
}
