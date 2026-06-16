// SPDX-License-Identifier: GPL-3.0-only
//! Atlas build, channel layout, fallback, ensure/growth, and rebuild tests. (M5 mechanical split from atlas.rs).

use super::*;

#[test]
fn atlas_has_positive_metrics_and_glyph_coverage() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 28.0);
    assert!(atlas.cell.width > 0 && atlas.cell.height > 0);
    assert!(atlas.cell.baseline <= atlas.cell.height);
    assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
    // A glyph with ink (e.g. 'M') must produce non-zero coverage.
    assert!(atlas.data.iter().any(|&v| v > 0));
    // UV rects exist for printable ASCII and not for control chars.
    assert!(atlas.uv_rect('A').is_some());
    assert!(atlas.uv_rect('\n').is_none());
}

#[test]
fn line_height_default_is_byte_identical_to_legacy_build() {
    // LINEHEIGHT: build_with_options at the default 1.0 multiplier must produce
    // a cell, dimensions and coverage buffer byte-identical to the historical
    // build_with_subpixel path — the leading is exactly zero.
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let legacy = GlyphAtlas::build_with_subpixel(&font, 24.0, SubpixelMode::Off);
    let leaded = GlyphAtlas::build_with_options(&font, 24.0, SubpixelMode::Off, 1.0);
    assert_eq!(leaded.cell, legacy.cell, "cell geometry must be unchanged");
    assert_eq!(leaded.width, legacy.width);
    assert_eq!(leaded.height, legacy.height);
    assert_eq!(leaded.data, legacy.data, "coverage must be byte-identical");
}

#[test]
fn line_height_above_one_adds_symmetric_leading() {
    // A line_height > 1.0 grows the cell height and shifts the baseline down by
    // the top leading, while the cell width and the rasterized glyph shape are
    // unchanged (the glyph simply sits lower in a taller slot).
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let base = GlyphAtlas::build_with_options(&font, 24.0, SubpixelMode::Off, 1.0);
    let tall = GlyphAtlas::build_with_options(&font, 24.0, SubpixelMode::Off, 1.5);
    assert_eq!(
        tall.cell.width, base.cell.width,
        "advance width is unchanged"
    );
    assert!(
        tall.cell.height > base.cell.height,
        "leading must grow the cell height ({} !> {})",
        tall.cell.height,
        base.cell.height
    );
    assert!(
        tall.cell.baseline >= base.cell.baseline,
        "baseline shifts down by the top leading"
    );
    assert!(tall.cell.baseline <= tall.cell.height);
}

#[test]
fn default_atlas_stays_single_channel() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);

    assert_eq!(atlas.subpixel_mode(), SubpixelMode::Off);
    assert_eq!(atlas.bytes_per_row(), atlas.width);
    assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
}

#[test]
fn subpixel_atlas_stores_rgb_coverage_without_geometry_change() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let gray = GlyphAtlas::build(&font, 24.0);
    let rgb = GlyphAtlas::build_with_subpixel(&font, 24.0, SubpixelMode::Rgb);

    assert_eq!(rgb.subpixel_mode(), SubpixelMode::Rgb);
    assert_eq!(rgb.cell, gray.cell);
    assert_eq!(rgb.width, gray.width);
    assert_eq!(rgb.height, gray.height);
    assert_eq!(rgb.bytes_per_row(), rgb.width * 4);
    assert_eq!(rgb.data.len(), (rgb.width * rgb.height * 4) as usize);

    let channels = subpixel_cell_channels(&rgb, rgb.uv_rect('M').unwrap());
    assert!(
        channels[0] > 0 && channels[1] > 0 && channels[2] > 0,
        "RGB subpixel atlas should populate all color channels: {channels:?}"
    );
    assert!(
        channels[3] > 0,
        "subpixel atlas should mark inked texels with opaque alpha"
    );
}

#[test]
fn fallback_box_is_visible_but_hollow() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let ink = cell_ink(&atlas, atlas.slot_uv(FALLBACK_SLOT));
    // Has visible ink (the box border) ...
    assert!(ink > 0, "fallback box should have ink");
    // ... but is not a solid block (hollow interior).
    let solid = (atlas.cell.width * atlas.cell.height) as u64 * 255;
    assert!(ink < solid, "fallback box should be hollow, not solid");
}

#[test]
fn uv_rect_falls_back_for_unsupported_printable() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let fallback = atlas.slot_uv(FALLBACK_SLOT);
    // ASCII resolves to its own cell, never the fallback.
    assert_ne!(atlas.uv_rect('A'), Some(fallback));
    // Unsupported printable codepoints share the one fallback box.
    assert_eq!(atlas.uv_rect('é'), Some(fallback));
    assert_eq!(atlas.uv_rect('★'), Some(fallback));
    assert_eq!(atlas.uv_rect('\u{1F600}'), Some(fallback));
    // Control and whitespace draw nothing.
    assert!(atlas.uv_rect('\t').is_none());
}

#[test]
fn ensure_rasterizes_real_glyph_and_flags_dirty() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(ch) = glyph_bearing_non_ascii(&font) else {
        eprintln!("skipping: font has no non-ASCII glyph");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let fallback = atlas.slot_uv(FALLBACK_SLOT);
    assert_eq!(atlas.uv_rect(ch), Some(fallback)); // not yet resident

    let uv = atlas.ensure(&font, ch).expect("real glyph uv");
    assert_ne!(uv, fallback, "ensure should pick a real slot, not fallback");
    assert!(atlas.take_dirty(), "insertion must flag dirty");
    // Now resident: the immutable lookup resolves to the same real slot.
    assert_eq!(atlas.uv_rect(ch), Some(uv));
    // The new cell actually got ink.
    assert!(cell_ink(&atlas, uv) > 0);

    // A repeat is a pure cache hit: same uv, no new slot, not dirty.
    let count = atlas.slot_count();
    let uv2 = atlas.ensure(&font, ch).expect("cached uv");
    assert_eq!(uv2, uv);
    assert_eq!(atlas.slot_count(), count);
    assert!(!atlas.take_dirty());
}

#[test]
fn ensure_missing_glyph_uses_fallback_without_a_slot() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let fallback = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    // A high private-use codepoint no monospace font maps.
    let uv = atlas.ensure(&font, '\u{10FFFD}').expect("fallback uv");
    assert_eq!(uv, fallback);
    assert_eq!(
        atlas.slot_count(),
        count,
        "fallback must not consume a slot"
    );
    assert!(!atlas.take_dirty(), "no pixels changed, so not dirty");
}

#[test]
fn ensure_grows_atlas_and_preserves_existing_glyphs() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(first) = glyph_bearing_non_ascii(&font) else {
        eprintln!("skipping: font has no non-ASCII glyph");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let base_height = atlas.height;
    let base_count = atlas.slot_count();

    // Insert the first glyph and remember its ink.
    let uv_first = atlas.ensure(&font, first).expect("first uv");
    let ink_first = cell_ink(&atlas, uv_first);
    assert!(
        atlas.height > base_height,
        "first dynamic insert should grow"
    );

    // Insert many more distinct glyph-bearing codepoints to force more
    // growth pages; every returned UV must stay within the bitmap.
    let mut inserted = 1u32;
    for ch in (0x00A1u32..=0x05FF).filter_map(char::from_u32) {
        if ch == first || !font_has_glyph(&font, ch) {
            continue;
        }
        let uv = atlas.ensure(&font, ch).expect("dynamic uv");
        assert!(uv[3] <= 1.0 + 1e-6, "uv must stay in bounds after growth");
        inserted += 1;
        if inserted >= 200 {
            break;
        }
    }
    assert!(
        atlas.slot_count() > base_count + 1,
        "atlas should have grown"
    );

    // The first glyph's pixels survived every intervening growth: its cell
    // offset never moved, so its ink (recomputed against the current size)
    // is unchanged.
    let ink_after = cell_ink(&atlas, atlas.uv_rect(first).unwrap());
    assert_eq!(
        ink_after, ink_first,
        "growth must not corrupt existing glyphs"
    );
    assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
}

#[test]
fn rebuild_is_a_full_invalidation() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(ch) = glyph_bearing_non_ascii(&font) else {
        eprintln!("skipping: font has no non-ASCII glyph");
        return;
    };
    let mut big = GlyphAtlas::build(&font, 24.0);
    big.ensure(&font, ch);
    assert!(big.slot_count() > FIRST_DYNAMIC_SLOT);

    // A size change is a fresh build: different cell metrics, no carried-over
    // dynamic glyphs (no mixed-size glyphs can coexist), revision reset.
    let small = GlyphAtlas::build(&font, 14.0);
    assert_ne!(
        big.cell, small.cell,
        "different px should change cell metrics"
    );
    assert_eq!(small.slot_count(), FIRST_DYNAMIC_SLOT, "no dynamic glyphs");
    assert_eq!(small.revision(), 0);
    assert_eq!(small.uv_rect(ch), Some(small.slot_uv(FALLBACK_SLOT)));
    assert_eq!(
        small.height,
        slot_h(small.cell) * FIRST_DYNAMIC_SLOT.div_ceil(ATLAS_COLS)
    );
}

/// The atlas bitmap reserves a border (bleed gutter + overflow margin) around
/// every slot, so the bitmap is wider/taller than a borderless pack and
/// adjacent inner cells are separated by `2·slot_border` pixels — the guard
/// against bleed plus the room for overflow ink.
#[test]
fn slots_carry_a_padding_gutter() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let border = slot_border(atlas.cell);
    // The border is at least the bleed gutter and adds the overflow margin.
    assert!(border >= ATLAS_PAD);
    assert_eq!(border, ATLAS_PAD + overflow_margin(atlas.cell));
    // Bitmap dimensions account for the full border on every slot.
    assert_eq!(atlas.width, atlas.cols * (atlas.cell.width + 2 * border));

    // Two horizontally-adjacent inner cells (slots 1 and 2) are separated by
    // a full 2·border-pixel gap, so sampling one cannot reach the other's ink
    // and each has room to overflow into its own margin.
    let a = atlas.slot_uv(1);
    let b = atlas.slot_uv(2);
    let a_right = (a[2] * atlas.width as f32).round() as i32;
    let b_left = (b[0] * atlas.width as f32).round() as i32;
    assert_eq!(b_left - a_right, (2 * border) as i32);
}
