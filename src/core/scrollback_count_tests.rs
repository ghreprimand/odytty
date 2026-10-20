// SPDX-License-Identifier: GPL-3.0-only
//! The row count of a logical line taken without projecting it matches the
//! projection's row count for every line shape, at every width.

use super::*;
use crate::core::types::{Attrs, Color};

/// A deterministic stream of row shapes: printable ASCII, trailing blanks,
/// controls and non-ASCII narrow scalars, wide owners with their
/// continuation, layout padding, combining marks, and wrapped runs. Cells
/// carry default or non-default attributes, and a row's trailing blanks may
/// have a background colour, which is the case where trimming decides the
/// count.
fn rows(seed: u64, count: usize, width: usize) -> Vec<Line> {
    let mut state = seed;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let mut out = Vec::new();
    for _ in 0..count {
        let mut cells = Vec::new();
        let kind = next() % 8;
        let attrs = |pick: usize| {
            let mut attrs = Attrs::default();
            match pick % 6 {
                0 => attrs.background = Color::Indexed(4),
                1 => attrs.set_bold(true),
                2 => attrs.set_underline(true),
                3 => attrs.foreground = Color::Rgb(200, 10, 10),
                _ => {}
            }
            attrs
        };
        let len = next() % (width + 1);
        while cells.len() < len {
            let pick = next() % 16;
            match (kind, pick) {
                (0..=3, _) | (_, 0..=9) => {
                    let ch = (b' ' + (next() % 95) as u8) as char;
                    cells.push(Cell::new(ch, attrs(next())));
                }
                (_, 10) if cells.len() + 2 <= width => {
                    cells.push(Cell::new('\u{4e16}', Attrs::default()));
                    cells.push(Cell::wide_spacer(Attrs::default()));
                }
                (_, 11) => cells.push(Cell::layout_blank(Attrs::default())),
                (_, 12) => {
                    let mut cell = Cell::new('e', Attrs::default());
                    cell.push_combining('\u{0301}');
                    cells.push(cell);
                }
                (_, 13) => cells.push(Cell::new('\u{e9}', Attrs::default())),
                (_, 14) => cells.push(Cell::new('\u{2502}', Attrs::default())),
                _ if next() % 2 == 0 => cells.push(Cell::new(' ', attrs(0))),
                _ => cells.push(Cell::blank()),
            }
        }
        cells.truncate(width);
        // Some rows end in coloured blanks, which are not trimmed.
        let tail = if next() % 4 == 0 {
            Cell::new(' ', attrs(0))
        } else {
            Cell::blank()
        };
        while cells.len() < width {
            cells.push(tail);
        }
        out.push(if next() % 3 == 0 {
            Line::wrapped(cells)
        } else {
            Line::unwrapped(cells)
        });
    }
    out
}

#[test]
fn counted_rows_match_the_projection_for_every_line_shape() {
    let mut scratch = Vec::new();
    let mut fast = 0usize;
    let mut coloured_tail = 0usize;
    let plain = StoredCell::from_cell(&Cell::blank());
    for seed in 0..40u64 {
        let source_width = 4 + (seed as usize % 9) * 10;
        for line in logical_from_physical(&rows(seed, 60, source_width)) {
            for width in 1..=120 {
                for ambiguous_wide in [false, true] {
                    let mut projected = Vec::new();
                    project_line_into(
                        line.counting_view(),
                        width,
                        line.open,
                        NO_ROWS,
                        &mut projected,
                        ambiguous_wide,
                    );
                    let counted = count_line_rows(&line, width, ambiguous_wide, &mut scratch);
                    assert_eq!(
                        counted,
                        projected.len(),
                        "seed {seed} width {width} ambiguous {ambiguous_wide}: {:?}",
                        line.cells
                    );
                }
            }
            if line.marks.is_empty()
                && line
                    .cells
                    .iter()
                    .all(|cell| matches!(cell.ch(), ' '..='~') && !cell.layout_padding())
            {
                fast += 1;
                if line
                    .cells
                    .last()
                    .is_some_and(|cell| cell.ch() == ' ' && *cell != plain)
                {
                    coloured_tail += 1;
                }
            }
        }
    }
    // Both paths ran: many lines take the count without projecting.
    assert!(fast > 100, "only {fast} lines took the counting path");
    // Some of them end in a coloured blank, which the trim must keep.
    assert!(
        coloured_tail > 10,
        "only {coloured_tail} counted lines end in a coloured blank"
    );
}
