//! Scrollback search: a pure, rendering-free engine that finds literal queries
//! across the combined scrollback + visible buffer and reports matches as
//! absolute cell ranges a front end can highlight and jump to.
//!
//! # Coordinate convention
//!
//! Matches use the same absolute-row convention as selection
//! (`crate::selection`): row `0` is the oldest scrollback line and rows count
//! downward through scrollback into the live screen. A [`SearchMatch`] carries
//! an inclusive `start`/`end` [`AbsolutePoint`] — `end.column` is the last cell
//! the match covers, so a wide (two-column) glyph at column `c` reports
//! `end.column == c + 1`.
//!
//! # What a match is
//!
//! The engine joins soft-wrapped physical rows (those whose [`SearchRow::wrapped`]
//! marker is set) into logical lines, then searches each logical line's text.
//! Because each cell keeps its own physical row, a match that crosses a soft-wrap
//! boundary naturally reports a `start` and `end` on different absolute rows.
//! Hard line breaks (a non-wrapped row) end a logical line, so a query never
//! matches across a hard newline.
//!
//! The searchable text of a logical line is the concatenation of each
//! non-continuation cell's grapheme (base char + any combining marks), with
//! trailing blank cells trimmed (matching the row-padding model used elsewhere
//! in core). Wide-glyph continuation spacers contribute no text of their own;
//! the wide lead's column span covers both cells.
//!
//! # Documented limitations (bounded, deterministic)
//!
//! - **Case folding** is per-`char` simple lowercase (the first scalar of
//!   [`char::to_lowercase`]). This covers ASCII and common scripts; full Unicode
//!   special-casing (e.g. `ß`→`ss`, locale-sensitive `İ`) is out of scope and the
//!   fold stays 1:1 so column mapping is exact.
//! - **No Unicode normalization**: a precomposed `é` (U+00E9) and a decomposed
//!   `e`+combining-acute are distinct and do not cross-match.
//! - **Matches are non-overlapping**, found greedily left-to-right within each
//!   logical line (e.g. `aa` in `aaa` yields one match at offset 0).
//! - **Wide pairs never straddle a wrap boundary** (guaranteed by the reflow and
//!   print paths), so a wide lead and its continuation always share one physical
//!   row.

use super::types::Cell;

/// An absolute cell position: `row` is the index into the combined
/// scrollback + screen buffer (row `0` = oldest scrollback), `column` is the
/// cell column within that physical row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsolutePoint {
    pub row: usize,
    pub column: usize,
}

/// A located match, with inclusive `start` and `end` cell positions. `end` is
/// the last cell the match covers (the trailing column of a wide glyph when the
/// match ends on one), and may sit on a later absolute row than `start` when the
/// match spans a soft-wrap boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: AbsolutePoint,
    pub end: AbsolutePoint,
}

/// Search tuning. Literal substring search; the only knob today is case
/// sensitivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    /// When `false` (the default), the query and buffer are compared with
    /// per-`char` simple lowercase folding.
    pub case_sensitive: bool,
}

impl SearchOptions {
    pub fn case_sensitive() -> Self {
        Self {
            case_sensitive: true,
        }
    }

    pub fn case_insensitive() -> Self {
        Self {
            case_sensitive: false,
        }
    }
}

/// One physical row of the combined buffer to search, borrowed in place. `cells`
/// is the row's cells in column order; `wrapped` mirrors `Line::wrapped` — true
/// when the row soft-wraps into the next, joining them into one logical line.
pub struct SearchRow<'a> {
    pub cells: &'a [Cell],
    pub wrapped: bool,
}

/// One searchable cell of a logical line: a grapheme plus the absolute span of
/// columns it occupies on its physical row (`start_col..=end_col`, which differ
/// only for wide glyphs).
struct Unit {
    row: usize,
    start_col: usize,
    end_col: usize,
}

/// Per-`char` simple lowercase fold (first scalar of `to_lowercase`). Kept 1:1
/// so a match's char offsets map back to exact cells.
fn fold_char(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

/// Search the combined buffer `rows` for `query`, returning every
/// non-overlapping match in reading order (top-to-bottom, left-to-right). The
/// result is sorted ascending by `start`, which [`find_next`]/[`find_prev`] rely
/// on. An empty query matches nothing.
pub fn search_rows(
    rows: &[SearchRow<'_>],
    query: &str,
    options: SearchOptions,
) -> Vec<SearchMatch> {
    let mut matches = Vec::new();
    if query.is_empty() {
        return matches;
    }
    let query_chars: Vec<char> = if options.case_sensitive {
        query.chars().collect()
    } else {
        query.chars().map(fold_char).collect()
    };

    let mut units: Vec<Unit> = Vec::new();
    let mut unit_text: Vec<String> = Vec::new();

    for (abs_row, row) in rows.iter().enumerate() {
        let cells = row.cells;
        let mut col = 0;
        while col < cells.len() {
            let cell = &cells[col];
            if cell.wide_continuation {
                // Consumed by its lead; contributes no independent unit.
                col += 1;
                continue;
            }
            let wide = col + 1 < cells.len() && cells[col + 1].wide_continuation;
            let end_col = if wide { col + 1 } else { col };
            units.push(Unit {
                row: abs_row,
                start_col: col,
                end_col,
            });
            unit_text.push(cell.grapheme());
            col += if wide { 2 } else { 1 };
        }

        if !row.wrapped {
            flush_line(
                &units,
                &unit_text,
                &query_chars,
                options.case_sensitive,
                &mut matches,
            );
            units.clear();
            unit_text.clear();
        }
    }
    // A trailing logical line whose last row was still marked wrapped.
    if !units.is_empty() {
        flush_line(
            &units,
            &unit_text,
            &query_chars,
            options.case_sensitive,
            &mut matches,
        );
    }

    matches
}

/// Match `query_chars` within one assembled logical line and push results.
fn flush_line(
    units: &[Unit],
    unit_text: &[String],
    query_chars: &[char],
    case_sensitive: bool,
    out: &mut Vec<SearchMatch>,
) {
    // Trim trailing blank cells (row padding); interior blanks are preserved.
    let mut keep = units.len();
    while keep > 0 && unit_text[keep - 1] == " " {
        keep -= 1;
    }
    if keep == 0 {
        return;
    }

    // Flatten kept units into a char sequence, remembering each char's owning
    // unit so a match's char offsets map back to exact cells.
    let mut folded: Vec<char> = Vec::new();
    let mut owners: Vec<usize> = Vec::new();
    for (unit_index, text) in unit_text[..keep].iter().enumerate() {
        for ch in text.chars() {
            folded.push(if case_sensitive { ch } else { fold_char(ch) });
            owners.push(unit_index);
        }
    }

    let qlen = query_chars.len();
    if folded.len() < qlen {
        return;
    }

    let mut i = 0;
    while i + qlen <= folded.len() {
        if folded[i..i + qlen] == *query_chars {
            let start_unit = &units[owners[i]];
            let end_unit = &units[owners[i + qlen - 1]];
            out.push(SearchMatch {
                start: AbsolutePoint {
                    row: start_unit.row,
                    column: start_unit.start_col,
                },
                end: AbsolutePoint {
                    row: end_unit.row,
                    column: end_unit.end_col,
                },
            });
            i += qlen; // non-overlapping
        } else {
            i += 1;
        }
    }
}

/// The first match starting strictly after `from`, wrapping to the first match
/// when none follow. `matches` must be sorted ascending by `start` (as returned
/// by [`search_rows`]). `None` only when there are no matches.
pub fn find_next(matches: &[SearchMatch], from: AbsolutePoint) -> Option<SearchMatch> {
    if matches.is_empty() {
        return None;
    }
    matches
        .iter()
        .find(|m| (m.start.row, m.start.column) > (from.row, from.column))
        .copied()
        .or_else(|| matches.first().copied())
}

/// The last match starting strictly before `from`, wrapping to the last match
/// when none precede. `matches` must be sorted ascending by `start` (as returned
/// by [`search_rows`]). `None` only when there are no matches.
pub fn find_prev(matches: &[SearchMatch], from: AbsolutePoint) -> Option<SearchMatch> {
    if matches.is_empty() {
        return None;
    }
    matches
        .iter()
        .rev()
        .find(|m| (m.start.row, m.start.column) < (from.row, from.column))
        .copied()
        .or_else(|| matches.last().copied())
}
