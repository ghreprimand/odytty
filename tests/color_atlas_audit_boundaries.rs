// SPDX-License-Identifier: GPL-3.0-only
// Project-authored tiny pixels exercise public atlas admission, without a GPU.
use odytty::atlas::CellSize;
use odytty::emoji::{ColorGlyphAtlas, ColorGlyphId, ColorGlyphKey};

fn key(width: u8) -> ColorGlyphKey {
    ColorGlyphKey::new(1, ColorGlyphId::Glyph(1), 1.0, 1.0, width)
}
fn atlas() -> ColorGlyphAtlas {
    ColorGlyphAtlas::new(CellSize {
        width: 1,
        height: 1,
        baseline: 1,
    })
}

#[test]
fn atlas_rejects_a_device_limit_below_initial_height() {
    let mut atlas = ColorGlyphAtlas::new(CellSize {
        width: 1,
        height: 16,
        baseline: 12,
    });
    assert_eq!((atlas.width, atlas.height), (32, 64));
    atlas.set_texture_dimension_limit(32);
    assert!(atlas.insert_premultiplied(key(1), 1, &[255; 64]).is_err());
    assert!(atlas.lookup(key(1)).is_none());
    assert_eq!(atlas.revision(), 0);
}

#[test]
fn atlas_rejects_a_device_limit_below_initial_width() {
    let mut atlas = atlas();
    atlas.set_texture_dimension_limit(16);
    assert!(atlas.insert_premultiplied(key(1), 1, &[255; 4]).is_err());
    assert!(atlas.lookup(key(1)).is_none());
    assert_eq!(atlas.revision(), 0);
}

#[test]
fn atlas_rejects_width_identity_disagreement_before_insertion() {
    let mut atlas = atlas();
    assert!(atlas.insert_premultiplied(key(1), 2, &[255; 8]).is_err());
    assert!(atlas.lookup(key(1)).is_none());
    assert_eq!(atlas.revision(), 0);
    assert!(!atlas.take_dirty());
}

#[test]
fn atlas_rejects_width_identity_disagreement_on_a_resident_key() {
    let mut atlas = atlas();
    let expected = atlas.insert_premultiplied(key(1), 1, &[255; 4]).unwrap();
    let revision = atlas.revision();
    atlas.take_dirty();
    assert!(atlas.insert_premultiplied(key(1), 2, &[255; 8]).is_err());
    assert_eq!(atlas.lookup(key(1)), Some(expected));
    assert_eq!(atlas.revision(), revision);
    assert!(!atlas.take_dirty());
}

#[test]
fn atlas_accepts_matching_widths_and_valid_device_dimensions() {
    let mut atlas = atlas();
    atlas.set_texture_dimension_limit(32);
    for width in [1, 2] {
        let bounds = atlas
            .insert_premultiplied(key(width), width, &vec![255; usize::from(width) * 4])
            .unwrap();
        assert_eq!(bounds.width_cells, width);
        assert_eq!(bounds.pixel_width, u32::from(width));
        assert_eq!(bounds.pixel_height, 1);
    }
}

#[test]
fn shrinking_device_limit_hides_resident_slots_without_mutation() {
    let mut atlas = atlas();
    let original = atlas.insert_premultiplied(key(1), 1, &[255; 4]).unwrap();
    let pixels = atlas.data.clone();
    let revision = atlas.revision();
    atlas.take_dirty();
    atlas.set_texture_dimension_limit(16);
    assert!(atlas.lookup(key(1)).is_none());
    assert!(atlas.insert_premultiplied(key(1), 1, &[255; 4]).is_err());
    assert_eq!(atlas.data, pixels);
    assert_eq!(atlas.revision(), revision);
    assert!(!atlas.take_dirty());
    atlas.set_texture_dimension_limit(32);
    assert_eq!(atlas.lookup(key(1)), Some(original));
}

#[test]
fn complete_growth_pages_stop_at_device_height_and_preserve_resident_hits() {
    let mut atlas = ColorGlyphAtlas::new(CellSize {
        width: 1,
        height: 5,
        baseline: 4,
    });
    atlas.set_texture_dimension_limit(39);
    for id in 0..64 {
        let key = ColorGlyphKey::new(1, ColorGlyphId::Glyph(id), 1.0, 1.0, 1);
        atlas.insert_premultiplied(key, 1, &[255; 20]).unwrap();
    }
    let resident = ColorGlyphKey::new(1, ColorGlyphId::Glyph(0), 1.0, 1.0, 1);
    let revision = atlas.revision();
    atlas.take_dirty();
    let overflow = ColorGlyphKey::new(1, ColorGlyphId::Glyph(64), 1.0, 1.0, 1);
    assert!(atlas.insert_premultiplied(overflow, 1, &[255; 20]).is_err());
    assert_eq!(atlas.height, 20);
    assert!(atlas.insert_premultiplied(resident, 1, &[255; 20]).is_ok());
    assert_eq!(atlas.revision(), revision);
    assert!(!atlas.take_dirty());
}
