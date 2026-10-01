// SPDX-License-Identifier: GPL-3.0-only
//! Aggregate retained-cell budget and windowed row projection.
//!
//! The line cap counts logical lines, so without a cell budget every retained
//! line could be a long wrapped line and memory grew with line length. And a
//! one-row history request used to hydrate every row of the logical line that
//! contains it, so a render of the viewport's top row over a near-limit line
//! rebuilt about a million cells.

use super::prompt_marks::PromptKind;
use super::screen::Line;
use super::scrollback::{HYDRATED_CELLS, Scrollback, resize_lazy};
use super::types::{Attrs, Cell, Dimensions, Position};

const WIDTH: usize = 80;

fn wrapped(ch: char) -> Line {
    Line::wrapped(vec![Cell::new(ch, Attrs::default()); WIDTH])
}

fn closed(ch: char) -> Line {
    Line::unwrapped(vec![Cell::new(ch, Attrs::default()); WIDTH])
}

/// Push one hard-terminated logical line of `rows` full physical rows.
fn push_long_line(sb: &mut Scrollback, ch: char, rows: usize, mark: Option<PromptKind>) {
    for row in 0..rows {
        let mut line = if row + 1 == rows {
            closed(ch)
        } else {
            wrapped(ch)
        };
        if row == 0 {
            line.prompt_mark = mark;
        }
        sb.push_row(line);
    }
}

fn hydrated_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = HYDRATED_CELLS.with(std::cell::Cell::get);
    let value = f();
    (value, HYDRATED_CELLS.with(std::cell::Cell::get) - before)
}

#[test]
fn long_wrapped_lines_are_evicted_by_the_aggregate_cell_budget() {
    // Ten lines allowed; the budget is 10 * 1,024 = 10,240 cells. Each line
    // here is 50 rows of 80 cells = 4,000 cells.
    let mut sb = Scrollback::with_limit(10);
    push_long_line(&mut sb, 'a', 50, Some(PromptKind::PromptStart));
    push_long_line(&mut sb, 'b', 50, Some(PromptKind::PromptStart));
    assert_eq!(sb.logical_len(), 2);
    assert_eq!(sb.retained_cells(), 8_000);
    let epoch = sb.trim_epoch();

    push_long_line(&mut sb, 'c', 50, Some(PromptKind::PromptStart));
    assert_eq!(
        sb.logical_len(),
        2,
        "the oldest line leaves once 12,000 cells exceed the 10,240-cell budget"
    );
    assert_eq!(sb.retained_cells(), 8_000);
    assert_ne!(
        sb.trim_epoch(),
        epoch,
        "budget eviction moves the row origin"
    );

    // What remains is intact: whole lines, their prompt marks, and wraps.
    let rows = sb.physical(WIDTH);
    assert_eq!(rows.len(), 100);
    assert_eq!(rows[0].cells[0].ch, 'b');
    assert_eq!(rows[0].prompt_mark, Some(PromptKind::PromptStart));
    assert_eq!(rows[50].cells[0].ch, 'c');
    assert_eq!(rows[50].prompt_mark, Some(PromptKind::PromptStart));
    assert!(rows[..49].iter().all(|row| row.wrapped));
    assert!(!rows[49].wrapped);
}

#[test]
fn ordinary_history_never_reaches_the_cell_budget() {
    let mut sb = Scrollback::with_limit(10);
    for index in 0..10u8 {
        sb.push_row(closed(char::from(b'0' + index)));
    }
    assert_eq!(sb.logical_len(), 10, "the line cap alone decides");
    assert_eq!(sb.retained_cells(), 800);
}

#[test]
fn the_last_line_is_never_evicted_by_the_budget() {
    // A single open line larger than the budget stays; the per-line ceiling
    // bounds it, not the aggregate budget.
    let mut sb = Scrollback::with_limit(1);
    for _ in 0..40 {
        sb.push_row(wrapped('z'));
    }
    assert_eq!(sb.logical_len(), 1);
    assert_eq!(sb.retained_cells(), 3_200);
}

#[test]
fn unbounded_history_has_no_cell_budget() {
    let mut sb = Scrollback::with_limit(0);
    for ch in ['a', 'b', 'c', 'd'] {
        push_long_line(&mut sb, ch, 50, None);
    }
    assert_eq!(sb.logical_len(), 4);
    assert_eq!(sb.retained_cells(), 16_000);
}

#[test]
fn retained_cell_count_follows_every_mutation() {
    let mut sb = Scrollback::with_limit(3);
    push_long_line(&mut sb, 'a', 3, None);
    sb.push_row(wrapped('b'));
    // Count-cap eviction.
    for ch in ['c', 'd', 'e'] {
        sb.push_row(closed(ch));
    }
    let _ = sb.retained_cells();
    // A resize pulls trailing lines back into the grid and returns overflow.
    let mut grid = vec![closed('g'); 2];
    let _ = resize_lazy(
        &mut sb,
        &mut grid,
        Dimensions::new(40, 4),
        Position::default(),
        false,
    );
    let _ = sb.retained_cells();
    sb.set_limit(1);
    let _ = sb.retained_cells();
    sb.clear();
    assert_eq!(sb.retained_cells(), 0);

    // The per-line ceiling drains the front of an open line past 1,048,576
    // cells (an unbounded store, so the budget is not what trims it).
    let mut sb = Scrollback::with_limit(0);
    for _ in 0..13_200 {
        sb.push_row(wrapped('z'));
    }
    let retained = sb.retained_cells();
    assert!(
        retained < 13_200 * WIDTH,
        "the ceiling drained the open line, leaving {retained} cells"
    );
}

#[test]
fn one_row_requests_over_a_near_limit_line_hydrate_only_that_row() {
    // One open logical line of 13,107 rows * 80 = 1,048,560 cells, just under
    // the 1,048,576-cell per-line ceiling, so nothing is trimmed.
    let mut sb = Scrollback::with_limit(0);
    let rows = 13_107;
    for index in 0..rows {
        sb.push_row(wrapped(if index % 2 == 0 { 'x' } else { 'y' }));
    }
    assert_eq!(sb.physical_len(WIDTH), rows);

    let (tail, cells) = hydrated_during(|| sb.physical_tail(WIDTH, 1));
    assert_eq!(cells, WIDTH, "the tail row alone is hydrated");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].cells[0].ch, 'x');
    assert!(tail[0].wrapped, "an open line's last row stays wrapped");

    let (row, cells) = hydrated_during(|| sb.physical_row(WIDTH, 6_001));
    assert_eq!(cells, WIDTH);
    assert_eq!(row.expect("row exists").cells[0].ch, 'y');

    let (range, cells) = hydrated_during(|| sb.physical_range(WIDTH, 7_000, 3));
    assert_eq!(cells, 3 * WIDTH);
    let chars: Vec<char> = range.iter().map(|row| row.cells[0].ch).collect();
    assert_eq!(chars, ['x', 'y', 'x']);
}

#[test]
fn windowed_rows_match_the_full_projection_across_wide_glyph_wraps() {
    // Wide glyphs at an odd width wrap early and move every later row
    // boundary, so a window must still find exactly the rows a full
    // projection produces.
    let mut cells = Vec::new();
    for index in 0..600 {
        if index % 3 == 0 {
            cells.push(Cell::new('漢', Attrs::default()));
            cells.push(Cell::wide_spacer(Attrs::default()));
        } else {
            cells.push(Cell::new('a', Attrs::default()));
        }
    }
    let mut first = Line::unwrapped(cells);
    first.prompt_mark = Some(PromptKind::OutputStart);
    let sb = Scrollback::from_physical(&[first, closed('t')]);
    for width in [7usize, 9, 13] {
        let full = sb.physical_all(width);
        for (index, expected) in full.iter().enumerate() {
            assert_eq!(sb.physical_row(width, index).as_ref(), Some(expected));
            let tail_len = full.len() - index;
            assert_eq!(sb.physical_tail(width, tail_len).as_slice(), &full[index..]);
            assert_eq!(
                sb.physical_range(width, index, 2).as_slice(),
                &full[index..(index + 2).min(full.len())]
            );
            // A range to the end covers whole later lines, which take the
            // full-projection path rather than the per-row window.
            assert_eq!(
                sb.physical_range(width, index, full.len()).as_slice(),
                &full[index..]
            );
        }
    }
}
