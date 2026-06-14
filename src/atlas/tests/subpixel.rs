// SPDX-License-Identifier: GPL-3.0-only
//! LCD filtering guards for subpixel coverage atlases.

use super::*;

#[test]
fn lcd_filter_reduces_synthetic_vertical_stem_fringe() {
    let width = 7;
    let mut data = vec![0u8; width * 4];
    let stem_x = 3;
    let base = stem_x * 4;
    data[base] = 255;
    data[base + 3] = 255;

    let before = channel_imbalance(&data, stem_x);
    lcd_filter_subpixel_region(
        &mut data,
        width as u32,
        SubpixelMode::Rgb,
        0,
        0,
        width as u32,
        1,
    );
    let after = channel_imbalance(&data, stem_x);

    assert_eq!(before, 255);
    assert!(
        after < before / 2,
        "LCD filter should materially reduce R/B edge imbalance: before={before}, after={after}"
    );
    assert!(
        data[(stem_x - 1) * 4 + 2] > 0 || data[(stem_x + 1) * 4] > 0,
        "coverage should redistribute across neighboring physical subpixels"
    );
}

#[test]
fn lcd_filter_preserves_row_energy_with_centered_coverage() {
    let width = 9;
    let mut data = vec![0u8; width * 4];
    for x in 3..6 {
        let base = x * 4;
        data[base] = 81;
        data[base + 1] = 162;
        data[base + 2] = 81;
        data[base + 3] = 255;
    }
    let before = rgb_sum(&data);

    lcd_filter_subpixel_region(
        &mut data,
        width as u32,
        SubpixelMode::Rgb,
        0,
        0,
        width as u32,
        1,
    );

    let after = rgb_sum(&data);
    assert!(
        after.abs_diff(before) <= 2,
        "LCD filter should preserve per-row RGB coverage energy: before={before}, after={after}"
    );
}

#[test]
fn lcd_filter_off_mode_is_byte_identical() {
    let width = 8;
    let mut data: Vec<u8> = (0..width).map(|i| (i * 17) as u8).collect();
    let before = data.clone();

    lcd_filter_subpixel_region(
        &mut data,
        width as u32,
        SubpixelMode::Off,
        0,
        0,
        width as u32,
        1,
    );

    assert_eq!(data, before, "grayscale coverage must never be filtered");
}

#[test]
fn subpixel_filter_keeps_atlas_geometry_unchanged() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let gray = GlyphAtlas::build(&font, 24.0);
    let rgb = GlyphAtlas::build_with_subpixel(&font, 24.0, SubpixelMode::Rgb);

    assert_eq!(rgb.cell, gray.cell);
    assert_eq!(rgb.width, gray.width);
    assert_eq!(rgb.height, gray.height);
    assert_eq!(rgb.slot_count(), gray.slot_count());
}

fn channel_imbalance(data: &[u8], x: usize) -> u8 {
    let base = x * 4;
    data[base].abs_diff(data[base + 2])
}

fn rgb_sum(data: &[u8]) -> u64 {
    data.chunks_exact(4)
        .map(|px| px[0] as u64 + px[1] as u64 + px[2] as u64)
        .sum()
}
