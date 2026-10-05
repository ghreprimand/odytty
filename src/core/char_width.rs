// SPDX-License-Identifier: GPL-3.0-only
//! Cell display width.
//!
//! Narrow mode is [`unicode_width::UnicodeWidthChar::width`]: East Asian
//! Ambiguous is one column. Wide mode is
//! [`unicode_width::UnicodeWidthChar::width_cjk`]: Ambiguous is two columns,
//! and the crate's other East Asian-context rules apply. No second width
//! table is kept here. Khmer U+17A4 and U+17D8 use explicit one-cell
//! compatibility overrides in both modes.
//!
//! These call sites measure overlay chrome, not the terminal grid, and stay
//! on `UnicodeWidthChar::width`:
//! - `src/native/overlay/render.rs`
//! - `src/native/app/tab_bar.rs`
//! - `src/native/app/overlay_registry.rs`
//! - `src/native/search_ui.rs` (the search status line)
//!
//! `src/atlas/mod.rs` `glyph_cells` also stays on the narrow table. A shared
//! atlas cannot cache two slot widths for one codepoint while two panes
//! disagree. The grid's `wide_continuation` cell is what reserves the second
//! column; the lead cell's glyph is drawn inside that span.

use unicode_width::UnicodeWidthChar;

/// Columns `ch` occupies. `None` from the crate (controls) becomes 1, matching
/// the historical `print_char` fallback. Combining marks stay 0.
pub(crate) fn char_display_width(ch: char, ambiguous_wide: bool) -> usize {
    // Frozen Khmer scalar compatibility widths, independent of mark absorption.
    // Source and reconstructed/history owners share the one-cell policy.
    if matches!(ch, '\u{17a4}' | '\u{17d8}') {
        return 1;
    }
    let measured = if ambiguous_wide {
        UnicodeWidthChar::width_cjk(ch)
    } else {
        UnicodeWidthChar::width(ch)
    };
    measured.unwrap_or(1)
}

/// Thai/Lao SARA AM keeps its preceding consonant as the source owner.
/// Pre-base vowels and unrelated preceding text remain separate owners.
pub(crate) fn thai_lao_spacing_extension(base: char, next: char) -> bool {
    match next {
        '\u{0e33}' => matches!(base, '\u{0e01}'..='\u{0e2e}'),
        '\u{0eb3}' => {
            matches!(base, '\u{0e81}'..='\u{0eae}')
                && !matches!(
                    base,
                    '\u{0e83}' | '\u{0e85}' | '\u{0e8b}' | '\u{0ea4}' | '\u{0ea6}'
                )
        }
        _ => false,
    }
}

/// Width of a retained owner. Script-specific additions stay bounded here;
/// Indic, Sinhala, Khmer, and Myanmar width units are bounded; recognized emoji sequences share the same bounded seam.
pub(crate) fn owner_display_width(base: char, extensions: &[char], ambiguous_wide: bool) -> usize {
    if super::emoji_width::has_two_cell_footprint(base, extensions)
        || super::indic::has_two_cell_footprint(base, extensions)
        || extensions
            .iter()
            .any(|&c| thai_lao_spacing_extension(base, c))
    {
        2
    } else {
        char_display_width(base, ambiguous_wide)
    }
}

/// Unicode 17.0.0 DerivedCoreProperties.txt Default_Ignorable_Code_Point.
/// https://www.unicode.org/Public/17.0.0/ucd/DerivedCoreProperties.txt
/// The caller also
/// requires scalar width zero; visible fillers keep their existing widths.
/// Standalone source retention for these scalars remains unsupported.
pub(crate) fn is_default_ignorable(ch: char) -> bool {
    matches!(ch as u32,
        0x00ad | 0x034f | 0x061c | 0x115f..=0x1160 | 0x17b4..=0x17b5
        | 0x180b..=0x180f | 0x200b..=0x200f | 0x202a..=0x202e
        | 0x2060..=0x206f | 0x3164 | 0xfe00..=0xfe0f | 0xfeff
        | 0xffa0 | 0xfff0..=0xfff8 | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a | 0xe0000..=0xe0fff)
}
