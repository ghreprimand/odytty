// SPDX-License-Identifier: GPL-3.0-only
//! Coverage bounds for project-authored glyphs and degenerate font metrics.

use super::*;

fn fixture_bytes() -> Vec<u8> {
    include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec()
}

fn set_table(bytes: &mut [u8], tag: &[u8; 4], edit: impl FnOnce(&mut [u8])) {
    let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
    let record = (0..count)
        .map(|i| 12 + i * 16)
        .find(|&at| &bytes[at..at + 4] == tag)
        .expect("fixture table");
    let offset = u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
    let length = u32::from_be_bytes(bytes[record + 12..record + 16].try_into().unwrap()) as usize;
    edit(&mut bytes[offset..offset + length]);
}

#[test]
fn degenerate_vertical_metrics_cannot_allocate_an_enormous_coverage_raster() {
    let mut bytes = fixture_bytes();
    set_table(&mut bytes, b"hhea", |table| {
        table[4..6].copy_from_slice(&1i16.to_be_bytes());
        table[6..10].fill(0);
    });
    set_table(&mut bytes, b"OS/2", |table| {
        table[62..64].copy_from_slice(&128u16.to_be_bytes());
        table[68..70].copy_from_slice(&1i16.to_be_bytes());
        table[70..74].fill(0);
        table[74..76].copy_from_slice(&1u16.to_be_bytes());
        table[76..78].fill(0);
    });
    let font = FontHandle::try_from_vec(bytes).expect("parse degenerate metrics fixture");
    assert_eq!(font.px_per_unit(20.0), 20.0, "one-unit vertical metrics");
    assert!(
        font.outline_glyph(font.glyph_id('M').with_scale(20.0))
            .is_none()
    );
}

#[test]
fn outline_refuses_non_finite_and_non_positive_scales_and_positions() {
    let font = FontHandle::try_from_vec(fixture_bytes()).expect("parse fixture");
    let id = font.glyph_id('M');
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -20.0] {
        for scale in [PxScale { x: value, y: 20.0 }, PxScale { x: 20.0, y: value }] {
            assert!(
                font.outline_glyph(id.with_scale(scale)).is_none(),
                "refuse invalid scale {scale:?}"
            );
        }
    }
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for position in [point(value, 0.0), point(0.0, value)] {
            assert!(
                font.outline_glyph(id.with_scale_and_position(20.0, position))
                    .is_none()
            );
        }
    }
}

#[test]
fn outline_refuses_a_finite_raster_above_the_axis_bound() {
    let font = FontHandle::try_from_vec(fixture_bytes()).expect("parse fixture");
    assert!(
        font.outline_glyph(font.glyph_id('M').with_scale(100_000.0))
            .is_none()
    );
}

#[test]
fn draw_rechecks_bounds_before_calling_the_scan_converter() {
    let outline = OutlinedGlyph {
        curves: Vec::new(),
        sf_h: 1.0,
        sf_v: 1.0,
        position: point(0.0, 0.0),
        px_bounds: Rect {
            min: point(0.0, 0.0),
            max: point(4097.0, 1.0),
        },
    };
    let mut pixels = 0;
    outline.draw(|_, _, _| pixels += 1);
    assert_eq!(pixels, 0, "oversized bounds never reach raster allocation");
}

#[test]
fn ordinary_finite_glyphs_still_draw_coverage() {
    let font = FontHandle::try_from_vec(fixture_bytes()).expect("parse fixture");
    let outline = font
        .outline_glyph(
            font.glyph_id('M')
                .with_scale_and_position(20.0, point(0.25, 0.5)),
        )
        .expect("ordinary outline");
    let bounds = outline.px_bounds();
    assert!(bounds.width() > 0.0 && bounds.width() < 100.0);
    assert!(bounds.height() > 0.0 && bounds.height() < 100.0);
    let mut ink = 0;
    outline.draw(|_, _, coverage| ink += usize::from(coverage > 0.0));
    assert!(ink > 0);
}
