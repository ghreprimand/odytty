use crate::core::{
    Attrs, Cell, Color, Dimensions, SearchMatch, SearchOptions, Snapshot, Terminal, find_next,
    find_prev,
};
use crate::selection::{self, AbsoluteCellPoint, AbsoluteSelectionRange, SelectionRange};

use unicode_width::UnicodeWidthChar;

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

    fn current_index(&self) -> Option<usize> {
        let current = self.current?;
        self.matches
            .iter()
            .position(|m| *m == current)
            .map(|i| i + 1)
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
) {
    if !search.open {
        return;
    }

    for search_match in &search.matches {
        if let Some(range) =
            visible_range_for_match(*search_match, viewport_offset, scrollback_len, dimensions)
        {
            apply_match_highlight(snapshot, range, Some(*search_match) == search.current);
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

fn apply_match_highlight(snapshot: &mut Snapshot, range: SelectionRange, current: bool) {
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
            if current {
                cell.attrs.inverse = false;
                cell.attrs.foreground = Color::Indexed(0);
                cell.attrs.background = Color::Indexed(11);
            } else {
                cell.attrs.inverse = true;
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
    Attrs {
        inverse: true,
        foreground: Color::Default,
        background: Color::Default,
        ..Attrs::default()
    }
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
        apply_search_ui(&mut snapshot, &ui, 0, 0, Dimensions::new(20, 2));

        assert_eq!(terminal.snapshot(), original);
        assert_eq!(snapshot.cells[20].ch, ' ');
        assert_eq!(snapshot.cells[21].ch, 'S');
        assert!(snapshot.cells[21].attrs.inverse);
    }
}
