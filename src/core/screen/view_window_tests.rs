// SPDX-License-Identifier: GPL-3.0-only
//! The windowed row accessors project only the rows they return:
//! [`Screen::visible_search_rows`] and [`Screen::paragraph_context_rows`]
//! match the whole-tail projection they replaced, row for row, across widths,
//! paragraph lengths around the context caps, offsets, caps, wide
//! characters, a reflowing resize, ambiguous width, and the alternate screen.

use super::*;
use crate::core::Terminal;

/// The whole-tail form of [`Screen::visible_search_rows`].
fn reference_visible(screen: &Screen, offset_rows: usize) -> Vec<VisibleRow> {
    let height = screen.dimensions.rows;
    let columns = screen.dimensions.columns;
    let scrollback_len = screen.scrollback.physical_len(columns);
    let offset = offset_rows.min(scrollback_len);
    let tail = screen.scrollback.physical_tail(columns, offset);
    tail.iter()
        .chain(screen.rows.iter())
        .take(height)
        .map(|line| VisibleRow {
            cells: line.cells.clone(),
            wrapped: line.wrapped,
            paragraph_bidi: line.paragraph_bidi,
        })
        .collect()
}

/// The whole-tail form of [`Screen::paragraph_context_rows`].
fn reference_context(
    screen: &Screen,
    offset_rows: usize,
    max_rows: usize,
) -> (Vec<VisibleRow>, bool) {
    let columns = screen.dimensions.columns;
    let scrollback_len = screen.scrollback.physical_len(columns);
    let offset = offset_rows.min(scrollback_len);
    let above = if offset == 0 && screen.primary_screen.is_some() {
        0
    } else {
        scrollback_len - offset
    };
    let wanted = above.min(max_rows.saturating_add(1));
    if wanted == 0 {
        return (Vec::new(), false);
    }
    let mut tail = screen.scrollback.physical_tail(columns, offset + wanted);
    tail.truncate(tail.len().saturating_sub(offset));
    let continuing = tail.iter().rev().take_while(|line| line.wrapped).count();
    let overflow = continuing > max_rows;
    let start = tail.len() - continuing.min(max_rows);
    let rows = if overflow {
        Vec::new()
    } else {
        tail[start..]
            .iter()
            .map(|line| VisibleRow {
                cells: line.cells.clone(),
                wrapped: line.wrapped,
                paragraph_bidi: line.paragraph_bidi,
            })
            .collect()
    };
    (rows, overflow)
}

const CAPS: [usize; 12] = [0, 1, 2, 5, 31, 32, 33, 63, 64, 65, 200, 10_000];

/// Paragraphs whose row counts at `columns` straddle the context caps, with
/// wide and right-to-left text.
fn feed(terminal: &mut Terminal, columns: usize) {
    for rows in [0, 1, 3, 31, 32, 33, 34, 63, 64, 65, 130, 2, 0, 300] {
        let mut text = String::new();
        let cells = rows * columns;
        let mut used = 0;
        let mut index = 0;
        while used < cells {
            match index % 5 {
                0 => {
                    text.push('\u{4e2d}');
                    used += 2;
                }
                1 => {
                    text.push('\u{5d0}');
                    used += 1;
                }
                2 => {
                    text.push('\u{b7}');
                    used += 1;
                }
                _ => {
                    text.push('a');
                    used += 1;
                }
            }
            index += 1;
        }
        text.push_str("\r\n");
        terminal.advance(text.as_bytes());
    }
}

fn offsets(scrollback_len: usize) -> Vec<usize> {
    (0..=scrollback_len + 2)
        .filter(|offset| *offset < 70 || offset % 13 == 0 || offset + 70 > scrollback_len)
        .collect()
}

fn assert_matches(terminal: &Terminal, label: &str) -> usize {
    let screen = terminal.screen();
    let columns = screen.dimensions.columns;
    let scrollback_len = screen.scrollback.physical_len(columns);
    for offset in offsets(scrollback_len) {
        assert_eq!(
            screen.visible_search_rows(offset),
            reference_visible(screen, offset),
            "{label}: visible rows at offset {offset}"
        );
        for cap in CAPS {
            assert_eq!(
                screen.paragraph_context_rows(offset, cap),
                reference_context(screen, offset, cap),
                "{label}: context at offset {offset}, cap {cap}"
            );
        }
    }
    scrollback_len
}

#[test]
fn windowed_rows_match_the_whole_tail_projection() {
    for (columns, rows) in [(7, 3), (13, 5), (80, 24)] {
        let mut terminal = Terminal::new(columns, rows);
        feed(&mut terminal, columns);
        let history = assert_matches(&terminal, &format!("{columns}x{rows}"));
        assert!(history > 300, "{columns}x{rows}: history to window into");
    }
}

#[test]
fn windowed_rows_match_after_a_reflowing_resize() {
    let mut terminal = Terminal::new(13, 5);
    feed(&mut terminal, 13);
    terminal.resize(9, 4);
    assert_matches(&terminal, "13x5 resized to 9x4");
    terminal.resize(21, 6);
    assert_matches(&terminal, "9x4 resized to 21x6");
}

#[test]
fn windowed_rows_match_with_ambiguous_wide() {
    let mut terminal = Terminal::new(11, 4);
    terminal.set_ambiguous_wide(true);
    feed(&mut terminal, 11);
    assert_matches(&terminal, "11x4 ambiguous wide");
}

#[test]
fn windowed_rows_match_on_the_alternate_screen() {
    let mut terminal = Terminal::new(9, 4);
    feed(&mut terminal, 9);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance("x".repeat(40).as_bytes());
    assert_eq!(assert_matches(&terminal, "9x4 alternate screen"), 0);
    assert_eq!(
        terminal.screen().paragraph_context_rows(0, 10_000),
        (Vec::new(), false),
        "the live alternate screen has no rows above it"
    );
}
