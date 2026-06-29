// SPDX-License-Identifier: GPL-3.0-only
//! Resize reflow: re-wrapping the combined scrollback + visible buffer to a new
//! grid size, plus the width-unchanged fast path.
//!
//! Split out of [`super::screen`] (per the modularity directive) because resize
//! is the heaviest core operation and warrants focused tests. Two entry points
//! drive [`super::screen::Screen::resize`]:
//!
//! - [`reflow_lines`] — the general path: rejoin soft-wrapped rows into logical
//!   lines and re-wrap them to the new width. Used whenever the column count
//!   changes (and as the correctness oracle for the fast path in tests).
//! - [`resize_keep_width`] — the width-unchanged fast path: at the same width,
//!   re-wrapping reproduces identical rows, so it re-windows and re-cursors at
//!   O(rows) instead of O(cells). Proven byte-identical to [`reflow_lines`] for
//!   width-unchanged resizes by the differential tests below.
//!
//! [`resize_buffer_rows`] is the simple non-reflowing row truncate/pad used for
//! the alternate screen (apps repaint, so no re-wrap is needed).

use unicode_width::UnicodeWidthChar;

use super::prompt_marks::PromptKind;
use super::screen::{Line, blank_row};
use super::types::*;

pub(in crate::core) fn resize_buffer_rows(
    rows: &mut Vec<Line>,
    scrollback: &mut Vec<Line>,
    dimensions: Dimensions,
    discard_removed_rows: bool,
) {
    for row in rows.iter_mut() {
        row.resize(dimensions.columns, Cell::blank());
    }

    if rows.len() > dimensions.rows {
        let removed = rows.len() - dimensions.rows;
        if discard_removed_rows {
            rows.drain(0..removed);
        } else {
            scrollback.extend(rows.drain(0..removed));
        }
    }

    rows.resize_with(dimensions.rows, || blank_row(dimensions.columns));
}
/// A logical line collected from soft-wrapped physical rows, plus the flat
/// offset of the cursor within it (if the cursor was on one of those rows).
struct LogicalLine {
    cells: Vec<Cell>,
    cursor_offset: Option<usize>,
    cursor_row_offset: Option<usize>,
    cursor_column: Option<usize>,
    start_row: usize,
    /// OSC 133 prompt mark (SH1) captured from the first physical row of this
    /// logical line; re-stamped onto the first re-wrapped physical row so marks
    /// survive a width-changing resize.
    prompt_mark: Option<PromptKind>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::core) struct ReflowOptions {
    pub preserve_cursor_physical_line: bool,
    pub cursor_pending_wrap: bool,
    pub collapse_prompt_start_row: Option<usize>,
    /// Whether the shell applied output since the last resize. The
    /// `preserve_cursor_physical_line` override re-anchors the cursor onto its
    /// old physical row offset on the bet that a SIGWINCH-driven shell repaint
    /// will immediately follow and correct it. That bet only holds when a
    /// repaint is actually coming, i.e. when there was output since the last
    /// resize (the Linux interactive case is always true). For back-to-back
    /// resizes with no intervening output (the Windows split/close-without-typing
    /// case over ConPTY, which does not repaint on a bare resize), honoring the
    /// override clamps the cursor column and ratchets it toward the prompt start
    /// across cycles. When this is false, the override is skipped and the
    /// content-accurate cursor is kept.
    pub repaint_expected: bool,
    /// Whether the backend's shell authoritatively repaints with ABSOLUTE
    /// positioning on every resize, so the terminal must NOT move the cursor
    /// itself. True for the ConPTY (Windows) backend: conhost reflows its own
    /// screen buffer and re-emits an absolute `CUP` on every
    /// `ResizePseudoConsole`, so any cursor translation OdyTTY performs only
    /// fights that repaint (flinging the cursor rows away from where PSReadLine
    /// places it). When true, cursor placement is deferred to the shell: the
    /// content/scrollback rewrap still runs (history recovery — ConPTY does not
    /// resend scrollback), but `cursor_dest` is forced to `None` so the cursor
    /// is kept at its incoming position clamped to the new dims, then corrected
    /// by the shell's repaint on the next pump tick. Default false: a POSIX PTY
    /// delegates resize repaint to the app's SIGWINCH handler, which Linux
    /// shells service with a RELATIVE repaint the override is built to
    /// cooperate with, so the unix backend keeps today's behavior byte-identical.
    pub shell_owns_cursor_on_resize: bool,
    /// Number of combined-buffer rows that precede the OLD visible window, i.e.
    /// the prefix rows the lazy resize prepended to the live grid before reflow
    /// (`cursor_prefix` in `resize_lazy_with_options`). The incoming `cursor.row`
    /// is a COMBINED-buffer row (`combined_cursor_prefix + old_visible_row`), so
    /// subtracting this recovers the incoming VISIBLE row. Only the `None` cursor
    /// arm (cursor placement deferred — the `shell_owns_cursor_on_resize` path)
    /// reads it; the `Some` arm maps the cursor through its content cell and
    /// already subtracts `visible_start`. Default `0`: direct callers pass a
    /// `cursor.row` that is already visible-relative (empty scrollback), so the
    /// subtraction is a no-op and their behavior is byte-identical.
    pub combined_cursor_prefix: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core) struct ReflowResult {
    pub cursor: Position,
    pub pending_wrap: bool,
    pub collapsed_prompt_start_row: Option<usize>,
}
/// Reflow the combined `scrollback` + `rows` buffer to `dimensions`, preserving
/// content by rejoining soft-wrapped rows into logical lines and re-wrapping
/// them to the new width. Replaces `scrollback` and `rows` in place and returns
/// the cursor's new visible-grid position.
///
/// Policy (bounded first-prototype reflow):
/// - Logical lines are formed by joining consecutive rows whose [`Line::wrapped`]
///   marker is set; a hard line break (no marker) ends a logical line.
/// - Trailing plain blanks are trimmed from each logical line before re-wrapping
///   (but never past the cursor column), so a cleared-but-tall screen does not
///   bloat into many blank rows on shrink.
/// - Wide glyphs are kept whole: a wide pair never straddles the right edge.
/// - The visible window is the bottom `dimensions.rows` rows of the reflowed
///   buffer; everything above becomes scrollback. The cursor is mapped to its
///   character's new location and clamped into the visible grid.
#[cfg(test)]
pub(in crate::core) fn reflow_lines(
    scrollback: &mut Vec<Line>,
    rows: &mut Vec<Line>,
    dimensions: Dimensions,
    cursor: Position,
) -> Position {
    reflow_lines_with_options(
        scrollback,
        rows,
        dimensions,
        cursor,
        ReflowOptions::default(),
    )
    .cursor
}

pub(in crate::core) fn reflow_lines_with_options(
    scrollback: &mut Vec<Line>,
    rows: &mut Vec<Line>,
    dimensions: Dimensions,
    cursor: Position,
    options: ReflowOptions,
) -> ReflowResult {
    let new_cols = dimensions.columns;
    let new_rows = dimensions.rows;

    // Combined buffer, oldest first. The cursor's absolute row is its visible
    // row offset by the current scrollback height.
    let cursor_abs_row = scrollback.len() + cursor.row;
    let mut combined: Vec<Line> = Vec::with_capacity(scrollback.len() + rows.len());
    combined.append(scrollback);
    combined.append(rows);

    // 1) Segment into logical lines, joining soft-wrapped rows and tracking the
    //    cursor's flat offset within its logical line.
    let mut logicals: Vec<LogicalLine> = Vec::new();
    let mut current: Vec<Cell> = Vec::new();
    let mut current_cursor: Option<usize> = None;
    let mut current_cursor_row_offset: Option<usize> = None;
    let mut current_cursor_column: Option<usize> = None;
    let mut current_row_count = 0usize;
    let mut current_start_row = 0usize;
    let mut current_mark: Option<PromptKind> = None;
    for (idx, line) in combined.iter().enumerate() {
        if current.is_empty() {
            // First physical row of a new logical line: capture its mark.
            current_mark = line.prompt_mark;
            current_start_row = idx;
        } else if current_mark.is_none() {
            // Adopt a mark stamped on a continuation row when the first row
            // carried none (first non-`None` mark in the logical line wins).
            current_mark = line.prompt_mark;
        }
        if idx == cursor_abs_row {
            let column = if options.cursor_pending_wrap {
                line.cells.len()
            } else {
                cursor.column.min(line.cells.len())
            };
            current_cursor = Some(current.len() + column);
            current_cursor_row_offset = Some(current_row_count);
            current_cursor_column = Some(cursor.column);
        }
        current.extend(line.cells.iter().copied());
        current_row_count += 1;
        if !line.wrapped {
            logicals.push(LogicalLine {
                cells: std::mem::take(&mut current),
                cursor_offset: current_cursor.take(),
                cursor_row_offset: current_cursor_row_offset.take(),
                cursor_column: current_cursor_column.take(),
                start_row: current_start_row,
                prompt_mark: current_mark.take(),
            });
            current_row_count = 0;
        }
    }
    // Flush a trailing logical line whose last row was still marked wrapped.
    if !current.is_empty() || current_cursor.is_some() {
        logicals.push(LogicalLine {
            cells: current,
            cursor_offset: current_cursor,
            cursor_row_offset: current_cursor_row_offset,
            cursor_column: current_cursor_column,
            start_row: current_start_row,
            prompt_mark: current_mark,
        });
    }

    // Drop trailing blank logical lines that are just unused grid padding below
    // the content/cursor, so a partially-filled screen does not inflate the
    // reflowed buffer (which would otherwise scroll content off the top). A line
    // is kept if it holds the cursor or any non-blank cell; interior blank lines
    // are preserved. (Trailing blank *output* lines collapse here — a bounded,
    // documented reflow limitation.)
    let plain = Cell::blank();
    while logicals.len() > 1 {
        let last = logicals.last().expect("non-empty");
        let is_padding =
            last.cursor_offset.is_none() && last.cells.iter().all(|cell| *cell == plain);
        if is_padding {
            logicals.pop();
        } else {
            break;
        }
    }

    // 2) Re-wrap each logical line to the new width.
    let mut new_combined: Vec<Line> = Vec::new();
    let mut cursor_dest: Option<(usize, usize)> = None;
    let mut pending_wrap_dest = false;
    let mut collapsed_prompt_start_dest: Option<usize> = None;

    for logical in &logicals {
        if let Some(start_row) = options.collapse_prompt_start_row
            && logical.start_row >= start_row
            && logical.start_row <= cursor_abs_row
        {
            let row_index = new_combined.len();
            let mut row_cells = logical.cells.clone();
            row_cells.truncate(new_cols);
            row_cells.resize(new_cols, plain);
            let mut row = Line::unwrapped(row_cells);
            row.prompt_mark = logical.prompt_mark;
            new_combined.push(row);

            if logical.start_row == start_row {
                collapsed_prompt_start_dest = Some(row_index);
            }
            if logical.cursor_offset.is_some() {
                let column = logical
                    .cursor_column
                    .unwrap_or(cursor.column)
                    .min(new_cols - 1);
                cursor_dest = Some((row_index, column));
                pending_wrap_dest = options.cursor_pending_wrap && column == new_cols - 1;
            }
            continue;
        }

        // First physical row this logical line will produce; the prompt mark is
        // re-anchored here after re-wrapping. A logical line always produces at
        // least one row, so this index is valid afterward.
        let first_row = new_combined.len();
        // Trim trailing plain blanks fully: the cursor is mapped separately
        // (see below), so trailing blanks never need to be materialized as
        // extra rows.
        let mut keep = logical.cells.len();
        while keep > 0 && logical.cells[keep - 1] == plain {
            keep -= 1;
        }
        let cells = &logical.cells[..keep];
        // Where the cursor sits within this logical line's content, clamped to
        // the trimmed length (a cursor past the content lands at end-of-line).
        let cursor_target = logical.cursor_offset.map(|off| off.min(keep));

        let mut row_cells: Vec<Cell> = Vec::with_capacity(new_cols);
        let mut produced_any = false;
        let mut i = 0;
        while i < cells.len() {
            let cell = cells[i];
            let is_wide_lead =
                !cell.wide_continuation && UnicodeWidthChar::width(cell.ch) == Some(2);
            // A wide glyph needs two columns; if the grid is too narrow to hold
            // a pair, degrade it to width 1 (conservative wide-glyph handling).
            let unit = if is_wide_lead && new_cols >= 2 { 2 } else { 1 };

            // If a wide unit will not fit in the remaining columns, pad the row
            // and wrap before placing it so the pair stays whole.
            if unit == 2 && row_cells.len() + unit > new_cols && !row_cells.is_empty() {
                while row_cells.len() < new_cols {
                    row_cells.push(plain);
                }
                new_combined.push(Line::wrapped(std::mem::take(&mut row_cells)));
                produced_any = true;
                row_cells = Vec::with_capacity(new_cols);
            }

            // Cursor on a content cell: record its destination before placing.
            if cursor_target == Some(i) {
                cursor_dest = Some((new_combined.len(), row_cells.len()));
            }

            if unit == 2 {
                row_cells.push(cell);
                let cont = if i + 1 < cells.len() && cells[i + 1].wide_continuation {
                    cells[i + 1]
                } else {
                    Cell::wide_spacer(cell.attrs)
                };
                row_cells.push(cont);
                // Skip a real continuation cell if it followed the lead.
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

            if row_cells.len() >= new_cols {
                new_combined.push(Line::wrapped(std::mem::take(&mut row_cells)));
                produced_any = true;
                row_cells = Vec::with_capacity(new_cols);
            }
        }

        // Cursor at end-of-content (just past the last char). Map it onto the
        // last row of this logical line rather than spilling onto a new row, so
        // a full line keeps the cursor at the right edge (pending-wrap), exactly
        // as the pre-reflow grid did.
        let mut cursor_end_of_content_pending = false;
        if cursor_target == Some(keep) {
            if !row_cells.is_empty() {
                // Partial final row: cursor sits just after the last char.
                cursor_dest = Some((new_combined.len(), row_cells.len().min(new_cols - 1)));
            } else if produced_any {
                // The content exactly filled the last (still-wrapped) row, so
                // the end-of-content cursor sits PAST that row's last column —
                // i.e. in the pending-wrap state. Record it so the physical
                // cursor round-trips back to the true end-of-content offset on
                // the next resize (the model re-derives the logical offset from
                // the physical cursor; without pending-wrap an exact-fill end
                // collapses to one column short and drifts across resizes).
                cursor_dest = Some((new_combined.len() - 1, new_cols - 1));
                cursor_end_of_content_pending = true;
            } else {
                // Empty logical line: cursor at the start of the blank row.
                cursor_dest = Some((new_combined.len(), 0));
            }
        }

        if !row_cells.is_empty() || !produced_any {
            // Final (line-ending) row of this logical line.
            while row_cells.len() < new_cols {
                row_cells.push(plain);
            }
            new_combined.push(Line::unwrapped(row_cells));
        } else if let Some(last) = new_combined.last_mut() {
            // The line ended exactly on a wrap boundary: the last row is the
            // logical line's terminator, not a continuation.
            last.wrapped = false;
        }

        // Re-anchor the prompt mark onto this logical line's first physical row.
        if let Some(first) = new_combined.get_mut(first_row) {
            first.prompt_mark = logical.prompt_mark;
        }

        if options.preserve_cursor_physical_line && logical.cursor_offset.is_some() {
            // Rows this logical line produced after re-wrapping (always >= 1).
            let produced_rows = new_combined.len() - first_row;
            let row_offset = logical.cursor_row_offset.unwrap_or(0);
            // The override re-anchors the cursor to its saved physical
            // (row_offset, column). It exists for the `4af1fd6` case: a wrapped
            // prompt whose SIGWINCH-repainting shell emits a relative
            // `CUU(row_offset) + ED` it expects to land on the prompt's first
            // row, so the anchor keeps the cursor that many rows below the
            // re-wrapped top for the clear to cover the whole prompt.
            //
            // Honor it ONLY when both hold:
            //   * `options.repaint_expected` — the shell applied output since
            //     the last resize, so a repaint is in the loop to correct the
            //     anchored (clamped) cursor. When false (back-to-back resizes
            //     with no intervening output — the Windows split/close case over
            //     ConPTY, which does not repaint on a bare resize), the anchor's
            //     column clamp is never healed and, because the model re-derives
            //     the logical offset from the physical cursor each resize, it
            //     RATCHETS the cursor toward the prompt start across cycles.
            //     Skipping the override keeps the content-accurate end-of-content
            //     cursor (lossless logical position) and breaks the ratchet. This
            //     changes NO Linux behavior: every interactive Linux resize is
            //     followed by a repaint, so this is true on the next resize there.
            //   * `row_offset < produced_rows` — the saved physical row still
            //     exists; when the line COLLAPSED to fewer rows (a wrapped prompt
            //     widened back to one row on a pane-close) the saved offset is
            //     stale and would drag the cursor backward into the prompt prefix.
            if options.repaint_expected && row_offset < produced_rows {
                let row = first_row + row_offset;
                let column = logical
                    .cursor_column
                    .unwrap_or(cursor.column)
                    .min(new_cols - 1);
                cursor_dest = Some((row, column));
                pending_wrap_dest = options.cursor_pending_wrap && column == new_cols - 1;
                cursor_end_of_content_pending = false;
            }
        }

        // An exact-fill end-of-content cursor that was NOT re-anchored above is
        // in the pending-wrap state (it sits past the last column of a full
        // row). Preserve that so it round-trips to the true offset next resize.
        if cursor_end_of_content_pending {
            pending_wrap_dest = true;
        }
    }

    // 3) Split into scrollback + a bottom-anchored visible window.
    let total = new_combined.len();
    let visible_start = total.saturating_sub(new_rows);
    let new_scrollback: Vec<Line> = new_combined.drain(0..visible_start).collect();
    let mut visible = new_combined;
    while visible.len() < new_rows {
        visible.push(blank_row(new_cols));
    }

    *scrollback = new_scrollback;
    *rows = visible;

    // Backend authoritatively repaints with absolute positioning on resize
    // (ConPTY/conhost): suppress ALL cursor translation and keep the incoming
    // cursor clamped to the new dims (the `None` arm below). The content rewrap
    // above still applied (history recovery); only cursor placement is deferred
    // to the shell's repaint. Keep the incoming pending-wrap — the shell owns it.
    if options.shell_owns_cursor_on_resize {
        cursor_dest = None;
        pending_wrap_dest = options.cursor_pending_wrap;
    }

    let cursor = match cursor_dest {
        Some((abs_row, col)) => {
            let column = col.min(new_cols - 1);
            pending_wrap_dest |= options.cursor_pending_wrap && column == new_cols - 1;
            Position {
                row: abs_row.saturating_sub(visible_start).min(new_rows - 1),
                column,
            }
        }
        // Cursor placement is deferred to the shell (the
        // `shell_owns_cursor_on_resize` / ConPTY path forces `cursor_dest =
        // None`): keep the incoming cursor at its old VISIBLE position, clamped
        // to the new dims, so it does not jump before the shell's absolute
        // repaint corrects it on the next pump tick.
        //
        // `cursor.row` here is a COMBINED-buffer row. In the production lazy
        // path it equals `combined_cursor_prefix + old_visible_row` (the resize
        // prepends `combined_cursor_prefix` pulled-scrollback rows to the live
        // grid before reflow). Subtracting the TRUE prefix recovers exactly the
        // old visible row regardless of how the prefix re-wrapped.
        //
        // We deliberately subtract `combined_cursor_prefix`, NOT `visible_start`:
        // when content ABOVE the cursor rewraps into more (or fewer) rows on a
        // width change, `visible_start` (= total - new_rows) diverges from the
        // prefix, so `cursor.row - visible_start` would fling the cursor off its
        // visible row (to the top on a shrink that expands the history above).
        // Computing the old visible row from the old grid geometry is equivalent
        // but needs the old dims threaded here too; subtracting the prefix is the
        // minimal, exact recovery. Direct callers pass prefix `0`, so this is a
        // no-op for them.
        None => Position {
            row: cursor
                .row
                .saturating_sub(options.combined_cursor_prefix)
                .min(new_rows - 1),
            column: cursor.column.min(new_cols - 1),
        },
    };
    ReflowResult {
        cursor,
        pending_wrap: pending_wrap_dest,
        collapsed_prompt_start_row: collapsed_prompt_start_dest
            .and_then(|row| row.checked_sub(visible_start))
            .filter(|row| *row < new_rows),
    }
}
/// Width-unchanged resize fast path: produces the byte-identical
/// `scrollback`/`rows`/cursor that [`reflow_lines`] would for a resize that does
/// not change the column count, but at O(rows) row moves instead of O(cells)
/// copies.
///
/// # Why this is equivalent to `reflow_lines`
///
/// When the width is unchanged, re-wrapping a logical line reproduces its exact
/// source physical rows: soft-wrapped (non-final) rows are full by construction,
/// so they carry no trailing blanks to trim, and a final row's trailing-blank
/// trim followed by re-padding to the same width yields the identical row. So
/// the only observable transforms `reflow_lines` performs are (a) collapsing
/// trailing blank *logical lines* into nothing, (b) re-anchoring the visible
/// window to the bottom `new_rows`, and (c) snapping the cursor column to the
/// end of its row's trimmed content. This function performs exactly those three
/// at row granularity. The differential `reflow_fast_path_tests` prove
/// byte-identical output against `reflow_lines` across the reachable state space.
///
/// Assumes the buffer is well-formed (a soft-wrapped row is full, never a blank
/// continuation), which holds for every state real terminal operations produce;
/// the width-changing path remains the general, unconditional reflow.
///
/// Thin `shell_owns_cursor=false` wrapper over [`resize_keep_width_with_options`],
/// used by the differential parity oracle tests; production goes through
/// `resize_keep_width_with_options` so the ConPTY cursor-deferral flag threads.
#[cfg(test)]
pub(in crate::core) fn resize_keep_width(
    scrollback: &mut Vec<Line>,
    rows: &mut Vec<Line>,
    dimensions: Dimensions,
    cursor: Position,
) -> Position {
    resize_keep_width_with_options(scrollback, rows, dimensions, cursor, false)
}

/// Width-unchanged fast path with the `shell_owns_cursor_on_resize` flag.
///
/// When `shell_owns_cursor` is true (the ConPTY backend), the cursor is kept at
/// its incoming column clamped to the new width instead of being snapped to its
/// row's trimmed content — mirroring the cursor-deferral the width-changing path
/// applies, so the shell's absolute repaint owns placement. When false the
/// behavior is byte-identical to today (the `resize_keep_width` wrapper above),
/// keeping `reflow_fast_path_tests` parity with `reflow_lines`.
pub(in crate::core) fn resize_keep_width_with_options(
    scrollback: &mut Vec<Line>,
    rows: &mut Vec<Line>,
    dimensions: Dimensions,
    cursor: Position,
    shell_owns_cursor: bool,
) -> Position {
    let width = dimensions.columns;
    let new_rows = dimensions.rows;
    let cursor_abs = scrollback.len() + cursor.row;

    // Combined buffer, oldest first — moved, not cell-copied.
    let mut combined: Vec<Line> = Vec::with_capacity(scrollback.len() + rows.len());
    combined.append(scrollback);
    combined.append(rows);

    // Snap the cursor column to its row's trimmed content, mirroring
    // `reflow_lines`: a cursor sitting on (or past) trailing blanks lands at the
    // end of content. An interior soft-wrapped row is full, so its content
    // length is the full width and the column is preserved. When the shell owns
    // the cursor (ConPTY), skip the snap and keep the incoming column clamped —
    // the shell's absolute repaint will place it.
    let plain = Cell::blank();
    let cursor_col = if shell_owns_cursor {
        cursor.column.min(width.saturating_sub(1))
    } else {
        let row = &combined[cursor_abs];
        let content_len = if row.wrapped {
            width
        } else {
            let mut k = row.cells.len();
            while k > 0 && row.cells[k - 1] == plain {
                k -= 1;
            }
            k
        };
        cursor.column.min(content_len).min(width.saturating_sub(1))
    };

    // Collapse trailing blank logical lines (a maximal `wrapped`-joined run whose
    // rows are all blank and that does not hold the cursor), keeping >= 1 row.
    let mut keep_end = combined.len();
    while keep_end > 1 {
        let mut line_start = keep_end - 1;
        while line_start > 0 && combined[line_start - 1].wrapped {
            line_start -= 1;
        }
        let holds_cursor = cursor_abs >= line_start && cursor_abs < keep_end;
        let all_blank = combined[line_start..keep_end]
            .iter()
            .all(|line| line.cells.iter().all(|cell| *cell == plain));
        if !holds_cursor && all_blank {
            keep_end = line_start;
        } else {
            break;
        }
    }
    combined.truncate(keep_end);

    // Bottom-anchored visible window; the rest becomes scrollback.
    let total = combined.len();
    let visible_start = total.saturating_sub(new_rows);
    let new_scrollback: Vec<Line> = combined.drain(0..visible_start).collect();
    let mut visible = combined;
    while visible.len() < new_rows {
        visible.push(blank_row(width));
    }

    *scrollback = new_scrollback;
    *rows = visible;

    Position {
        row: cursor_abs.saturating_sub(visible_start).min(new_rows - 1),
        column: cursor_col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 8;

    /// A content row: `text` left-aligned, blank-padded to width `W`, ending a
    /// logical line (unwrapped).
    fn content(text: &str) -> Line {
        let mut cells: Vec<Cell> = text
            .chars()
            .map(|c| Cell::new(c, Attrs::default()))
            .collect();
        assert!(cells.len() <= W);
        while cells.len() < W {
            cells.push(Cell::blank());
        }
        Line::unwrapped(cells)
    }

    /// A full, soft-wrapped row of `W` repeated chars (continues into the next).
    fn wrapped_full(ch: char) -> Line {
        Line::wrapped(vec![Cell::new(ch, Attrs::default()); W])
    }

    fn blank() -> Line {
        blank_row(W)
    }

    /// Run both the oracle (`reflow_lines`) and the fast path
    /// (`resize_keep_width`) on identical clones at the unchanged width `W` and
    /// `new_rows`, asserting byte-identical scrollback, rows, and cursor.
    fn assert_parity(scrollback: &[Line], rows: &[Line], cursor: Position, new_rows: usize) {
        let dims = Dimensions::new(W, new_rows);

        let mut sb_oracle = scrollback.to_vec();
        let mut rows_oracle = rows.to_vec();
        let cur_oracle = reflow_lines(&mut sb_oracle, &mut rows_oracle, dims, cursor);

        let mut sb_fast = scrollback.to_vec();
        let mut rows_fast = rows.to_vec();
        let cur_fast = resize_keep_width(&mut sb_fast, &mut rows_fast, dims, cursor);

        assert_eq!(
            cur_fast, cur_oracle,
            "cursor mismatch (new_rows={new_rows}, cursor={cursor:?})"
        );
        assert_eq!(
            sb_fast, sb_oracle,
            "scrollback mismatch (new_rows={new_rows}, cursor={cursor:?})"
        );
        assert_eq!(
            rows_fast, rows_oracle,
            "rows mismatch (new_rows={new_rows}, cursor={cursor:?})"
        );
    }

    /// Sweep a state across grow/shrink/same/extreme target heights.
    fn sweep(scrollback: &[Line], rows: &[Line], cursor: Position) {
        let h = rows.len();
        for &nr in &[
            1usize,
            2,
            h.saturating_sub(3).max(1),
            h,
            h + 1,
            h + 5,
            h + 20,
        ] {
            assert_parity(scrollback, rows, cursor, nr);
        }
    }

    #[test]
    fn parity_fresh_blank_grid() {
        let rows = vec![blank(), blank(), blank(), blank()];
        sweep(&[], &rows, Position { row: 0, column: 0 });
    }

    #[test]
    fn parity_content_then_blanks_cursor_at_content_end() {
        let rows = vec![
            content("hello"),
            content("world"),
            blank(),
            blank(),
            blank(),
        ];
        sweep(&[], &rows, Position { row: 1, column: 5 });
    }

    #[test]
    fn parity_cursor_on_blank_line_beyond_content() {
        // Cursor parked far below content on a blank line (CUP into blank area):
        // the oracle keeps that line and snaps the column to 0.
        let rows = vec![content("abc"), blank(), blank(), blank(), blank(), blank()];
        sweep(&[], &rows, Position { row: 4, column: 6 });
    }

    #[test]
    fn parity_cursor_on_trailing_blanks_of_content_row() {
        // Cursor sits on the trailing blanks of a content row; the oracle snaps
        // the column to the end of trimmed content.
        let rows = vec![content("hi"), blank(), blank()];
        for col in 0..W {
            assert_parity(
                &[],
                &rows,
                Position {
                    row: 0,
                    column: col,
                },
                3,
            );
            assert_parity(
                &[],
                &rows,
                Position {
                    row: 0,
                    column: col,
                },
                6,
            );
            assert_parity(
                &[],
                &rows,
                Position {
                    row: 0,
                    column: col,
                },
                1,
            );
        }
    }

    #[test]
    fn parity_soft_wrapped_logical_line() {
        // A logical line spanning two physical rows (full wrapped + partial).
        let rows = vec![wrapped_full('x'), content("tail"), blank(), blank()];
        // Cursor on the interior (full) row: column preserved.
        sweep(&[], &rows, Position { row: 0, column: 3 });
        // Cursor on the continuation row.
        sweep(&[], &rows, Position { row: 1, column: 2 });
    }

    #[test]
    fn parity_deep_scrollback() {
        let mut sb: Vec<Line> = Vec::new();
        for i in 0..200 {
            sb.push(content(&format!("L{i}")[..(3.min(format!("L{i}").len()))]));
        }
        let rows = vec![content("vis0"), content("vis1"), blank(), blank()];
        // Cursor at bottom, mid, and scrolled-back-equivalent positions.
        sweep(&sb, &rows, Position { row: 1, column: 4 });
        sweep(&sb, &rows, Position { row: 0, column: 0 });
    }

    #[test]
    fn parity_all_blank_with_scrollback() {
        let sb = vec![content("old"), blank(), content("data")];
        let rows = vec![blank(), blank(), blank()];
        sweep(&sb, &rows, Position { row: 0, column: 0 });
    }

    #[test]
    fn parity_interior_blank_lines_preserved() {
        // Interior blank lines (between content) must survive; only trailing
        // padding collapses.
        let rows = vec![content("a"), blank(), content("b"), blank(), blank()];
        sweep(&[], &rows, Position { row: 2, column: 1 });
    }

    #[test]
    fn parity_single_row_grid() {
        let rows = vec![content("solo")];
        sweep(&[], &rows, Position { row: 0, column: 2 });
    }

    #[test]
    fn parity_cursor_at_far_bottom_shrink() {
        // Tall grid with content near the bottom, cursor at the last row; shrink
        // hard so the cursor's row would scroll into history.
        let mut rows = vec![blank(); 10];
        rows[8] = content("near");
        rows[9] = content("bottom");
        sweep(&[], &rows, Position { row: 9, column: 6 });
    }

    /// Direct behavioral check (not just parity): a width-unchanged grow keeps
    /// content anchored and does not corrupt rows.
    #[test]
    fn keep_width_grow_preserves_content() {
        let rows = vec![content("keep"), content("me"), blank()];
        let mut sb = Vec::new();
        let mut r = rows.clone();
        let cur = resize_keep_width(
            &mut sb,
            &mut r,
            Dimensions::new(W, 6),
            Position { row: 1, column: 2 },
        );
        assert_eq!(r.len(), 6);
        assert_eq!(r[0], content("keep"));
        assert_eq!(r[1], content("me"));
        assert_eq!(cur, Position { row: 1, column: 2 });
    }

    #[test]
    fn widen_collapsing_wrapped_prompt_keeps_cursor_at_content_end() {
        // A prompt that WRAPPED to two physical rows at a narrow width, with the
        // cursor parked at end-of-content (cursor_row_offset == 1). Widening so
        // the whole prompt fits on ONE row collapses it back to a single row:
        // the saved physical (row_offset, column) is now stale, and the
        // `preserve_cursor_physical_line` override must NOT drag the cursor
        // backward into the prompt prefix. It must keep the content-accurate
        // end-of-content position. Regression for the PSReadLine-over-ConPTY
        // cursor drag observed after a pane split closes (widen back to 1 row).
        let prompt = "PS C:\\Users>"; // 12 chars
        let chars: Vec<char> = prompt.chars().collect();
        assert_eq!(chars.len(), 12);

        // Build the narrow (width 8) grid exactly as it exists pre-resize: row0
        // is a full, soft-wrapped row of the first 8 chars; row1 holds the
        // remaining 4 chars, blank-padded, ending the logical line.
        let narrow = 8usize;
        let row0: Vec<Cell> = chars[..narrow]
            .iter()
            .map(|&c| Cell::new(c, Attrs::default()))
            .collect();
        let mut row1: Vec<Cell> = chars[narrow..]
            .iter()
            .map(|&c| Cell::new(c, Attrs::default()))
            .collect();
        let content_in_row1 = row1.len(); // 4: cursor sits just past these
        while row1.len() < narrow {
            row1.push(Cell::blank());
        }

        let mut scrollback: Vec<Line> = Vec::new();
        let mut rows = vec![Line::wrapped(row0), Line::unwrapped(row1)];
        // Cursor at end-of-content on the second physical row (row_offset == 1).
        let cursor = Position {
            row: 1,
            column: content_in_row1,
        };

        // Widen to a width where the whole 12-char prompt fits on one row.
        let result = reflow_lines_with_options(
            &mut scrollback,
            &mut rows,
            Dimensions::new(16, 4),
            cursor,
            ReflowOptions {
                preserve_cursor_physical_line: true,
                cursor_pending_wrap: false,
                collapse_prompt_start_row: None,
                // A repaint follows a real widen on Linux; the collapse guard
                // (row_offset >= produced_rows) is what protects this case, not
                // the discriminator, so honor the override here.
                repaint_expected: true,
                shell_owns_cursor_on_resize: false,
                combined_cursor_prefix: 0,
            },
        );

        // The cursor must land at the true end of content (column == prompt
        // length), NOT clamped to the stale narrow-row column (4) inside the
        // path.
        assert_eq!(
            result.cursor,
            Position {
                row: 0,
                column: chars.len(),
            },
            "cursor dragged into prompt prefix on collapse-widen"
        );
    }

    #[test]
    fn multi_cycle_resize_does_not_ratchet_cursor_into_prompt() {
        // The multi-cycle column RATCHET, exercised at the reflow layer with the
        // `repaint_expected` discriminator set realistically. A single-line
        // prompt with the cursor at end-of-content (empty input) is driven
        // through repeated narrow->wide->narrower->wide reflows. The terminal
        // model stores the cursor ONLY as physical (row, column) + pending_wrap,
        // with no memory of the logical offset, so each reflow's output cursor
        // becomes the next reflow's input. This harness mirrors `Screen::resize`
        // EXACTLY — re-feeding BOTH `cursor` and `pending_wrap`, threading
        // `cursor_pending_wrap` back in, and setting `repaint_expected` true ONLY
        // for the first resize after the (single) prompt print and false for the
        // back-to-back resizes that follow with no intervening output (the
        // Windows split/close-without-typing case; the Screen sets the flag in
        // `print_char` and clears it at the end of every `resize`).
        //
        // Without the discriminator gate the override clamps the column on each
        // narrowing and — because the offset is re-derived from the displaced
        // physical cursor — ratchets it monotonically toward the prompt start
        // (col ~1 by the end). With the gate, only the first (non-wrapping)
        // resize honors the override and every subsequent wrapping resize keeps
        // the content-accurate cursor, which round-trips the true offset
        // losslessly, so the column never ratchets.
        let text = "PS C:\\Users\\foo>"; // 16 chars, no trailing input
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        assert_eq!(len, 16);

        // A single hard logical line of `text`, blank-padded to `width`.
        fn prompt_line(width: usize, chars: &[char]) -> Vec<Line> {
            let mut cells: Vec<Cell> = chars
                .iter()
                .map(|&c| Cell::new(c, Attrs::default()))
                .collect();
            while cells.len() < width {
                cells.push(Cell::blank());
            }
            vec![Line::unwrapped(cells)]
        }

        // Tall enough that even width 2 (16 chars -> 8 rows) stays fully visible,
        // so the logical line never straddles the scrollback boundary.
        const H: usize = 12;
        let mut scrollback: Vec<Line> = Vec::new();
        let mut rows = prompt_line(80, &chars);
        let mut cursor = Position {
            row: 0,
            column: len,
        };
        let mut pending = false;

        // Recipe F: open a split (narrow), then close it (widen back to 80),
        // progressively narrower. The FIRST narrow width (40) does NOT wrap the
        // 16-col prompt — matching the real case where the first split rarely
        // wraps a short prompt — so the one override that fires (repaint_expected
        // true, set by the prompt print) is harmless. The later narrow widths
        // (10, 6, 2) DO wrap, but arrive with repaint_expected false.
        for (i, &w) in [40usize, 80, 10, 80, 6, 80, 2, 80].iter().enumerate() {
            let dims = Dimensions::new(w, H);
            let result = reflow_lines_with_options(
                &mut scrollback,
                &mut rows,
                dims,
                cursor,
                ReflowOptions {
                    preserve_cursor_physical_line: true,
                    cursor_pending_wrap: pending,
                    collapse_prompt_start_row: None,
                    // Output (the prompt print) happened only before cycle 0; no
                    // output between subsequent resizes, so the flag is true for
                    // the first resize and false thereafter.
                    repaint_expected: i == 0,
                    shell_owns_cursor_on_resize: false,
                    combined_cursor_prefix: 0,
                },
            );
            cursor = result.cursor;
            pending = result.pending_wrap;
        }

        // After the final widen to 80 the prompt is one 16-col line; the cursor
        // must still be at end-of-content (col 16), NOT ratcheted into the path.
        assert_eq!(
            cursor.column, len,
            "cursor ratcheted into the prompt (got col {})",
            cursor.column
        );
    }

    /// Build a single long logical line of `text_len` 'x' chars, soft-wrapped at
    /// `dims.columns` into as many physical rows as needed (full rows marked
    /// `wrapped`, the tail `unwrapped` + blank-padded), then blank-padded to
    /// `dims.rows`. Returns `(scrollback=empty, rows, cursor)`.
    fn long_wrapped_line(
        text_len: usize,
        dims: Dimensions,
        cursor_row: usize,
        cursor_col: usize,
    ) -> (Vec<Line>, Vec<Line>, Position) {
        let w = dims.columns;
        let mut rows: Vec<Line> = Vec::new();
        let mut remaining = text_len;
        while remaining > 0 {
            let take = remaining.min(w);
            let mut cells: Vec<Cell> = (0..take)
                .map(|_| Cell::new('x', Attrs::default()))
                .collect();
            if remaining <= w {
                while cells.len() < w {
                    cells.push(Cell::blank());
                }
                rows.push(Line::unwrapped(cells));
            } else {
                rows.push(Line::wrapped(cells));
            }
            remaining -= take;
        }
        while rows.len() < dims.rows {
            rows.push(blank_row(w));
        }
        (
            Vec::new(),
            rows,
            Position {
                row: cursor_row,
                column: cursor_col,
            },
        )
    }

    #[test]
    fn shell_owns_cursor_defers_placement_and_flag_false_unchanged() {
        // A long logical line wrapped at width 50 with the cursor on a
        // continuation row. With `shell_owns_cursor_on_resize` TRUE the cursor
        // stays at its incoming position clamped to the new dims (the foot model:
        // defer to the shell's absolute repaint). With the flag FALSE the same
        // inputs translate the cursor (today's behavior) to a DIFFERENT position
        // — proving the flag is load-bearing and the buffer is non-trivial.
        let old = Dimensions::new(50, 28);
        let new = Dimensions::new(100, 28);
        let cin_row = 2;
        let cin_col = 12;
        let text_len = 140; // 3 wrapped rows at width 50 (50 + 50 + 40)

        let (mut sb_t, mut rows_t, cursor) = long_wrapped_line(text_len, old, cin_row, cin_col);
        let res_true = reflow_lines_with_options(
            &mut sb_t,
            &mut rows_t,
            new,
            cursor,
            ReflowOptions {
                preserve_cursor_physical_line: true,
                cursor_pending_wrap: false,
                collapse_prompt_start_row: None,
                repaint_expected: true,
                shell_owns_cursor_on_resize: true,
                combined_cursor_prefix: 0,
            },
        );
        assert_eq!(
            res_true.cursor,
            Position {
                row: cin_row.min(new.rows - 1),
                column: cin_col.min(new.columns - 1),
            },
            "flag true must keep the incoming cursor clamped to new dims"
        );

        let (mut sb_f, mut rows_f, cursor_f) = long_wrapped_line(text_len, old, cin_row, cin_col);
        let res_false = reflow_lines_with_options(
            &mut sb_f,
            &mut rows_f,
            new,
            cursor_f,
            ReflowOptions {
                preserve_cursor_physical_line: true,
                cursor_pending_wrap: false,
                collapse_prompt_start_row: None,
                repaint_expected: true,
                shell_owns_cursor_on_resize: false,
                combined_cursor_prefix: 0,
            },
        );
        assert_ne!(
            res_false.cursor, res_true.cursor,
            "flag false must translate the cursor (proves the flag changes behavior)"
        );
    }

    #[test]
    fn shell_owns_cursor_trace_replay_pins_operator_failures() {
        // The three real divergences captured in the on-device resize-storm
        // trace, where today's reflow flung the cursor far from where
        // PSReadLine's absolute repaint placed it. With
        // `shell_owns_cursor_on_resize` true the cursor must stay at its
        // incoming position clamped to the new dims — no row/col jump — so the
        // shell's repaint owns placement.
        //
        // ((old_cols, old_rows), (in_row, in_col), (new_cols, new_rows),
        //  (documented_bad_out_row, documented_bad_out_col))
        let cases = [
            (
                (50usize, 28usize),
                (8usize, 12usize),
                (100usize, 28usize),
                (5usize, 62usize),
            ),
            ((100, 28), (16, 34), (50, 28), (24, 34)),
            ((31, 27), (24, 16), (100, 27), (26, 46)),
        ];
        for ((oc, or), (cin_row, cin_col), (nc, nr), (bad_row, bad_col)) in cases {
            // A long line that wraps at the old width and is tall enough to put
            // the cursor on a wrapped continuation row, so today's rewrap would
            // genuinely relocate it.
            let text_len = oc * (cin_row + 2);
            let (mut sb, mut rows, cursor) =
                long_wrapped_line(text_len, Dimensions::new(oc, or), cin_row, cin_col);

            let result = reflow_lines_with_options(
                &mut sb,
                &mut rows,
                Dimensions::new(nc, nr),
                cursor,
                ReflowOptions {
                    preserve_cursor_physical_line: true,
                    cursor_pending_wrap: false,
                    collapse_prompt_start_row: None,
                    repaint_expected: true,
                    shell_owns_cursor_on_resize: true,
                    combined_cursor_prefix: 0,
                },
            );

            let expected = Position {
                row: cin_row.min(nr - 1),
                column: cin_col.min(nc - 1),
            };
            assert_eq!(
                result.cursor, expected,
                "shell-owns-cursor must keep incoming cursor clamped ({oc}x{or} -> {nc}x{nr})"
            );
            assert_ne!(
                result.cursor,
                Position {
                    row: bad_row,
                    column: bad_col,
                },
                "must not reproduce the captured fling ({oc}x{or} -> {nc}x{nr})"
            );
        }
    }

    #[test]
    fn shell_owns_cursor_fast_path_keeps_incoming_skips_trailing_snap() {
        // The width-UNCHANGED fast path (`resize_keep_width_with_options`) is
        // production-reachable on Windows via a row-only resize (dragging the
        // window's bottom edge): `resize_lazy_with_options` routes it to the
        // fast path, where ConPTY still repaints absolutely, so the cursor must
        // be kept (not snapped to trailing content). Buffer: a short-content row
        // with the cursor sitting in the trailing-blank region past the content.
        let rows = vec![content("abc"), blank(), blank(), blank()];
        // Cursor at column 6, well past "abc" (content end = 3), in the blanks.
        let cursor = Position { row: 0, column: 6 };
        // Same width W (fast path), shrink rows (row-only resize).
        let dims = Dimensions::new(W, 3);

        let mut sb_true = Vec::new();
        let mut rows_true = rows.clone();
        let cur_true =
            resize_keep_width_with_options(&mut sb_true, &mut rows_true, dims, cursor, true);

        let mut sb_false = Vec::new();
        let mut rows_false = rows.clone();
        let cur_false =
            resize_keep_width_with_options(&mut sb_false, &mut rows_false, dims, cursor, false);

        // shell_owns_cursor: keep the incoming column clamped to width, skip the
        // trailing-content snap.
        assert_eq!(
            cur_true.column,
            cursor.column.min(W - 1),
            "fast path must keep the incoming cursor when the shell owns it"
        );
        // Default path snaps the cursor back to the content end (column 3), so
        // the two must differ — proving the fast-path branch is load-bearing.
        assert_eq!(
            cur_false.column, 3,
            "default fast path snaps to content end"
        );
        assert_ne!(
            cur_true.column, cur_false.column,
            "fast-path shell-owns-cursor branch must change behavior"
        );
    }
}
