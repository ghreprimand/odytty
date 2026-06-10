//! Scrollback storage plus the logical-line projection machinery that the lazy
//! resize path (commit C2) is built on.
//!
//! # Two-commit plan
//!
//! Scrollback is a sequence of *physical* rows wrapped to the width that was
//! active when each row scrolled off the top of the visible grid. Resize is
//! therefore O(total scrollback): [`super::reflow::reflow_lines`] rejoins every
//! physical row into logical lines and re-wraps all of it to the new width, even
//! history the user never looks at (~46 ms at 50k lines).
//!
//! - **C1 (this commit):** [`Scrollback`] wraps the physical `Vec<Line>` so the
//!   storage seam is established and `Screen` no longer touches a raw `Vec`. The
//!   resize primitives keep operating on the physical rows in place, so behavior
//!   *and performance* are byte-for-byte and microsecond-for-microsecond
//!   identical to before (the P1-a width-unchanged fast path is fully
//!   preserved). Alongside it this module lands the *logical-line projection*
//!   ([`logical_from_physical`] + [`project_logical`]) with a differential
//!   parity suite proving the projection reproduces eager-reflow output exactly.
//!   These are not yet on the hot path — they are the validated foundation for
//!   C2.
//! - **C2 (next commit):** flip [`Scrollback`] to store logical lines as the
//!   source of truth with a memoized physical projection, and make resize
//!   re-wrap only the visible tail while deferring history — turning the
//!   O(total) width-change cost into ~O(visible).
//!
//! # Coordinate contract (unchanged)
//!
//! The physical view preserves the existing absolute-row convention: row 0 is
//! the oldest physical scrollback row, counting down through scrollback into the
//! live grid. Search and selection coordinates are unaffected — see
//! [`super::search`]. No `Snapshot` / `TerminalModel` surface changes.

use unicode_width::UnicodeWidthChar;

use super::screen::Line;
use super::types::Cell;

/// Scrollback storage. In C1 this is a thin wrapper over the physical rows; C2
/// replaces the backing store with logical lines + a projection cache without
/// changing this type's method surface used by [`super::screen::Screen`].
#[derive(Debug, Clone)]
pub(in crate::core) struct Scrollback {
    physical: Vec<Line>,
}

impl Scrollback {
    pub(in crate::core) fn new() -> Self {
        Self {
            physical: Vec::new(),
        }
    }

    /// Number of physical scrollback rows.
    pub(in crate::core) fn len(&self) -> usize {
        self.physical.len()
    }

    // Rounds out the `len` API (clippy `len_without_is_empty`); used by tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::core) fn is_empty(&self) -> bool {
        self.physical.is_empty()
    }

    /// The physical scrollback rows (oldest first) for read-only access by the
    /// snapshot and search bridges.
    pub(in crate::core) fn physical(&self) -> &[Line] {
        &self.physical
    }

    /// Mutable access to the physical rows for the resize primitives, which
    /// re-window/re-wrap in place exactly as before.
    pub(in crate::core) fn physical_mut(&mut self) -> &mut Vec<Line> {
        &mut self.physical
    }

    /// Append one physical row that has just scrolled off the visible grid.
    pub(in crate::core) fn push_row(&mut self, row: Line) {
        self.physical.push(row);
    }

    /// Clear all scrollback.
    pub(in crate::core) fn clear(&mut self) {
        self.physical.clear();
    }
}

/// One logical line: a hard-terminated line whose soft-wrap runs have been
/// rejoined into a single flat cell vector. `open` is true when the line's last
/// physical row was soft-wrapped — i.e. the logical line is not yet hard
/// terminated and continues into whatever follows (the next physical row that
/// scrolls off, or the live grid). An open line is only ever the *last* line in
/// a store.
///
/// Foundation for the C2 lazy projection; exercised now by the parity suite.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(in crate::core) struct LogicalLine {
    cells: Vec<Cell>,
    open: bool,
}

/// Rebuild logical lines from physical rows (the inverse of [`project_logical`]).
/// Consecutive rows are joined into one logical line until a non-`wrapped`
/// (hard-terminated) row ends it; a trailing run that ends on a `wrapped` row
/// becomes an `open` logical line.
///
/// Not yet wired into [`Scrollback`] (C2); validated by the parity suite.
#[cfg_attr(not(test), allow(dead_code))]
pub(in crate::core) fn logical_from_physical(rows: &[Line]) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    let mut current: Vec<Cell> = Vec::new();
    for row in rows {
        current.extend(row.cells.iter().copied());
        if !row.wrapped {
            lines.push(LogicalLine {
                cells: std::mem::take(&mut current),
                open: false,
            });
        }
    }
    if !current.is_empty() {
        // Trailing rows ended on a soft-wrap: an open logical line.
        lines.push(LogicalLine {
            cells: current,
            open: true,
        });
    }
    lines
}

/// Project logical lines to physical rows at `width` — the inverse of
/// [`logical_from_physical`]. Reproduces eager-reflow output exactly so the C2
/// switch is behavior-identical (proven by the parity suite).
#[cfg_attr(not(test), allow(dead_code))]
pub(in crate::core) fn project_logical(lines: &[LogicalLine], width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for line in lines {
        project_line_into(&line.cells, width, line.open, &mut out);
    }
    out
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
///   `wrapped` iff the logical line is `open` (still continues), otherwise it is
///   hard-terminated (`unwrapped`).
#[cfg_attr(not(test), allow(dead_code))]
fn project_line_into(cells: &[Cell], width: usize, open: bool, out: &mut Vec<Line>) {
    let plain = Cell::blank();

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
}
