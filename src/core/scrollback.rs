// SPDX-License-Identifier: GPL-3.0-only
//! Logical-line scrollback storage with a lazily-projected physical view.
//!
//! # Why this exists
//!
//! Scrollback used to be a `Vec<Line>` of *physical* rows wrapped to the width
//! that was active when each row scrolled off the top of the visible grid. That
//! made resize O(total scrollback): [`super::reflow::reflow_lines`] rejoined
//! every physical row into logical lines and re-wrapped all of it to the new
//! width, even history the user never looks at (~46 ms at 50k lines).
//!
//! This module stores scrollback as **logical lines** — hard-terminated lines
//! with their soft-wrap runs rejoined — and computes the physical view (what the
//! renderer, search, and `scrollback_len` need) by *projecting* each logical
//! line back to physical rows at the current width. The projection is memoized
//! ([`Projection`]) so repeated reads at a stable width are cheap and only
//! rebuilt when the width changes.
//!
//! [`resize_lazy`] re-wraps only the bottom of the buffer — the trailing logical
//! lines needed to fill the new visible window, plus the live grid — and leaves
//! deep history untouched as logical lines, projected lazily the next time it is
//! read (xterm-style "re-wrap on access"). Resizing while viewing the live tail
//! therefore costs ~O(visible) instead of O(total scrollback).
//!
//! # Correctness strategy
//!
//! [`resize_lazy`] reuses the unchanged [`super::reflow::reflow_lines`] /
//! [`super::reflow::resize_keep_width`] primitives on the bounded subset, so
//! cursor mapping, bottom-anchoring, and trailing-blank collapse are exactly the
//! eager behavior. The differential parity suite proves the lazy result (visible
//! rows, cursor, full physical projection at every offset, and search) is
//! byte/coordinate-identical to running the eager primitive over the whole
//! buffer.
//!
//! # Coordinate contract (unchanged)
//!
//! The physical view preserves the existing absolute-row convention: row 0 is
//! the oldest physical scrollback row, counting down through scrollback into the
//! live grid. Search and selection coordinates are unaffected — see
//! [`super::search`]. No `Snapshot` / `TerminalModel` surface changes.
//!
//! # Single-threaded invariant
//!
//! The projection cache uses [`RefCell`] so the scrollback accessors on
//! [`super::screen::Screen`] stay `&self`. A `Terminal` is driven from a single
//! thread (the front end serializes all access), so the `RefCell` is never
//! borrowed concurrently; `Screen` is `!Sync` as a result, matching its existing
//! usage.

use std::cell::{Ref, RefCell};

use unicode_width::UnicodeWidthChar;

use super::prompt_marks::PromptKind;
use super::reflow::{ReflowOptions, reflow_lines_with_options, resize_keep_width};
use super::screen::Line;
use super::types::{Cell, Dimensions, Position};

/// One logical line: a hard-terminated line whose soft-wrap runs have been
/// rejoined into a single flat cell vector. `open` is true when the line's last
/// physical row was soft-wrapped — the logical line is not yet hard terminated
/// and continues into whatever follows (the next physical row that scrolls off,
/// or the live grid). An open line is only ever the *last* line in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core) struct LogicalLine {
    cells: Vec<Cell>,
    open: bool,
    /// OSC 133 prompt mark of this logical line (SH1), captured from the first
    /// physical row that formed it. Re-stamped onto the first physical row when
    /// the line is projected back to a grid width (see [`project_line_into`]), so
    /// the mark survives scroll-out and re-wrap. `None` for an unmarked line.
    prompt_mark: Option<PromptKind>,
}

/// Memoized physical projection of the logical store at a single width.
#[derive(Debug, Clone)]
struct Projection {
    /// Width the cached rows were wrapped at; `None` means invalid.
    width: Option<usize>,
    rows: Vec<Line>,
}

impl Projection {
    fn empty() -> Self {
        Self {
            width: None,
            rows: Vec::new(),
        }
    }
}

/// Logical-line scrollback with a lazily-(re)built physical projection.
#[derive(Debug, Clone)]
pub(in crate::core) struct Scrollback {
    lines: Vec<LogicalLine>,
    cache: RefCell<Projection>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::core) struct ResizeOptions {
    pub preserve_cursor_physical_line: bool,
    pub cursor_pending_wrap: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core) struct ResizeResult {
    pub cursor: Position,
    pub pending_wrap: bool,
}

impl Scrollback {
    pub(in crate::core) fn new() -> Self {
        Self {
            lines: Vec::new(),
            cache: RefCell::new(Projection::empty()),
        }
    }

    /// Build a store from physical rows (test oracle helper).
    #[cfg(test)]
    pub(in crate::core) fn from_physical(rows: &[Line]) -> Self {
        Self {
            lines: logical_from_physical(rows),
            cache: RefCell::new(Projection::empty()),
        }
    }

    /// Number of physical rows the scrollback projects to at `width`.
    pub(in crate::core) fn physical_len(&self, width: usize) -> usize {
        self.ensure_cache(width);
        self.cache.borrow().rows.len()
    }

    #[cfg(test)]
    pub(in crate::core) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The physical projection at `width` (oldest first), byte-identical to what
    /// eager reflow would store as scrollback. Borrows the memoized cache,
    /// rebuilding it first if the width changed.
    pub(in crate::core) fn physical(&self, width: usize) -> Ref<'_, Vec<Line>> {
        self.ensure_cache(width);
        Ref::map(self.cache.borrow(), |c| &c.rows)
    }

    /// Append one physical row that has just scrolled off the visible grid.
    /// Extends the trailing open logical line when the previous row soft-wrapped,
    /// otherwise starts a new logical line.
    pub(in crate::core) fn push_row(&mut self, row: Line) {
        let wrapped = row.wrapped;
        if let Some(last) = self.lines.last_mut()
            && last.open
        {
            // Continuation of an open logical line: extend cells. A continuation
            // row normally carries no mark, but if the logical line's true first
            // row already scrolled off before the mark was stamped (the mark
            // landed on a still-visible continuation row), adopt it here so the
            // line keeps its first non-`None` mark.
            last.cells.extend(row.cells.iter().copied());
            last.open = wrapped;
            if last.prompt_mark.is_none() {
                last.prompt_mark = row.prompt_mark;
            }
        } else {
            self.lines.push(LogicalLine {
                cells: row.cells,
                open: wrapped,
                prompt_mark: row.prompt_mark,
            });
        }
        self.invalidate();
    }

    /// Clear all scrollback.
    pub(in crate::core) fn clear(&mut self) {
        self.lines.clear();
        self.invalidate();
    }

    /// Whether any stored logical line carries an OSC 133 prompt mark (SH1).
    /// Cheap O(lines) scan over the logical store (no projection), used to keep
    /// the prompt-marks change flag honest on clear/resize.
    pub(in crate::core) fn any_prompt_mark(&self) -> bool {
        self.lines.iter().any(|l| l.prompt_mark.is_some())
    }

    fn invalidate(&self) {
        let mut cache = self.cache.borrow_mut();
        cache.width = None;
        cache.rows.clear();
    }

    fn ensure_cache(&self, width: usize) {
        {
            let cache = self.cache.borrow();
            if cache.width == Some(width) {
                return;
            }
        }
        let rows = project_logical(&self.lines, width);
        let mut cache = self.cache.borrow_mut();
        cache.width = Some(width);
        cache.rows = rows;
    }
}

/// Resize `rows` (the live grid) and `sb` (scrollback) to `new_dims`, re-wrapping
/// only the bottom of the buffer and leaving deep history as logical lines for
/// lazy projection. Returns the cursor's new visible-grid position.
///
/// Reuses the eager reflow primitives on a bounded subset — the trailing logical
/// lines needed to fill the new window plus the live grid — so the visible
/// result, cursor, and the overflow returned to scrollback match the eager path
/// exactly (proven by the differential parity suite). `width_unchanged` selects
/// the O(rows) [`resize_keep_width`] fast path (preserving P1-a) over the general
/// [`reflow_lines`].
#[cfg(test)]
pub(in crate::core) fn resize_lazy(
    sb: &mut Scrollback,
    rows: &mut Vec<Line>,
    new_dims: Dimensions,
    cursor: Position,
    width_unchanged: bool,
) -> Position {
    resize_lazy_with_options(
        sb,
        rows,
        new_dims,
        cursor,
        width_unchanged,
        ResizeOptions::default(),
    )
    .cursor
}

pub(in crate::core) fn resize_lazy_with_options(
    sb: &mut Scrollback,
    rows: &mut Vec<Line>,
    new_dims: Dimensions,
    cursor: Position,
    width_unchanged: bool,
    options: ResizeOptions,
) -> ResizeResult {
    let new_rows = new_dims.rows;
    let new_width = new_dims.columns;

    // Pull trailing logical lines into the re-wrap subset: enough to fill the new
    // window, always including the open tail (which continues into the live
    // grid), and through any trailing blank run so trailing-blank collapse
    // matches the eager oracle exactly.
    let mut pulled: Vec<LogicalLine> = Vec::new();
    let mut pulled_rows = 0usize;
    loop {
        let need_more = pulled_rows < new_rows;
        let extend_blank =
            pulled_rows >= new_rows && sb.lines.last().is_some_and(|l| cells_all_blank(&l.cells));
        if !(need_more || extend_blank) {
            break;
        }
        match sb.lines.pop() {
            Some(line) => {
                pulled_rows += count_projected_rows(&line.cells, new_width, line.open);
                pulled.push(line);
            }
            None => break,
        }
    }
    pulled.reverse();

    // Build the subset fed to the (unchanged) reflow primitive.
    let mut subset: Vec<Line> = Vec::new();
    if width_unchanged {
        // Project at the unchanged width: full-width rows, no mid-line padding
        // (open lines are exact multiples of the width), so the keep-width fast
        // path's well-formedness assumption holds.
        subset = project_logical(&pulled, new_width);
    } else {
        // One mega-row per logical line (all cells, marked open/closed). The
        // reflow primitive rejoins by the wrapped flag — cell count is
        // irrelevant — so no projection/padding is needed and an open line joins
        // to the live grid without inserted blanks.
        for line in &pulled {
            let mut mega = if line.open {
                Line::wrapped(line.cells.clone())
            } else {
                Line::unwrapped(line.cells.clone())
            };
            // Carry the prompt mark onto the mega-row so `reflow_lines` re-anchors
            // it onto the first re-wrapped physical row.
            mega.prompt_mark = line.prompt_mark;
            subset.push(mega);
        }
    }
    let cursor_prefix = subset.len();
    subset.append(rows);
    let cursor_in = Position {
        row: cursor_prefix + cursor.row,
        column: cursor.column,
    };

    // Reflow the subset; `overflow` is the part above the new window that returns
    // to scrollback, `subset` becomes the new visible window.
    let mut overflow: Vec<Line> = Vec::new();
    let result = if width_unchanged {
        let cursor = resize_keep_width(&mut overflow, &mut subset, new_dims, cursor_in);
        ResizeResult {
            cursor,
            pending_wrap: options.cursor_pending_wrap && cursor.column == new_width - 1,
        }
    } else {
        let reflow = reflow_lines_with_options(
            &mut overflow,
            &mut subset,
            new_dims,
            cursor_in,
            ReflowOptions {
                preserve_cursor_physical_line: options.preserve_cursor_physical_line,
                cursor_pending_wrap: options.cursor_pending_wrap,
            },
        );
        ResizeResult {
            cursor: reflow.cursor,
            pending_wrap: reflow.pending_wrap,
        }
    };

    *rows = subset;
    // Remaining sb.lines are all hard-terminated (the open tail was pulled), so
    // appending the overflow rows merges into logical lines correctly.
    for row in overflow {
        sb.push_row(row);
    }
    sb.invalidate();
    result
}

/// Rebuild logical lines from physical rows (the inverse of [`project_logical`]).
/// Consecutive rows are joined into one logical line until a non-`wrapped`
/// (hard-terminated) row ends it; a trailing run that ends on a `wrapped` row
/// becomes an `open` logical line.
#[cfg(test)]
pub(in crate::core) fn logical_from_physical(rows: &[Line]) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    let mut current: Vec<Cell> = Vec::new();
    let mut current_mark: Option<PromptKind> = None;
    for row in rows {
        if current.is_empty() {
            // First physical row of a new logical line: capture its mark.
            current_mark = row.prompt_mark;
        } else if current_mark.is_none() {
            // Adopt a mark stamped on a continuation row when the first row
            // carried none (first non-`None` mark in the logical line wins).
            current_mark = row.prompt_mark;
        }
        current.extend(row.cells.iter().copied());
        if !row.wrapped {
            lines.push(LogicalLine {
                cells: std::mem::take(&mut current),
                open: false,
                prompt_mark: current_mark.take(),
            });
        }
    }
    if !current.is_empty() {
        lines.push(LogicalLine {
            cells: current,
            open: true,
            prompt_mark: current_mark,
        });
    }
    lines
}

/// Project logical lines to physical rows at `width`.
pub(in crate::core) fn project_logical(lines: &[LogicalLine], width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for line in lines {
        project_line_into(&line.cells, width, line.open, line.prompt_mark, &mut out);
    }
    out
}

fn cells_all_blank(cells: &[Cell]) -> bool {
    let plain = Cell::blank();
    cells.iter().all(|c| *c == plain)
}

fn count_projected_rows(cells: &[Cell], width: usize, open: bool) -> usize {
    let mut tmp = Vec::new();
    project_line_into(cells, width, open, None, &mut tmp);
    tmp.len()
}

/// Project one logical line's cells to physical rows at `width`, appending to
/// `out`. Mirrors the per-logical-line wrapping rules in
/// [`super::reflow::reflow_lines`] exactly:
///
/// - Trailing blank cells are trimmed before wrapping (a no-op for `open` lines,
///   which are full by construction), then the final row is re-padded to width.
/// - Wide glyphs are kept whole: a two-column glyph never straddles the right
///   edge; if the grid is too narrow for a pair the lead degrades to width 1 and
///   an orphaned continuation spacer is dropped.
/// - Every row except the last is marked `wrapped`. The last row is marked
///   `wrapped` iff the logical line is `open`, otherwise it is hard-terminated.
/// - `mark` (the logical line's OSC 133 prompt mark, SH1) is stamped onto the
///   FIRST physical row produced; continuation rows keep their default `None`.
fn project_line_into(
    cells: &[Cell],
    width: usize,
    open: bool,
    mark: Option<PromptKind>,
    out: &mut Vec<Line>,
) {
    let plain = Cell::blank();
    // Index of this logical line's first physical row in `out`; the prompt mark
    // is re-anchored here after the rows are produced. A logical line always
    // produces at least one row, so this index is always valid afterward.
    let first_row = out.len();

    // Trim trailing plain blanks (matches reflow). Open lines carry none.
    let mut keep = cells.len();
    while keep > 0 && cells[keep - 1] == plain {
        keep -= 1;
    }
    let cells = &cells[..keep];

    let mut row_cells: Vec<Cell> = Vec::with_capacity(width);
    let mut produced_any = false;
    let mut i = 0;
    while i < cells.len() {
        let cell = cells[i];
        let is_wide_lead = !cell.wide_continuation && UnicodeWidthChar::width(cell.ch) == Some(2);
        let unit = if is_wide_lead && width >= 2 { 2 } else { 1 };

        // Wrap before a wide pair that would straddle the right edge.
        if unit == 2 && row_cells.len() + unit > width && !row_cells.is_empty() {
            while row_cells.len() < width {
                row_cells.push(plain);
            }
            out.push(Line::wrapped(std::mem::take(&mut row_cells)));
            produced_any = true;
            row_cells = Vec::with_capacity(width);
        }

        if unit == 2 {
            row_cells.push(cell);
            let cont = if i + 1 < cells.len() && cells[i + 1].wide_continuation {
                cells[i + 1]
            } else {
                Cell::wide_spacer(cell.attrs)
            };
            row_cells.push(cont);
            i += if i + 1 < cells.len() && cells[i + 1].wide_continuation {
                2
            } else {
                1
            };
        } else {
            // Drop an orphaned continuation cell (its lead was degraded).
            if !cell.wide_continuation {
                row_cells.push(cell);
            }
            i += 1;
        }

        if row_cells.len() >= width {
            out.push(Line::wrapped(std::mem::take(&mut row_cells)));
            produced_any = true;
            row_cells = Vec::with_capacity(width);
        }
    }

    if !row_cells.is_empty() || !produced_any {
        while row_cells.len() < width {
            row_cells.push(plain);
        }
        out.push(if open {
            Line::wrapped(row_cells)
        } else {
            Line::unwrapped(row_cells)
        });
    } else if let Some(last) = out.last_mut() {
        // Content filled exactly to a wrap boundary: the final row's marker is
        // the line's continuation state.
        last.wrapped = open;
    }

    // Re-anchor the prompt mark onto this logical line's first physical row.
    if let Some(first) = out.get_mut(first_row) {
        first.prompt_mark = mark;
    }
}
