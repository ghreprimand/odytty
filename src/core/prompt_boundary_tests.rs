// SPDX-License-Identifier: GPL-3.0-only
//! Command-end boundaries carried by prompt marks follow their cell when a
//! logical line is relaid at another width, including when layout padding
//! appears or disappears, and when the mark was stamped on a continuation row.

use super::prompt_marks::PromptKind;
use super::screen::Line;
use super::scrollback::{Scrollback, logical_from_physical, project_logical};
use super::types::{Attrs, Cell};

fn cell(ch: char) -> Cell {
    Cell::new(ch, Attrs::default())
}

/// `AB` and a padded edge at width 3, then the wide owner and `$` on the next
/// row: the command end sits before `$`, five cells into the line.
fn padded_line() -> Vec<Line> {
    let mut first = Line::wrapped(vec![
        cell('A'),
        cell('B'),
        Cell::layout_blank(Attrs::default()),
    ]);
    first.prompt_mark = Some(PromptKind::CommandEndAt {
        exit: Some(0),
        logical_offset: 5,
    });
    let second = Line::unwrapped(vec![
        cell('\u{6f22}'),
        Cell::wide_spacer(Attrs::default()),
        cell('$'),
    ]);
    vec![first, second]
}

fn end_offset(kind: Option<PromptKind>) -> Option<u32> {
    kind.and_then(PromptKind::boundary_offset)
}

#[test]
fn projection_without_padding_moves_the_boundary_back_by_the_padding() {
    let lines = logical_from_physical(&padded_line());
    let wide = project_logical(lines.iter(), 6);
    // `AB` then the two-cell owner: `$` now starts at column 4.
    assert_eq!(end_offset(wide[0].prompt_mark), Some(4));
    // Projected back at the original width, the padding returns with it.
    let narrow = project_logical(lines.iter(), 3);
    assert_eq!(end_offset(narrow[0].prompt_mark), Some(5));
}

#[test]
fn scrollback_mark_queries_agree_with_the_projection() {
    let mut scrollback = Scrollback::new();
    for row in padded_line() {
        scrollback.push_row(row);
    }
    assert_eq!(end_offset(scrollback.prompt_mark_at(6, 0)), Some(4));
    let rows = scrollback.prompt_mark_rows(6);
    assert_eq!(rows.len(), 1);
    assert_eq!(end_offset(Some(rows[0].1)), Some(4));
}

#[test]
fn a_mark_adopted_from_a_continuation_row_measures_from_the_line_start() {
    let mut rows = vec![
        Line::wrapped(vec![cell('a'), cell('b'), cell('c')]),
        Line::unwrapped(vec![cell('d'), cell('$'), Cell::blank()]),
    ];
    // Stamped while only the continuation row was visible: one cell into it.
    rows[1].prompt_mark = Some(PromptKind::CommandEndAt {
        exit: None,
        logical_offset: 1,
    });

    let lines = logical_from_physical(&rows);
    let projected = project_logical(lines.iter(), 3);
    assert_eq!(end_offset(projected[0].prompt_mark), Some(4));

    let mut scrollback = Scrollback::new();
    for row in rows {
        scrollback.push_row(row);
    }
    assert_eq!(end_offset(scrollback.prompt_mark_at(3, 0)), Some(4));
}
