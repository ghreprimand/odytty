// SPDX-License-Identifier: GPL-3.0-only
//! Alternate-zero control (`font_zero`): the body face's OpenType `zero`
//! feature swaps only the `0` glyph, never the cell metrics.
//!
//! Fixtures are the bundled OFL faces: JetBrains Mono carries a `zero` lookup
//! (`zero` -> `zero.zero`), Victor Mono carries none.

use super::*;
use crate::text::{BUNDLED_FONT_FAMILY, FontStyle, JETBRAINS_FONT_FAMILY, load_bundled_style_for};

const STYLES: [FontStyle; 4] = [
    FontStyle::Regular,
    FontStyle::Bold,
    FontStyle::Italic,
    FontStyle::BoldItalic,
];

fn face(family: &str, style: FontStyle) -> FontHandle {
    load_bundled_style_for(family, style).expect("bundled face loads")
}

/// The full slot (gutter included) of a printable-ASCII glyph in the prebuilt
/// Regular block.
fn ascii_slot(atlas: &GlyphAtlas, ch: char) -> Vec<u8> {
    let slot = ch as u32 - FIRST_CHAR + 1;
    let (ox, oy) = slot_offset(slot, atlas.cols, atlas.cell);
    let bpp = atlas.subpixel.bytes_per_pixel();
    let mut out = Vec::new();
    for y in oy..oy + slot_h(atlas.cell) {
        let row = ((y * atlas.width + ox) * bpp) as usize;
        out.extend_from_slice(&atlas.data[row..row + (slot_w(atlas.cell) * bpp) as usize]);
    }
    out
}

/// The inner cell pixels a UV rect points at (grayscale atlas).
fn cell_pixels(atlas: &GlyphAtlas, uv: [f32; 4]) -> Vec<u8> {
    let (cx, cy) = inner_origin(atlas, uv);
    let mut out = Vec::new();
    for y in cy..cy + atlas.cell.height {
        let row = (y * atlas.width + cx) as usize;
        out.extend_from_slice(&atlas.data[row..row + atlas.cell.width as usize]);
    }
    out
}

#[test]
fn zero_feature_remaps_only_the_zero_glyph() {
    for style in STYLES {
        let off = face(JETBRAINS_FONT_FAMILY, style);
        let on = off.clone().with_zero_feature(true);
        assert!(on.has_zero_alternate(), "{style:?} face carries `zero`");
        let alternate = on.glyph_id('0');
        assert_ne!(alternate, off.glyph_id('0'), "{style:?}: `0` is remapped");
        assert_ne!(alternate.0, 0, "{style:?}: the alternate is a real glyph");
        assert!(
            crate::text::font_provides_outline_glyph(&on, '0'),
            "{style:?}: the alternate has an inked outline"
        );
        for code in FIRST_CHAR..=LAST_CHAR {
            let ch = char::from_u32(code).unwrap();
            if ch != '0' {
                assert_eq!(on.glyph_id(ch), off.glyph_id(ch), "{style:?} {ch:?}");
            }
        }
        let cleared = on.with_zero_feature(false);
        assert!(!cleared.has_zero_alternate());
        assert_eq!(cleared.glyph_id('0'), off.glyph_id('0'));
    }
}

#[test]
fn zero_feature_preserves_cell_width_height_and_baseline() {
    for family in [JETBRAINS_FONT_FAMILY, BUNDLED_FONT_FAMILY] {
        let off = face(family, FontStyle::Regular);
        let on = off.clone().with_zero_feature(true);
        for px in [9.0, 13.0, 16.0, 21.5, 28.0, 48.0] {
            for line_height in [1.0, 1.25] {
                for subpixel in [SubpixelMode::Off, SubpixelMode::Rgb] {
                    let a = GlyphAtlas::build_with_options(&off, px, subpixel, line_height);
                    let b = GlyphAtlas::build_with_options(&on, px, subpixel, line_height);
                    assert_eq!(
                        a.cell, b.cell,
                        "{family} {px}px x{line_height} {subpixel:?}"
                    );
                    assert_eq!((a.width, a.height), (b.width, b.height));
                }
            }
        }
    }
}

#[test]
fn zero_feature_changes_only_the_zero_slot_pixels() {
    let off = face(JETBRAINS_FONT_FAMILY, FontStyle::Regular);
    let on = off.clone().with_zero_feature(true);
    for subpixel in [SubpixelMode::Off, SubpixelMode::Rgb] {
        let a = GlyphAtlas::build_with_subpixel(&off, 28.0, subpixel);
        let b = GlyphAtlas::build_with_subpixel(&on, 28.0, subpixel);
        for code in FIRST_CHAR..=LAST_CHAR {
            let ch = char::from_u32(code).unwrap();
            let (sa, sb) = (ascii_slot(&a, ch), ascii_slot(&b, ch));
            if ch == '0' {
                assert_ne!(sa, sb, "{subpixel:?}: the `0` slot draws the alternate");
                assert!(sb.iter().any(|&v| v > 0), "the alternate is inked");
            } else {
                assert_eq!(sa, sb, "{subpixel:?}: {ch:?} is unchanged");
            }
        }
        // The fallback box (slot 0) and every other byte are identical too.
        let zero = ascii_slot(&a, '0').len();
        let differing = a.data.iter().zip(&b.data).filter(|(x, y)| x != y).count();
        assert!(differing > 0 && differing <= zero);
    }
}

#[test]
fn zero_feature_reaches_styled_faces_through_the_dynamic_region() {
    for style in [FontStyle::Bold, FontStyle::Italic, FontStyle::BoldItalic] {
        let off = face(JETBRAINS_FONT_FAMILY, style);
        let on = off.clone().with_zero_feature(true);
        let regular = face(JETBRAINS_FONT_FAMILY, FontStyle::Regular);
        let mut a = GlyphAtlas::build(&regular, 28.0);
        let mut b = GlyphAtlas::build(&regular, 28.0);
        let ua = a.ensure_styled(&off, style, '0').expect("styled 0");
        let ub = b.ensure_styled(&on, style, '0').expect("styled 0");
        assert_ne!(cell_pixels(&a, ua), cell_pixels(&b, ub), "{style:?}");
        let ua = a.ensure_styled(&off, style, '8').expect("styled 8");
        let ub = b.ensure_styled(&on, style, '8').expect("styled 8");
        assert_eq!(cell_pixels(&a, ua), cell_pixels(&b, ub), "{style:?} 8");
    }
}

#[test]
fn face_without_a_zero_lookup_renders_byte_identically() {
    for style in STYLES {
        let off = face(BUNDLED_FONT_FAMILY, style);
        let on = off.clone().with_zero_feature(true);
        assert!(
            !on.has_zero_alternate(),
            "{style:?}: Victor Mono has no `zero`"
        );
        assert_eq!(on.glyph_id('0'), off.glyph_id('0'));
        let a = GlyphAtlas::build(&off, 28.0);
        let b = GlyphAtlas::build(&on, 28.0);
        assert_eq!(a.cell, b.cell);
        assert!(a.data == b.data, "{style:?}: atlas bytes are identical");
    }
}

#[test]
fn clearing_the_control_restores_the_default_atlas_bytes() {
    let off = face(JETBRAINS_FONT_FAMILY, FontStyle::Regular);
    let round_trip = off.clone().with_zero_feature(true).with_zero_feature(false);
    let a = GlyphAtlas::build(&off, 28.0);
    let b = GlyphAtlas::build(&round_trip, 28.0);
    assert!(a.data == b.data);
}

#[test]
fn faces_without_a_zero_lookup_or_zero_glyph_keep_the_cmap() {
    // The bidi fixture face has a GSUB without `zero`; the symbol face has
    // neither a `0` nor a GSUB.
    for bytes in [
        include_bytes!("../../../tests/fixtures/fonts/bidi-mixed.ttf").as_slice(),
        include_bytes!("../../../tests/fixtures/fonts/symbol-markers-inked.ttf").as_slice(),
    ] {
        let off = FontHandle::try_from_vec(bytes.to_vec()).expect("fixture parses");
        let on = off.clone().with_zero_feature(true);
        assert!(!on.has_zero_alternate());
        assert_eq!(on.glyph_id('0'), off.glyph_id('0'));
    }
}
