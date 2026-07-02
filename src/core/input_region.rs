// SPDX-License-Identifier: GPL-3.0-only
//! Core-owned model of the live editable prompt-input region (B-DESIGN §2).
//!
//! The select+Delete feature must never send bytes to the shell that do not
//! correspond to a real edit of the shell's line-editor buffer — a wrong delete
//! is worse than a no-op. That charter requires the input-region geometry to be
//! computed HERE, in core, where the soft-wrap `wrapped` flags, the cursor, the
//! OSC 133 `B` input-start mark, and the (future) private edit-region signal all
//! co-reside. The flat [`Snapshot`](crate::core::Snapshot) handed to native has
//! no per-row wrap flag and no logical-line grouping, so any multi-row
//! derivation done consumer-side would be guessing.
//!
//! [`derive_input_region`] is a pure function over borrowed row views so the
//! whole geometry model is unit-testable without a `Screen` (and without a
//! GPU). `Screen::input_region()` assembles the live inputs.

use super::screen::Line;
use super::types::Position;

/// Authoritative bounds of the live editable input, computed in core.
///
/// Rows are in **visible-viewport coordinates** offset by the caller-supplied
/// `scrollback_len`, i.e. the same ABSOLUTE row space (`scrollback_len +
/// visible_row`) used by `active_prompt_input_start` and the selection ranges.
/// `None` from the deriver means no editable input is present and the caller
/// must no-op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputRegion {
    /// First physical row of input (the OSC 133 `B` row), absolute.
    pub start_row: usize,
    /// Column where input begins on `start_row` (the `133;B` column).
    pub start_col: usize,
    /// Inclusive last physical row of input, absolute. Equals `start_row` for
    /// single-row input.
    pub end_row: usize,
    /// Exclusive right edge of input on `end_row` (one past the last input
    /// cell, counting wide-glyph continuation cells). Under
    /// [`InputCertainty::Exact`] this is authoritative; under
    /// [`InputCertainty::RightEdgeUnknown`] it is today's last-non-blank
    /// heuristic, which may include right-aligned decorations or
    /// autosuggestions (B-DESIGN §2.4) — callers must not synthesize
    /// destructive edits from it beyond the pre-existing single-row behavior.
    pub end_col: usize,
    /// One entry per inter-row boundary in `[start_row, end_row)`; length is
    /// `end_row - start_row`. Empty for single-row input.
    pub joins: Vec<RowJoin>,
    /// Synthesis gate. Only [`InputCertainty::Exact`] is eligible for edit
    /// synthesis under the approved fallback ladder; everything else no-ops.
    pub certainty: InputCertainty,
}

/// Classification of one physical-row boundary inside the input region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowJoin {
    /// The break is a terminal soft-wrap (`Line::wrapped == true`): the edit
    /// buffer has NO newline here; traversal is purely horizontal.
    SoftWrap,
    /// The break is a real newline in the edit buffer (continuation prompt /
    /// PS2 / `begin…end`): traversal needs a vertical line motion.
    HardNewline,
}

/// How much of the region geometry is known-good.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputCertainty {
    /// Full geometry known: start, real right edge, and every join
    /// classified. Eligible for synthesis.
    Exact,
    /// Rows and joins known but the right edge is a heuristic (last non-blank
    /// cell) that may include decorations/autosuggestions (ODP-3).
    RightEdgeUnknown,
    /// Geometry itself is in doubt (cursor off-region, unclassifiable join,
    /// stale mark). Always a no-op.
    Unknown,
}

/// The latest OdyTTY-private edit-region report from a cooperating shell
/// (`OSC 133;P;odytty-edit;len=N;cur=M[;nl=…]`, B-DESIGN §3.1). Counts are in
/// **runes** (shell character counts, e.g. zsh `$#BUFFER`); core reconciles
/// runes to display cells against its own grid (NF-D1). Stored on `Screen`
/// with the same lifecycle as `active_prompt_input_start` and consumed by
/// [`derive_input_region`] to make the right edge `Exact`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRegionSignal {
    /// Edit-buffer length in runes.
    pub len: usize,
    /// Cursor offset into the buffer in runes (0-based).
    pub cur: usize,
    /// Rune offsets at which the buffer contains a real `\n` (hard newline).
    /// Empty when the buffer is single-line.
    pub newlines: Vec<usize>,
}

/// Derive the live input region from stock screen state (B-DESIGN §2.3).
///
/// * `rows` — the visible viewport rows (top to bottom).
/// * `scrollback_len` — physical scrollback rows above the viewport, i.e. the
///   absolute row of `rows[0]`.
/// * `columns` — grid width.
/// * `input_start` — the OSC 133 `B` mark as `(absolute_row, column)`.
/// * `cursor` — cursor in visible coordinates.
/// * `signal` — the private edit-region report, when a cooperating shell has
///   emitted one this repaint (`None` on the stock path).
///
/// Stock path (no signal): the region extends forward across soft-wrapped rows
/// (all joins [`RowJoin::SoftWrap`]; stock data cannot see hard newlines as
/// input) and the right edge is the last-non-blank heuristic, so certainty is
/// at most [`InputCertainty::RightEdgeUnknown`]. A cursor outside the region's
/// rows downgrades to [`InputCertainty::Unknown`].
pub(in crate::core) fn derive_input_region(
    rows: &[Line],
    scrollback_len: usize,
    columns: usize,
    input_start: Option<(usize, usize)>,
    cursor: Position,
    signal: Option<&EditRegionSignal>,
) -> Option<InputRegion> {
    let (input_row, input_col) = input_start?;
    if columns == 0 || input_col >= columns {
        return None;
    }
    // The region model only covers input that is fully on the live screen; a
    // mark that has scrolled into scrollback is not editable geometry.
    let start_visible = input_row.checked_sub(scrollback_len)?;
    if start_visible >= rows.len() {
        return None;
    }

    // Forward walk: input continues across soft-wrapped rows (§2.3 step 3,
    // stock path). Stock data classifies every join as SoftWrap; it cannot see
    // hard newlines as input membership.
    let mut end_visible = start_visible;
    let mut joins = Vec::new();
    while rows[end_visible].wrapped && end_visible + 1 < rows.len() {
        joins.push(RowJoin::SoftWrap);
        end_visible += 1;
    }

    // `signal` is consumed by the B2 slice (exact right edge); the stock path
    // below is the only rung in B0.
    let _ = signal;

    // Heuristic right edge on the last input row (§2.4): rightmost non-blank,
    // non-continuation cell, maxed with the cell just left of the cursor when
    // the cursor sits on that row. Stored EXCLUSIVE. Only feeds
    // RightEdgeUnknown; a single-row region whose heuristic end falls left of
    // the input start means "no editable content" => None (pre-existing
    // fail-safe).
    let end_row_cells = &rows[end_visible].cells;
    let last_content = end_row_cells
        .iter()
        .enumerate()
        .rev()
        .find(|(_, cell)| {
            !cell.wide_continuation && (cell.ch != ' ' || !cell.combining().is_empty())
        })
        .map(|(column, _)| column);
    let cursor_end = if cursor.row == end_visible {
        cursor.column.saturating_sub(1)
    } else {
        0
    };
    let inclusive_end = last_content.map_or(cursor_end, |column| column.max(cursor_end));
    if end_visible == start_visible && inclusive_end < input_col {
        return None;
    }
    let end_col = inclusive_end.min(columns - 1) + 1;

    // Certainty (§2.3 step 4, stock path): the right edge is heuristic =>
    // at most RightEdgeUnknown; a cursor outside the region's rows means the
    // mark is stale or the shell is doing something we cannot model => Unknown.
    let certainty = if cursor.row < start_visible || cursor.row > end_visible {
        InputCertainty::Unknown
    } else {
        InputCertainty::RightEdgeUnknown
    };

    Some(InputRegion {
        start_row: scrollback_len + start_visible,
        start_col: input_col,
        end_row: scrollback_len + end_visible,
        end_col,
        joins,
        certainty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{Attrs, Cell};

    fn line(text: &str, wrapped: bool) -> Line {
        let cells = text
            .chars()
            .map(|ch| Cell::new(ch, Attrs::default()))
            .collect();
        if wrapped {
            Line::wrapped(cells)
        } else {
            Line::unwrapped(cells)
        }
    }

    fn pad(mut l: Line, columns: usize) -> Line {
        while l.cells.len() < columns {
            l.cells.push(Cell::default());
        }
        l
    }

    const COLS: usize = 20;

    fn rows(specs: &[(&str, bool)]) -> Vec<Line> {
        specs
            .iter()
            .map(|(text, wrapped)| pad(line(text, *wrapped), COLS))
            .collect()
    }

    fn at(row: usize, column: usize) -> Position {
        Position { row, column }
    }

    #[test]
    fn no_mark_means_no_region() {
        let rows = rows(&[("$ abc", false)]);
        assert_eq!(
            derive_input_region(&rows, 0, COLS, None, at(0, 5), None),
            None
        );
    }

    #[test]
    fn single_row_region_uses_last_non_blank_heuristic() {
        let rows = rows(&[("$ abc", false)]);
        let region = derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 5), None)
            .expect("region on the marked row");
        assert_eq!(region.start_row, 0);
        assert_eq!(region.start_col, 2);
        assert_eq!(region.end_row, 0);
        // 'c' is at column 4; exclusive right edge is 5.
        assert_eq!(region.end_col, 5);
        assert!(region.joins.is_empty());
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
    }

    #[test]
    fn cursor_past_content_extends_the_heuristic_edge() {
        // Trailing typed spaces: cursor at column 8 with content ending at 4
        // must keep the cell left of the cursor inside the region (mirrors the
        // pre-existing `editable_input_end_column` cursor max).
        let rows = rows(&[("$ abc", false)]);
        let region = derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 8), None)
            .expect("region on the marked row");
        assert_eq!(region.end_col, 8);
    }

    #[test]
    fn empty_input_yields_none() {
        // Prompt only, mark at column 2, nothing typed: the heuristic edge
        // (prompt glyph at column 0) falls left of the input start.
        let rows = rows(&[("$ ", false)]);
        assert_eq!(
            derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 2), None),
            None
        );
    }

    #[test]
    fn mark_in_scrollback_yields_none() {
        let rows = rows(&[("$ abc", false)]);
        // scrollback_len 5, mark at absolute row 3 => scrolled off.
        assert_eq!(
            derive_input_region(&rows, 5, COLS, Some((3, 2)), at(0, 5), None),
            None
        );
    }

    #[test]
    fn scrollback_offsets_absolute_rows() {
        let rows = rows(&[("$ abc", false)]);
        let region = derive_input_region(&rows, 7, COLS, Some((7, 2)), at(0, 5), None)
            .expect("region on the marked row");
        assert_eq!(region.start_row, 7);
        assert_eq!(region.end_row, 7);
    }

    #[test]
    fn soft_wrapped_input_extends_across_rows_with_softwrap_joins() {
        let rows = rows(&[("$ echo aaaaaaaaaaaaa", true), ("bbb", false)]);
        let region = derive_input_region(&rows, 0, COLS, Some((0, 2)), at(1, 3), None)
            .expect("wrapped region");
        assert_eq!(region.start_row, 0);
        assert_eq!(region.end_row, 1);
        assert_eq!(region.joins, vec![RowJoin::SoftWrap]);
        // Last row content 'bbb' => exclusive edge 3.
        assert_eq!(region.end_col, 3);
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
    }

    #[test]
    fn cursor_outside_region_rows_downgrades_to_unknown() {
        let rows = rows(&[("$ abc", false), ("output", false)]);
        let region = derive_input_region(&rows, 0, COLS, Some((0, 2)), at(1, 0), None)
            .expect("region still derived");
        assert_eq!(region.certainty, InputCertainty::Unknown);
    }

    #[test]
    fn wide_continuation_cells_are_not_content() {
        // A wide glyph: lead + continuation. The continuation cell must not be
        // picked as the rightmost content cell.
        let mut rows = rows(&[("$ ", false)]);
        rows[0].cells[2] = Cell::new('漢', Attrs::default());
        rows[0].cells[3] = Cell::wide_spacer(Attrs::default());
        let region = derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 4), None)
            .expect("region with wide glyph");
        // Lead at column 2 is the last content cell; cursor_end = 3 wins the
        // max => exclusive edge 4 (covers both cells of the glyph).
        assert_eq!(region.end_col, 4);
    }

    #[test]
    fn mark_column_past_width_yields_none() {
        let rows = rows(&[("$ abc", false)]);
        assert_eq!(
            derive_input_region(&rows, 0, COLS, Some((0, COLS)), at(0, 5), None),
            None
        );
    }
}
