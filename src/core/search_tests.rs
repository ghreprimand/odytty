// SPDX-License-Identifier: GPL-3.0-only
//! Deterministic fixtures for the scrollback search engine
//! ([`super::search`]): literal matching, case modes, wide/combining cells,
//! soft-wrap-spanning matches, trailing-blank trimming, and next/prev
//! wraparound.

use super::search::*;
use super::types::{Attrs, Cell};

/// Build a row of single-width cells, one per char.
fn row(s: &str) -> Vec<Cell> {
    s.chars().map(|c| Cell::new(c, Attrs::default())).collect()
}

/// Pad `cells` with trailing blank cells out to `width`.
fn padded(s: &str, width: usize) -> Vec<Cell> {
    let mut cells = row(s);
    while cells.len() < width {
        cells.push(Cell::blank());
    }
    cells
}

fn srow(cells: &[Cell], wrapped: bool) -> SearchRow<'_> {
    SearchRow { cells, wrapped }
}

fn at(row: usize, column: usize) -> AbsolutePoint {
    AbsolutePoint { row, column }
}

#[test]
fn match_count_is_capped_for_a_broadly_matching_query() {
    // A single-character query that matches once per row across far more rows
    // than the cap must not return an unbounded match vector.
    let cell_rows: Vec<Vec<Cell>> = (0..(MAX_SEARCH_MATCHES + 500)).map(|_| row("x")).collect();
    let rows: Vec<SearchRow<'_>> = cell_rows.iter().map(|c| srow(c, false)).collect();
    let m = search_rows(&rows, "x", SearchOptions::case_sensitive());
    assert_eq!(
        m.len(),
        MAX_SEARCH_MATCHES,
        "matches must be capped at MAX_SEARCH_MATCHES"
    );
}

#[test]
fn literal_substring_match_reports_inclusive_span() {
    let cells = row("hello world");
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "world", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 6));
    assert_eq!(m[0].end, at(0, 10));
}

#[test]
fn case_insensitive_matches_mixed_case() {
    let cells = row("Hello World");
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "WORLD", SearchOptions::case_insensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 6));
    assert_eq!(m[0].end, at(0, 10));
}

#[test]
fn case_sensitive_rejects_wrong_case() {
    let cells = row("Hello World");
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "world", SearchOptions::case_sensitive());
    assert!(m.is_empty());
}

#[test]
fn default_options_are_case_insensitive() {
    assert!(!SearchOptions::default().case_sensitive);
    let cells = row("ABC");
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "abc", SearchOptions::default());
    assert_eq!(m.len(), 1);
}

#[test]
fn multiple_non_overlapping_matches_in_reading_order() {
    let cells = row("ababab");
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "ab", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 3);
    assert_eq!(m[0].start, at(0, 0));
    assert_eq!(m[1].start, at(0, 2));
    assert_eq!(m[2].start, at(0, 4));
}

#[test]
fn overlapping_pattern_yields_non_overlapping_matches() {
    // "aa" in "aaa": greedy left-to-right finds one match at offset 0.
    let cells = row("aaa");
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "aa", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 0));
    assert_eq!(m[0].end, at(0, 1));
}

#[test]
fn no_match_returns_empty() {
    let cells = row("hello");
    let rows = [srow(&cells, false)];
    assert!(search_rows(&rows, "zzz", SearchOptions::case_sensitive()).is_empty());
}

#[test]
fn empty_query_matches_nothing() {
    let cells = row("hello");
    let rows = [srow(&cells, false)];
    assert!(search_rows(&rows, "", SearchOptions::case_sensitive()).is_empty());
}

#[test]
fn absolute_row_indexes_through_scrollback_then_screen() {
    // Rows 0,1 stand in for scrollback; row 2 for the live screen.
    let c0 = row("alpha");
    let c1 = row("beta");
    let c2 = row("gamma");
    let rows = [srow(&c0, false), srow(&c1, false), srow(&c2, false)];
    let m = search_rows(&rows, "gamma", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(2, 0));
}

#[test]
fn hard_break_does_not_join_lines() {
    let c0 = row("foo");
    let c1 = row("bar");
    let rows = [srow(&c0, false), srow(&c1, false)];
    // "foobar" must not match across a hard (non-wrapped) line break.
    assert!(search_rows(&rows, "foobar", SearchOptions::case_sensitive()).is_empty());
}

#[test]
fn soft_wrap_spanning_match_crosses_rows() {
    // Row 0 wraps into row 1: logical line "abcdef".
    let c0 = row("abc");
    let c1 = row("def");
    let rows = [srow(&c0, true), srow(&c1, false)];
    let m = search_rows(&rows, "cd", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 2));
    assert_eq!(m[0].end, at(1, 0));
}

#[test]
fn trailing_wrapped_row_is_still_searched() {
    // A final row left marked wrapped (no following row) still flushes.
    let c0 = row("abc");
    let rows = [srow(&c0, true)];
    let m = search_rows(&rows, "abc", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 0));
}

#[test]
fn trailing_blank_padding_is_trimmed() {
    // "ab" padded to width 6 with blanks: "b " must not match into padding,
    // but the content itself matches.
    let cells = padded("ab", 6);
    let rows = [srow(&cells, false)];
    assert!(search_rows(&rows, "b ", SearchOptions::case_sensitive()).is_empty());
    let m = search_rows(&rows, "ab", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
}

#[test]
fn interior_blanks_are_preserved() {
    let cells = row("a b");
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "a b", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 0));
    assert_eq!(m[0].end, at(0, 2));
}

#[test]
fn wide_glyph_match_spans_both_columns() {
    // "a" + wide '世' (lead + continuation spacer) + "b".
    let mut cells = vec![Cell::new('a', Attrs::default())];
    cells.push(Cell::new('世', Attrs::default()));
    cells.push(Cell::wide_spacer(Attrs::default()));
    cells.push(Cell::new('b', Attrs::default()));
    let rows = [srow(&cells, false)];

    let m = search_rows(&rows, "世", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 1));
    assert_eq!(m[0].end, at(0, 2)); // covers lead + continuation

    // The trailing "b" sits at column 3, past the wide continuation.
    let mb = search_rows(&rows, "b", SearchOptions::case_sensitive());
    assert_eq!(mb.len(), 1);
    assert_eq!(mb[0].start, at(0, 3));
}

#[test]
fn match_ending_on_wide_glyph_reports_continuation_column() {
    // "a" + wide '世': searching "a世" ends on the wide continuation column.
    let mut cells = vec![Cell::new('a', Attrs::default())];
    cells.push(Cell::new('世', Attrs::default()));
    cells.push(Cell::wide_spacer(Attrs::default()));
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "a世", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 0));
    assert_eq!(m[0].end, at(0, 2));
}

#[test]
fn combining_mark_cell_matches_base_char() {
    // Cell 'e' + combining acute (U+0301): grapheme "e\u{301}".
    let mut e = Cell::new('e', Attrs::default());
    assert!(e.push_combining('\u{301}'));
    let cells = vec![Cell::new('r', Attrs::default()), e];
    let rows = [srow(&cells, false)];

    // Query of the base char matches at the combining cell.
    let m = search_rows(&rows, "e", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 1));
    assert_eq!(m[0].end, at(0, 1));

    // Query of the full cluster also matches.
    let mc = search_rows(&rows, "e\u{301}", SearchOptions::case_sensitive());
    assert_eq!(mc.len(), 1);
    assert_eq!(mc[0].start, at(0, 1));
}

#[test]
fn combining_cluster_matches_bare_base_query() {
    // "re\u{301}": query "re" matches r+e (the combining mark trails e and is
    // not required by the query).
    let mut e = Cell::new('e', Attrs::default());
    e.push_combining('\u{301}');
    let cells = vec![Cell::new('r', Attrs::default()), e];
    let rows = [srow(&cells, false)];
    let m = search_rows(&rows, "re", SearchOptions::case_sensitive());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].start, at(0, 0));
    assert_eq!(m[0].end, at(0, 1));
}

#[test]
fn find_next_advances_and_wraps() {
    let cells = row("ababab");
    let rows = [srow(&cells, false)];
    let matches = search_rows(&rows, "ab", SearchOptions::case_sensitive());
    assert_eq!(matches.len(), 3);

    // From before the second match -> next strictly after column 0.
    assert_eq!(find_next(&matches, at(0, 0)).unwrap().start, at(0, 2));
    // Strictly-after semantics: from a match start skips to the next.
    assert_eq!(find_next(&matches, at(0, 2)).unwrap().start, at(0, 4));
    // Past the last match wraps to the first.
    assert_eq!(find_next(&matches, at(0, 4)).unwrap().start, at(0, 0));
}

#[test]
fn find_prev_retreats_and_wraps() {
    let cells = row("ababab");
    let rows = [srow(&cells, false)];
    let matches = search_rows(&rows, "ab", SearchOptions::case_sensitive());

    // From after the last match -> last.
    assert_eq!(find_prev(&matches, at(0, 5)).unwrap().start, at(0, 4));
    // Strictly-before semantics.
    assert_eq!(find_prev(&matches, at(0, 4)).unwrap().start, at(0, 2));
    // Before the first match wraps to the last.
    assert_eq!(find_prev(&matches, at(0, 0)).unwrap().start, at(0, 4));
}

#[test]
fn find_next_prev_empty_is_none() {
    let matches: Vec<SearchMatch> = Vec::new();
    assert!(find_next(&matches, at(0, 0)).is_none());
    assert!(find_prev(&matches, at(0, 0)).is_none());
}

#[test]
fn matches_are_sorted_ascending_by_start() {
    // Multi-row buffer; confirm the result ordering find_next/prev rely on.
    let c0 = row("xx ab");
    let c1 = row("ab xx");
    let rows = [srow(&c0, false), srow(&c1, false)];
    let matches = search_rows(&rows, "ab", SearchOptions::case_sensitive());
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].start, at(0, 3));
    assert_eq!(matches[1].start, at(1, 0));
    assert!(
        (matches[0].start.row, matches[0].start.column)
            < (matches[1].start.row, matches[1].start.column)
    );
}
