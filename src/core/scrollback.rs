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
//! line back to physical rows at the current width.
//!
//! What is memoized is the projection's **shape**, not the projection
//! ([`Projection`]): each logical line's first physical row index at the
//! current width. Width changes rebuild it; output appends and front eviction
//! update it incrementally.
//! Rows themselves are produced on demand. Memoizing the rows instead meant
//! retaining a full second physical copy of the store — measured at parity with
//! the logical ring at depth, so a deep scrollback was paid for twice — while
//! every per-frame consumer reads only a viewport-sized tail of it.
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
//! The projection-shape cache uses [`RefCell`] so the scrollback accessors on
//! [`super::screen::Screen`] stay `&self`. A `Terminal` is driven from a single
//! thread (the front end serializes all access), so the `RefCell` is never
//! borrowed concurrently; `Screen` is `!Sync` as a result, matching its existing
//! usage.

use std::cell::RefCell;
use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

use super::button::{ButtonId, ButtonSpan, MAX_BUTTON_SPANS_PER_LINE, SpanReprojector};
use super::prompt_marks::PromptKind;
use super::reflow::{ReflowOptions, reflow_lines_with_options, resize_keep_width_with_options};
use super::screen::{Line, blank_row};
use super::stored_cell::{
    MarkTable, StoredCell, adopt_row_cells, hydrate_all, marks_bytes, stored_cells_bytes,
};
use super::types::{Cell, Dimensions, Position};
use crate::memory_report::ScrollbackBytes;

/// Heap bytes a **live-grid** cell allocation of `capacity` cells occupies.
///
/// The ring counts its own cells with
/// [`super::stored_cell::stored_cells_bytes`], because the ring stores
/// [`StoredCell`] and the grid stores [`Cell`]. They were one function while
/// the two were the same type; keeping it shared now would misattribute one of
/// them.
pub(in crate::core) fn cells_bytes(capacity: usize) -> u64 {
    (capacity as u64).saturating_mul(std::mem::size_of::<Cell>() as u64)
}

/// Heap bytes a button-span allocation of `capacity` spans occupies. Zero for
/// the overwhelmingly common span-free line, which never allocates.
pub(in crate::core) fn spans_bytes(capacity: usize) -> u64 {
    (capacity as u64).saturating_mul(std::mem::size_of::<ButtonSpan>() as u64)
}

/// One logical line: a hard-terminated line whose soft-wrap runs have been
/// rejoined into a single flat cell vector. `open` is true when the line's last
/// physical row was soft-wrapped — the logical line is not yet hard terminated
/// and continues into whatever follows (the next physical row that scrolls off,
/// or the live grid). An open line is only ever the *last* line in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core) struct LogicalLine {
    /// The line's cells in the ring's narrow representation. Combining marks
    /// are not here — they are in `marks`, keyed by index into this vector.
    cells: Vec<StoredCell>,
    /// Combining marks of this logical line's cells, in FLAT-cell coordinates
    /// (the same space `cells` indexes) — the same carry `button_spans` uses,
    /// for the same reason and with the same lifetime. Empty and unallocated
    /// for the overwhelmingly common mark-free line, and evicted with the line
    /// because it *is* part of the line.
    marks: MarkTable,
    open: bool,
    /// OSC 133 prompt mark of this logical line (SH1), captured from the first
    /// physical row that formed it. Re-stamped onto the first physical row when
    /// the line is projected back to a grid width (see [`project_line_into`]), so
    /// the mark survives scroll-out and re-wrap. `None` for an unmarked line.
    prompt_mark: Option<PromptKind>,
    /// Button spans of this logical line in FLAT-cell coordinates (the same
    /// space `cells` indexes). Carried like `prompt_mark`, extended from "mark
    /// on the first row" to "column ranges within the line": physical-row
    /// spans are offset into flat coordinates when rows merge in
    /// [`Scrollback::push_row`], and re-projected onto physical rows by
    /// [`project_line_into`], so buttons survive scroll-out and re-wrap.
    /// Empty for the overwhelmingly common span-free line (no allocation).
    button_spans: Vec<ButtonSpan>,
}

/// Slack tolerated on a finalized logical line before its allocation is
/// reclaimed, in cells. Reclaiming is a reallocate-and-copy, so it is only
/// worth doing when the waste exceeds the cost of moving the line; below this
/// band the reclaim would churn the allocator for a few dozen bytes.
///
/// A grid-adopted line (the common hard-terminated case) arrives with capacity
/// already equal to its length and is skipped entirely by this test — the
/// reclaim exists for the merge path, where amortized doubling leaves up to
/// half a line's allocation unused.
const FINALIZE_SLACK_TOLERANCE: usize = 64;

impl LogicalLine {
    /// Reclaim reserved-but-unused capacity on a line whose length is now
    /// final.
    ///
    /// Only ever called at the hard-terminate transition. That timing is the
    /// whole design: an open logical line is only ever the *last* line in the
    /// store and is the only line `push_row` extends, so shrinking at the
    /// transition cannot reintroduce per-push reallocation — after it, the
    /// line is never appended to again. Shrinking on every push instead would
    /// defeat the amortized growth the merge path depends on and make a
    /// soft-wrapped stream quadratic.
    fn finalize_capacity(&mut self) {
        debug_assert!(
            !self.open,
            "capacity is only final on a hard-terminated line"
        );
        if self.cells.capacity() - self.cells.len() > FINALIZE_SLACK_TOLERANCE {
            self.cells.shrink_to_fit();
        }
        if self.button_spans.capacity() - self.button_spans.len() > FINALIZE_SLACK_TOLERANCE {
            self.button_spans.shrink_to_fit();
        }
        // The mark sidecar is reclaimed on the same transition and by the same
        // rule as the cells it describes. Reclaiming capacity must not reorder
        // or drop entries, so this is a `shrink_to_fit` and nothing else.
        self.marks.finalize_capacity();
        self.marks.debug_check(self.cells.len());
    }

    /// This line's cells as full `Cell`s, with combining marks re-attached.
    fn hydrate(&self) -> Vec<Cell> {
        hydrate_all(&self.cells, &self.marks)
    }
}

/// One logical line's inputs to [`project_line_into`], grouped so the
/// projection's signature stays readable and so a caller cannot pair a line's
/// cells with another line's marks by argument order.
#[derive(Clone, Copy)]
struct LineView<'a> {
    cells: &'a [StoredCell],
    marks: &'a MarkTable,
    prompt_mark: Option<PromptKind>,
    spans: &'a [ButtonSpan],
}

impl LogicalLine {
    /// This line as projection inputs, with everything the projection reads
    /// taken from the same line by construction.
    fn view(&self) -> LineView<'_> {
        LineView {
            cells: &self.cells,
            marks: &self.marks,
            prompt_mark: self.prompt_mark,
            spans: &self.button_spans,
        }
    }

    /// This line as projection inputs for a caller that only needs the row
    /// count. Button spans cannot influence how many rows a line produces —
    /// the reprojector only records positions during the walk and attaches
    /// spans to rows after it — so they are dropped rather than walked.
    fn counting_view(&self) -> LineView<'_> {
        LineView {
            spans: &[],
            ..self.view()
        }
    }
}

/// Memoized *shape* of the physical projection at a single width.
///
/// This used to memoize the projection itself — every physical row, with its
/// own copy of every cell. That is a full second copy of the store's content,
/// and at depth it was measured at parity with the logical ring: a 100k-line
/// store paid for its scrollback twice.
///
/// It is replaced by the projection's shape: how many physical rows each
/// logical line produces at this width, and the total. That is the only part of
/// the projection every consumer needs, it is what makes an absolute row index
/// resolvable to a logical line without materializing anything, and it costs
/// one `usize` per logical line instead of a row of cells per row.
///
/// Rows themselves are produced on demand and not retained, because no consumer
/// needs them retained: the render path reads a viewport-sized tail, the point
/// queries read one row, and the two whole-store readers (search and the prompt
/// -mark enumeration) are user-initiated rather than per-frame. See
/// [`Scrollback::physical_tail`] and [`Scrollback::physical_all`].
#[derive(Debug, Clone)]
struct Projection {
    /// Width the cached shape was computed at; `None` means invalid.
    width: Option<usize>,
    /// Absolute index of each logical line's **first** physical row at `width`,
    /// parallel to [`Scrollback::lines`] and in the same order. Strictly
    /// increasing (every logical line projects to at least one row), which is
    /// what lets an absolute row index be resolved to its owning line by binary
    /// search rather than by walking the store.
    row_starts: VecDeque<usize>,
    /// Absolute start represented by local physical row zero. Keeping starts
    /// monotonic lets front eviction advance this origin without shifting the
    /// remaining entries.
    base_row: usize,
    /// Total physical rows, so a length query is O(1) and the last line's row
    /// count is derivable without a special case.
    total_rows: usize,
}

impl Projection {
    fn empty() -> Self {
        Self {
            width: None,
            row_starts: VecDeque::new(),
            base_row: 0,
            total_rows: 0,
        }
    }

    /// Index of the logical line owning absolute physical row `row`, with that
    /// line's first row index. `None` when `row` is past the end.
    ///
    /// Binary search over the strictly-increasing starts makes a point query
    /// cost O(log lines) rather than O(lines). That matters because the
    /// pointer hit-test resolves a row on every mouse move, and a linear walk
    /// would have made deep scrollback progressively more expensive to hover
    /// over — trading the bytes this change saves for a latency regression.
    fn locate(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.total_rows {
            return None;
        }
        let absolute_row = self.base_row + row;
        let mut low = 0usize;
        let mut high = self.row_starts.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.row_starts[middle] <= absolute_row {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let line_index = low - 1;
        Some((line_index, self.row_starts[line_index] - self.base_row))
    }

    fn push_line(&mut self, rows: usize) {
        let start = self.base_row + self.total_rows;
        self.row_starts.push_back(start);
        self.total_rows += rows;
    }

    fn update_last_line(&mut self, rows: usize) {
        let start = *self
            .row_starts
            .back()
            .expect("an extended logical line has a cached start");
        self.total_rows = start - self.base_row + rows;
    }

    fn evict_front(&mut self, count: usize) {
        for _ in 0..count {
            self.row_starts.pop_front();
        }
        let new_base = self
            .row_starts
            .front()
            .copied()
            .unwrap_or(self.base_row + self.total_rows);
        self.total_rows -= new_base - self.base_row;
        self.base_row = new_base;
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct LimitEnforcement {
    changed: bool,
    evicted_lines: usize,
}

/// Default maximum number of logical lines retained in scrollback. Each scrolled
/// -off hard-terminated line is one logical line, so this is the user-facing
/// "lines of history" cap. Chosen to match the common terminal default (xterm,
/// kitty, alacritty all default near this) and bounds steady-state memory: the
/// ring stores [`StoredCell`] at 28 B/cell, so at 80 columns 10k lines of
/// hard-terminated history measures 23.8 MB of ring. (This note has now been
/// wrong twice, in the same way: it said 36 B/cell and ~28 MB after `Cell` grew
/// to 44, then 44 B/cell and ~34.5 MB after the ring stopped storing `Cell`.
/// Both times the model outlived the code. The figure above is a measurement —
/// `stage_b_cell_shrink_projection`, hard 10,000 — not an arithmetic product,
/// which is why it does not equal 10,000 x 80 x 28.) Without a cap, a
/// process that streams
/// unbounded output (`yes`, `cat bigfile`, a runaway loop) would grow OdyTTY's
/// memory until the OS OOM-killed it. See [`Scrollback::push_row`].
pub(in crate::core) const DEFAULT_SCROLLBACK_LIMIT: usize = 10_000;

/// Defensive ceiling on the cell count of a *single* logical line. The line cap
/// counts hard-terminated lines, so a stream with no line terminator at all
/// (e.g. `cat /dev/zero`) would otherwise grow one ever-open logical line
/// without bound. When an open line exceeds this many cells, the oldest cells
/// are dropped from its front (equivalent to that history scrolling away). The
/// bound is generous (1,048,576 cells ≈ 36 MB) so it never trims realistic
/// content; it exists purely to keep the pathological no-newline case bounded.
const MAX_LOGICAL_LINE_CELLS: usize = 1 << 20;

/// Logical-line scrollback with a lazily-(re)built physical projection.
#[derive(Debug, Clone)]
pub(in crate::core) struct Scrollback {
    lines: VecDeque<LogicalLine>,
    cache: RefCell<Projection>,
    /// Maximum retained logical lines; oldest are evicted past this in
    /// [`Scrollback::push_row`]. `0` means unbounded (history is never trimmed).
    limit: usize,
    /// Monotonic notice that absolute row zero moved because history was
    /// removed from the front. Native selection/search/copy-mode coordinates
    /// use that origin and must be invalidated when this changes.
    trim_epoch: u64,
    /// Monotonic count of physical rows ever pushed into this store. Cheap
    /// (no projection) anchor unit for "how far has the visible grid scrolled
    /// since X" bookkeeping (the open button run); unaffected by trims.
    pushed_rows: u64,
    /// Button span references dropped by this store (a logical line evicted
    /// from the ring, a front-drained open line, or a full clear) whose table
    /// refcounts the owner has not yet decremented. The store cannot reach the
    /// `ButtonTable` (it lives on `Screen`), so drops accumulate here and the
    /// owner drains them via [`Scrollback::take_freed_button_ids`].
    freed_button_ids: Vec<ButtonId>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::core) struct ResizeOptions {
    pub preserve_cursor_physical_line: bool,
    pub cursor_pending_wrap: bool,
    pub collapse_prompt_start_row: Option<usize>,
    /// Whether the shell applied output since the last resize, so a repaint is
    /// expected to follow this one (the `preserve_cursor_physical_line`
    /// override is only safe to honor when true — see `ReflowOptions`).
    pub repaint_expected: bool,
    /// Whether the backend authoritatively repaints with absolute positioning
    /// on resize, so the terminal defers cursor placement to the shell (the
    /// ConPTY backend — see `ReflowOptions::shell_owns_cursor_on_resize`).
    pub shell_owns_cursor_on_resize: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core) struct ResizeResult {
    pub cursor: Position,
    pub pending_wrap: bool,
    pub collapsed_prompt_start_row: Option<usize>,
}

impl Scrollback {
    pub(in crate::core) fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            cache: RefCell::new(Projection::empty()),
            limit: DEFAULT_SCROLLBACK_LIMIT,
            trim_epoch: 0,
            pushed_rows: 0,
            freed_button_ids: Vec::new(),
        }
    }

    /// Build a store with an explicit logical-line limit (`0` = unbounded).
    pub(in crate::core) fn with_limit(limit: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            cache: RefCell::new(Projection::empty()),
            limit,
            trim_epoch: 0,
            pushed_rows: 0,
            freed_button_ids: Vec::new(),
        }
    }

    /// Rebuild scrollback from owned physical rows copied out of a snapshot.
    ///
    /// The restored limit is large enough to retain every captured logical line
    /// while keeping the normal default cap for future output when the snapshot
    /// contains less history than the default.
    pub(in crate::core) fn from_physical_rows(rows: &[Line]) -> Self {
        let lines: VecDeque<LogicalLine> = logical_from_physical(rows).into();
        let limit = DEFAULT_SCROLLBACK_LIMIT.max(lines.len());
        Self {
            lines,
            cache: RefCell::new(Projection::empty()),
            limit,
            trim_epoch: 0,
            pushed_rows: 0,
            freed_button_ids: Vec::new(),
        }
    }

    /// The active logical-line limit (`0` = unbounded).
    pub(in crate::core) fn limit(&self) -> usize {
        self.limit
    }

    /// Set the logical-line limit and immediately trim any excess history so a
    /// lowered limit takes effect at once. `0` disables trimming (unbounded).
    pub(in crate::core) fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        if self.enforce_limit().changed {
            self.invalidate();
        }
    }

    /// Build a store from physical rows (test oracle helper). Unbounded so the
    /// differential reflow-parity suite compares pure re-wrap behavior without
    /// the eviction cap perturbing large corpora.
    #[cfg(test)]
    pub(in crate::core) fn from_physical(rows: &[Line]) -> Self {
        Self {
            lines: logical_from_physical(rows).into(),
            cache: RefCell::new(Projection::empty()),
            limit: 0,
            trim_epoch: 0,
            pushed_rows: 0,
            freed_button_ids: Vec::new(),
        }
    }

    /// Number of physical rows the scrollback projects to at `width`.
    ///
    /// Served from the cached shape, so this no longer materializes the store
    /// to count it.
    pub(in crate::core) fn physical_len(&self, width: usize) -> usize {
        self.ensure_cache(width);
        self.cache.borrow().total_rows
    }

    pub(in crate::core) fn trim_epoch(&self) -> u64 {
        self.trim_epoch
    }

    #[cfg(test)]
    pub(in crate::core) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Test window proving that a hot-path append preserved the cached shape.
    #[cfg(test)]
    pub(in crate::core) fn projection_cached_at(&self, width: usize) -> bool {
        self.cache.borrow().width == Some(width)
    }

    /// The **whole** physical projection at `width` (oldest first),
    /// byte-identical to what eager reflow would store as scrollback.
    ///
    /// Materializes the entire store and does not retain it. Reserved for the
    /// two consumers that genuinely need every row — full-buffer search and the
    /// prompt-mark enumeration's fallback — both of which are user-initiated,
    /// not per-frame. Anything that needs a viewport uses
    /// [`Scrollback::physical_tail`]; anything that needs one row uses
    /// [`Scrollback::physical_row`]; anything that needs only the count uses
    /// [`Scrollback::physical_len`].
    pub(in crate::core) fn physical_all(&self, width: usize) -> Vec<Line> {
        project_logical(&self.lines, width)
    }

    /// Test window onto the whole projection.
    ///
    /// An alias for [`Scrollback::physical_all`], kept because the projection
    /// suites assert against every row and should keep doing so: the windowed
    /// accessors are only correct if the full projection they are windows onto
    /// is correct, and that is what these tests pin.
    #[cfg(test)]
    pub(in crate::core) fn physical(&self, width: usize) -> Vec<Line> {
        self.physical_all(width)
    }

    /// The last `n` physical rows at `width`, oldest first.
    ///
    /// Projects only the trailing logical lines needed to cover `n` rows, so
    /// the cost is `O(n)` in rows rather than `O(store)`. Returns fewer than
    /// `n` rows only when the store projects to fewer than `n` in total.
    ///
    /// This is the render path's accessor. The viewport is always the tail of
    /// the combined scrollback-plus-grid buffer, so a tail projection is
    /// exactly what every per-frame consumer reads, and the rows above it never
    /// have to exist.
    pub(in crate::core) fn physical_tail(&self, width: usize, n: usize) -> Vec<Line> {
        if n == 0 {
            return Vec::new();
        }
        self.ensure_cache(width);
        // Whole logical lines are projected, never a suffix of one: a line's
        // wrapping is only correct when the line is projected as a unit, so
        // taking a suffix of its cells would re-wrap from the wrong offset.
        // The first line to pull is therefore the one *containing* the first
        // wanted row, found by binary search.
        let (start_line, total_rows) = {
            let cache = self.cache.borrow();
            let first_wanted = cache.total_rows.saturating_sub(n);
            match cache.locate(first_wanted) {
                Some((line_index, _)) => (line_index, cache.total_rows),
                // `locate` returns `None` only for an empty store, since
                // `first_wanted < total_rows` whenever any row exists.
                None => return Vec::new(),
            }
        };
        let mut rows = project_logical(self.lines.iter().skip(start_line), width);
        let n = n.min(total_rows);
        // The pulled lines may project to more than `n` rows; keep the tail.
        if rows.len() > n {
            rows.drain(0..rows.len() - n);
        }
        rows
    }

    /// The single physical row at absolute index `row` at `width`, or `None`
    /// when the index is past the end.
    ///
    /// Resolves the index through the cached shape to the one logical line that
    /// owns it and projects only that line.
    pub(in crate::core) fn physical_row(&self, width: usize, row: usize) -> Option<Line> {
        self.ensure_cache(width);
        let (line_index, first_row) = self.cache.borrow().locate(row)?;
        let line = self.lines.get(line_index)?;
        let mut rows = Vec::new();
        project_line_into(line.view(), width, line.open, true, &mut rows);
        rows.into_iter().nth(row - first_row)
    }

    /// Absolute physical row index of each stored logical line's **first** row
    /// at `width`, paired with that line's prompt mark, for lines that carry
    /// one.
    ///
    /// [`project_line_into`] stamps a logical line's mark onto the first
    /// physical row it produces and leaves continuation rows unmarked, so the
    /// marked rows are exactly the first-row indices of marked logical lines.
    /// Serving the enumeration from the cached shape therefore returns the same
    /// pairs the materialized projection would, without materializing it.
    pub(in crate::core) fn prompt_mark_rows(&self, width: usize) -> Vec<(usize, PromptKind)> {
        self.ensure_cache(width);
        let cache = self.cache.borrow();
        let mut out = Vec::new();
        for (line, &start) in self.lines.iter().zip(cache.row_starts.iter()) {
            if let Some(kind) = line.prompt_mark {
                out.push((start - cache.base_row, kind));
            }
        }
        out
    }

    /// The prompt mark at absolute physical row `row`, if any.
    ///
    /// The point-query counterpart of [`Scrollback::prompt_mark_rows`], and
    /// exact for the same reason: only a logical line's first physical row can
    /// carry a mark, so any row that is not a line's first row is unmarked by
    /// construction.
    pub(in crate::core) fn prompt_mark_at(&self, width: usize, row: usize) -> Option<PromptKind> {
        self.ensure_cache(width);
        let (line_index, first_row) = self.cache.borrow().locate(row)?;
        if first_row != row {
            // A continuation row, which never carries a mark.
            return None;
        }
        self.lines.get(line_index).and_then(|line| line.prompt_mark)
    }

    /// Append one physical row that has just scrolled off the visible grid.
    /// Extends the trailing open logical line when the previous row soft-wrapped,
    /// otherwise starts a new logical line.
    pub(in crate::core) fn push_row(&mut self, row: Line) {
        self.pushed_rows = self.pushed_rows.wrapping_add(1);
        let wrapped = row.wrapped;
        let mut appended_line = false;
        if let Some(last) = self.lines.back_mut()
            && last.open
        {
            // Continuation of an open logical line: extend cells. A continuation
            // row normally carries no mark, but if the logical line's true first
            // row already scrolled off before the mark was stamped (the mark
            // landed on a still-visible continuation row), adopt it here so the
            // line keeps its first non-`None` mark.
            // Cells and their marks are appended together by the store's one
            // re-keying entry point, so the offset the append starts at is
            // applied to both or to neither.
            let offset = last.cells.len();
            adopt_row_cells(&mut last.cells, &mut last.marks, &row.cells);
            last.open = wrapped;
            if last.prompt_mark.is_none() {
                last.prompt_mark = row.prompt_mark;
            }
            // Button spans arrive in row-local columns; offset them into the
            // logical line's flat-cell space. The per-line cap holds across
            // the merge: spans past it are dropped and their references
            // surrendered (defensive; the definition paths already cap).
            for span in row.button_spans {
                if last.button_spans.len() >= MAX_BUTTON_SPANS_PER_LINE {
                    self.freed_button_ids.push(span.id);
                    continue;
                }
                last.button_spans.push(ButtonSpan {
                    id: span.id,
                    start_col: span.start_col + offset,
                    len: span.len,
                });
            }
            // The merge path is where slack accumulates: `extend` grows the
            // flat cell vector by doubling. If this row hard-terminated the
            // line, its length is now final and the overshoot is pure waste.
            if !wrapped {
                last.finalize_capacity();
            }
        } else {
            let mut cells = Vec::new();
            let mut marks = MarkTable::default();
            adopt_row_cells(&mut cells, &mut marks, &row.cells);
            let mut line = LogicalLine {
                cells,
                marks,
                open: wrapped,
                prompt_mark: row.prompt_mark,
                button_spans: row.button_spans,
            };
            // A line adopted whole from a grid row is normally already exact,
            // so this is a no-op under the tolerance; it is applied anyway
            // because rows arriving from reflow overflow can carry slack.
            if !wrapped {
                line.finalize_capacity();
            }
            self.lines.push_back(line);
            appended_line = true;
        }
        let enforcement = self.enforce_limit();

        // A valid projection shape stays valid across the hot append path.
        // Count only the changed trailing line, then discard starts evicted
        // from the front. This avoids walking the full history while the
        // terminal lock is held on every output frame.
        let cached_width = self.cache.borrow().width;
        if let Some(width) = cached_width {
            let last_rows = self
                .lines
                .back()
                .map_or(0, |line| count_projected_rows(line, width));
            let mut cache = self.cache.borrow_mut();
            if appended_line {
                cache.push_line(last_rows);
            } else {
                cache.update_last_line(last_rows);
            }
            cache.evict_front(enforcement.evicted_lines);
        }
    }

    /// Evict oldest history so the store stays within `limit` logical lines and
    /// no single (open) logical line exceeds [`MAX_LOGICAL_LINE_CELLS`] cells.
    /// Reports whether anything changed and how many whole lines left the
    /// front. Called after every `push_row` and on `set_limit`; a `0` limit only
    /// enforces the per-line cell ceiling.
    fn enforce_limit(&mut self) -> LimitEnforcement {
        let mut result = LimitEnforcement::default();

        if self.limit != 0 && self.lines.len() > self.limit {
            // VecDeque front eviction: each `pop_front` is O(1) (no memmove of the
            // retained tail), so steady-state eviction under sustained output is
            // O(1) per scrolled line instead of the O(limit) front-shift a
            // `Vec::drain(0..excess)` performed once at the cap.
            let excess = self.lines.len() - self.limit;
            for _ in 0..excess {
                if let Some(line) = self.lines.pop_front() {
                    // A line leaving the ring surrenders its button-span
                    // references; the owner drains these and decrements the
                    // table refcounts (sticky buttons free at zero).
                    self.freed_button_ids
                        .extend(line.button_spans.iter().map(|span| span.id));
                }
            }
            result.changed = true;
            result.evicted_lines = excess;
        }

        // Bound the pathological no-terminator case: a never-closed logical
        // line accreting cells forever. By this store's invariant an open line
        // is only ever the LAST line (see `LogicalLine::open`), so the ceiling
        // must inspect `lines.last_mut()` — checking `lines[0]` bounds only the
        // single-line store and lets a runaway stream after any closed history
        // line grow without bound. Drop oldest cells from the line's front.
        //
        // Hysteresis: trim only when the line crosses the high-water mark
        // (`> MAX_LOGICAL_LINE_CELLS`), but when it does, drain all the way down
        // to the low-water mark (`MAX_LOGICAL_LINE_CELLS - SLACK`) rather than
        // exactly to the ceiling. A naive "drain to exactly MAX after every
        // push" is O(n) per push once saturated — `Vec::drain(0..W)` shifts the
        // whole ~MAX-element buffer left each call — making a long never-newline
        // stream O(n²) (a real live-terminal jank on binary spew / `yes` / a
        // stuck redraw, not just a slow test). With a slack band the front-drain
        // fires only once per ~SLACK/row-width pushes, amortizing to O(1) per
        // push. Peak length stays ≤ MAX + (one over-ceiling push), so the
        // per-line ceiling and memory bound are preserved.
        const SLACK: usize = MAX_LOGICAL_LINE_CELLS / 2;
        if let Some(last) = self.lines.back_mut()
            && last.cells.len() > MAX_LOGICAL_LINE_CELLS
        {
            let drop = last.cells.len() - (MAX_LOGICAL_LINE_CELLS - SLACK);
            last.cells.drain(0..drop);
            // The mark sidecar is keyed by flat index, so a front-drain of the
            // cells has to shift it by the same amount or every surviving mark
            // lands on a different base character than the one it belongs to.
            last.marks.drop_front(drop);
            last.marks.debug_check(last.cells.len());
            // Shift surviving button spans left with the drained cells; spans
            // fully inside the drained front are freed, spans straddling the
            // cut keep their surviving tail.
            if !last.button_spans.is_empty() {
                let mut kept = Vec::with_capacity(last.button_spans.len());
                for span in last.button_spans.drain(..) {
                    let end = span.start_col.saturating_add(span.len);
                    if span.start_col >= drop {
                        kept.push(ButtonSpan {
                            id: span.id,
                            start_col: span.start_col - drop,
                            len: span.len,
                        });
                    } else if end > drop {
                        kept.push(ButtonSpan {
                            id: span.id,
                            start_col: 0,
                            len: end - drop,
                        });
                    } else {
                        self.freed_button_ids.push(span.id);
                    }
                }
                last.button_spans = kept;
            }
            result.changed = true;
        }

        if result.changed {
            self.trim_epoch = self.trim_epoch.wrapping_add(1);
        }
        result
    }

    /// Clear all scrollback.
    pub(in crate::core) fn clear(&mut self) {
        if !self.lines.is_empty() {
            self.trim_epoch = self.trim_epoch.wrapping_add(1);
        }
        for line in &self.lines {
            self.freed_button_ids
                .extend(line.button_spans.iter().map(|span| span.id));
        }
        self.lines.clear();
        self.invalidate();
    }

    /// NF6 (C16 seam, scrollback side): hard-terminate the trailing open
    /// logical line. An open tail promises that visible row 0 is its physical
    /// continuation — the projection marks the tail's last physical row
    /// `wrapped`, and reflow (`Screen::resize`) fuses it with row 0. An
    /// operation that replaces the visible screen wholesale (ED2) breaks that
    /// promise; the tail must close or the next resize fuses scrolled-off
    /// history with UNRELATED fresh content. No-op when the store is empty or
    /// the tail is already hard-terminated.
    pub(in crate::core) fn sever_trailing_wrap(&mut self) {
        if let Some(last) = self.lines.back_mut()
            && last.open
        {
            last.open = false;
            // The tail's length is final from here: severing is exactly the
            // hard-terminate transition, reached by a different route.
            last.finalize_capacity();
            self.invalidate();
        }
    }

    /// Whether any stored logical line carries an OSC 133 prompt mark (SH1).
    /// Cheap O(lines) scan over the logical store (no projection), used to keep
    /// the prompt-marks change flag honest on clear/resize.
    pub(in crate::core) fn any_prompt_mark(&self) -> bool {
        self.lines.iter().any(|l| l.prompt_mark.is_some())
    }

    /// Monotonic count of physical rows ever pushed into this store (no
    /// projection cost — see the field doc).
    pub(in crate::core) fn pushed_row_count(&self) -> u64 {
        self.pushed_rows
    }

    /// Whether any button-span references have been surrendered since the last
    /// drain. Cheap gate so the hot scroll path skips the drain entirely.
    pub(in crate::core) fn has_freed_button_ids(&self) -> bool {
        !self.freed_button_ids.is_empty()
    }

    /// Drain the surrendered button-span references (see
    /// [`Scrollback::push_row`] / eviction). The owner decrements the button
    /// table's refcounts with these.
    pub(in crate::core) fn take_freed_button_ids(&mut self) -> Vec<ButtonId> {
        std::mem::take(&mut self.freed_button_ids)
    }

    /// Append every button id referenced by stored logical lines to `out` (one
    /// entry per span — the refcount unit). Used by the post-resize refcount
    /// rebuild; O(lines) with an empty-vec check per line.
    pub(in crate::core) fn collect_button_ids(&self, out: &mut Vec<ButtonId>) {
        for line in &self.lines {
            out.extend(line.button_spans.iter().map(|span| span.id));
        }
    }

    /// Test window: flat-coordinate button spans of stored logical line
    /// `index` (0 = oldest).
    #[cfg(test)]
    pub(in crate::core) fn logical_button_spans(&self, index: usize) -> &[ButtonSpan] {
        self.lines
            .get(index)
            .map(|line| line.button_spans.as_slice())
            .unwrap_or(&[])
    }

    /// Test window: number of stored logical lines.
    #[cfg(test)]
    pub(in crate::core) fn logical_len(&self) -> usize {
        self.lines.len()
    }

    /// Test window: the ring's byte total decomposed by what the bytes are
    /// *made of*, rather than by which allocation holds them —
    /// `(ring slots, cell capacity in cells, button-span capacity in spans)`.
    ///
    /// [`Self::stored_bytes`] reports the ring as one figure because that is
    /// the resident cost. This split exists for a different question: only the
    /// cell term scales with `size_of::<Cell>()`, so projecting the effect of a
    /// change to `Cell`'s layout requires knowing how much of the ring is cells
    /// and how much is per-line overhead that such a change would not move.
    #[cfg(test)]
    pub(in crate::core) fn ring_composition(&self) -> (usize, usize, usize) {
        let cells = self.lines.iter().map(|l| l.cells.capacity()).sum();
        let spans = self.lines.iter().map(|l| l.button_spans.capacity()).sum();
        (self.lines.capacity(), cells, spans)
    }

    /// Heap bytes this store currently holds, split by what holds them.
    ///
    /// The logical-line ring and the memoized projection shape are reported
    /// separately rather than summed. They are structurally different costs:
    /// the ring is the content, the projection is the cached description of how
    /// that content wraps at the current width, and they are reclaimed by
    /// different means. A single total cannot say which one a change moved,
    /// which makes any before/after comparison unattributable — and the
    /// projection field is exactly where that mattered, because it once held a
    /// full second copy of the ring and now holds one `usize` per line.
    ///
    /// `ring_slack` is the reserved-but-unused part of `ring` — capacity minus
    /// length, across the ring's own slots and each line's allocations. It is a
    /// **breakdown of `ring`, not an addition to it**, so that reclaimable
    /// waste is visible as a figure rather than inferred from a model.
    ///
    /// Capacities are used rather than lengths because capacity is what the
    /// process is resident for. Saturating throughout: an attribution figure
    /// must never wrap into a smaller number.
    pub(in crate::core) fn stored_bytes(&self) -> ScrollbackBytes {
        let slot = std::mem::size_of::<LogicalLine>() as u64;
        let ring_slots = (self.lines.capacity() as u64).saturating_mul(slot);
        let ring_slot_slack =
            (self.lines.capacity().saturating_sub(self.lines.len()) as u64).saturating_mul(slot);
        let (contents, contents_slack) =
            self.lines
                .iter()
                .fold((0u64, 0u64), |(bytes, slack), line| {
                    let used = stored_cells_bytes(line.cells.capacity())
                        .saturating_add(spans_bytes(line.button_spans.capacity()))
                        .saturating_add(marks_bytes(line.marks.capacity()));
                    let unused = stored_cells_bytes(line.cells.capacity() - line.cells.len())
                        .saturating_add(spans_bytes(
                            line.button_spans.capacity() - line.button_spans.len(),
                        ))
                        .saturating_add(marks_bytes(line.marks.capacity() - line.marks.len()));
                    (bytes.saturating_add(used), slack.saturating_add(unused))
                });
        // The projection is now a shape, not a copy: one `usize` per logical
        // line. It stays a separate reported figure rather than being folded
        // into `ring` because it is still a real retained cost and because a
        // future change to how the projection is held has to remain visible in
        // the same field it was measured in.
        let cache = self.cache.borrow();
        let projection = (cache.row_starts.capacity() as u64)
            .saturating_mul(std::mem::size_of::<usize>() as u64);
        ScrollbackBytes {
            ring: ring_slots.saturating_add(contents),
            projection,
            ring_slack: ring_slot_slack.saturating_add(contents_slack),
        }
    }

    fn invalidate(&self) {
        let mut cache = self.cache.borrow_mut();
        cache.width = None;
        cache.row_starts.clear();
        cache.base_row = 0;
        cache.total_rows = 0;
    }

    /// Rebuild the cached projection shape for `width` if it is not current.
    ///
    /// Each logical line is projected into one reused scratch buffer purely to
    /// count the rows it produces, and the rows are dropped. The row count is
    /// therefore produced by the same code that produces the rows, so the
    /// cached shape cannot disagree with the projection it describes — a
    /// separate arithmetic row-count model would be a second implementation of
    /// the wrapping rule and would drift from it.
    ///
    /// Peak extra memory is one logical line's worth of rows, not the store's.
    fn ensure_cache(&self, width: usize) {
        {
            let cache = self.cache.borrow();
            if cache.width == Some(width) {
                return;
            }
        }
        let mut scratch: Vec<Line> = Vec::new();
        let mut row_starts = VecDeque::with_capacity(self.lines.len());
        let mut total_rows = 0usize;
        for line in &self.lines {
            scratch.clear();
            project_line_into(line.counting_view(), width, line.open, false, &mut scratch);
            row_starts.push_back(total_rows);
            total_rows += scratch.len();
        }
        let mut cache = self.cache.borrow_mut();
        cache.width = Some(width);
        cache.row_starts = row_starts;
        cache.base_row = 0;
        cache.total_rows = total_rows;
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

    // ConPTY content-reflow suppression: on a backend whose shell authoritatively reflows + repaints the
    // viewport on resize (ConPTY/conhost), a width change must NOT run our own
    // competing rewrap of the live grid -- the two reflow engines disagree on
    // soft-wrap boundaries and strand the input-line tail, compounding across
    // repeated narrow<->wide cycles. Route the live grid through truncate/pad
    // instead (mirroring the alt-screen non-reflow path) and let conhost's
    // absolute repaint own the viewport next tick. The width-unchanged fast
    // path already does no content rewrap, so this is gated on a width change.
    if options.shell_owns_cursor_on_resize && !width_unchanged {
        return resize_shell_owns_no_rewrap(sb, rows, new_dims, cursor, options);
    }

    // Pull trailing logical lines into the re-wrap subset: enough to fill the new
    // window, always including the open tail (which continues into the live
    // grid), and through any trailing blank run so trailing-blank collapse
    // matches the eager oracle exactly.
    let mut pulled: Vec<LogicalLine> = Vec::new();
    let mut pulled_rows = 0usize;
    loop {
        let need_more = pulled_rows < new_rows;
        let extend_blank = pulled_rows >= new_rows && sb.lines.back().is_some_and(line_all_blank);
        if !(need_more || extend_blank) {
            break;
        }
        match sb.lines.pop_back() {
            Some(line) => {
                pulled_rows += count_projected_rows(&line, new_width);
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
            // The reflow primitives are unchanged and work in `Cell`, so the
            // mega-row is hydrated here. Marks ride the cells they belong to
            // through reflow exactly as they always did, and the result comes
            // back through `push_row`, which re-narrows it.
            let mut mega = if line.open {
                Line::wrapped(line.hydrate())
            } else {
                Line::unwrapped(line.hydrate())
            };
            // Carry the prompt mark onto the mega-row so `reflow_lines` re-anchors
            // it onto the first re-wrapped physical row. Button spans ride the
            // same way: flat coordinates ARE row-local coordinates on a
            // single mega-row, and `reflow_lines` re-projects them.
            mega.prompt_mark = line.prompt_mark;
            mega.button_spans = line.button_spans.clone();
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
        let cursor = resize_keep_width_with_options(
            &mut overflow,
            &mut subset,
            new_dims,
            cursor_in,
            options.shell_owns_cursor_on_resize,
        );
        ResizeResult {
            cursor,
            pending_wrap: options.cursor_pending_wrap && cursor.column == new_width - 1,
            collapsed_prompt_start_row: None,
        }
    } else {
        let collapse_prompt_start_row = options
            .collapse_prompt_start_row
            .map(|row| cursor_prefix + row);
        let reflow = reflow_lines_with_options(
            &mut overflow,
            &mut subset,
            new_dims,
            cursor_in,
            ReflowOptions {
                preserve_cursor_physical_line: options.preserve_cursor_physical_line,
                cursor_pending_wrap: options.cursor_pending_wrap,
                collapse_prompt_start_row,
                repaint_expected: options.repaint_expected,
                shell_owns_cursor_on_resize: options.shell_owns_cursor_on_resize,
                // The reflow sees `cursor_in.row = cursor_prefix + cursor.row`, a
                // combined-buffer row. When cursor placement is deferred to the
                // shell, the `None` arm subtracts this to recover the incoming
                // visible row instead of clamping the combined offset.
                combined_cursor_prefix: cursor_prefix,
            },
        );
        ResizeResult {
            cursor: reflow.cursor,
            pending_wrap: reflow.pending_wrap,
            collapsed_prompt_start_row: reflow.collapsed_prompt_start_row,
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

/// Live-grid truncate/pad path for the `shell_owns_cursor_on_resize`
/// backend (ConPTY/conhost) on a width change.
///
/// conhost reflows its own screen buffer and re-emits an absolute repaint of the
/// visible viewport on every `ResizePseudoConsole`. OdyTTY must therefore NOT
/// run its own competing rewrap on the live grid: the two engines disagree on
/// where soft-wrap boundaries fall for the active input line, stranding the tail
/// at a stale wrap column, and because each subsequent resize re-derives logical
/// lines from the now-inconsistent rows the error ACCUMULATES across repeated
/// narrow<->wide cycles. This mirrors the alternate-screen non-reflow posture
/// (`reflow::resize_buffer_rows`, rationale "apps repaint"): the live grid is
/// truncate/padded to the new width with no rejoin/rewrap, and conhost's
/// absolute repaint redraws the viewport on the next pump tick.
///
/// Decision (a) (settled by `shell_owns_resize_preserves_scrollback_projection`,
/// option (b) rejected): scrollback is left untouched here. It is stored as
/// width-independent logical lines and re-projected to the new width on access,
/// so history stays readable without a competing rewrap. We deliberately do NOT
/// pull trailing scrollback into the viewport the way the normal lazy path does
/// -- conhost owns the viewport, so pulling + rewrapping it would re-introduce
/// the exact fight this path exists to avoid. Rows that overflow the new (taller
/// -> shorter) window scroll into scrollback, since conhost does not resend it.
///
/// Cursor placement is already deferred to the shell under `shell_owns_cursor`;
/// the incoming cursor is kept, clamped to the new dims, and corrected by the
/// shell's absolute repaint next tick.
fn resize_shell_owns_no_rewrap(
    sb: &mut Scrollback,
    rows: &mut Vec<Line>,
    new_dims: Dimensions,
    cursor: Position,
    options: ResizeOptions,
) -> ResizeResult {
    let new_rows = new_dims.rows;
    let new_width = new_dims.columns;

    // Truncate/pad each live row to the new width (no rejoin/rewrap).
    for row in rows.iter_mut() {
        row.resize(new_width, Cell::blank());
    }
    // Rows above the new window scroll into scrollback (conhost does not resend
    // it, so OdyTTY keeps its own history); pad a short grid up to new_rows.
    if rows.len() > new_rows {
        let removed = rows.len() - new_rows;
        for row in rows.drain(0..removed) {
            sb.push_row(row);
        }
    }
    rows.resize_with(new_rows, || blank_row(new_width));
    // Force scrollback to re-project at the new width on next access.
    sb.invalidate();

    let column = cursor.column.min(new_width - 1);
    let row = cursor.row.min(new_rows - 1);
    ResizeResult {
        cursor: Position { row, column },
        pending_wrap: options.cursor_pending_wrap && column == new_width - 1,
        collapsed_prompt_start_row: None,
    }
}

/// Rebuild logical lines from physical rows (the inverse of [`project_logical`]).
/// Consecutive rows are joined into one logical line until a non-`wrapped`
/// (hard-terminated) row ends it; a trailing run that ends on a `wrapped` row
/// becomes an `open` logical line.
pub(in crate::core) fn logical_from_physical(rows: &[Line]) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    let mut current: Vec<StoredCell> = Vec::new();
    let mut current_marks = MarkTable::default();
    let mut current_mark: Option<PromptKind> = None;
    let mut current_spans: Vec<ButtonSpan> = Vec::new();
    for row in rows {
        if current.is_empty() {
            // First physical row of a new logical line: capture its mark.
            current_mark = row.prompt_mark;
        } else if current_mark.is_none() {
            // Adopt a mark stamped on a continuation row when the first row
            // carried none (first non-`None` mark in the logical line wins).
            current_mark = row.prompt_mark;
        }
        // Row-local button spans offset into the joined flat-cell space.
        for span in &row.button_spans {
            current_spans.push(ButtonSpan {
                id: span.id,
                start_col: span.start_col + current.len(),
                len: span.len,
            });
        }
        // Same single re-keying entry point as `push_row`: the flat offset the
        // append starts at is applied to the cells and their marks together.
        adopt_row_cells(&mut current, &mut current_marks, &row.cells);
        if !row.wrapped {
            let mut line = LogicalLine {
                cells: std::mem::take(&mut current),
                marks: std::mem::take(&mut current_marks),
                open: false,
                prompt_mark: current_mark.take(),
                button_spans: std::mem::take(&mut current_spans),
            };
            // Same accumulation as the `push_row` merge path: `current` grew
            // by doubling across the rows that joined into this line, and
            // `mem::take` hands that overshoot to the finished line.
            line.finalize_capacity();
            lines.push(line);
        }
    }
    if !current.is_empty() {
        lines.push(LogicalLine {
            cells: current,
            marks: current_marks,
            open: true,
            prompt_mark: current_mark,
            button_spans: current_spans,
        });
    }
    lines
}

/// Project logical lines to physical rows at `width`.
pub(in crate::core) fn project_logical<'a, I>(lines: I, width: usize) -> Vec<Line>
where
    I: IntoIterator<Item = &'a LogicalLine>,
{
    let mut out = Vec::new();
    for line in lines {
        project_line_into(line.view(), width, line.open, true, &mut out);
    }
    out
}

/// Whether a logical line is entirely plain blanks.
///
/// A marked cell is never blank however plain its base looks, for the same
/// reason the trailing-blank trim consults the sidecar: the marks are no longer
/// inside the cell being compared.
fn line_all_blank(line: &LogicalLine) -> bool {
    let plain = StoredCell::from_cell(&Cell::blank());
    line.marks.is_empty() && line.cells.iter().all(|c| *c == plain)
}

fn count_projected_rows(line: &LogicalLine, width: usize) -> usize {
    let mut tmp = Vec::new();
    project_line_into(line.counting_view(), width, line.open, false, &mut tmp);
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
/// - `spans` (the logical line's button spans, flat-cell coordinates) are
///   re-projected onto the produced rows as row-local segments — the
///   `prompt_mark` carry extended to column ranges. Span-free lines (the
///   overwhelmingly common case) pay only an `is_empty` check.
///
/// # `materialize`
///
/// Callers that only need to know *how many* rows a line produces — the
/// projection-shape cache and the resize subset pull — pass `false`, and the
/// rows are emitted with their flags and their count but without their cells.
///
/// This is one implementation, not two. Every wrapping decision reads
/// `row_len`, which is maintained identically in both modes; materializing only
/// gates whether a cell is also *written*. `row_len` and the row vector are
/// asserted equal at every push under `debug_assertions`, so the two modes
/// cannot silently diverge — a missed increment fails immediately in every
/// debug test run rather than producing a shape that disagrees with the
/// projection it describes.
///
/// It exists because the ring stores [`StoredCell`] and rebuilding a full
/// `Cell` is real work: rebuilding every cell in the store purely to count rows
/// and then dropping them was measured at 21.5 ms of a 38.5 ms cache rebuild at
/// 100,000 lines.
fn project_line_into(
    line: LineView<'_>,
    width: usize,
    open: bool,
    materialize: bool,
    out: &mut Vec<Line>,
) {
    let LineView {
        cells,
        marks,
        prompt_mark: mark,
        spans,
    } = line;
    let plain = StoredCell::from_cell(&Cell::blank());
    // Index of this logical line's first physical row in `out`; the prompt mark
    // is re-anchored here after the rows are produced. A logical line always
    // produces at least one row, so this index is always valid afterward.
    let first_row = out.len();
    let mut reprojector = if spans.is_empty() {
        None
    } else {
        Some(SpanReprojector::new())
    };

    // Trim trailing plain blanks (matches reflow). Open lines carry none.
    //
    // A stored cell that compares equal to `plain` is not necessarily blank:
    // its combining marks live in the sidecar and are not part of the
    // comparison. Trimming on the base alone would silently discard marks
    // attached to a space — which a `Cell` comparison could never do, because
    // the marks were inside the cell. The sidecar is consulted so the trim
    // means the same thing it did before.
    let mut keep = cells.len();
    while keep > 0 && cells[keep - 1] == plain && !marks.any_at_or_after(keep - 1) {
        keep -= 1;
    }
    let cells = &cells[..keep];

    let blank = Cell::blank();
    let mut row_cells: Vec<Cell> = Vec::with_capacity(if materialize { width } else { 0 });
    // Logical length of the row being built. This — not `row_cells.len()` — is
    // what every wrapping decision below reads, so the decisions are identical
    // whether or not the cells are written.
    let mut row_len = 0usize;
    // Emit one cell into the row: always counted, written only when
    // materializing. The debug assertion pins the two together, so the counting
    // mode cannot drift from the mode it is supposed to describe.
    macro_rules! emit {
        ($make:expr) => {{
            row_len += 1;
            if materialize {
                row_cells.push($make);
                debug_assert_eq!(
                    row_cells.len(),
                    row_len,
                    "row length drifted from its cells"
                );
            }
        }};
    }
    macro_rules! finish_row {
        ($line:expr) => {{
            out.push($line);
            row_len = 0;
            row_cells = Vec::with_capacity(if materialize { width } else { 0 });
        }};
    }
    let mut produced_any = false;
    let mut i = 0;
    while i < cells.len() {
        let cell = cells[i];
        let is_wide_lead =
            !cell.wide_continuation() && UnicodeWidthChar::width(cell.ch()) == Some(2);
        let unit = if is_wide_lead && width >= 2 { 2 } else { 1 };

        // Wrap before a wide pair that would straddle the right edge.
        if unit == 2 && row_len + unit > width && row_len != 0 {
            while row_len < width {
                emit!(blank);
            }
            finish_row!(Line::wrapped(std::mem::take(&mut row_cells)));
            produced_any = true;
        }

        if unit == 2 {
            if let Some(rec) = reprojector.as_mut() {
                rec.record(i, out.len(), row_len);
                if i + 1 < cells.len() && cells[i + 1].wide_continuation() {
                    rec.record(i + 1, out.len(), row_len + 1);
                }
            }
            let has_cont = i + 1 < cells.len() && cells[i + 1].wide_continuation();
            emit!(cell.hydrate(marks.marks_at(i)));
            if has_cont {
                emit!(cells[i + 1].hydrate(marks.marks_at(i + 1)));
                i += 2;
            } else {
                emit!(Cell::wide_spacer(cell.attrs()));
                i += 1;
            }
        } else {
            // Drop an orphaned continuation cell (its lead was degraded).
            if !cell.wide_continuation() {
                if let Some(rec) = reprojector.as_mut() {
                    rec.record(i, out.len(), row_len);
                }
                emit!(cell.hydrate(marks.marks_at(i)));
            }
            i += 1;
        }

        if row_len >= width {
            finish_row!(Line::wrapped(std::mem::take(&mut row_cells)));
            produced_any = true;
        }
    }

    if row_len != 0 || !produced_any {
        while row_len < width {
            emit!(blank);
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

    // Re-anchor button spans onto the produced rows as row-local segments.
    if let Some(rec) = reprojector {
        for (row, span) in rec.project(spans, keep, first_row) {
            if let Some(line) = out.get_mut(row) {
                line.button_spans.push(span);
            }
        }
    }
}
