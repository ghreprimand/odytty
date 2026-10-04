// SPDX-License-Identifier: GPL-3.0-only
//! Per-cell visual placement of a snapshot under bidi display plans.
//!
//! [`BidiDisplayMap`] turns a [`Snapshot`] plus each row's soft-wrap flag into
//! the visual column, resolved level, and presentation (mirrored) character of
//! every cell, using the headless plans of [`crate::core::BidiLayout`]. It is
//! presentation only: cells, cursor, selection, search, and copy stay logical.
//!
//! The renderer consumes a map only through a test-only entry point; no
//! setting, flag, or menu reaches it, and every production caller renders
//! without one. Known limits of this seam, which interaction work must close
//! before any user option exists:
//!
//! - A paragraph is the run of soft-wrapped rows inside the snapshot. A
//!   paragraph whose first rows lie above the snapshot (in scrollback) is
//!   resolved from its visible rows only.
//! - A Bidi_Mirrored character with no Bidi_Mirroring_Glyph pair (U+2211, for
//!   example) draws unmirrored.
//! - Cursor, selection highlight geometry, pointer hit testing, and search
//!   highlights are not mapped here.
//!
//! Platform-neutral: pure computation, the same on Linux, macOS, and Windows.

use std::ops::Range;

use crate::core::{BidiLayout, BidiOwner, Snapshot, bidi_mirroring_glyph};

/// Visual placement of every cell of one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidiDisplayMap {
    columns: usize,
    rows: usize,
    /// Visual column of each cell, row-major. A wide continuation sits at its
    /// lead's visual column plus its subcell, so a wide owner keeps its
    /// lead-then-continuation arrangement.
    visual: Vec<u32>,
    /// Resolved level of each cell (a continuation copies its lead).
    levels: Vec<u8>,
    /// Presentation character for a mirrored lead cell.
    mirrors: Vec<Option<char>>,
    /// Whether each row's placement differs from the identity layout.
    reordered_rows: Vec<bool>,
}

impl BidiDisplayMap {
    /// Plan every paragraph of `snapshot`. `wrapped[row]` is the row's
    /// soft-wrap flag: `true` when its content continues on the next row, as
    /// `Line::wrapped` records it. Missing flags count as hard line ends.
    pub fn plan(snapshot: &Snapshot, wrapped: &[bool]) -> Self {
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        let cells = rows.saturating_mul(columns).min(snapshot.cells.len());
        let mut map = Self {
            columns,
            rows,
            visual: (0..cells)
                .map(|index| (index % columns.max(1)) as u32)
                .collect(),
            levels: vec![0; cells],
            mirrors: vec![None; cells],
            reordered_rows: vec![false; rows],
        };
        if columns == 0 || cells < rows * columns {
            return map;
        }
        let mut first = 0;
        while first < rows {
            let mut last = first;
            while last + 1 < rows && wrapped.get(last).copied().unwrap_or(false) {
                last += 1;
            }
            map.plan_paragraph(snapshot, first..last + 1);
            first = last + 1;
        }
        map
    }

    fn plan_paragraph(&mut self, snapshot: &Snapshot, rows: Range<usize>) {
        let columns = self.columns;
        let mut text = String::new();
        // (row, lead column, byte range, width) of each owner in logical order.
        let mut owners: Vec<(usize, usize, Range<usize>, u8)> = Vec::new();
        let mut row_counts = Vec::with_capacity(rows.len());
        for row in rows.clone() {
            let cells = &snapshot.cells[row * columns..(row + 1) * columns];
            let before = owners.len();
            let mut column = 0;
            while column < columns {
                let start = text.len();
                text.push_str(&cells[column].grapheme());
                let mut width = 1usize;
                while column + width < columns && cells[column + width].wide_continuation {
                    width += 1;
                }
                // A width beyond the plan's cap makes the paragraph malformed,
                // which yields the identity layout.
                let width = u8::try_from(width).unwrap_or(u8::MAX);
                owners.push((row, column, start..text.len(), width));
                column += usize::from(width.max(1));
            }
            row_counts.push(owners.len() - before);
        }
        let plan_owners: Vec<BidiOwner<'_>> = owners
            .iter()
            .map(|(_, _, bytes, width)| BidiOwner {
                text: &text[bytes.clone()],
                width: *width,
            })
            .collect();
        let BidiLayout::Reordered(plan) = BidiLayout::plan(&plan_owners, &row_counts) else {
            return;
        };
        for (owner, (row, lead, bytes, width)) in owners.iter().enumerate() {
            let Some((plan_row, span)) = plan.owner_visual_span(owner) else {
                continue;
            };
            debug_assert_eq!(rows.start + plan_row, *row);
            let level = plan.owner_level(owner).unwrap_or(0);
            for subcell in 0..usize::from(*width) {
                let index = row * columns + lead + subcell;
                self.visual[index] = (span.start + subcell) as u32;
                self.levels[index] = level;
            }
            if plan.is_mirrored(owner) {
                self.mirrors[row * columns + lead] = text[bytes.clone()]
                    .chars()
                    .next()
                    .and_then(bidi_mirroring_glyph);
            }
            if span.start != *lead || level != 0 {
                self.reordered_rows[*row] = true;
            }
        }
    }

    /// Whether any row differs from the identity layout.
    pub fn is_identity(&self) -> bool {
        !self.reordered_rows.iter().any(|row| *row)
    }

    /// Whether `row` differs from the identity layout: an owner moved, or an
    /// owner resolved to a non-zero level.
    pub fn row_is_reordered(&self, row: usize) -> bool {
        self.reordered_rows.get(row).copied().unwrap_or(false)
    }

    /// Visual column of the cell at logical (`row`, `column`); the logical
    /// column itself outside the map.
    pub fn visual_column(&self, row: usize, column: usize) -> usize {
        self.index(row, column)
            .map_or(column, |index| self.visual[index] as usize)
    }

    /// Resolved level of the cell at logical (`row`, `column`).
    pub fn level(&self, row: usize, column: usize) -> u8 {
        self.index(row, column)
            .map_or(0, |index| self.levels[index])
    }

    /// The character to draw for a mirrored lead cell, if any.
    pub fn mirrored_char(&self, row: usize, column: usize) -> Option<char> {
        self.index(row, column)
            .and_then(|index| self.mirrors[index])
    }

    /// The visual columns covered by logical columns `columns` of `row` when
    /// they all share one resolved level, which keeps them adjacent on screen
    /// in one direction. `None` for a mixed-level or out-of-range span.
    pub fn uniform_visual_span(&self, row: usize, columns: Range<usize>) -> Option<Range<usize>> {
        if columns.is_empty() || columns.end > self.columns || row >= self.rows {
            return None;
        }
        let level = self.level(row, columns.start);
        let mut low = usize::MAX;
        let mut high = 0;
        for column in columns {
            if self.level(row, column) != level {
                return None;
            }
            let visual = self.visual_column(row, column);
            low = low.min(visual);
            high = high.max(visual + 1);
        }
        Some(low..high)
    }

    fn index(&self, row: usize, column: usize) -> Option<usize> {
        (row < self.rows && column < self.columns).then(|| row * self.columns + column)
    }
}
