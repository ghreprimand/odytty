// SPDX-License-Identifier: GPL-3.0-only
use super::*;

fn ink(atlas: &GlyphAtlas, q: GlyphBounds) -> Vec<u8> {
    let (x, y) = inner_origin(atlas, q.uv);
    let bpp = atlas.subpixel.bytes_per_pixel() as usize;
    let mut pixels = Vec::new();
    for yy in y..y + q.height {
        for xx in x..x + q.width {
            let offset = (yy as usize * atlas.width as usize + xx as usize) * bpp;
            pixels.extend_from_slice(&atlas.data[offset..offset + bpp]);
        }
    }
    pixels
}

#[test]
fn positive_advance_mark_keeps_complete_owner_ink() {
    let _render = crate::test_lock::render_globals_lock();
    let font = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/positive-advance-marks.ttf").to_vec(),
    )
    .expect("authored fixture");
    for px in [16.0, 20.0, 24.0, 48.0] {
        let scaled = font.as_scaled(px.into());
        assert!(scaled.h_advance(font.glyph_id('\u{0301}')) > 0.0);
        assert_eq!(scaled.h_advance(font.glyph_id('\u{0300}')), 0.0);
        for mode in [SubpixelMode::Off, SubpixelMode::Rgb, SubpixelMode::Bgr] {
            let mut atlas = GlyphAtlas::build_with_subpixel(&font, px, mode);
            atlas.set_synthetic_styles(true, true, true);
            for style in [
                FontStyle::Regular,
                FontStyle::Bold,
                FontStyle::Italic,
                FontStyle::BoldItalic,
            ] {
                for mark in ['\u{0301}', '\u{0300}'] {
                    atlas
                        .ensure_styled(&font, style, mark)
                        .expect("mapped mark");
                }
                let positive = atlas
                    .combining_mark_quad(style, '\u{0301}')
                    .expect("positive mark");
                let zero = atlas
                    .combining_mark_quad(style, '\u{0300}')
                    .expect("zero mark");
                assert_eq!(
                    (
                        positive.offset_x,
                        positive.offset_y,
                        positive.width,
                        positive.height
                    ),
                    (zero.offset_x, zero.offset_y, zero.width, zero.height),
                    "{px} {mode:?} {style:?}: equivalent mark outlines keep complete ink over their owner"
                );
                assert_eq!(
                    ink(&atlas, positive),
                    ink(&atlas, zero),
                    "{px} {mode:?} {style:?}: no slot clipping"
                );
            }
        }
    }
}
