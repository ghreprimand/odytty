// SPDX-License-Identifier: GPL-3.0-only
//! Preallocation checks use project-authored font data and never allocate hostile geometry.

use super::*;

fn font() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec(),
    )
    .expect("project-authored font fixture")
}

fn edit_table(bytes: &mut [u8], tag: &[u8; 4], edit: impl FnOnce(&mut [u8])) {
    let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
    let at = (0..count)
        .map(|i| 12 + i * 16)
        .find(|&at| &bytes[at..at + 4] == tag)
        .expect("fixture table");
    let offset = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
    let len = u32::from_be_bytes(bytes[at + 12..at + 16].try_into().unwrap()) as usize;
    edit(&mut bytes[offset..offset + len]);
}

#[test]
fn hostile_advance_is_bounded_before_bitmap_allocation() {
    let mut bytes = include_bytes!("../../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec();
    edit_table(&mut bytes, b"hhea", |t| {
        t[4..6].copy_from_slice(&1i16.to_be_bytes());
        t[6..10].fill(0);
    });
    edit_table(&mut bytes, b"OS/2", |t| {
        t[62..64].copy_from_slice(&128u16.to_be_bytes());
        t[68..70].copy_from_slice(&1i16.to_be_bytes());
        t[70..74].fill(0);
        t[74..76].copy_from_slice(&1u16.to_be_bytes());
        t[76..78].fill(0);
    });
    edit_table(&mut bytes, b"hmtx", |t| {
        for metric in t.as_chunks_mut::<4>().0 {
            metric[..2].copy_from_slice(&(i16::MAX as u16).to_be_bytes());
        }
    });
    let face = FontHandle::try_from_vec(bytes).expect("hostile metrics fixture");
    let advance = face
        .as_scaled(PxScale::from(20.0))
        .h_advance(face.glyph_id('M'));
    assert!(advance > 512.0, "hostile advance is {advance}");
    // Measure only: the hostile dimensions never reach bitmap allocation.
    let cell = super::super::build::cell_geometry(&face, 20.0, 1.0);
    assert!(
        cell.width <= 512 && cell.height <= 512,
        "unsafe cell: {cell:?}"
    );
    assert!(cell.baseline <= cell.height);
}

#[test]
fn invalid_scale_cannot_create_unbounded_cell_geometry() {
    for px in [f32::INFINITY, f32::NAN, f32::MAX] {
        let cell = super::super::build::cell_geometry(&font(), px, 2.0);
        assert!(
            cell.width <= 512 && cell.height <= 512,
            "unsafe cell: {cell:?}"
        );
        assert!(cell.baseline <= cell.height);
    }
}

#[test]
fn growth_slot_ceiling_respects_bitmap_budget_without_allocating_it() {
    let mut atlas = GlyphAtlas::build_with_subpixel(&wide_font(), 64.0, SubpixelMode::Rgb);
    atlas.set_texture_dimension_limit(u32::MAX);
    let rows = atlas.max_slots.div_ceil(atlas.cols);
    let bytes = atlas_byte_len(atlas.width, rows * slot_h(atlas.cell), 4);
    assert!(bytes <= 192 * 1024 * 1024, "growth admits {bytes} bytes");
    assert!(rows * slot_h(atlas.cell) <= 8192);
}

#[test]
fn ordinary_geometry_is_preserved() {
    let face = font();
    let scaled = face.as_scaled(PxScale::from(24.0));
    let cell = super::super::build::cell_geometry(&face, 24.0, 1.0);
    assert_eq!(
        cell.width,
        scaled.h_advance(face.glyph_id('M')).ceil() as u32
    );
    assert_eq!(
        cell.height,
        (scaled.ascent() - scaled.descent()).ceil() as u32
    );
    assert_eq!(cell.baseline, scaled.ascent().round() as u32);
}

fn wide_font() -> FontHandle {
    let mut bytes = include_bytes!("../../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec();
    edit_table(&mut bytes, b"hmtx", |table| {
        for metric in table.as_chunks_mut::<4>().0 {
            metric[..2].copy_from_slice(&7000u16.to_be_bytes());
        }
    });
    FontHandle::try_from_vec(bytes).expect("wide metric fixture")
}

#[test]
fn native_initial_geometry_respects_device_limit_before_allocation() {
    let atlas = GlyphAtlas::build_with_dimension_limit(&font(), 128.0, SubpixelMode::Rgb, 1.0, 512);
    assert_eq!(
        atlas.cell,
        CellSize {
            width: 8,
            height: 16,
            baseline: 13
        }
    );
    assert!(atlas.width <= 512 && atlas.height <= 512);
    assert_eq!(
        atlas.data.len(),
        atlas_byte_len(atlas.width, atlas.height, 4)
    );
    assert!(atlas.data.len() <= MAX_ATLAS_BYTES);
    let tiny = GlyphAtlas::build_with_dimension_limit(&font(), 64.0, SubpixelMode::Off, 1.0, 112);
    assert!(tiny.width <= 112 && tiny.height <= 112);
}

#[test]
fn allocator_rechecks_byte_bound_without_allocating_the_refused_page() {
    let mut atlas = GlyphAtlas::build_with_subpixel(&wide_font(), 64.0, SubpixelMode::Rgb);
    let before = (
        atlas.height,
        atlas.capacity_rows,
        atlas.data.clone(),
        atlas.revision,
    );
    // Bypass only the slot ceiling to exercise the allocation boundary itself.
    atlas.next_slot = atlas.max_slots;
    atlas.max_slots = u32::MAX;
    assert_eq!(atlas.allocate_slots(1), None);
    assert_eq!(
        (
            atlas.height,
            atlas.capacity_rows,
            atlas.data,
            atlas.revision
        ),
        before
    );
}

#[test]
fn supported_hidpi_maximum_keeps_natural_cell_geometry() {
    for bytes in [
        include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/victor-mono/VictorMono-Regular.otf").as_slice(),
    ] {
        let face = FontHandle::try_from_vec(bytes.to_vec()).expect("embedded OFL font");
        let cell = super::super::build::cell_geometry(&face, 288.0, 2.0);
        let scaled = face.as_scaled(PxScale::from(288.0));
        assert_eq!(
            cell.width,
            scaled.h_advance(face.glyph_id('M')).ceil() as u32
        );
        assert_eq!(
            cell.height,
            2 * (scaled.ascent() - scaled.descent()).ceil() as u32
        );
        // Admission only: do not allocate the maximum-size bitmap in this test.
        assert!(super::super::build::initial_geometry_fits(
            cell,
            SubpixelMode::Rgb,
            8192
        ));
    }
}
