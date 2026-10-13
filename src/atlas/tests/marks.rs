// SPDX-License-Identifier: GPL-3.0-only
//! Combining-mark classification follows the pinned Unicode 17.0.0 data, and
//! marks drawn from the project-authored `combining-marks.ttf` land on their
//! base cell.

use super::*;

/// Every Mn and Me range of Unicode 17.0.0, generated with the table.
const UCD_MARKS: &str = include_str!("../../../tests/fixtures/unicode-marks/marks.txt");

fn ucd_marks() -> Vec<(u32, u32)> {
    UCD_MARKS
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let (span, category) = line.split_once(';').expect("range;category");
            assert!(matches!(category, "Mn" | "Me"), "{line}");
            let (start, end) = span.split_once("..").unwrap_or((span, span));
            (
                u32::from_str_radix(start, 16).expect("hex start"),
                u32::from_str_radix(end, 16).expect("hex end"),
            )
        })
        .collect()
}

#[test]
fn combining_marks_match_the_pinned_unicode_fixture() {
    let ranges = ucd_marks();
    assert!(ranges.len() > 300, "fixture holds the full Mn and Me set");
    let mut mismatches = Vec::new();
    for ch in (0..=0x10FFFF).filter_map(char::from_u32) {
        let cp = ch as u32;
        let listed = ranges
            .iter()
            .any(|&(start, end)| (start..=end).contains(&cp));
        if is_combining_mark(ch) != (listed && !is_variation_selector(ch)) {
            mismatches.push(format!("U+{cp:04X}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "differs from Unicode 17.0.0: {mismatches:?}"
    );
}

/// Fails before the generated table: the hand-written table listed these
/// spacing marks and unassigned code points and missed the newer marks.
#[test]
fn spacing_and_unassigned_code_points_are_not_marks_and_new_marks_are() {
    for cp in [
        0x0903, 0x302E, 0x302F, 0x1171E, 0x16FF0, 0x1D165, 0x1D16D, 0x1ADE, 0x1AFF, 0x20F1, 0x20FF,
    ] {
        let ch = char::from_u32(cp).expect("scalar");
        assert!(!is_combining_mark(ch), "U+{cp:04X} is not Mn or Me");
    }
    for cp in [0x0903, 0x302E, 0x1171E, 0x1D165] {
        let ch = char::from_u32(cp).expect("scalar");
        assert!(
            should_attempt_fallback(ch),
            "spacing mark U+{cp:04X} may fall back"
        );
    }
    for cp in [
        0xA670, 0xA9BD, 0x10D69, 0x10EFC, 0x113BB, 0x11B60, 0x11F5A, 0x1E6E3, 0x1E6F5,
    ] {
        let ch = char::from_u32(cp).expect("scalar");
        assert!(is_combining_mark(ch), "U+{cp:04X} is Mn or Me");
        assert!(
            !should_attempt_fallback(ch),
            "mark U+{cp:04X} stays off the fallback chain"
        );
    }
    for ch in ['\u{0301}', '\u{1ACF}', '\u{1D167}'] {
        assert!(is_combining_mark(ch), "{ch:?} stays a mark");
    }
    for ch in ['\u{FE0F}', '\u{E0100}'] {
        assert!(!is_combining_mark(ch), "variation selectors stay separate");
    }
}

fn mark_font() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/combining-marks.ttf").to_vec(),
    )
    .expect("project-authored font fixture")
}

/// First and last inked column of the inner cell a UV rect points at.
fn ink_columns(atlas: &GlyphAtlas, uv: [f32; 4]) -> Option<(u32, u32)> {
    let (cx, cy) = inner_origin(atlas, uv);
    let inked: Vec<u32> = (0..atlas.cell.width)
        .filter(|&x| {
            (cy..cy + atlas.cell.height)
                .any(|y| atlas.data[(y * atlas.width + cx + x) as usize] > 0)
        })
        .collect();
    Some((*inked.first()?, *inked.last()?))
}

/// Fails before the generated table: the newer marks drew left of their slot
/// and the listed spacing marks one cell to the right, so neither inked the
/// cell box. Every glyph in the fixture is the same bar, so a mark drawn over
/// its base inks the columns the base letter inks, within the one pixel by
/// which the whole-pixel cell width and the font's advance can differ.
#[test]
fn changed_marks_ink_the_same_columns_as_their_base() {
    let font = mark_font();
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let base_uv = atlas
        .ensure_styled(&font, FontStyle::Regular, 'x')
        .expect("base glyph");
    let base = ink_columns(&atlas, base_uv).expect("base inks its cell");
    assert!(
        base.0 > 0 && base.1 + 1 < atlas.cell.width,
        "bar sits inside the cell"
    );
    let mut misplaced = Vec::new();
    for ch in [
        '\u{0301}',
        '\u{0903}',
        '\u{302E}',
        '\u{A670}',
        '\u{11F5A}',
        '\u{1E6E3}',
    ] {
        let uv = atlas
            .ensure_styled(&font, FontStyle::Regular, ch)
            .expect("fixture glyph");
        let columns = ink_columns(&atlas, uv);
        if !columns
            .is_some_and(|(first, last)| first.abs_diff(base.0) <= 1 && last.abs_diff(base.1) <= 1)
        {
            misplaced.push(format!("U+{:04X} inks {columns:?}", ch as u32));
        }
        assert!(
            atlas.combining_mark_quad(FontStyle::Regular, ch).is_some(),
            "U+{:04X} is resident as a real glyph",
            ch as u32
        );
    }
    assert!(
        misplaced.is_empty(),
        "base cell columns are {base:?}: {misplaced:?}"
    );
}
