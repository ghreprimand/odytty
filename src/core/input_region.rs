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
    /// Authoritative per-row input spans as `(start_col, end_col_exclusive)`,
    /// index 0 = `start_row`. Populated ONLY under [`InputCertainty::Exact`]
    /// (from the reconciled rune walk, with wrap-filler cells excluded —
    /// B-DESIGN B1); empty otherwise. Consumers flatten these spans into the
    /// single logical horizontal axis for R5 synthesis.
    pub row_spans: Vec<(usize, usize)>,
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

    // TIER-A path (B2, §2.3 step 3): when the shell reported its authoritative
    // buffer geometry this repaint, reconcile it against the grid. Any
    // inconsistency — the signal predating further typing, a rune walk that
    // does not land exactly, a cursor that disagrees — falls back to the stock
    // heuristic below (RightEdgeUnknown => no-op), never to a guessed edit.
    if let Some(signal) = signal {
        if !signal.newlines.is_empty() {
            // Hard newlines in the buffer (ODP-2 default): the geometry is
            // real but horizontal motion cannot traverse it => Unknown, no-op.
            return Some(InputRegion {
                start_row: scrollback_len + start_visible,
                start_col: input_col,
                end_row: scrollback_len + end_visible,
                end_col: input_col,
                joins: vec![RowJoin::HardNewline; end_visible - start_visible],
                certainty: InputCertainty::Unknown,
                row_spans: Vec::new(),
            });
        }
        // Rune walk across the region's rows (single-row = B2; soft-wrapped
        // multi-row = B1). Any inconsistency falls through to the heuristic.
        if signal.len > 0
            && let Some(geometry) = reconcile_signal(
                rows,
                start_visible,
                end_visible,
                input_col,
                columns,
                cursor,
                signal,
            )
        {
            return Some(InputRegion {
                start_row: scrollback_len + start_visible,
                start_col: input_col,
                end_row: scrollback_len + end_visible,
                end_col: geometry.end_col,
                joins,
                certainty: InputCertainty::Exact,
                row_spans: geometry.row_spans,
            });
        }
    }

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
        row_spans: Vec::new(),
    })
}

/// Result of a successful signal↔grid reconciliation.
struct ReconciledGeometry {
    /// Per-region-row `(start_col, end_col_exclusive)` input spans, index 0 =
    /// the region's first row. Wrap-filler cells are excluded.
    row_spans: Vec<(usize, usize)>,
    /// Exclusive right edge on the region's LAST row.
    end_col: usize,
}

/// Cap on ambiguous wrap-filler boundaries we are willing to enumerate
/// (2^N assignments). Real inputs rarely have even one; past this the
/// geometry is declared unresolvable and the caller degrades to a no-op.
const MAX_AMBIGUOUS_WRAP_FILLERS: usize = 3;

/// Reconcile an edit-region signal against the grid across the region's rows
/// (NF-D1, generalized to soft-wrapped multi-row input for B1).
///
/// The shell counts **runes** (characters); the grid stores **cells** (wide
/// glyphs occupy two, combining marks fold into their base cell, and a wide
/// glyph that does not fit at a row's right edge leaves a display-only blank
/// *wrap-filler* cell that corresponds to NO buffer character). Walk the
/// buffer's runes across the region's rows and accept the signal as `Exact`
/// only when the length lands exactly on the LAST row, the cursor offset lands
/// exactly on a glyph boundary, and the signal-derived cursor cell equals the
/// live grid cursor. The cursor check is the staleness detector: a signal
/// emitted before further typing disagrees with the grid cursor and falls back
/// to the heuristic path.
///
/// Wrap-filler ambiguity: a blank last cell on a wrapped row followed by a
/// wide lead on the next row is indistinguishable from a *typed* space that
/// happened to land on the last column before a wide glyph. Both readings are
/// enumerated; the signal is accepted only when EXACTLY ONE assignment
/// validates end-to-end — two coherent readings would mean two different edit
/// geometries, and a wrong delete is worse than a no-op.
fn reconcile_signal(
    rows: &[Line],
    start_visible: usize,
    end_visible: usize,
    input_col: usize,
    columns: usize,
    cursor: Position,
    signal: &EditRegionSignal,
) -> Option<ReconciledGeometry> {
    let boundaries = end_visible - start_visible;
    let ambiguous: Vec<usize> = (0..boundaries)
        .filter(|&rel| {
            wrap_filler_candidate(&rows[start_visible + rel], &rows[start_visible + rel + 1])
        })
        .collect();
    if ambiguous.len() > MAX_AMBIGUOUS_WRAP_FILLERS {
        return None;
    }
    let mut accepted: Option<ReconciledGeometry> = None;
    for mask in 0u32..(1 << ambiguous.len()) {
        let mut skip_filler = vec![false; boundaries];
        for (bit, &rel) in ambiguous.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                skip_filler[rel] = true;
            }
        }
        if let Some(geometry) = walk_assignment(
            rows,
            start_visible,
            end_visible,
            input_col,
            columns,
            cursor,
            signal,
            &skip_filler,
        ) {
            if accepted.is_some() {
                // Two coherent readings: geometry is ambiguous => degrade.
                return None;
            }
            accepted = Some(geometry);
        }
    }
    accepted
}

/// Whether the boundary between `upper` (wrapped) and `lower` could carry a
/// wrap-filler cell: `upper`'s last cell is a plain blank and `lower` starts
/// with a wide lead (the only way `print_char` produces a filler).
fn wrap_filler_candidate(upper: &Line, lower: &Line) -> bool {
    let Some(last) = upper.cells.last() else {
        return false;
    };
    let blank_last = !last.wide_continuation && last.ch == ' ' && last.combining().is_empty();
    let wide_lead_first = lower
        .cells
        .first()
        .is_some_and(|cell| !cell.wide_continuation)
        && lower
            .cells
            .get(1)
            .is_some_and(|cell| cell.wide_continuation);
    blank_last && wide_lead_first
}

/// One rune walk under a fixed wrap-filler assignment. Returns the reconciled
/// geometry only when the walk is fully coherent (see [`reconcile_signal`]).
#[expect(
    clippy::too_many_arguments,
    reason = "pure derivation seam shared with reconcile_signal"
)]
fn walk_assignment(
    rows: &[Line],
    start_visible: usize,
    end_visible: usize,
    input_col: usize,
    columns: usize,
    cursor: Position,
    signal: &EditRegionSignal,
    skip_filler: &[bool],
) -> Option<ReconciledGeometry> {
    let last_rel = end_visible - start_visible;
    let mut consumed = 0usize;
    let mut cursor_landing: Option<(usize, usize)> = None;
    let mut row_spans = Vec::with_capacity(last_rel + 1);
    let mut end_col: Option<usize> = None;
    'rows: for rel in 0..=last_rel {
        let cells = &rows[start_visible + rel].cells;
        let row_start = if rel == 0 { input_col } else { 0 };
        // Non-final rows must be consumed to their input end: a wrapped row is
        // full by construction, minus at most one filler cell before a wide
        // lead on the next row.
        let row_limit = if rel < last_rel && skip_filler[rel] {
            columns - 1
        } else {
            columns
        };
        if consumed == signal.cur && cursor_landing.is_none() {
            // Only reachable pre-consumption on the first row (cur == 0).
            cursor_landing = Some((rel, row_start));
        }
        let mut col = row_start;
        while col < row_limit && col < cells.len() {
            if cells[col].wide_continuation {
                col += 1;
                continue;
            }
            consumed += 1 + cells[col].combining().len();
            col += 1;
            // Absorb this glyph's trailing continuation cells so no boundary
            // lands between a wide lead and its spacer.
            while col < cells.len() && cells[col].wide_continuation {
                col += 1;
            }
            if cursor_landing.is_none() {
                match consumed.cmp(&signal.cur) {
                    std::cmp::Ordering::Equal => {
                        // At the end of a non-final row's input the shell
                        // displays the cursor at the start of the next row.
                        cursor_landing = Some(if rel < last_rel && col >= row_limit {
                            (rel + 1, 0)
                        } else {
                            (rel, col)
                        });
                    }
                    // cur points inside a combining cluster: no cell boundary.
                    std::cmp::Ordering::Greater => return None,
                    std::cmp::Ordering::Less => {}
                }
            }
            match consumed.cmp(&signal.len) {
                std::cmp::Ordering::Equal => {
                    if rel != last_rel {
                        // The buffer ends but the grid says the logical line
                        // continues onto further wrapped rows: incoherent.
                        return None;
                    }
                    end_col = Some(col);
                    row_spans.push((row_start, col));
                    break 'rows;
                }
                std::cmp::Ordering::Greater => return None,
                std::cmp::Ordering::Less => {}
            }
        }
        if rel == last_rel {
            // Ran out of cells on the last row before consuming `len`.
            return None;
        }
        row_spans.push((row_start, row_limit));
    }
    let end_col = end_col?;
    let (landing_rel, landing_col) = cursor_landing?;
    (cursor.row == start_visible + landing_rel && cursor.column == landing_col)
        .then_some(ReconciledGeometry { row_spans, end_col })
}

/// Parse the payload of the OdyTTY-private edit-region OSC
/// (`133;P;odytty-edit;len=<N>;cur=<M>[;nl=<c0,c1,…>]`, B-DESIGN §3.1).
/// `parts` are the `;`-split parts after `133` (so `parts[0] == b"P"`).
/// Malformed payloads, unknown signal names (versioning: a future
/// `odytty-edit2` is ignored by this parser), and `cur > len` all return
/// `None` — the caller leaves existing state untouched and never panics.
pub(in crate::core) fn parse_edit_region_osc(parts: &[&[u8]]) -> Option<EditRegionSignal> {
    if parts.first().copied() != Some(b"P".as_slice())
        || parts.get(1).copied() != Some(b"odytty-edit".as_slice())
    {
        return None;
    }
    let len = parse_field(parts.get(2)?, b"len=")?;
    let cur = parse_field(parts.get(3)?, b"cur=")?;
    if cur > len {
        return None;
    }
    let newlines = match parts.get(4) {
        None => Vec::new(),
        Some(part) => {
            let list = part.strip_prefix(b"nl=")?;
            let mut offsets = Vec::new();
            for entry in list.split(|&b| b == b',') {
                let offset = parse_usize_ascii(entry)?;
                if offset > len {
                    return None;
                }
                offsets.push(offset);
            }
            offsets
        }
    };
    Some(EditRegionSignal { len, cur, newlines })
}

fn parse_field(part: &[u8], key: &[u8]) -> Option<usize> {
    parse_usize_ascii(part.strip_prefix(key)?)
}

fn parse_usize_ascii(bytes: &[u8]) -> Option<usize> {
    // Bounded: the OSC accumulator already caps payload size, and a genuine
    // buffer length never approaches usize limits; reject overflow instead of
    // wrapping.
    if bytes.is_empty() || bytes.len() > 12 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
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

    // ---- B2: TIER-A signal reconciliation ----

    fn signal(len: usize, cur: usize) -> EditRegionSignal {
        EditRegionSignal {
            len,
            cur,
            newlines: Vec::new(),
        }
    }

    #[test]
    fn signal_makes_right_edge_exact_and_excludes_decorations() {
        // Input "abc" at col 2; a right-aligned decoration ("23.1s"-style)
        // rendered further right on the same row. The stock heuristic would
        // claim the decoration as input; the signal's len bounds it exactly.
        let rows = rows(&[("$ abc          23.1s", false)]);
        let region =
            derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 5), Some(&signal(3, 3)))
                .expect("exact region");
        assert_eq!(region.certainty, InputCertainty::Exact);
        assert_eq!(region.start_col, 2);
        assert_eq!(region.end_col, 5, "decoration cells must be excluded");
    }

    #[test]
    fn stale_signal_falls_back_to_heuristic() {
        // Cursor at col 7 (user typed more since the report): the
        // signal-derived cursor (col 5) disagrees => never Exact.
        let rows = rows(&[("$ abcde", false)]);
        let region =
            derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 7), Some(&signal(3, 3)))
                .expect("fallback region");
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
        // Heuristic edge: last content 'e' at col 6 => exclusive 7.
        assert_eq!(region.end_col, 7);
    }

    #[test]
    fn signal_len_overrunning_the_row_falls_back() {
        let rows = rows(&[("$ abc", false)]);
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(0, 5),
            Some(&signal(100, 3)),
        )
        .expect("fallback region");
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
    }

    #[test]
    fn signal_with_hard_newlines_is_unknown() {
        let rows = rows(&[("$ for x in 1 2", false)]);
        let sig = EditRegionSignal {
            len: 12,
            cur: 12,
            newlines: vec![9],
        };
        let region = derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 14), Some(&sig))
            .expect("region derived");
        assert_eq!(region.certainty, InputCertainty::Unknown, "ODP-2: no-op");
    }

    #[test]
    fn wide_glyphs_reconcile_runes_to_cells() {
        // Buffer "漢字" = 2 runes but 4 cells. len=2, cur=2 => cursor at col 6,
        // exact edge at col 6 (covering both wide pairs).
        let mut rows = rows(&[("$ ", false)]);
        rows[0].cells[2] = Cell::new('漢', Attrs::default());
        rows[0].cells[3] = Cell::wide_spacer(Attrs::default());
        rows[0].cells[4] = Cell::new('字', Attrs::default());
        rows[0].cells[5] = Cell::wide_spacer(Attrs::default());
        let region =
            derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 6), Some(&signal(2, 2)))
                .expect("exact region");
        assert_eq!(region.certainty, InputCertainty::Exact);
        assert_eq!(region.end_col, 6);
    }

    #[test]
    fn combining_marks_count_as_runes() {
        // "e" + combining acute = 2 runes in the shell, 1 cell in the grid.
        let mut rows = rows(&[("$ e", false)]);
        assert!(rows[0].cells[2].push_combining('\u{0301}'));
        let region =
            derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 3), Some(&signal(2, 2)))
                .expect("exact region");
        assert_eq!(region.certainty, InputCertainty::Exact);
        assert_eq!(region.end_col, 3);
    }

    #[test]
    fn rune_offset_inside_combining_cluster_falls_back() {
        // cur=1 points between the base and its combining mark: no cell
        // boundary corresponds => never Exact.
        let mut rows = rows(&[("$ e", false)]);
        assert!(rows[0].cells[2].push_combining('\u{0301}'));
        let region =
            derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 3), Some(&signal(2, 1)))
                .expect("fallback region");
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
    }

    #[test]
    fn empty_buffer_signal_falls_back_to_heuristic_path() {
        // len=0 (fish emits at the prompt before typing): no Exact claim; the
        // stock path sees no content right of the mark => None, the
        // pre-existing "nothing to delete" outcome.
        let rows = rows(&[("$ ", false)]);
        assert_eq!(
            derive_input_region(&rows, 0, COLS, Some((0, 2)), at(0, 2), Some(&signal(0, 0))),
            None
        );
    }

    // ---- B1: soft-wrapped multi-row reconciliation (T8/T9) ----

    /// T8 core seam: a wrapped logical line with a coherent signal becomes an
    /// Exact multi-row region with authoritative per-row spans.
    #[test]
    fn soft_wrapped_signal_yields_exact_region_with_spans() {
        // Row 0: "$ " + 18 input chars (cols 2..19), wrapped; row 1: 5 chars.
        let rows = rows(&[("$ aaaaaaaaaaaaaaaaaa", true), ("bbbbb", false)]);
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(1, 5),
            Some(&signal(23, 23)),
        )
        .expect("exact wrapped region");
        assert_eq!(region.certainty, InputCertainty::Exact);
        assert_eq!(region.start_row, 0);
        assert_eq!(region.end_row, 1);
        assert_eq!(region.end_col, 5);
        assert_eq!(region.joins, vec![RowJoin::SoftWrap]);
        assert_eq!(region.row_spans, vec![(2, COLS), (0, 5)]);
    }

    #[test]
    fn three_row_wrap_reconciles_and_spans_all_rows() {
        let rows = rows(&[
            ("$ aaaaaaaaaaaaaaaaaa", true), // 18 runes
            ("bbbbbbbbbbbbbbbbbbbb", true), // 20 runes
            ("ccc", false),                 // 3 runes
        ]);
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(2, 3),
            Some(&signal(41, 41)),
        )
        .expect("exact 3-row region");
        assert_eq!(region.certainty, InputCertainty::Exact);
        assert_eq!(region.row_spans, vec![(2, COLS), (0, COLS), (0, 3)]);
        assert_eq!(region.end_col, 3);
    }

    /// Cursor mid-wrap (not at the end of the buffer) still reconciles: the
    /// staleness detector accepts the true mid-buffer cursor cell.
    #[test]
    fn cursor_mid_wrap_reconciles() {
        let rows = rows(&[("$ aaaaaaaaaaaaaaaaaa", true), ("bbbbb", false)]);
        // cur=20: 18 runes on row 0, 2 on row 1 => cursor cell (1, 2).
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(1, 2),
            Some(&signal(23, 20)),
        )
        .expect("exact wrapped region");
        assert_eq!(region.certainty, InputCertainty::Exact);
    }

    /// A cursor offset landing exactly on the wrap boundary maps to the start
    /// of the next row (where the shell displays it), not one past row end.
    #[test]
    fn cursor_on_wrap_boundary_normalizes_to_next_row_start() {
        let rows = rows(&[("$ aaaaaaaaaaaaaaaaaa", true), ("bbbbb", false)]);
        // cur=18 consumes exactly row 0 => cursor cell (1, 0).
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(1, 0),
            Some(&signal(23, 18)),
        )
        .expect("exact wrapped region");
        assert_eq!(region.certainty, InputCertainty::Exact);
    }

    /// T9: a wide glyph that did not fit at the right edge leaves a wrap-filler
    /// blank; the walk must skip it (it is no buffer character) and still land
    /// exactly.
    #[test]
    fn wrap_filler_before_wide_glyph_is_excluded() {
        // Row 0: "$ " + 17 chars (cols 2..18) + filler blank at col 19,
        // wrapped; row 1: wide glyph + "xy".
        let mut rows = rows(&[("$ aaaaaaaaaaaaaaaaa", true), ("", false)]);
        rows[1].cells[0] = Cell::new('漢', Attrs::default());
        rows[1].cells[1] = Cell::wide_spacer(Attrs::default());
        rows[1].cells[2] = Cell::new('x', Attrs::default());
        rows[1].cells[3] = Cell::new('y', Attrs::default());
        // Buffer runes: 17 + 1 + 2 = 20; cursor at (1, 4).
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(1, 4),
            Some(&signal(20, 20)),
        )
        .expect("exact region with filler skipped");
        assert_eq!(region.certainty, InputCertainty::Exact);
        // Filler cell (col 19) excluded from row 0's span.
        assert_eq!(region.row_spans, vec![(2, COLS - 1), (0, 4)]);
        assert_eq!(region.end_col, 4);
    }

    /// The ambiguous twin of the filler case: a TYPED space on the last column
    /// before a wide glyph is a real buffer character. Only the reading that
    /// counts it satisfies len+cursor, so it reconciles exactly.
    #[test]
    fn typed_space_before_wide_glyph_counts_as_a_rune() {
        // Row 0: "$ " + 17 chars + typed ' ' at col 19, wrapped; row 1: wide.
        let mut rows = rows(&[("$ aaaaaaaaaaaaaaaaa", true), ("", false)]);
        rows[1].cells[0] = Cell::new('漢', Attrs::default());
        rows[1].cells[1] = Cell::wide_spacer(Attrs::default());
        // Buffer runes: 17 + 1 (space) + 1 (wide) = 19; cursor at (1, 2).
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(1, 2),
            Some(&signal(19, 19)),
        )
        .expect("exact region counting the typed space");
        assert_eq!(region.certainty, InputCertainty::Exact);
        assert_eq!(region.row_spans, vec![(2, COLS), (0, 2)]);
    }

    /// Two ambiguous filler boundaries admitting two coherent readings: the
    /// geometry cannot be trusted, so the signal must be REJECTED (charter: a
    /// wrong delete is worse than a no-op).
    #[test]
    fn two_coherent_filler_readings_fall_back() {
        // Row 0: 17 chars + blank at col 19, wrapped; row 1: wide + 17 chars
        // + blank at col 19, wrapped; row 2: wide + "cc".
        // skip-first-only and skip-second-only both total 38 runes.
        let mut rows = rows(&[("$ aaaaaaaaaaaaaaaaa", true), ("", true), ("", false)]);
        rows[1].cells[0] = Cell::new('漢', Attrs::default());
        rows[1].cells[1] = Cell::wide_spacer(Attrs::default());
        for col in 2..19 {
            rows[1].cells[col] = Cell::new('b', Attrs::default());
        }
        rows[2].cells[0] = Cell::new('字', Attrs::default());
        rows[2].cells[1] = Cell::wide_spacer(Attrs::default());
        rows[2].cells[2] = Cell::new('c', Attrs::default());
        rows[2].cells[3] = Cell::new('c', Attrs::default());
        // skip-first: 17 + (1+17+1) + (1+2) = 39? Recomputed in-test below via
        // the accepted len: skip-first-only = 17 + 19 + 3 = 39,
        // skip-second-only = 18 + 18 + 3 = 39 — both coherent at cur=len with
        // the cursor at (2, 4).
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(2, 4),
            Some(&signal(39, 39)),
        )
        .expect("region still derived");
        assert_eq!(
            region.certainty,
            InputCertainty::RightEdgeUnknown,
            "two coherent readings must degrade, never pick one"
        );
    }

    /// Companion to [`two_coherent_filler_readings_fall_back`]: the same grid
    /// with a len that only the skip-BOTH reading satisfies reconciles Exact —
    /// proving the enumeration finds filler assignments and the two-reading
    /// rejection above is a genuine ambiguity, not a walk failure.
    #[test]
    fn unique_filler_reading_reconciles_exactly() {
        let mut rows = rows(&[("$ aaaaaaaaaaaaaaaaa", true), ("", true), ("", false)]);
        rows[1].cells[0] = Cell::new('漢', Attrs::default());
        rows[1].cells[1] = Cell::wide_spacer(Attrs::default());
        for col in 2..19 {
            rows[1].cells[col] = Cell::new('b', Attrs::default());
        }
        rows[2].cells[0] = Cell::new('字', Attrs::default());
        rows[2].cells[1] = Cell::wide_spacer(Attrs::default());
        rows[2].cells[2] = Cell::new('c', Attrs::default());
        rows[2].cells[3] = Cell::new('c', Attrs::default());
        // len=38 is satisfied ONLY by skipping both fillers: 17 + 18 + 3.
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(2, 4),
            Some(&signal(38, 38)),
        )
        .expect("exact region");
        assert_eq!(region.certainty, InputCertainty::Exact);
        assert_eq!(region.row_spans, vec![(2, COLS - 1), (0, COLS - 1), (0, 4)]);
    }

    /// A signal whose buffer ends before the grid's wrapped continuation is
    /// incoherent (stale): fall back.
    #[test]
    fn signal_ending_before_last_wrapped_row_falls_back() {
        let rows = rows(&[("$ aaaaaaaaaaaaaaaaaa", true), ("bbbbb", false)]);
        // len=10 ends mid-row-0 while the grid says the line wraps on.
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(1, 5),
            Some(&signal(10, 10)),
        )
        .expect("region still derived");
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
    }

    /// Stale multi-row signal (cursor disagrees) falls back to the heuristic.
    #[test]
    fn stale_multi_row_signal_falls_back() {
        let rows = rows(&[("$ aaaaaaaaaaaaaaaaaa", true), ("bbbbb", false)]);
        let region = derive_input_region(
            &rows,
            0,
            COLS,
            Some((0, 2)),
            at(1, 5),
            Some(&signal(23, 20)), // cur maps to (1,2), grid cursor at (1,5)
        )
        .expect("region still derived");
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
    }

    /// Heuristic (non-Exact) regions must not carry spans: only the reconciled
    /// walk may feed R5 synthesis.
    #[test]
    fn heuristic_regions_have_no_row_spans() {
        let rows = rows(&[("$ aaaaaaaaaaaaaaaaaa", true), ("bbbbb", false)]);
        let region = derive_input_region(&rows, 0, COLS, Some((0, 2)), at(1, 5), None)
            .expect("heuristic region");
        assert_eq!(region.certainty, InputCertainty::RightEdgeUnknown);
        assert!(region.row_spans.is_empty());
    }

    // ---- B2: private OSC parse (T18) ----

    fn parts(raw: &[&str]) -> Vec<Vec<u8>> {
        raw.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    fn parse(raw: &[&str]) -> Option<EditRegionSignal> {
        let owned = parts(raw);
        let borrowed: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
        parse_edit_region_osc(&borrowed)
    }

    #[test]
    fn parses_well_formed_edit_region_reports() {
        assert_eq!(
            parse(&["P", "odytty-edit", "len=3", "cur=1"]),
            Some(signal(3, 1))
        );
        assert_eq!(
            parse(&["P", "odytty-edit", "len=12", "cur=5", "nl=3,9"]),
            Some(EditRegionSignal {
                len: 12,
                cur: 5,
                newlines: vec![3, 9],
            })
        );
    }

    #[test]
    fn rejects_malformed_and_unknown_reports_without_panicking() {
        // Unknown / versioned signal name.
        assert_eq!(parse(&["P", "odytty-edit2", "len=3", "cur=1"]), None);
        // Missing / malformed fields.
        assert_eq!(parse(&["P", "odytty-edit"]), None);
        assert_eq!(parse(&["P", "odytty-edit", "len=", "cur=1"]), None);
        assert_eq!(parse(&["P", "odytty-edit", "len=x", "cur=1"]), None);
        assert_eq!(parse(&["P", "odytty-edit", "cur=1", "len=3"]), None);
        // cur beyond len is incoherent.
        assert_eq!(parse(&["P", "odytty-edit", "len=3", "cur=4"]), None);
        // nl offset beyond len is incoherent.
        assert_eq!(parse(&["P", "odytty-edit", "len=3", "cur=1", "nl=9"]), None);
        // Absurd magnitude rejected, no overflow.
        assert_eq!(
            parse(&["P", "odytty-edit", "len=99999999999999999999", "cur=1"]),
            None
        );
    }
}
