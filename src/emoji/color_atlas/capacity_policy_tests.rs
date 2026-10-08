// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored bounded atlas policy fixtures, no font or GPU discovery.
use super::*;

fn tiny_cell() -> CellSize {
    CellSize {
        width: 1,
        height: 1,
        baseline: 1,
    }
}
fn key(id: u32) -> ColorGlyphKey {
    ColorGlyphKey::new(1, ColorGlyphId::Glyph(id), 1.0, 1.0, 1)
}

#[test]
fn fourfold_residency_preserves_pixels_and_refuses_the_next_slot() {
    let mut atlas = ColorGlyphAtlas::new(tiny_cell());
    atlas.set_texture_dimension_limit(2048);
    let pixels = [4, 3, 2, 5];
    for id in 0..16384 {
        atlas
            .insert_premultiplied(key(id), 1, &pixels)
            .expect("fourfold residency");
    }
    assert!(atlas.lookup(key(0)).is_some());
    assert!(atlas.lookup(key(16383)).is_some());
    atlas.take_dirty();
    let revision = atlas.revision();
    let len = atlas.data.len();
    assert_eq!(
        atlas.insert_premultiplied(key(16384), 1, &pixels),
        Err(ColorGlyphAtlasError::Full)
    );
    assert_eq!(atlas.revision(), revision);
    assert_eq!(atlas.data.len(), len);
    assert!(!atlas.take_dirty());
    assert_eq!(&atlas.data[..4], &pixels);
}

#[test]
fn zero_cell_geometry_declines_before_bitmap_admission() {
    let mut atlas = ColorGlyphAtlas::new(CellSize {
        width: 0,
        height: 1,
        baseline: 0,
    });
    assert_eq!(
        atlas.insert_premultiplied(key(0), 1, &[]),
        Err(ColorGlyphAtlasError::Full)
    );
}

#[test]
fn initial_device_refusal_is_a_transparent_nonresident_texture() {
    for (cell, limit) in [
        (
            CellSize {
                width: 3,
                height: 1,
                baseline: 1,
            },
            64,
        ),
        (
            CellSize {
                width: 1,
                height: 3,
                baseline: 1,
            },
            8,
        ),
        (
            CellSize {
                width: u32::MAX,
                height: u32::MAX,
                baseline: 0,
            },
            2048,
        ),
    ] {
        let mut atlas = ColorGlyphAtlas::with_texture_dimension_limit(cell, limit);
        assert_eq!((atlas.width, atlas.height), (1, 1));
        assert_eq!(atlas.data, [0; 4]);
        assert_eq!(atlas.cell, cell);
        assert!(atlas.width <= limit && atlas.height <= limit);
        assert_eq!(
            atlas.insert_premultiplied(key(0), 1, &[]),
            Err(ColorGlyphAtlasError::Full)
        );
        assert!(atlas.lookup(key(0)).is_none());
        atlas.set_texture_dimension_limit(u32::MAX);
        assert_eq!(
            atlas.insert_premultiplied(key(0), 1, &[]),
            Err(ColorGlyphAtlasError::Full)
        );
        assert_eq!(atlas.revision(), 0);
        assert!(!atlas.take_dirty());
    }
}

#[test]
fn checked_bitmap_budget_refuses_overflow_without_allocating() {
    assert_eq!(bitmap_byte_len(16384, 4096), Some(256 * 1024 * 1024));
    assert_eq!(bitmap_byte_len(16384, 4097), None);
    assert_eq!(bitmap_byte_len(u32::MAX, u32::MAX), None);
    assert!(
        initial_layout(
            CellSize {
                width: u32::MAX,
                height: 1,
                baseline: 0
            },
            u32::MAX
        )
        .is_none()
    );
    assert!(
        initial_layout(
            CellSize {
                width: 1,
                height: u32::MAX,
                baseline: 0
            },
            u32::MAX
        )
        .is_none()
    );
    assert!(
        initial_layout(
            CellSize {
                width: 1,
                height: 1,
                baseline: 1
            },
            0
        )
        .is_none()
    );
}

#[test]
fn exact_initial_width_and_growth_height_limit_keep_residents() {
    let cell = CellSize {
        width: 2,
        height: 2,
        baseline: 1,
    };
    let mut atlas = ColorGlyphAtlas::with_texture_dimension_limit(cell, 64);
    assert_eq!((atlas.width, atlas.height, atlas.data.len()), (64, 8, 2048));
    let pixels = [5; 16];
    for id in 0..512 {
        atlas
            .insert_premultiplied(key(id), 1, &pixels)
            .expect("within device");
    }
    assert_eq!(atlas.height, 64);
    atlas.take_dirty();
    let revision = atlas.revision();
    assert_eq!(
        atlas.insert_premultiplied(key(512), 1, &pixels),
        Err(ColorGlyphAtlasError::Full)
    );
    assert_eq!(atlas.revision(), revision);
    assert!(!atlas.take_dirty());
    assert!(atlas.lookup(key(0)).is_some());
    assert!(atlas.lookup(key(511)).is_some());
}
