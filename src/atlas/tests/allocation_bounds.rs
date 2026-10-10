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
    atlas.set_texture_dimension_limit(16384);
    let rows = atlas.max_slots.div_ceil(atlas.cols);
    let bytes = atlas_byte_len(atlas.width, rows * slot_h(atlas.cell), 4);
    assert!(bytes <= 192 * 1024 * 1024, "growth admits {bytes} bytes");
    assert!(rows * slot_h(atlas.cell) <= 16384);
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

#[test]
fn ordinary_hidpi_growth_retains_device_capacity() {
    let mut atlas = GlyphAtlas::build(&font(), 24.0);
    // Model an ordinary 34px-high HiDPI cell without allocating its full cache.
    atlas.cell = CellSize {
        width: 18,
        height: 34,
        baseline: 26,
    };
    atlas.width = atlas.cols * slot_w(atlas.cell);
    atlas.set_texture_dimension_limit(16384);
    assert_eq!(atlas.max_slots, 5024);
    assert!(atlas.max_slots > 5000);
    assert!(
        atlas_byte_len(
            atlas.width,
            atlas.max_slots / atlas.cols * slot_h(atlas.cell),
            1
        ) < MAX_ATLAS_BYTES
    );
}

#[test]
fn growth_crosses_construction_axis_ceiling_without_moving_slots() {
    let mut atlas = GlyphAtlas::build(&font(), 24.0);
    atlas.set_texture_dimension_limit(16384);
    let resident = atlas.data.clone();
    let spans = atlas.slot_span.clone();
    let beyond = (8192 / slot_h(atlas.cell) + 1) * atlas.cols;
    while atlas.next_slot <= beyond {
        let next = atlas.next_slot;
        assert_eq!(atlas.allocate_slots(1), Some(next));
    }
    assert!(atlas.height > 8192 && atlas.height <= 16384);
    assert_eq!(&atlas.data[..resident.len()], resident.as_slice());
    assert_eq!(&atlas.slot_span[..spans.len()], spans.as_slice());
}

#[test]
fn minimal_fallback_constructor_uses_actual_gutter_extent() {
    let atlas = GlyphAtlas::build_with_dimension_limit(&font(), 24.0, SubpixelMode::Off, 1.0, 112);
    assert_eq!((atlas.cell.width, atlas.cell.height), (1, 1));
    assert_eq!((atlas.width, atlas.height), (112, 42));
}

#[test]
fn native_constructor_retains_growth_limit_above_initial_ceiling() {
    let headless = GlyphAtlas::build(&font(), 24.0);
    let native =
        GlyphAtlas::build_with_dimension_limit(&font(), 24.0, SubpixelMode::Off, 1.0, 16384);
    assert_eq!(native.data, headless.data);
    assert_eq!(native.cell, headless.cell);
    assert!(native.max_slots > headless.max_slots);
}
