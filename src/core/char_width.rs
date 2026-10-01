// SPDX-License-Identifier: GPL-3.0-only
//! Cell display width.
//!
//! Narrow mode is [`unicode_width::UnicodeWidthChar::width`]: East Asian
//! Ambiguous is one column. Wide mode is
//! [`unicode_width::UnicodeWidthChar::width_cjk`]: Ambiguous is two columns,
//! and the crate's other East Asian-context rules apply. No second width
//! table is kept here.
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
    let measured = if ambiguous_wide {
        UnicodeWidthChar::width_cjk(ch)
    } else {
        UnicodeWidthChar::width(ch)
    };
    measured.unwrap_or(1)
}
