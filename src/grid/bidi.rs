// SPDX-License-Identifier: GPL-3.0-only
//! Per-cell visual placement of a snapshot under bidi display plans.
//!
//! [`BidiDisplayMap`] turns a [`Snapshot`] plus each row's soft-wrap flag into
//! the visual column, resolved level, and presentation (mirrored) character of
//! every cell, using the headless plans of [`crate::core::BidiLayout`]. It is
//! presentation only: cells, cursor, selection, search, and copy stay logical.
//!
//! The renderer, cursor, and pointer consume a map only behind test-only
//! gates; no setting, flag, or menu reaches it, and every production caller
//! renders and hit-tests without one.
//!
//! A paragraph is a run of soft-wrapped rows. [`BidiDisplayMap::plan_with_context`]
//! takes the soft-wrapped rows directly above the snapshot, so a paragraph that
//! begins in scrollback is resolved as a whole; a paragraph longer than the
//! plan's row or owner cap resolves to the identity layout, exactly as
//! [`BidiLayout::plan`] does for any over-cap paragraph.
//! [`BidiDisplayMap::logical_column`] inverts the placement for hit testing,
//! and [`BidiDisplayMap::embedded`] places a content map inside a decorated
//! frame whose chrome rows and columns stay in identity layout.
//!
//! Known limit: a Bidi_Mirrored character with no Bidi_Mirroring_Glyph pair
//! (U+2211, for example) draws unmirrored.
//!
//! Platform-neutral: pure computation, the same on Linux, macOS, and Windows.

use std::ops::Range;

use crate::core::{
    BidiLayout, BidiOwner, Cell, MAX_BIDI_OWNER_WIDTH, MAX_BIDI_PARAGRAPH_OWNERS,
    MAX_BIDI_PARAGRAPH_ROWS, Snapshot, bidi_mirroring_glyph,
};

/// The most rows above a snapshot that can belong to a paragraph the plan
/// would still reorder at `columns`. A paragraph that needs more context rows
/// than this is over the plan's row or owner cap, so it resolves to the
/// identity layout whatever its content. Each row holds at least
/// `columns / MAX_BIDI_OWNER_WIDTH` owners (rounded up).
pub fn max_bidi_context_rows(columns: usize) -> usize {
    let min_owners_per_row = columns.div_ceil(usize::from(MAX_BIDI_OWNER_WIDTH)).max(1);
    MAX_BIDI_PARAGRAPH_ROWS.min(MAX_BIDI_PARAGRAPH_OWNERS / min_owners_per_row)
}

/// The soft-wrapped rows directly above a snapshot that open its first
/// paragraph, top to bottom. Each row's cells may be shorter than the snapshot
/// width (they are padded with blank cells for planning).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BidiParagraphContext {
    pub rows: Vec<Vec<Cell>>,
    /// The paragraph extends further up than [`max_bidi_context_rows`]: it is
    /// over the plan's cap, so the first paragraph keeps the identity layout.
    pub overflow: bool,
}

/// Visual placement of every cell of one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BidiDisplayMap {
    columns: usize,
    rows: usize,
    /// Visual column of each cell, row-major. A wide continuation sits at its
    /// lead's visual column plus its subcell, so a wide owner keeps its
    /// lead-then-continuation arrangement.
    visual: Vec<u32>,
    /// Logical column shown at each visual column, row-major: the inverse of
    /// [`Self::visual`] within each row.
    logical: Vec<u32>,
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
        Self::plan_with_context(snapshot, wrapped, &BidiParagraphContext::default())
    }

    /// [`Self::plan`] with the rows above the snapshot that open its first
    /// paragraph. Only snapshot rows are mapped; context rows only take part
    /// in resolving the paragraph.
    pub fn plan_with_context(
        snapshot: &Snapshot,
        wrapped: &[bool],
        context: &BidiParagraphContext,
    ) -> Self {
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        let cells = rows.saturating_mul(columns).min(snapshot.cells.len());
        let mut map = Self::identity(columns, rows);
        if columns == 0 || cells < rows * columns {
            return map;
        }
        let blank = Cell::default();
        let padded: Vec<Vec<Cell>> = context
            .rows
            .iter()
            .map(|row| {
                let mut row: Vec<Cell> = row.iter().take(columns).copied().collect();
                row.resize(columns, blank);
                row
            })
            .collect();
        let mut first = 0;
        while first < rows {
            let mut last = first;
            while last + 1 < rows && wrapped.get(last).copied().unwrap_or(false) {
                last += 1;
            }
            let mut paragraph: Vec<&[Cell]> = Vec::with_capacity(last + 1 - first);
            if first == 0 {
                if context.overflow {
                    first = last + 1;
                    continue;
                }
                paragraph.extend(padded.iter().map(Vec::as_slice));
            }
            let leading = paragraph.len();
            for row in first..=last {
                paragraph.push(&snapshot.cells[row * columns..(row + 1) * columns]);
            }
            map.plan_paragraph(&paragraph, leading, first);
            first = last + 1;
        }
        map.build_inverse();
        map
    }

    /// The identity placement of a `columns` x `rows` frame.
    pub fn identity(columns: usize, rows: usize) -> Self {
        let cells = rows.saturating_mul(columns);
        Self {
            columns,
            rows,
            visual: (0..cells)
                .map(|index| (index % columns.max(1)) as u32)
                .collect(),
            logical: (0..cells)
                .map(|index| (index % columns.max(1)) as u32)
                .collect(),
            levels: vec![0; cells],
            mirrors: vec![None; cells],
            reordered_rows: vec![false; rows],
        }
    }

    /// This content map placed inside a decorated `columns` x `rows` frame
    /// whose content grid starts at (`row_offset`, `column_offset`). Chrome
    /// cells outside the content grid keep the identity layout; content
    /// columns move only within the content grid. A content grid that does
    /// not fit yields the identity frame.
    pub fn embedded(
        &self,
        columns: usize,
        rows: usize,
        row_offset: usize,
        column_offset: usize,
    ) -> Self {
        let mut frame = Self::identity(columns, rows);
        if row_offset.saturating_add(self.rows) > rows
            || column_offset.saturating_add(self.columns) > columns
        {
            return frame;
        }
        for row in 0..self.rows {
            let frame_row = row + row_offset;
            frame.reordered_rows[frame_row] = self.reordered_rows[row];
            for column in 0..self.columns {
                let source = row * self.columns + column;
                let target = frame_row * columns + column + column_offset;
                frame.visual[target] = self.visual[source] + column_offset as u32;
                frame.logical[target] = self.logical[source] + column_offset as u32;
                frame.levels[target] = self.levels[source];
                frame.mirrors[target] = self.mirrors[source];
            }
        }
        frame
    }

    /// Plan one paragraph of physical `rows`, the first `leading` of which lie
    /// above the snapshot; snapshot row `first_row` is `rows[leading]`.
    fn plan_paragraph(&mut self, rows: &[&[Cell]], leading: usize, first_row: usize) {
        let columns = self.columns;
        let mut text = String::new();
        // (paragraph row, lead column, byte range, width) of each owner in
        // logical order.
        let mut owners: Vec<(usize, usize, Range<usize>, u8)> = Vec::new();
        let mut row_counts = Vec::with_capacity(rows.len());
        for (paragraph_row, cells) in rows.iter().enumerate() {
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
                owners.push((paragraph_row, column, start..text.len(), width));
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
        for (owner, (paragraph_row, lead, bytes, width)) in owners.iter().enumerate() {
            let Some(row) = paragraph_row
                .checked_sub(leading)
                .map(|visible| first_row + visible)
            else {
                continue;
            };
            let Some((plan_row, span)) = plan.owner_visual_span(owner) else {
                continue;
            };
            debug_assert_eq!(plan_row, *paragraph_row);
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
                self.reordered_rows[row] = true;
            }
        }
    }

    /// Fill [`Self::logical`] as the per-row inverse of [`Self::visual`].
    fn build_inverse(&mut self) {
        let columns = self.columns;
        for row in 0..self.rows {
            if !self.reordered_rows[row] {
                continue;
            }
            for column in 0..columns {
                let visual = self.visual[row * columns + column] as usize;
                if visual < columns {
                    self.logical[row * columns + visual] = column as u32;
                }
            }
        }
    }

    /// Return `row` to the identity layout (overlay text drawn over it reads
    /// in logical order).
    pub fn reset_row(&mut self, row: usize) {
        if row >= self.rows {
            return;
        }
        let columns = self.columns;
        for column in 0..columns {
            let index = row * columns + column;
            self.visual[index] = column as u32;
            self.logical[index] = column as u32;
            self.levels[index] = 0;
            self.mirrors[index] = None;
        }
        self.reordered_rows[row] = false;
    }

    /// Reset to identity every row whose characters differ between `planned`
    /// (the snapshot this map was planned from) and `painted` (the same frame
    /// after overlay painters drew over it): overlay text reads in logical
    /// order. Attribute-only changes such as selection and search highlights
    /// keep the placement. Mismatched dimensions reset every row.
    pub fn reset_rows_changed_between(&mut self, planned: &Snapshot, painted: &Snapshot) {
        let columns = self.columns;
        if planned.dimensions != painted.dimensions
            || planned.dimensions.columns != columns
            || planned.dimensions.rows != self.rows
        {
            *self = Self::identity(columns, self.rows);
            return;
        }
        if columns == 0 {
            return;
        }
        for (row, (before, after)) in planned
            .cells
            .chunks(columns)
            .zip(painted.cells.chunks(columns))
            .enumerate()
        {
            let changed = before.iter().zip(after).any(|(a, b)| {
                a.grapheme() != b.grapheme() || a.wide_continuation != b.wide_continuation
            });
            if changed {
                self.reset_row(row);
            }
        }
    }

    /// A hash of the complete placement (dimensions, visual columns, levels,
    /// and mirrors) for render-cache keys: a row whose placement alone changes
    /// changes the hash.
    pub fn placement_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
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

    /// Logical column of the cell drawn at visual (`row`, `visual_column`): the
    /// cell a pointer over that screen column addresses. The column itself
    /// outside the map.
    pub fn logical_column(&self, row: usize, visual_column: usize) -> usize {
        self.index(row, visual_column)
            .map_or(visual_column, |index| self.logical[index] as usize)
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
