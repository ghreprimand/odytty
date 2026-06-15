// SPDX-License-Identifier: GPL-3.0-only
//! Tests for [`Screen::visible_search_rows`] — the windowed, `wrapped`-carrying
//! viewport row accessor that feeds the hint / quick-select scanner. Validates
//! the offset windowing (mirroring `snapshot_with_scrollback`), the soft-wrap
//! flag, the screen-relative row order, and the `as_search_row` borrow.

use super::*;

/// Text of each visible row, trailing blanks trimmed (wide-continuation cells
/// skipped), so a window reads as plain strings.
fn row_text(rows: &[VisibleRow]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.cells
                .iter()
                .filter(|cell| !cell.wide_continuation)
                .map(|cell| cell.ch)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn offset_zero_is_the_live_visible_screen() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

    // Live tail shows the bottom two rows; scrollback holds "one","two".
    assert_eq!(terminal.screen().scrollback_len(), 2);
    let rows = terminal.visible_search_rows(0);
    assert_eq!(rows.len(), 2, "one row per visible viewport row");
    assert_eq!(row_text(&rows), ["three", "four"]);
}

#[test]
fn positive_offset_pages_up_into_scrollback() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

    // Same windowing as snapshot_with_scrollback: offset 1 → "two","three".
    assert_eq!(row_text(&terminal.visible_search_rows(1)), ["two", "three"]);
    // Offset 2 reaches the oldest stored rows.
    assert_eq!(row_text(&terminal.visible_search_rows(2)), ["one", "two"]);
}

#[test]
fn offset_clamps_beyond_history() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

    // Any offset past the available scrollback clamps to the oldest window,
    // never panicking or reading past row 0.
    assert_eq!(row_text(&terminal.visible_search_rows(999)), ["one", "two"]);
    assert_eq!(
        row_text(&terminal.visible_search_rows(999)),
        row_text(&terminal.visible_search_rows(2))
    );
}

#[test]
fn carries_the_soft_wrap_flag() {
    // "abcdefgh" at width 5 wraps: row 0 "abcde" (wrapped), row 1 "fgh".
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"abcdefgh");

    let rows = terminal.visible_search_rows(0);
    assert_eq!(row_text(&rows), ["abcde", "fgh"]);
    assert!(rows[0].wrapped, "the wrapped first row must report wrapped");
    assert!(
        !rows[1].wrapped,
        "the final row of the logical line is not wrapped"
    );
}

#[test]
fn rows_are_top_to_bottom_in_screen_order() {
    // Row index 0 is the top visible row — the coordinate the renderer paints
    // hint labels in. Verify the order matches the visible snapshot.
    let mut terminal = Terminal::new(5, 3);
    terminal.advance(b"aa\r\nbb\r\ncc");
    assert_eq!(
        row_text(&terminal.visible_search_rows(0)),
        ["aa", "bb", "cc"]
    );
}

#[test]
fn as_search_row_borrows_cells_and_wrapped() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"abcdefgh");

    let rows = terminal.visible_search_rows(0);
    let borrowed = rows[0].as_search_row();
    assert!(borrowed.wrapped);
    assert_eq!(borrowed.cells.len(), rows[0].cells.len());
    // The borrowed view points at the owned row's cells.
    assert_eq!(borrowed.cells.first().map(|c| c.ch), Some('a'));
}

#[test]
fn empty_scrollback_any_offset_is_the_visible_screen() {
    // No scrolled-out rows: every offset clamps to the live window, never panics.
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"hi\r\nyo");
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(row_text(&terminal.visible_search_rows(0)), ["hi", "yo"]);
    assert_eq!(row_text(&terminal.visible_search_rows(7)), ["hi", "yo"]);
}
