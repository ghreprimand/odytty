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
use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

use super::button::{ButtonId, ButtonSpan, MAX_BUTTON_SPANS_PER_LINE, SpanReprojector};
use super::prompt_marks::PromptKind;
use super::reflow::{ReflowOptions, reflow_lines_with_options, resize_keep_width_with_options};
use super::screen::{Line, blank_row};
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
    /// Button spans of this logical line in FLAT-cell coordinates (the same
    /// space `cells` indexes). Carried like `prompt_mark`, extended from "mark
    /// on the first row" to "column ranges within the line": physical-row
    /// spans are offset into flat coordinates when rows merge in
    /// [`Scrollback::push_row`], and re-projected onto physical rows by
    /// [`project_line_into`], so buttons survive scroll-out and re-wrap.
    /// Empty for the overwhelmingly common span-free line (no allocation).
    button_spans: Vec<ButtonSpan>,
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

/// Default maximum number of logical lines retained in scrollback. Each scrolled
/// -off hard-terminated line is one logical line, so this is the user-facing
/// "lines of history" cap. Chosen to match the common terminal default (xterm,
/// kitty, alacritty all default near this) and bounds steady-state memory: at
/// ~36 B/cell and 80 columns a logical line is ~2.9 KB, so 10k lines is ~28 MB
/// of scrollback before projection. Without a cap, a process that streams
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
        if self.enforce_limit() {
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
    pub(in crate::core) fn physical_len(&self, width: usize) -> usize {
        self.ensure_cache(width);
        self.cache.borrow().rows.len()
    }

    pub(in crate::core) fn trim_epoch(&self) -> u64 {
        self.trim_epoch
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
        self.pushed_rows = self.pushed_rows.wrapping_add(1);
        let wrapped = row.wrapped;
        if let Some(last) = self.lines.back_mut()
            && last.open
        {
            // Continuation of an open logical line: extend cells. A continuation
            // row normally carries no mark, but if the logical line's true first
            // row already scrolled off before the mark was stamped (the mark
            // landed on a still-visible continuation row), adopt it here so the
            // line keeps its first non-`None` mark.
            let offset = last.cells.len();
            last.cells.extend(row.cells.iter().copied());
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
        } else {
            self.lines.push_back(LogicalLine {
                cells: row.cells,
                open: wrapped,
                prompt_mark: row.prompt_mark,
                button_spans: row.button_spans,
            });
        }
        self.enforce_limit();
        self.invalidate();
    }

    /// Evict oldest history so the store stays within `limit` logical lines and
    /// no single (open) logical line exceeds [`MAX_LOGICAL_LINE_CELLS`] cells.
    /// Returns `true` if anything was trimmed. Called after every `push_row` and
    /// on `set_limit`; a `0` limit only enforces the per-line cell ceiling.
    fn enforce_limit(&mut self) -> bool {
        let mut trimmed = false;

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
            trimmed = true;
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
            trimmed = true;
        }

        if trimmed {
            self.trim_epoch = self.trim_epoch.wrapping_add(1);
        }
        trimmed
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
        let extend_blank =
            pulled_rows >= new_rows && sb.lines.back().is_some_and(|l| cells_all_blank(&l.cells));
        if !(need_more || extend_blank) {
            break;
        }
        match sb.lines.pop_back() {
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
    let mut current: Vec<Cell> = Vec::new();
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
        current.extend(row.cells.iter().copied());
        if !row.wrapped {
            lines.push(LogicalLine {
                cells: std::mem::take(&mut current),
                open: false,
                prompt_mark: current_mark.take(),
                button_spans: std::mem::take(&mut current_spans),
            });
        }
    }
    if !current.is_empty() {
        lines.push(LogicalLine {
            cells: current,
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
        project_line_into(
            &line.cells,
            width,
            line.open,
            line.prompt_mark,
            &line.button_spans,
            &mut out,
        );
    }
    out
}

fn cells_all_blank(cells: &[Cell]) -> bool {
    let plain = Cell::blank();
    cells.iter().all(|c| *c == plain)
}

fn count_projected_rows(cells: &[Cell], width: usize, open: bool) -> usize {
    let mut tmp = Vec::new();
    project_line_into(cells, width, open, None, &[], &mut tmp);
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
fn project_line_into(
    cells: &[Cell],
    width: usize,
    open: bool,
    mark: Option<PromptKind>,
    spans: &[ButtonSpan],
    out: &mut Vec<Line>,
) {
    let plain = Cell::blank();
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
            if let Some(rec) = reprojector.as_mut() {
                rec.record(i, out.len(), row_cells.len());
                if i + 1 < cells.len() && cells[i + 1].wide_continuation {
                    rec.record(i + 1, out.len(), row_cells.len() + 1);
                }
            }
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
                if let Some(rec) = reprojector.as_mut() {
                    rec.record(i, out.len(), row_cells.len());
                }
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

    // Re-anchor button spans onto the produced rows as row-local segments.
    if let Some(rec) = reprojector {
        for (row, span) in rec.project(spans, keep, first_row) {
            if let Some(line) = out.get_mut(row) {
                line.button_spans.push(span);
            }
        }
    }
}
