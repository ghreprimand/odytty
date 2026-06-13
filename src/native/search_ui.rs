use crate::core::{
    AbsolutePoint, Attrs, Cell, Color, Dimensions, SearchMatch, SearchOptions, Snapshot, Terminal,
    find_next, find_prev,
};
use crate::selection::{self, AbsoluteCellPoint, AbsoluteSelectionRange, SelectionRange};

use unicode_width::UnicodeWidthChar;

/// Themed search-highlight treatment (ID1). When supplied to
/// [`apply_search_ui`], non-active matches are painted with `fill`/`fg` and the
/// active match with `active_fill`/`active_fg` (all sRGB bytes) instead of the
/// historical inverse / hardcoded black-on-yellow. Foregrounds are precomputed
/// by the caller, RV1-floored over their respective fills, so readability holds
/// at the active `min_contrast`. Passing `None` preserves the byte-identical
/// default path, keeping the plain render pixel-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SearchStyle {
    /// Non-active match fill background (sRGB), from the theme `search` role.
    pub(super) fill: [u8; 3],
    /// Non-active match foreground over `fill` (sRGB), RV1-floored.
    pub(super) fg: [u8; 3],
    /// Active match fill background (sRGB), a brightened `search` derivative.
    pub(super) active_fill: [u8; 3],
    /// Active match foreground over `active_fill` (sRGB), RV1-floored.
    pub(super) active_fg: [u8; 3],
}

#[derive(Debug, Clone)]
pub(super) struct SearchUi {
    open: bool,
    query: String,
    matches: Vec<SearchMatch>,
    current: Option<SearchMatch>,
    options: SearchOptions,
}

impl Default for SearchUi {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            matches: Vec::new(),
            current: None,
            options: SearchOptions::case_insensitive(),
        }
    }
}

impl SearchUi {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn open(&mut self) {
        self.open = true;
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.matches.clear();
        self.current = None;
    }

    pub(super) fn reset_for_reflow(&mut self) {
        self.close();
    }

    pub(super) fn push_char(&mut self, ch: char) {
        if !ch.is_control() {
            self.query.push(ch);
        }
    }

    pub(super) fn backspace(&mut self) {
        self.query.pop();
    }

    pub(super) fn refresh(&mut self, terminal: &Terminal) {
        if self.query.is_empty() {
            self.matches.clear();
            self.current = None;
            return;
        }

        let previous = self.current;
        self.matches = terminal.search(&self.query, self.options);
        self.current = previous
            .filter(|current| self.matches.iter().any(|m| m == current))
            .or_else(|| self.matches.first().copied());
    }

    pub(super) fn next(&mut self) {
        self.current = match self.current {
            Some(current) => find_next(&self.matches, current.start),
            None => self.matches.first().copied(),
        };
    }

    pub(super) fn prev(&mut self) {
        self.current = match self.current {
            Some(current) => find_prev(&self.matches, current.start),
            None => self.matches.last().copied(),
        };
    }

    pub(super) fn viewport_offset_for_current(
        &self,
        scrollback_len: usize,
        dimensions: Dimensions,
    ) -> Option<usize> {
        self.current
            .map(|current| viewport_offset_for_match(current.start.row, scrollback_len, dimensions))
    }

    pub(super) fn render_signature(&self) -> SearchRenderSignature {
        SearchRenderSignature {
            open: self.open,
            query: self.query.clone(),
            matches: self
                .matches
                .iter()
                .map(SearchMatchSignature::from)
                .collect(),
            current: self.current.map(SearchMatchSignature::from),
        }
    }

    fn current_index(&self) -> Option<usize> {
        let current = self.current?;
        self.matches
            .iter()
            .position(|m| *m == current)
            .map(|i| i + 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchRenderSignature {
    pub(super) open: bool,
    pub(super) query: String,
    pub(super) matches: Vec<SearchMatchSignature>,
    pub(super) current: Option<SearchMatchSignature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SearchMatchSignature {
    pub(super) start: (usize, usize),
    pub(super) end: (usize, usize),
}

impl From<&SearchMatch> for SearchMatchSignature {
    fn from(value: &SearchMatch) -> Self {
        Self::from(*value)
    }
}

impl From<SearchMatch> for SearchMatchSignature {
    fn from(value: SearchMatch) -> Self {
        fn point(value: AbsolutePoint) -> (usize, usize) {
            (value.row, value.column)
        }
        Self {
            start: point(value.start),
            end: point(value.end),
        }
    }
}

pub(super) fn viewport_offset_for_match(
    absolute_row: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
) -> usize {
    let centered_top = absolute_row.saturating_sub(dimensions.rows / 2);
    let top = centered_top.min(scrollback_len);
    scrollback_len.saturating_sub(top)
}

pub(super) fn apply_search_ui(
    snapshot: &mut Snapshot,
    search: &SearchUi,
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    themed: Option<SearchStyle>,
) {
    if !search.open {
        return;
    }

    for search_match in &search.matches {
        if let Some(range) =
            visible_range_for_match(*search_match, viewport_offset, scrollback_len, dimensions)
        {
            apply_match_highlight(
                snapshot,
                range,
                Some(*search_match) == search.current,
                themed,
            );
        }
    }

    apply_search_bar(snapshot, search);
}

fn visible_range_for_match(
    search_match: SearchMatch,
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
) -> Option<SelectionRange> {
    selection::visible_range_from_absolute(
        AbsoluteSelectionRange {
            start: AbsoluteCellPoint {
                row: search_match.start.row,
                column: search_match.start.column,
            },
            end: AbsoluteCellPoint {
                row: search_match.end.row,
                column: search_match.end.column,
            },
        },
        viewport_offset,
        scrollback_len,
        dimensions,
    )
}

fn apply_match_highlight(
    snapshot: &mut Snapshot,
    range: SelectionRange,
    current: bool,
    themed: Option<SearchStyle>,
) {
    let start_row = range.start.row.min(snapshot.dimensions.rows - 1);
    let end_row = range.end.row.min(snapshot.dimensions.rows - 1);

    for row in start_row..=end_row {
        let start_column = if row == start_row {
            range.start.column.min(snapshot.dimensions.columns - 1)
        } else {
            0
        };
        let end_column = if row == end_row {
            range.end.column.min(snapshot.dimensions.columns - 1)
        } else {
            snapshot.dimensions.columns - 1
        };
        let offset = row * snapshot.dimensions.columns;
        for cell in &mut snapshot.cells[offset + start_column..=offset + end_column] {
            match (themed, current) {
                // Themed active match: brightened fill + floored fg.
                (Some(style), true) => {
                    cell.attrs.set_inverse(false);
                    cell.attrs.foreground =
                        Color::Rgb(style.active_fg[0], style.active_fg[1], style.active_fg[2]);
                    cell.attrs.background = Color::Rgb(
                        style.active_fill[0],
                        style.active_fill[1],
                        style.active_fill[2],
                    );
                }
                // Themed non-active match: search-role fill + floored fg.
                (Some(style), false) => {
                    cell.attrs.set_inverse(false);
                    cell.attrs.foreground = Color::Rgb(style.fg[0], style.fg[1], style.fg[2]);
                    cell.attrs.background = Color::Rgb(style.fill[0], style.fill[1], style.fill[2]);
                }
                // Default active match: historical black-on-yellow.
                (None, true) => {
                    cell.attrs.set_inverse(false);
                    cell.attrs.foreground = Color::Indexed(0);
                    cell.attrs.background = Color::Indexed(11);
                }
                // Default non-active match: historical inverse, byte-identical.
                (None, false) => cell.attrs.set_inverse(true),
            }
        }
    }
}

fn apply_search_bar(snapshot: &mut Snapshot, search: &SearchUi) {
    let columns = snapshot.dimensions.columns;
    let row = snapshot.dimensions.rows - 1;
    let start = row * columns;
    let attrs = search_bar_attrs();
    for cell in &mut snapshot.cells[start..start + columns] {
        *cell = Cell::new(' ', attrs);
    }

    let status = match (
        search.current_index(),
        search.matches.len(),
        search.query.is_empty(),
    ) {
        (_, _, true) => " Search: ".to_owned(),
        (Some(index), total, false) => {
            format!(" Search: {}  {index}/{total}", search.query)
        }
        (None, _, false) => format!(" Search: {}  0/0", search.query),
    };
    write_overlay_text(snapshot, row, &status, attrs);
}

fn write_overlay_text(snapshot: &mut Snapshot, row: usize, text: &str, attrs: Attrs) {
    let columns = snapshot.dimensions.columns;
    let offset = row * columns;
    let mut column = 0;

    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if width > 2 || column + width > columns {
            break;
        }

        snapshot.cells[offset + column] = Cell::new(ch, attrs);
        if width == 2 {
            snapshot.cells[offset + column + 1] = Cell::wide_spacer(attrs);
        }
        column += width;
    }
}

fn search_bar_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Default;
    attrs.background = Color::Default;
    attrs.set_inverse(true);
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AbsolutePoint;
    use crate::core::Terminal;

    fn point(row: usize, column: usize) -> AbsolutePoint {
        AbsolutePoint { row, column }
    }

    fn smatch(row: usize, column: usize) -> SearchMatch {
        SearchMatch {
            start: point(row, column),
            end: point(row, column),
        }
    }

    #[test]
    fn query_state_opens_edits_and_closes() {
        let mut ui = SearchUi::default();

        ui.open();
        ui.push_char('a');
        ui.push_char('b');
        ui.push_char('\n');
        assert!(ui.is_open());
        assert_eq!(ui.query, "ab");

        ui.backspace();
        assert_eq!(ui.query, "a");

        ui.close();
        assert!(!ui.is_open());
        assert!(ui.query.is_empty());
        assert!(ui.matches.is_empty());
        assert_eq!(ui.current, None);
    }

    #[test]
    fn refresh_uses_case_insensitive_search_by_default() {
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(b"Alpha beta");

        let mut ui = SearchUi::default();
        ui.open();
        ui.push_char('a');
        ui.push_char('l');
        ui.refresh(&terminal);

        assert_eq!(ui.matches.len(), 1);
        assert_eq!(ui.current.unwrap().start, point(0, 0));
    }

    #[test]
    fn navigation_wraps_in_match_order() {
        let mut ui = SearchUi {
            matches: vec![smatch(0, 2), smatch(1, 4), smatch(3, 1)],
            current: Some(smatch(0, 2)),
            ..SearchUi::default()
        };

        ui.next();
        assert_eq!(ui.current, Some(smatch(1, 4)));
        ui.next();
        assert_eq!(ui.current, Some(smatch(3, 1)));
        ui.next();
        assert_eq!(ui.current, Some(smatch(0, 2)));

        ui.prev();
        assert_eq!(ui.current, Some(smatch(3, 1)));
    }

    #[test]
    fn viewport_jump_centers_scrollback_match_and_clamps_live_rows() {
        let dims = Dimensions::new(80, 10);

        assert_eq!(viewport_offset_for_match(12, 20, dims), 13);
        assert_eq!(viewport_offset_for_match(27, 20, dims), 0);
    }

    #[test]
    fn overlay_replaces_bottom_row_without_touching_terminal_state() {
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(b"first\nsecond");
        let original = terminal.snapshot();
        let mut snapshot = original.clone();

        let mut ui = SearchUi::default();
        ui.open();
        ui.push_char('s');
        ui.refresh(&terminal);
        apply_search_ui(&mut snapshot, &ui, 0, 0, Dimensions::new(20, 2), None);

        assert_eq!(terminal.snapshot(), original);
        assert_eq!(snapshot.cells[20].ch, ' ');
        assert_eq!(snapshot.cells[21].ch, 'S');
        assert!(snapshot.cells[21].attrs.inverse());
    }

    fn themed_style() -> SearchStyle {
        SearchStyle {
            fill: [0x5C, 0x50, 0x1F],
            fg: [0xF0, 0xEC, 0xD8],
            active_fill: [0x9A, 0x88, 0x44],
            active_fg: [0x10, 0x0E, 0x06],
        }
    }

    #[test]
    fn default_active_match_uses_black_on_yellow_and_non_active_inverse() {
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(b"al al");
        let mut snapshot = terminal.snapshot();

        let mut ui = SearchUi::default();
        ui.open();
        ui.push_char('a');
        ui.push_char('l');
        ui.refresh(&terminal);
        // Two matches; first is current.
        apply_search_ui(&mut snapshot, &ui, 0, 0, Dimensions::new(20, 2), None);

        // Active match (cols 0..=1): black-on-yellow, inverse cleared.
        assert!(!snapshot.cells[0].attrs.inverse());
        assert_eq!(snapshot.cells[0].attrs.foreground, Color::Indexed(0));
        assert_eq!(snapshot.cells[0].attrs.background, Color::Indexed(11));
        // Non-active match (cols 3..=4): plain inverse.
        assert!(snapshot.cells[3].attrs.inverse());
    }

    #[test]
    fn themed_search_paints_role_fills_with_distinct_active_treatment() {
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(b"al al");
        let mut snapshot = terminal.snapshot();

        let mut ui = SearchUi::default();
        ui.open();
        ui.push_char('a');
        ui.push_char('l');
        ui.refresh(&terminal);
        apply_search_ui(
            &mut snapshot,
            &ui,
            0,
            0,
            Dimensions::new(20, 2),
            Some(themed_style()),
        );

        // Active match: brightened fill + floored fg, inverse cleared.
        assert!(!snapshot.cells[0].attrs.inverse());
        assert_eq!(
            snapshot.cells[0].attrs.background,
            Color::Rgb(0x9A, 0x88, 0x44)
        );
        assert_eq!(
            snapshot.cells[0].attrs.foreground,
            Color::Rgb(0x10, 0x0E, 0x06)
        );
        // Non-active match: search-role fill + floored fg, inverse cleared.
        assert!(!snapshot.cells[3].attrs.inverse());
        assert_eq!(
            snapshot.cells[3].attrs.background,
            Color::Rgb(0x5C, 0x50, 0x1F)
        );
        assert_eq!(
            snapshot.cells[3].attrs.foreground,
            Color::Rgb(0xF0, 0xEC, 0xD8)
        );
    }

    #[test]
    fn search_highlight_takes_precedence_over_selection_on_overlap() {
        // Selection paints first, then the search overlay; on overlapping
        // cells the search treatment must win (matches the render order in
        // app/mod.rs: apply_highlight then apply_search_ui).
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(b"alpha");
        let mut snapshot = terminal.snapshot();

        // Themed selection across the whole row.
        let sel_range = SelectionRange {
            start: selection::CellPoint { row: 0, column: 0 },
            end: selection::CellPoint { row: 0, column: 4 },
        };
        selection::apply_highlight(
            &mut snapshot,
            sel_range,
            Some(selection::SelectionStyle {
                fill: [0x24, 0x33, 0x52],
                fg: [0xEA, 0xEE, 0xF4],
            }),
        );

        let mut ui = SearchUi::default();
        ui.open();
        ui.push_char('a');
        ui.push_char('l');
        ui.refresh(&terminal);
        apply_search_ui(
            &mut snapshot,
            &ui,
            0,
            0,
            Dimensions::new(20, 2),
            Some(themed_style()),
        );

        // Cols 0..=1 are both selected and the (current) search match: search wins.
        assert_eq!(
            snapshot.cells[0].attrs.background,
            Color::Rgb(0x9A, 0x88, 0x44)
        );
        // Cols 2..=4 are selected only: selection fill remains.
        assert_eq!(
            snapshot.cells[2].attrs.background,
            Color::Rgb(0x24, 0x33, 0x52)
        );
    }
}
