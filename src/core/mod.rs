use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub columns: usize,
    pub rows: usize,
}

impl Dimensions {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            columns: columns.max(1),
            rows: rows.max(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Default for Color {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub foreground: Color,
    pub background: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub attrs: Attrs,
    pub wide_continuation: bool,
}

impl Cell {
    pub fn blank() -> Self {
        Self::blank_with_bg(Color::Default)
    }

    pub fn blank_with_bg(background: Color) -> Self {
        let attrs = Attrs {
            background,
            ..Attrs::default()
        };

        Self {
            ch: ' ',
            attrs,
            wide_continuation: false,
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

/// One physical row of cells plus a soft-wrap marker.
///
/// `wrapped` is `true` when this row's content continues onto the next physical
/// row because auto-wrap ran at the right edge (a *soft* line break), and
/// `false` when the row ends at a hard line break (newline) or screen edge with
/// no continuation. The marker lets [`Screen::resize`] rejoin soft-wrapped rows
/// into logical lines and re-wrap them to a new width, so text that scrolls off
/// a narrowed window reappears when it is widened again.
///
/// `Line` derefs to its `cells` vector, so existing `row[col]`, `row.iter()`,
/// `row.get(..)`, and `row.resize(..)` call sites keep working unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    cells: Vec<Cell>,
    wrapped: bool,
}

impl Line {
    /// A row that ends a logical line (hard break / no continuation).
    fn unwrapped(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            wrapped: false,
        }
    }

    /// A row that soft-wraps into the next physical row.
    fn wrapped(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            wrapped: true,
        }
    }
}

impl std::ops::Deref for Line {
    type Target = Vec<Cell>;

    fn deref(&self) -> &Self::Target {
        &self.cells
    }
}

impl std::ops::DerefMut for Line {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cells
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub dimensions: Dimensions,
    pub cursor: Position,
    pub cursor_visible: bool,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyRegion {
    Clean,
    Full,
}

pub trait TerminalModel {
    fn dimensions(&self) -> Dimensions;
    fn cursor(&self) -> Position;
    fn cell(&self, row: usize, column: usize) -> Option<Cell>;
    fn snapshot(&self) -> Snapshot;
    fn take_dirty(&mut self) -> DirtyRegion;
}

#[derive(Debug, Clone)]
pub struct Screen {
    dimensions: Dimensions,
    rows: Vec<Line>,
    scrollback: Vec<Line>,
    cursor: Position,
    cursor_visible: bool,
    pending_wrap: bool,
    saved_cursor: Option<SavedCursor>,
    primary_screen: Option<StoredScreen>,
    scroll_region: Option<ScrollRegion>,
    /// DECOM (origin mode, private mode 6). When set, CUP/HVP/VPA addressing is
    /// relative to the active scroll region top and constrained within it.
    origin_mode: bool,
    bracketed_paste: bool,
    current_attrs: Attrs,
    dirty: DirtyRegion,
    host_output: Vec<u8>,
    last_graphic_char: Option<char>,
    tab_stops: Vec<bool>,
}

#[derive(Debug, Clone)]
struct StoredScreen {
    rows: Vec<Line>,
    scrollback: Vec<Line>,
    cursor: Position,
    pending_wrap: bool,
    saved_cursor: Option<SavedCursor>,
    scroll_region: Option<ScrollRegion>,
    origin_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SavedCursor {
    position: Position,
    pending_wrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollRegion {
    top: usize,
    bottom: usize,
}

impl Screen {
    pub fn new(columns: usize, rows: usize) -> Self {
        let dimensions = Dimensions::new(columns, rows);
        Self {
            dimensions,
            rows: vec![blank_row(dimensions.columns); dimensions.rows],
            scrollback: Vec::new(),
            cursor: Position::default(),
            cursor_visible: true,
            pending_wrap: false,
            saved_cursor: None,
            primary_screen: None,
            scroll_region: None,
            origin_mode: false,
            bracketed_paste: false,
            current_attrs: Attrs::default(),
            dirty: DirtyRegion::Full,
            host_output: Vec::new(),
            last_graphic_char: None,
            tab_stops: default_tab_stops(dimensions.columns),
        }
    }

    pub fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    pub fn cursor(&self) -> Position {
        self.cursor
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn cell(&self, row: usize, column: usize) -> Option<Cell> {
        self.rows
            .get(row)
            .and_then(|line| line.get(column))
            .copied()
    }

    /// Resize the grid to `columns` × `rows`, preserving content.
    ///
    /// The active **primary** screen reflows: soft-wrapped physical rows are
    /// rejoined into logical lines (using each row's [`Line::wrapped`] marker),
    /// then re-wrapped to the new width across the combined scrollback + visible
    /// buffer. This means text that wraps off a narrowed window is recoverable
    /// when it is widened again, rather than being truncated at the right edge.
    ///
    /// The **alternate** screen does not reflow: full-screen TUI applications
    /// own their layout and repaint on resize (`SIGWINCH`), so the alternate
    /// grid is simply truncated/padded to the new size. The stored primary
    /// screen behind it is still reflowed so leaving the alternate screen after
    /// a resize is coherent. Alternate-screen isolation and the no-scrollback
    /// rule for the alternate buffer are preserved.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let dimensions = Dimensions::new(columns, rows);

        if self.primary_screen.is_some() {
            // Alternate screen active: truncate/pad the app-managed grid (it
            // repaints), but never feed the alternate buffer into scrollback.
            resize_buffer_rows(&mut self.rows, &mut self.scrollback, dimensions, true);
            self.cursor.row = self.cursor.row.min(dimensions.rows - 1);
            self.cursor.column = self.cursor.column.min(dimensions.columns - 1);

            if let Some(mut primary) = self.primary_screen.take() {
                let cursor = reflow_lines(
                    &mut primary.scrollback,
                    &mut primary.rows,
                    dimensions,
                    primary.cursor,
                );
                primary.cursor = cursor;
                primary.pending_wrap = false;
                primary.scroll_region = clamp_scroll_region(primary.scroll_region, dimensions);
                self.primary_screen = Some(primary);
            }
        } else {
            // Primary screen active: reflow visible + scrollback to the new width
            // so wrapped content is preserved across shrink/grow.
            self.cursor = reflow_lines(
                &mut self.scrollback,
                &mut self.rows,
                dimensions,
                self.cursor,
            );
        }

        self.dimensions = dimensions;
        self.pending_wrap = false;
        self.resize_tab_stops(dimensions.columns);
        self.scroll_region = clamp_scroll_region(self.scroll_region, dimensions);
        self.mark_dirty();
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            dimensions: self.dimensions,
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            cells: self
                .rows
                .iter()
                .flat_map(|line| line.iter())
                .copied()
                .collect(),
        }
    }

    /// Produce a visible-grid snapshot at a scrollback viewport offset.
    ///
    /// `offset_rows` counts how many rows the viewport is paged *upward* into
    /// scrollback. Offset `0` is the live visible screen and is byte-for-byte
    /// identical to [`snapshot`](Self::snapshot). Positive offsets page upward;
    /// the offset is clamped to the available scrollback so callers cannot read
    /// past the oldest stored row.
    ///
    /// The composed buffer is `scrollback` (oldest→newest) followed by the live
    /// `rows`; the returned viewport is the `dimensions.rows`-tall window whose
    /// bottom edge sits `offset_rows` above the live bottom. Each emitted row is
    /// normalized to `dimensions.columns` so the `cells` length always equals
    /// `dimensions.rows * dimensions.columns`.
    ///
    /// Cursor policy: at offset `0` the live cursor and its visibility carry
    /// through unchanged; for any nonzero (scrolled-back) offset the cursor is
    /// hidden (`cursor_visible == false`) because it does not belong to the
    /// historical viewport. The cursor position is reported unchanged.
    ///
    /// Alternate-screen isolation is preserved for free: entering the alternate
    /// screen moves the primary scrollback into off-screen storage, so an
    /// alternate-screen `Screen` has empty scrollback and every offset clamps to
    /// the live grid — primary history never leaks into alternate snapshots.
    pub fn snapshot_with_scrollback(&self, offset_rows: usize) -> Snapshot {
        let height = self.dimensions.rows;
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.len();
        let offset = offset_rows.min(scrollback_len);

        if offset == 0 {
            return self.snapshot();
        }

        // Combined buffer index of the row just below the viewport bottom.
        let total = scrollback_len + height;
        let window_end = total - offset;
        let window_start = window_end - height;

        let mut cells = Vec::with_capacity(height * columns);
        for index in window_start..window_end {
            let row = if index < scrollback_len {
                &self.scrollback[index]
            } else {
                &self.rows[index - scrollback_len]
            };
            for column in 0..columns {
                cells.push(row.get(column).copied().unwrap_or_else(Cell::blank));
            }
        }

        Snapshot {
            dimensions: self.dimensions,
            cursor: self.cursor,
            cursor_visible: false,
            cells,
        }
    }

    pub fn plain_text(&self) -> String {
        self.rows
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|cell| !cell.wide_continuation)
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    fn print_char(&mut self, ch: char) {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1);
        if width == 0 {
            return;
        }

        self.last_graphic_char = Some(ch);

        if self.pending_wrap {
            // The row we are leaving filled to the right edge and the logical
            // line continues here: mark it as a soft wrap so resize can rejoin.
            self.rows[self.cursor.row].wrapped = true;
            self.carriage_return();
            self.line_feed();
            self.pending_wrap = false;
        }

        if self.cursor.column + width > self.dimensions.columns {
            // A wide glyph does not fit in the remaining columns; it continues
            // the logical line on the next row (also a soft wrap).
            self.rows[self.cursor.row].wrapped = true;
            self.carriage_return();
            self.line_feed();
        }

        let row = self.cursor.row;
        let column = self.cursor.column;
        self.rows[row][column] = Cell {
            ch,
            attrs: self.current_attrs,
            wide_continuation: false,
        };

        if width == 2 && column + 1 < self.dimensions.columns {
            self.rows[row][column + 1] = Cell {
                ch: ' ',
                attrs: self.current_attrs,
                wide_continuation: true,
            };
        }

        if self.cursor.column + width >= self.dimensions.columns {
            self.cursor.column = self.dimensions.columns - 1;
            self.pending_wrap = true;
        } else {
            self.cursor.column += width;
        }
        self.mark_dirty();
    }

    fn backspace(&mut self) {
        self.cursor.column = self.cursor.column.saturating_sub(1);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn tab(&mut self) {
        let last = self.dimensions.columns - 1;
        // Advance to the next tab stop strictly right of the cursor; if none
        // exists, clamp to the right edge.
        let next = ((self.cursor.column + 1)..self.dimensions.columns)
            .find(|&column| self.tab_stops.get(column).copied().unwrap_or(false))
            .unwrap_or(last);
        self.cursor.column = next;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// HTS (ESC H): set a tab stop at the current cursor column.
    fn set_tab_stop(&mut self) {
        if let Some(stop) = self.tab_stops.get_mut(self.cursor.column) {
            *stop = true;
        }
    }

    /// TBC (CSI Ps g): clear tab stops. Ps=0 (the default) clears the stop at
    /// the current column; Ps=3 clears every tab stop. Other selectors ignored.
    fn clear_tab_stop(&mut self, mode: usize) {
        match mode {
            0 => {
                if let Some(stop) = self.tab_stops.get_mut(self.cursor.column) {
                    *stop = false;
                }
            }
            3 => self.tab_stops.iter_mut().for_each(|stop| *stop = false),
            _ => {}
        }
    }

    /// Resize the tab-stop table coherently. Existing stops in the retained
    /// column range are preserved; when growing, newly exposed columns receive
    /// the default every-8 stops; when shrinking, the table is truncated so
    /// stops beyond the new width can no longer be used.
    fn resize_tab_stops(&mut self, columns: usize) {
        let old = self.tab_stops.len();
        if columns > old {
            self.tab_stops.resize(columns, false);
            for column in (old..columns).filter(|column| column % 8 == 0 && *column >= 8) {
                self.tab_stops[column] = true;
            }
        } else {
            self.tab_stops.truncate(columns);
        }
    }

    fn carriage_return(&mut self) {
        self.cursor.column = 0;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn line_feed(&mut self) {
        self.pending_wrap = false;
        if self
            .scroll_region
            .is_some_and(|region| self.cursor.row == region.bottom)
        {
            self.scroll_up_region();
        } else if self.cursor.row + 1 == self.dimensions.rows && self.scroll_region.is_none() {
            self.scroll_up_full();
        } else if self.cursor.row + 1 < self.dimensions.rows {
            self.cursor.row += 1;
            self.mark_dirty();
        } else {
            self.mark_dirty();
        }
    }

    fn scroll_up_full(&mut self) {
        let removed = self.rows.remove(0);
        let background = self.current_attrs.background;

        if self.primary_screen.is_none() {
            self.scrollback.push(removed);
        }

        self.rows
            .push(blank_row_with_bg(self.dimensions.columns, background));
        self.mark_dirty();
    }

    fn scroll_up_region(&mut self) {
        if let Some(region) = self.scroll_region {
            let background = self.current_attrs.background;
            self.rows.remove(region.top);
            self.rows.insert(
                region.bottom,
                blank_row_with_bg(self.dimensions.columns, background),
            );
            self.mark_dirty();
        }
    }

    /// SU (CSI Ps S): scroll the active region up by `count` lines, discarding
    /// lines off the top of the region and filling at the bottom with BCE-aware
    /// blank rows. Falls back to the full screen when no DECSTBM region is set.
    /// Never feeds scrollback (no pollution) and does not move the cursor.
    fn scroll_region_up(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        let count = count.max(1).min(bottom - top + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(top);
            self.rows.insert(
                bottom,
                blank_row_with_bg(self.dimensions.columns, background),
            );
        }
        self.mark_dirty();
    }

    /// SD (CSI Ps T): scroll the active region down by `count` lines, discarding
    /// lines off the bottom of the region and filling at the top with BCE-aware
    /// blank rows. Falls back to the full screen when no DECSTBM region is set.
    /// Never feeds scrollback (no pollution) and does not move the cursor.
    fn scroll_region_down(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        let count = count.max(1).min(bottom - top + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(bottom);
            self.rows
                .insert(top, blank_row_with_bg(self.dimensions.columns, background));
        }
        self.mark_dirty();
    }

    /// Active vertical scroll margins. Falls back to the full screen when no
    /// explicit DECSTBM region is set (the standard behaviour for RI/IL/DL).
    fn effective_region(&self) -> (usize, usize) {
        match self.scroll_region {
            Some(region) => (region.top, region.bottom),
            None => (0, self.dimensions.rows - 1),
        }
    }

    /// RI (ESC M): at the top margin, scroll the region down by one; otherwise
    /// move the cursor up one row. Never feeds scrollback.
    fn reverse_index(&mut self) {
        self.pending_wrap = false;
        let (top, bottom) = self.effective_region();
        let background = self.current_attrs.background;

        if self.cursor.row == top {
            self.rows.remove(bottom);
            self.rows
                .insert(top, blank_row_with_bg(self.dimensions.columns, background));
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
        self.mark_dirty();
    }

    /// IL (CSI Ps L): insert `count` blank lines at the cursor row, scrolling
    /// the rows below it down within the region. Lines pushed past the region
    /// bottom are discarded (never to scrollback). No-op outside the region.
    fn insert_lines(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }

        let count = count.max(1).min(bottom - self.cursor.row + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(bottom);
            self.rows.insert(
                self.cursor.row,
                blank_row_with_bg(self.dimensions.columns, background),
            );
        }

        self.cursor.column = 0;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DL (CSI Ps M): delete `count` lines at the cursor row, scrolling the
    /// rows below it up within the region and filling blanks at the region
    /// bottom. No-op outside the region.
    fn delete_lines(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }

        let count = count.max(1).min(bottom - self.cursor.row + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(self.cursor.row);
            self.rows.insert(
                bottom,
                blank_row_with_bg(self.dimensions.columns, background),
            );
        }

        self.cursor.column = 0;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// ICH (CSI Ps @): insert `count` blank cells at the cursor, shifting the
    /// rest of the line right. Cells pushed past the right edge are discarded.
    /// Row-local: no wrap, no scroll, cursor stays in place. Fill blanks use
    /// the active background color and otherwise default attributes, matching
    /// xterm-style background-color-erase behavior for insert fills.
    fn insert_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);
        let blank = self.current_blank();

        let row = &mut self.rows[self.cursor.row];
        for _ in 0..count {
            row.insert(column, blank);
        }
        row.truncate(columns);

        sanitize_wide_row(row, blank);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DCH (CSI Ps P): delete `count` cells at the cursor, shifting the rest of
    /// the line left and filling blanks at the right edge. Row-local: no wrap,
    /// no scroll, cursor stays in place. Fill blanks use the active background
    /// color and otherwise default attributes, matching xterm-style
    /// background-color-erase behavior for delete fills.
    fn delete_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);
        let blank = self.current_blank();

        let row = &mut self.rows[self.cursor.row];
        for _ in 0..count {
            row.remove(column);
        }
        while row.len() < columns {
            row.push(blank);
        }

        sanitize_wide_row(row, blank);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// ECH (CSI Ps X): erase `count` cells from the cursor in place, overwriting
    /// them with blanks WITHOUT shifting the rest of the line. Row-local: no
    /// wrap, no scroll, cursor stays put. Blanks use the active background
    /// color and otherwise default attributes, matching xterm-style
    /// background-color-erase behavior.
    fn erase_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);
        let blank = self.current_blank();

        let row = &mut self.rows[self.cursor.row];
        for cell in &mut row[column..column + count] {
            *cell = blank;
        }

        sanitize_wide_row(row, blank);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// REP (CSI Ps b): repeat the last printed graphic character `count` times,
    /// using normal print processing (so the current SGR attrs apply and
    /// autowrap behaves as if the character were typed again). Omitted/zero
    /// count = 1. No-op when no graphic character has been printed yet.
    /// Replaying through `print_char` means a wide last character repeats as a
    /// wide glyph and wraps correctly.
    fn repeat_char(&mut self, count: usize) {
        let Some(ch) = self.last_graphic_char else {
            return;
        };
        let count = count.max(1);
        for _ in 0..count {
            self.print_char(ch);
        }
    }

    fn move_up(&mut self, count: usize) {
        self.cursor.row = self.cursor.row.saturating_sub(count);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn move_down(&mut self, count: usize) {
        self.cursor.row = (self.cursor.row + count).min(self.dimensions.rows - 1);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn move_right(&mut self, count: usize) {
        self.cursor.column = (self.cursor.column + count).min(self.dimensions.columns - 1);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn move_left(&mut self, count: usize) {
        self.cursor.column = self.cursor.column.saturating_sub(count);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn move_to(&mut self, row: usize, column: usize) {
        self.cursor.row = row.saturating_sub(1).min(self.dimensions.rows - 1);
        self.cursor.column = column.saturating_sub(1).min(self.dimensions.columns - 1);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// CUP/HVP/VPA addressing honoring DECOM (origin mode).
    ///
    /// `row`/`column` are 1-based. When origin mode is off this is identical to
    /// [`Screen::move_to`] (full-screen absolute addressing). When origin mode
    /// is on, the row is interpreted relative to the active scroll region top
    /// and clamped to the region bottom, so a program that set DECSTBM + DECOM
    /// can address rows `1..=region_height` without escaping the region. The
    /// column is unaffected by origin mode (no horizontal margins here).
    fn move_to_origin(&mut self, row: usize, column: usize) {
        let column = column.saturating_sub(1).min(self.dimensions.columns - 1);
        if self.origin_mode {
            let (top, bottom) = self.effective_region();
            let target = top + row.max(1) - 1;
            self.cursor.row = target.min(bottom);
        } else {
            self.cursor.row = row.saturating_sub(1).min(self.dimensions.rows - 1);
        }
        self.cursor.column = column;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn erase_display(&mut self, mode: usize) {
        let background = self.current_attrs.background;
        match mode {
            0 => {
                self.erase_line_from_cursor();
                for row in self.cursor.row + 1..self.dimensions.rows {
                    self.rows[row] = blank_row_with_bg(self.dimensions.columns, background);
                }
            }
            1 => {
                for row in 0..self.cursor.row {
                    self.rows[row] = blank_row_with_bg(self.dimensions.columns, background);
                }
                self.erase_line_to_cursor();
            }
            2 | 3 => {
                for row in &mut self.rows {
                    *row = blank_row_with_bg(self.dimensions.columns, background);
                }
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
        self.mark_dirty();
    }

    fn erase_line(&mut self, mode: usize) {
        match mode {
            0 => self.erase_line_from_cursor(),
            1 => self.erase_line_to_cursor(),
            2 => {
                self.rows[self.cursor.row] = self.current_blank_row();
            }
            _ => {}
        }
        self.mark_dirty();
    }

    fn erase_line_from_cursor(&mut self) {
        let blank = self.current_blank();
        for column in self.cursor.column..self.dimensions.columns {
            self.rows[self.cursor.row][column] = blank;
        }
        self.mark_dirty();
    }

    fn erase_line_to_cursor(&mut self) {
        let blank = self.current_blank();
        for column in 0..=self.cursor.column {
            self.rows[self.cursor.row][column] = blank;
        }
        self.mark_dirty();
    }

    fn current_blank(&self) -> Cell {
        Cell::blank_with_bg(self.current_attrs.background)
    }

    fn current_blank_row(&self) -> Line {
        blank_row_with_bg(self.dimensions.columns, self.current_attrs.background)
    }

    fn mark_dirty(&mut self) {
        self.dirty = DirtyRegion::Full;
    }

    fn apply_sgr(&mut self, params: &Params) {
        let codes = sgr_codes(params);
        let codes = if codes.is_empty() { vec![0] } else { codes };
        let mut index = 0;

        while index < codes.len() {
            match codes[index] {
                0 => self.current_attrs = Attrs::default(),
                1 => self.current_attrs.bold = true,
                3 => self.current_attrs.italic = true,
                4 => self.current_attrs.underline = true,
                7 => self.current_attrs.inverse = true,
                22 => self.current_attrs.bold = false,
                23 => self.current_attrs.italic = false,
                24 => self.current_attrs.underline = false,
                27 => self.current_attrs.inverse = false,
                30..=37 => {
                    self.current_attrs.foreground = Color::Indexed((codes[index] - 30) as u8)
                }
                39 => self.current_attrs.foreground = Color::Default,
                40..=47 => {
                    self.current_attrs.background = Color::Indexed((codes[index] - 40) as u8)
                }
                49 => self.current_attrs.background = Color::Default,
                90..=97 => {
                    self.current_attrs.foreground = Color::Indexed((codes[index] - 90 + 8) as u8);
                }
                100..=107 => {
                    self.current_attrs.background = Color::Indexed((codes[index] - 100 + 8) as u8);
                }
                38 | 48 => {
                    if let Some((color, consumed)) = parse_extended_color(&codes[index..]) {
                        if codes[index] == 38 {
                            self.current_attrs.foreground = color;
                        } else {
                            self.current_attrs.background = color;
                        }
                        index += consumed - 1;
                    }
                }
                _ => {}
            }

            index += 1;
        }
    }

    fn set_cursor_mode(&mut self, params: &Params, intermediates: &[u8], action: char) {
        if intermediates != b"?" {
            return;
        }

        for mode in private_mode_params(params) {
            match mode {
                6 => {
                    // DECOM: toggling origin mode homes the cursor to the
                    // (region-relative when on, screen when off) origin.
                    self.origin_mode = action == 'h';
                    self.move_to_origin(1, 1);
                }
                25 => {
                    self.cursor_visible = action == 'h';
                    self.mark_dirty();
                }
                1049 => {
                    if action == 'h' {
                        self.enter_alternate_screen();
                    } else {
                        self.leave_alternate_screen();
                    }
                }
                2004 => {
                    self.bracketed_paste = action == 'h';
                }
                _ => {}
            }
        }
    }

    fn device_attributes(&mut self, params: &Params, intermediates: &[u8]) {
        if intermediates.is_empty() && param_or(params, 0, 0) == 0 {
            self.host_output.extend_from_slice(b"\x1b[?1;2c");
        }
    }

    fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            position: self.cursor,
            pending_wrap: self.pending_wrap,
        });
    }

    fn restore_cursor(&mut self) {
        if let Some(saved_cursor) = self.saved_cursor {
            self.cursor = Position {
                row: saved_cursor.position.row.min(self.dimensions.rows - 1),
                column: saved_cursor
                    .position
                    .column
                    .min(self.dimensions.columns - 1),
            };
            self.pending_wrap = saved_cursor.pending_wrap;
            self.mark_dirty();
        }
    }

    fn enter_alternate_screen(&mut self) {
        if self.primary_screen.is_some() {
            return;
        }

        let primary_screen = StoredScreen {
            rows: std::mem::replace(
                &mut self.rows,
                vec![blank_row(self.dimensions.columns); self.dimensions.rows],
            ),
            scrollback: std::mem::take(&mut self.scrollback),
            cursor: self.cursor,
            pending_wrap: self.pending_wrap,
            saved_cursor: self.saved_cursor,
            scroll_region: self.scroll_region,
            origin_mode: self.origin_mode,
        };

        self.cursor = Position::default();
        self.pending_wrap = false;
        self.saved_cursor = None;
        self.scroll_region = None;
        self.origin_mode = false;
        self.primary_screen = Some(primary_screen);
        self.mark_dirty();
    }

    fn leave_alternate_screen(&mut self) {
        if let Some(primary_screen) = self.primary_screen.take() {
            self.rows = primary_screen.rows;
            self.scrollback = primary_screen.scrollback;
            self.cursor = Position {
                row: primary_screen.cursor.row.min(self.dimensions.rows - 1),
                column: primary_screen
                    .cursor
                    .column
                    .min(self.dimensions.columns - 1),
            };
            self.pending_wrap = primary_screen.pending_wrap;
            self.saved_cursor = primary_screen.saved_cursor;
            self.scroll_region = primary_screen.scroll_region;
            self.origin_mode = primary_screen.origin_mode;
            self.mark_dirty();
        }
    }

    fn set_scroll_region(&mut self, params: &Params) {
        let top = param_or(params, 0, 1).saturating_sub(1);
        let bottom = param_or(params, 1, self.dimensions.rows).saturating_sub(1);

        self.scroll_region = if top < bottom && bottom < self.dimensions.rows {
            Some(ScrollRegion { top, bottom })
        } else {
            None
        };

        // DECSTBM homes the cursor: to the region top-left when origin mode is
        // on, otherwise to the screen top-left (consistent with prior behavior).
        self.move_to_origin(1, 1);
        self.mark_dirty();
    }

    /// RIS (ESC c): hard reset. Returns the terminal to its power-on state —
    /// exits the alternate screen, clears the visible grid and scrollback,
    /// drops saved cursor / scroll region, resets attributes, cursor
    /// visibility, bracketed paste and pending wrap, homes the cursor, and
    /// discards any pending host output.
    fn hard_reset(&mut self) {
        self.primary_screen = None;
        self.rows = vec![blank_row(self.dimensions.columns); self.dimensions.rows];
        self.scrollback.clear();
        self.cursor = Position::default();
        self.cursor_visible = true;
        self.pending_wrap = false;
        self.saved_cursor = None;
        self.scroll_region = None;
        self.origin_mode = false;
        self.bracketed_paste = false;
        self.current_attrs = Attrs::default();
        self.host_output.clear();
        self.last_graphic_char = None;
        // RIS restores the default every-8 tab stops (DECSTR does not — see
        // soft_reset).
        self.tab_stops = default_tab_stops(self.dimensions.columns);
        self.mark_dirty();
    }

    /// DECSTR (CSI ! p): soft reset. Resets modes and cursor state without
    /// touching the visible cells or scrollback. Cursor policy: homed to the
    /// top-left (documented in tests), matching xterm's DECSTR behaviour.
    /// Tab stops are deliberately PRESERVED — per the VT220 soft-reset
    /// definition, DECSTR does not clear tab stops; only RIS does.
    fn soft_reset(&mut self) {
        self.cursor = Position::default();
        self.cursor_visible = true;
        self.pending_wrap = false;
        self.saved_cursor = None;
        self.scroll_region = None;
        self.origin_mode = false;
        self.bracketed_paste = false;
        self.current_attrs = Attrs::default();
        self.host_output.clear();
        self.last_graphic_char = None;
        self.mark_dirty();
    }
}

impl TerminalModel for Screen {
    fn dimensions(&self) -> Dimensions {
        self.dimensions()
    }

    fn cursor(&self) -> Position {
        self.cursor()
    }

    fn cell(&self, row: usize, column: usize) -> Option<Cell> {
        self.cell(row, column)
    }

    fn snapshot(&self) -> Snapshot {
        self.snapshot()
    }

    fn take_dirty(&mut self) -> DirtyRegion {
        let dirty = self.dirty;
        self.dirty = DirtyRegion::Clean;
        dirty
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\x08' => self.backspace(),
            b'\t' => self.tab(),
            b'\n' | b'\x0b' | b'\x0c' => self.line_feed(),
            b'\r' => self.carriage_return(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }

        match action {
            'A' => self.move_up(param_or(params, 0, 1)),
            'B' => self.move_down(param_or(params, 0, 1)),
            'C' => self.move_right(param_or(params, 0, 1)),
            'D' => self.move_left(param_or(params, 0, 1)),
            'G' => self.move_to(self.cursor.row + 1, param_or(params, 0, 1)),
            'H' | 'f' => self.move_to_origin(param_or(params, 0, 1), param_or(params, 1, 1)),
            'S' => self.scroll_region_up(param_or(params, 0, 1)),
            'T' => self.scroll_region_down(param_or(params, 0, 1)),
            '@' => self.insert_chars(param_or(params, 0, 1)),
            'b' => self.repeat_char(param_or(params, 0, 1)),
            'J' => self.erase_display(param_or(params, 0, 0)),
            'K' => self.erase_line(param_or(params, 0, 0)),
            'L' => self.insert_lines(param_or(params, 0, 1)),
            'M' => self.delete_lines(param_or(params, 0, 1)),
            'P' => self.delete_chars(param_or(params, 0, 1)),
            'X' => self.erase_chars(param_or(params, 0, 1)),
            'c' => self.device_attributes(params, intermediates),
            'd' => self.move_to_origin(param_or(params, 0, 1), self.cursor.column + 1),
            'g' => self.clear_tab_stop(param_or(params, 0, 0)),
            'h' | 'l' => self.set_cursor_mode(params, intermediates, action),
            'm' => self.apply_sgr(params),
            'p' if intermediates == b"!" => self.soft_reset(),
            'r' => self.set_scroll_region(params),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }

        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'M' => self.reverse_index(),
            b'c' => self.hard_reset(),
            b'H' => self.set_tab_stop(),
            _ => {}
        }
    }
}

pub struct Terminal {
    parser: vte::Parser,
    screen: Screen,
}

impl Terminal {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            parser: vte::Parser::new(),
            screen: Screen::new(columns, rows),
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }

    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.screen.resize(columns, rows);
    }

    pub fn take_host_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.screen.host_output)
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.screen.bracketed_paste_enabled()
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn snapshot(&self) -> Snapshot {
        self.screen.snapshot()
    }

    /// Snapshot the visible grid at a scrollback viewport `offset_rows` (0 ==
    /// live screen). See [`Screen::snapshot_with_scrollback`] for the offset,
    /// clamping, cursor, and alternate-screen policy.
    pub fn snapshot_with_scrollback(&self, offset_rows: usize) -> Snapshot {
        self.screen.snapshot_with_scrollback(offset_rows)
    }
}

fn blank_row(columns: usize) -> Line {
    Line::unwrapped(vec![Cell::blank(); columns])
}

fn blank_row_with_bg(columns: usize, background: Color) -> Line {
    Line::unwrapped(vec![Cell::blank_with_bg(background); columns])
}

fn default_tab_stops(columns: usize) -> Vec<bool> {
    let mut stops = vec![false; columns];
    for column in (8..columns).step_by(8) {
        stops[column] = true;
    }
    stops
}

/// Repair wide-character pairs broken by a row-local shift (ICH/DCH). A
/// wide glyph occupies a lead cell plus a `wide_continuation` spacer; shifting
/// can orphan either half. Blank any continuation cell whose lead is missing,
/// and any wide lead whose continuation slot no longer carries the flag
/// (including a wide lead shifted into the last column with no room to follow).
fn sanitize_wide_row(row: &mut [Cell], blank: Cell) {
    let columns = row.len();
    for index in 0..columns {
        if row[index].wide_continuation {
            let lead_ok = index > 0
                && !row[index - 1].wide_continuation
                && UnicodeWidthChar::width(row[index - 1].ch) == Some(2);
            if !lead_ok {
                row[index] = blank;
            }
        } else if UnicodeWidthChar::width(row[index].ch) == Some(2) {
            let cont_ok = index + 1 < columns && row[index + 1].wide_continuation;
            if !cont_ok {
                row[index] = blank;
            }
        }
    }
}

fn resize_buffer_rows(
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
fn reflow_lines(
    scrollback: &mut Vec<Line>,
    rows: &mut Vec<Line>,
    dimensions: Dimensions,
    cursor: Position,
) -> Position {
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
    for (idx, line) in combined.iter().enumerate() {
        if idx == cursor_abs_row {
            current_cursor = Some(current.len() + cursor.column.min(line.cells.len()));
        }
        current.extend(line.cells.iter().copied());
        if !line.wrapped {
            logicals.push(LogicalLine {
                cells: std::mem::take(&mut current),
                cursor_offset: current_cursor.take(),
            });
        }
    }
    // Flush a trailing logical line whose last row was still marked wrapped.
    if !current.is_empty() || current_cursor.is_some() {
        logicals.push(LogicalLine {
            cells: current,
            cursor_offset: current_cursor,
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

    for logical in &logicals {
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
                    Cell {
                        ch: ' ',
                        attrs: cell.attrs,
                        wide_continuation: true,
                    }
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
        if cursor_target == Some(keep) {
            if !row_cells.is_empty() {
                // Partial final row: cursor sits just after the last char.
                cursor_dest = Some((new_combined.len(), row_cells.len().min(new_cols - 1)));
            } else if produced_any {
                // The content exactly filled the last (still-wrapped) row.
                cursor_dest = Some((new_combined.len() - 1, new_cols - 1));
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

    match cursor_dest {
        Some((abs_row, col)) => Position {
            row: abs_row.saturating_sub(visible_start).min(new_rows - 1),
            column: col.min(new_cols - 1),
        },
        None => Position {
            row: cursor.row.min(new_rows - 1),
            column: cursor.column.min(new_cols - 1),
        },
    }
}

fn clamp_scroll_region(
    region: Option<ScrollRegion>,
    dimensions: Dimensions,
) -> Option<ScrollRegion> {
    region.and_then(|region| {
        let top = region.top.min(dimensions.rows - 1);
        let bottom = region.bottom.min(dimensions.rows - 1);
        (top < bottom).then_some(ScrollRegion { top, bottom })
    })
}

fn param_or(params: &Params, index: usize, default: usize) -> usize {
    params
        .iter()
        .nth(index)
        .and_then(|param| param.first())
        .copied()
        .map(usize::from)
        .unwrap_or(default)
}

fn private_mode_params(params: &Params) -> impl Iterator<Item = u16> + '_ {
    params.iter().filter_map(|param| param.first().copied())
}

fn sgr_codes(params: &Params) -> Vec<u16> {
    params
        .iter()
        .filter_map(|param| param.first().copied())
        .filter(|value| ![b'?' as u16, b'>' as u16, b'<' as u16, b'=' as u16].contains(value))
        .collect()
}

fn parse_extended_color(codes: &[u16]) -> Option<(Color, usize)> {
    match codes {
        [_, 5, index, ..] => Some((Color::Indexed((*index).min(255) as u8), 3)),
        [_, 2, red, green, blue, ..] => Some((
            Color::Rgb(
                (*red).min(255) as u8,
                (*green).min(255) as u8,
                (*blue).min(255) as u8,
            ),
            5,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_blank_with_background(
        terminal: &Terminal,
        row: usize,
        column: usize,
        background: Color,
    ) {
        let cell = terminal.screen().cell(row, column).unwrap();
        assert_eq!(cell.ch, ' ');
        assert_eq!(
            cell.attrs,
            Attrs {
                background,
                ..Attrs::default()
            }
        );
        assert!(!cell.wide_continuation);
    }

    #[test]
    fn prints_plain_text_into_owned_grid() {
        let mut terminal = Terminal::new(10, 3);

        terminal.advance(b"hello\r\nody");

        assert_eq!(terminal.screen().plain_text(), "hello\nody\n");
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 3 });
    }

    #[test]
    fn applies_basic_sgr_attributes() {
        let mut terminal = Terminal::new(10, 2);

        terminal.advance(b"\x1b[1;31mR\x1b[0mN");

        let red = terminal.screen().cell(0, 0).unwrap();
        let normal = terminal.screen().cell(0, 1).unwrap();
        assert_eq!(red.ch, 'R');
        assert!(red.attrs.bold);
        assert_eq!(red.attrs.foreground, Color::Indexed(1));
        assert_eq!(normal.ch, 'N');
        assert_eq!(normal.attrs, Attrs::default());
    }

    #[test]
    fn responds_to_primary_device_attributes() {
        let mut terminal = Terminal::new(10, 2);

        terminal.advance(b"\x1b[c");

        assert_eq!(terminal.take_host_output(), b"\x1b[?1;2c");
        assert!(terminal.take_host_output().is_empty());
    }

    #[test]
    fn saves_and_restores_cursor_with_escape_sequences() {
        let mut terminal = Terminal::new(8, 2);

        terminal.advance(b"abc\x1b7XX\x1b8Z");

        assert_eq!(terminal.screen().plain_text(), "abcZX\n");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
    }

    #[test]
    fn saves_and_restores_cursor_with_csi_sequences() {
        let mut terminal = Terminal::new(8, 2);

        terminal.advance(b"abc\x1b[sXX\x1b[uZ");

        assert_eq!(terminal.screen().plain_text(), "abcZX\n");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
    }

    #[test]
    fn isolates_alternate_screen_from_primary_screen() {
        let mut terminal = Terminal::new(8, 3);

        terminal.advance(b"PRI\x1b[?1049hALT\x1b[?1049lMARY");

        assert_eq!(terminal.screen().plain_text(), "PRIMARY\n\n");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 7 });
    }

    #[test]
    fn scroll_region_scrolls_only_inside_margins() {
        let mut terminal = Terminal::new(8, 4);

        terminal.advance(b"top\r\none\r\ntwo\r\nbot");
        terminal.advance(b"\x1b[2;3r\x1b[3;1H\nX");

        assert_eq!(terminal.screen().plain_text(), "top\ntwo\nX\nbot");
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn tracks_bracketed_paste_mode() {
        let mut terminal = Terminal::new(8, 2);

        assert!(!terminal.bracketed_paste_enabled());

        terminal.advance(b"\x1b[?2004h");
        assert!(terminal.bracketed_paste_enabled());

        terminal.advance(b"\x1b[?2004l");
        assert!(!terminal.bracketed_paste_enabled());
    }

    #[test]
    fn applies_multiple_dec_private_modes_in_one_sequence() {
        let mut terminal = Terminal::new(8, 2);

        terminal.advance(b"\x1b[?25;2004l");

        assert!(!terminal.snapshot().cursor_visible);
        assert!(!terminal.bracketed_paste_enabled());

        terminal.advance(b"\x1b[?25;2004h");

        assert!(terminal.snapshot().cursor_visible);
        assert!(terminal.bracketed_paste_enabled());
    }

    #[test]
    fn line_feed_at_screen_bottom_outside_active_region_does_not_scroll_full_screen() {
        let mut terminal = Terminal::new(8, 4);

        terminal.advance(b"head\r\none\r\ntwo\r\nfoot");
        terminal.advance(b"\x1b[2;3r\x1b[4;1H\nZ");

        assert_eq!(terminal.screen().plain_text(), "head\none\ntwo\nZoot");
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn handles_cursor_movement_and_erase_line() {
        let mut terminal = Terminal::new(8, 2);

        terminal.advance(b"abcdef\x1b[3D\x1b[KZ");

        assert_eq!(terminal.screen().plain_text(), "abcZ\n");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
    }

    #[test]
    fn background_color_erase_applies_to_ed_el_and_ech() {
        let mut terminal = Terminal::new(6, 3);
        let red = Color::Indexed(1);

        terminal.advance(b"abcdef\r\nghijkl\r\nmnopqr");
        terminal.advance(b"\x1b[1;34;41m\x1b[2;3H\x1b[J");

        assert_eq!(terminal.screen().cell(1, 1).unwrap().ch, 'h');
        assert_blank_with_background(&terminal, 1, 2, red);
        assert_blank_with_background(&terminal, 2, 5, red);

        terminal.advance(b"\x1b[1;1H\x1b[K");
        assert_blank_with_background(&terminal, 0, 0, red);
        assert_blank_with_background(&terminal, 0, 5, red);

        terminal.advance(b"\x1b[2;1Hzzzzzz\x1b[2;2H\x1b[2X");
        assert_eq!(terminal.screen().plain_text(), "\nz  zzz\n");
        assert_blank_with_background(&terminal, 1, 1, red);
        assert_blank_with_background(&terminal, 1, 2, red);
    }

    #[test]
    fn background_color_erase_uses_default_after_sgr_49_and_reset() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance(b"\x1b[1;34;41mabcdef\x1b[49m\x1b[1;2H\x1b[K");
        assert_blank_with_background(&terminal, 0, 1, Color::Default);
        assert_blank_with_background(&terminal, 0, 5, Color::Default);

        terminal.advance(b"\x1b[41m\x1b[1;1H\x1b[0m\x1b[X");
        assert_blank_with_background(&terminal, 0, 0, Color::Default);
    }

    #[test]
    fn wraps_after_right_edge_on_next_printable() {
        let mut terminal = Terminal::new(5, 2);

        terminal.advance(b"abcdeF");

        assert_eq!(terminal.screen().plain_text(), "abcde\nF");
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 1 });
    }

    #[test]
    fn scrolls_at_bottom() {
        let mut terminal = Terminal::new(5, 2);

        terminal.advance(b"one\r\ntwo\r\nthree");

        assert_eq!(terminal.screen().plain_text(), "two\nthree");
        assert_eq!(terminal.screen().scrollback_len(), 1);
    }

    fn snapshot_rows(snapshot: &Snapshot) -> Vec<String> {
        let columns = snapshot.dimensions.columns;
        snapshot
            .cells
            .chunks(columns)
            .map(|row| {
                row.iter()
                    .filter(|cell| !cell.wide_continuation)
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn scrollback_snapshot_offset_zero_matches_live() {
        let mut terminal = Terminal::new(5, 2);
        terminal.advance(b"one\r\ntwo\r\nthree");

        // Offset 0 is byte-for-byte identical to the live snapshot, cursor and
        // visibility included.
        assert_eq!(
            terminal.snapshot_with_scrollback(0),
            terminal.snapshot(),
            "offset 0 must equal the live snapshot"
        );
    }

    #[test]
    fn scrollback_snapshot_mixes_scrollback_and_visible_rows() {
        let mut terminal = Terminal::new(5, 2);
        terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

        // Visible "three/four"; scrollback holds "one","two".
        assert_eq!(terminal.screen().scrollback_len(), 2);
        assert_eq!(
            snapshot_rows(&terminal.snapshot_with_scrollback(0)),
            ["three", "four"]
        );
        // Offset 1 pages up one row: scrollback "two" + visible "three".
        assert_eq!(
            snapshot_rows(&terminal.snapshot_with_scrollback(1)),
            ["two", "three"]
        );
        // Offset 2 reaches the oldest stored rows.
        assert_eq!(
            snapshot_rows(&terminal.snapshot_with_scrollback(2)),
            ["one", "two"]
        );
    }

    #[test]
    fn scrollback_snapshot_clamps_beyond_history() {
        let mut terminal = Terminal::new(5, 2);
        terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

        // Any offset past the available scrollback clamps to the oldest window.
        let clamped = terminal.snapshot_with_scrollback(999);
        assert_eq!(snapshot_rows(&clamped), ["one", "two"]);
        assert_eq!(clamped, terminal.snapshot_with_scrollback(2));
    }

    #[test]
    fn scrollback_snapshot_hides_cursor_when_scrolled() {
        let mut terminal = Terminal::new(5, 2);
        terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

        // Offset 0 keeps the live cursor visible; any scrolled-back offset hides
        // it because the cursor does not belong to the historical viewport.
        assert!(terminal.snapshot_with_scrollback(0).cursor_visible);
        assert!(!terminal.snapshot_with_scrollback(1).cursor_visible);
        assert!(!terminal.snapshot_with_scrollback(999).cursor_visible);
    }

    #[test]
    fn scrollback_snapshot_isolates_alternate_screen() {
        let mut terminal = Terminal::new(5, 2);
        // Build primary scrollback, then enter the alternate screen.
        terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(terminal.screen().scrollback_len(), 2);
        terminal.advance(b"\x1b[?1049h");

        // Alternate screen has no scrollback: every offset clamps to the live
        // alternate grid and primary history never leaks in.
        assert_eq!(terminal.screen().scrollback_len(), 0);
        let live = terminal.snapshot();
        assert_eq!(terminal.snapshot_with_scrollback(0), live);
        assert_eq!(terminal.snapshot_with_scrollback(5), live);
    }

    #[test]
    fn background_color_erase_applies_to_scroll_and_line_fills() {
        let mut terminal = Terminal::new(4, 3);

        terminal.advance(b"r0\r\nr1\r\nr2");
        terminal.advance(b"\x1b[42m\x1b[3;1H\n");
        assert_blank_with_background(&terminal, 2, 0, Color::Indexed(2));
        assert_blank_with_background(&terminal, 2, 3, Color::Indexed(2));

        terminal.advance(b"\x1b[43m\x1b[2;1H\x1b[L");
        assert_blank_with_background(&terminal, 1, 0, Color::Indexed(3));
        assert_blank_with_background(&terminal, 1, 3, Color::Indexed(3));

        terminal.advance(b"\x1b[44m\x1b[2;1H\x1b[M");
        assert_blank_with_background(&terminal, 2, 0, Color::Indexed(4));
        assert_blank_with_background(&terminal, 2, 3, Color::Indexed(4));
    }

    #[test]
    fn background_color_erase_applies_inside_scroll_regions() {
        let mut terminal = Terminal::new(4, 4);

        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
        terminal.advance(b"\x1b[45m\x1b[2;3r\x1b[3;1H\n");
        assert_blank_with_background(&terminal, 2, 0, Color::Indexed(5));
        assert_blank_with_background(&terminal, 2, 3, Color::Indexed(5));

        terminal.advance(b"\x1b[46m\x1b[2;1H\x1bM");
        assert_blank_with_background(&terminal, 1, 0, Color::Indexed(6));
        assert_blank_with_background(&terminal, 1, 3, Color::Indexed(6));
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn reverse_index_at_top_margin_scrolls_region_down() {
        let mut terminal = Terminal::new(8, 4);

        // Fill four rows, set region rows 2..=3 (1-based 2;3 -> top=1, bottom=2),
        // home into the region top, then RI to scroll the region down by one.
        terminal.advance(b"top\r\none\r\ntwo\r\nbot");
        terminal.advance(b"\x1b[2;3r"); // homes cursor to 1,1 (top-left)
        terminal.advance(b"\x1b[2;1H"); // move to region top (row index 1)
        terminal.advance(b"\x1bM"); // RI

        // Region (rows 1,2) scrolls down: blank inserted at top of region,
        // former bottom-of-region line discarded. Outside rows untouched.
        assert_eq!(terminal.screen().plain_text(), "top\n\none\nbot");
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn reverse_index_below_top_moves_cursor_up() {
        let mut terminal = Terminal::new(8, 3);

        terminal.advance(b"\x1b[3;1H"); // row index 2
        terminal.advance(b"\x1bM"); // RI moves cursor up, no scroll

        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
        assert_eq!(terminal.screen().plain_text(), "\n\n");
    }

    #[test]
    fn scroll_up_default_count_moves_content_up_one_line() {
        let mut terminal = Terminal::new(4, 4);
        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
        // SU with no param defaults to 1: every line shifts up one, blank at
        // the bottom, top line discarded. No scrollback pollution.
        terminal.advance(b"\x1b[S");
        assert_eq!(terminal.screen().plain_text(), "r1\nr2\nr3\n");
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn scroll_up_explicit_count_and_clamp() {
        let mut terminal = Terminal::new(4, 3);
        terminal.advance(b"r0\r\nr1\r\nr2");
        // SU 2 shifts content up two lines.
        terminal.advance(b"\x1b[2S");
        assert_eq!(terminal.screen().plain_text(), "r2\n\n");
        // SU with a count past the screen height clamps to a full clear.
        terminal.advance(b"\x1b[99S");
        assert_eq!(terminal.screen().plain_text(), "\n\n");
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn scroll_down_default_count_moves_content_down_one_line() {
        let mut terminal = Terminal::new(4, 4);
        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
        // SD with no param defaults to 1: blank inserted at the top, bottom
        // line discarded.
        terminal.advance(b"\x1b[T");
        assert_eq!(terminal.screen().plain_text(), "\nr0\nr1\nr2");
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn scroll_up_and_down_respect_scroll_region() {
        let mut terminal = Terminal::new(4, 4);
        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
        // Region = rows index 1..=2 (1-based 2;3). SU 1 scrolls only inside it.
        terminal.advance(b"\x1b[2;3r");
        terminal.advance(b"\x1b[S");
        assert_eq!(terminal.screen().plain_text(), "r0\nr2\n\nr3");
        // SD 1 inside the same region pushes the region content back down.
        terminal.advance(b"\x1b[T");
        assert_eq!(terminal.screen().plain_text(), "r0\n\nr2\nr3");
        assert_eq!(terminal.screen().scrollback_len(), 0);
    }

    #[test]
    fn scroll_up_fill_rows_use_background_color() {
        let mut terminal = Terminal::new(4, 3);
        terminal.advance(b"r0\r\nr1\r\nr2");
        // BCE: SU's blank bottom row carries the active background color.
        terminal.advance(b"\x1b[41m\x1b[S");
        assert_blank_with_background(&terminal, 2, 0, Color::Indexed(1));
        assert_blank_with_background(&terminal, 2, 3, Color::Indexed(1));
    }

    #[test]
    fn scroll_down_fill_rows_use_background_color() {
        let mut terminal = Terminal::new(4, 3);
        terminal.advance(b"r0\r\nr1\r\nr2");
        // BCE: SD's blank top row carries the active background color.
        terminal.advance(b"\x1b[42m\x1b[T");
        assert_blank_with_background(&terminal, 0, 0, Color::Indexed(2));
        assert_blank_with_background(&terminal, 0, 3, Color::Indexed(2));
    }

    #[test]
    fn origin_mode_makes_cup_relative_to_region_top() {
        let mut terminal = Terminal::new(8, 6);
        // Region rows index 2..=4 (1-based 3;5), enable DECOM.
        terminal.advance(b"\x1b[3;5r\x1b[?6h");
        // After DECOM enable the cursor homes to the region top (row index 2).
        assert_eq!(terminal.screen().cursor().row, 2);
        // CUP row 1 addresses the region top, not the screen top.
        terminal.advance(b"\x1b[1;1H");
        assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
        // CUP row 2 is the second row of the region (screen index 3).
        terminal.advance(b"\x1b[2;1H");
        assert_eq!(terminal.screen().cursor(), Position { row: 3, column: 0 });
    }

    #[test]
    fn origin_mode_clamps_cup_to_region_bottom() {
        let mut terminal = Terminal::new(8, 6);
        terminal.advance(b"\x1b[3;5r\x1b[?6h");
        // CUP far past the region bottom clamps to the region bottom (index 4),
        // never escaping into rows below the region.
        terminal.advance(b"\x1b[99;1H");
        assert_eq!(terminal.screen().cursor().row, 4);
    }

    #[test]
    fn origin_mode_off_addresses_full_screen() {
        let mut terminal = Terminal::new(8, 6);
        terminal.advance(b"\x1b[3;5r"); // region set, DECOM off (default)
        // With DECOM off, CUP row 1 is the screen top regardless of the region.
        terminal.advance(b"\x1b[1;1H");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
        // And the cursor can address rows outside the region.
        terminal.advance(b"\x1b[6;1H");
        assert_eq!(terminal.screen().cursor().row, 5);
    }

    #[test]
    fn origin_mode_disable_homes_to_screen_top() {
        let mut terminal = Terminal::new(8, 6);
        terminal.advance(b"\x1b[3;5r\x1b[?6h");
        assert_eq!(terminal.screen().cursor().row, 2);
        // Disabling DECOM homes back to the screen top-left.
        terminal.advance(b"\x1b[?6l");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    }

    #[test]
    fn origin_mode_applies_to_vpa() {
        let mut terminal = Terminal::new(8, 6);
        terminal.advance(b"\x1b[3;5r\x1b[?6h");
        // VPA (CSI Ps d) row 2 is region-relative under DECOM (screen index 3).
        terminal.advance(b"\x1b[2d");
        assert_eq!(terminal.screen().cursor().row, 3);
    }

    #[test]
    fn origin_mode_reset_by_ris_and_decstr() {
        // RIS clears DECOM.
        let mut terminal = Terminal::new(8, 6);
        terminal.advance(b"\x1b[3;5r\x1b[?6h\x1bc");
        terminal.advance(b"\x1b[1;1H");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

        // DECSTR (soft reset) also clears DECOM.
        let mut terminal = Terminal::new(8, 6);
        terminal.advance(b"\x1b[3;5r\x1b[?6h\x1b[!p");
        terminal.advance(b"\x1b[1;1H");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    }

    #[test]
    fn decstbm_homes_to_region_top_under_origin_mode() {
        let mut terminal = Terminal::new(8, 6);
        // Enable DECOM first, then set a new region: DECSTBM homes to the new
        // region's top (index 1) rather than the screen top.
        terminal.advance(b"\x1b[?6h\x1b[2;4r");
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
    }

    #[test]
    fn insert_lines_within_region_preserves_outside_rows() {
        let mut terminal = Terminal::new(8, 5);

        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3\r\nr4");
        terminal.advance(b"\x1b[2;4r"); // region rows index 1..=3
        terminal.advance(b"\x1b[3;1H"); // cursor at row index 2 (inside region)
        terminal.advance(b"\x1b[L"); // IL 1

        // Blank inserted at row 2; rows 2..3 shift down; region bottom (r3) lost.
        // Rows 0 and 4 (outside region) untouched. No scrollback pollution.
        assert_eq!(terminal.screen().plain_text(), "r0\nr1\n\nr2\nr4");
        assert_eq!(terminal.screen().scrollback_len(), 0);
        assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
    }

    #[test]
    fn delete_lines_within_region_preserves_outside_rows() {
        let mut terminal = Terminal::new(8, 5);

        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3\r\nr4");
        terminal.advance(b"\x1b[2;4r"); // region rows index 1..=3
        terminal.advance(b"\x1b[2;1H"); // cursor at row index 1 (region top)
        terminal.advance(b"\x1b[M"); // DL 1

        // r1 deleted; r2,r3 shift up; blank fills region bottom (row 3).
        // Rows 0 and 4 (outside region) untouched.
        assert_eq!(terminal.screen().plain_text(), "r0\nr2\nr3\n\nr4");
        assert_eq!(terminal.screen().scrollback_len(), 0);
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
    }

    #[test]
    fn insert_and_delete_lines_outside_region_are_noops() {
        let mut terminal = Terminal::new(8, 4);

        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
        terminal.advance(b"\x1b[2;3r"); // region rows index 1..=2
        terminal.advance(b"\x1b[4;1H"); // cursor at row index 3 (outside region)
        terminal.advance(b"\x1b[L"); // IL -> no-op
        terminal.advance(b"\x1b[M"); // DL -> no-op

        assert_eq!(terminal.screen().plain_text(), "r0\nr1\nr2\nr3");
    }

    // ICH (CSI Ps @) / DCH (CSI Ps P): row-local insert/delete of cells. Baseline
    // verified against xterm/Ghostty — cursor stays put, no wrap/scroll, shifted
    // cells keep their attrs, fill blanks use the current background color and
    // otherwise default attributes.
    #[test]
    fn insert_chars_shifts_right_and_keeps_cursor() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance(b"abcdef");
        terminal.advance(b"\x1b[1;3H"); // cursor at column index 2 (the 'c')
        terminal.advance(b"\x1b[2@"); // ICH 2

        // "ab" + 2 blanks + "cd"; "ef" pushed off the right edge are discarded.
        assert_eq!(terminal.screen().plain_text(), "ab  cd");
        // Cursor unchanged.
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
    }

    #[test]
    fn delete_chars_shifts_left_and_keeps_cursor() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance(b"abcdef");
        terminal.advance(b"\x1b[1;2H"); // cursor at column index 1 (the 'b')
        terminal.advance(b"\x1b[2P"); // DCH 2

        // "a" + "def" shifted left + 2 blanks at the right edge.
        assert_eq!(terminal.screen().plain_text(), "adef");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
    }

    #[test]
    fn insert_and_delete_chars_default_and_zero_count_is_one() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance(b"abcdef");
        terminal.advance(b"\x1b[1;1H");
        terminal.advance(b"\x1b[@"); // ICH, omitted count -> 1
        assert_eq!(terminal.screen().plain_text(), " abcde");

        terminal.advance(b"\x1b[1;1H");
        terminal.advance(b"\x1b[0P"); // DCH, zero count -> 1
        assert_eq!(terminal.screen().plain_text(), "abcde");
    }

    #[test]
    fn insert_chars_count_clamps_to_remaining_columns() {
        let mut terminal = Terminal::new(5, 1);

        terminal.advance(b"abcde");
        terminal.advance(b"\x1b[1;3H"); // column index 2
        terminal.advance(b"\x1b[99@"); // ICH count far exceeds remaining 3 columns

        // Everything from the cursor is blanked; "ab" preserved.
        assert_eq!(terminal.screen().plain_text(), "ab");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
    }

    #[test]
    fn insert_and_delete_chars_fill_with_current_background() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance(b"abcdef\x1b[42m\x1b[1;3H\x1b[2@");
        assert_eq!(terminal.screen().plain_text(), "ab  cd");
        assert_blank_with_background(&terminal, 0, 2, Color::Indexed(2));
        assert_blank_with_background(&terminal, 0, 3, Color::Indexed(2));

        terminal.advance(b"\x1b[43m\x1b[1;2H\x1b[2P");
        assert_blank_with_background(&terminal, 0, 4, Color::Indexed(3));
        assert_blank_with_background(&terminal, 0, 5, Color::Indexed(3));
    }

    #[test]
    fn delete_chars_preserves_attrs_of_shifted_cells() {
        let mut terminal = Terminal::new(6, 1);

        // 'a' plain, then bold-red "XY", then plain 'z'.
        terminal.advance(b"a\x1b[1;31mXY\x1b[0mz");
        terminal.advance(b"\x1b[1;1H"); // cursor at 'a'
        terminal.advance(b"\x1b[1P"); // DCH 1 -> delete 'a', shift left

        assert_eq!(terminal.screen().plain_text(), "XYz");
        // Shifted X/Y keep their bold-red attrs.
        let x = terminal.screen().cell(0, 0).unwrap();
        assert_eq!(x.ch, 'X');
        assert!(x.attrs.bold);
        assert_eq!(x.attrs.foreground, Color::Indexed(1));
    }

    #[test]
    fn delete_chars_cleans_up_orphaned_wide_continuation() {
        let mut terminal = Terminal::new(6, 1);

        // Wide glyph occupies cols 0-1 (lead + continuation), then "ab".
        terminal.advance("世ab".as_bytes());
        // Sanity: continuation spacer present at col 1.
        assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);

        terminal.advance(b"\x1b[1;1H"); // cursor at the wide lead
        terminal.advance(b"\x1b[1P"); // DCH 1 -> remove the lead

        // DCH removes ONE cell (the wide lead). Its continuation spacer shifts
        // into col 0 and is cleaned to a blank in place — so a single leading
        // blank remains, then "ab". The orphaned continuation must NOT survive
        // as a dangling spacer. plain_text only trims trailing space, so the
        // leading blank is retained.
        let plain = terminal.screen().plain_text();
        assert_eq!(plain, " ab");
        // No cell still flagged as a wide continuation.
        assert!(
            (0..6).all(|c| !terminal.screen().cell(0, c).unwrap().wide_continuation),
            "no orphaned wide-continuation cells should remain"
        );
    }

    // ECH (CSI Ps X): row-local erase-in-place. Unlike DCH it does NOT shift the
    // line — it overwrites count cells with BCE blanks. Cursor stays put,
    // pending_wrap clears, count clamps to the row tail.
    #[test]
    fn erase_chars_blanks_in_place_without_shifting() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance(b"abcdef");
        terminal.advance(b"\x1b[1;2H"); // cursor at column index 1 (the 'b')
        terminal.advance(b"\x1b[2X"); // ECH 2 -> erase 'b','c' in place

        // No shift: "a" + 2 blanks + "def".
        assert_eq!(terminal.screen().plain_text(), "a  def");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
    }

    #[test]
    fn erase_chars_default_and_zero_count_is_one() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance(b"abcdef");
        terminal.advance(b"\x1b[1;1H");
        terminal.advance(b"\x1b[X"); // omitted count -> 1
        assert_eq!(terminal.screen().plain_text(), " bcdef");

        terminal.advance(b"\x1b[1;3H");
        terminal.advance(b"\x1b[0X"); // zero count -> 1
        assert_eq!(terminal.screen().plain_text(), " b def");
    }

    #[test]
    fn erase_chars_count_clamps_to_row_tail() {
        let mut terminal = Terminal::new(5, 1);

        terminal.advance(b"abcde");
        terminal.advance(b"\x1b[1;3H"); // column index 2
        terminal.advance(b"\x1b[99X"); // far exceeds remaining 3 columns

        // Erases from cursor to end of row; "ab" preserved, cursor unchanged.
        assert_eq!(terminal.screen().plain_text(), "ab");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
    }

    #[test]
    fn erase_chars_is_row_local_and_resets_attrs() {
        let mut terminal = Terminal::new(6, 2);

        // Row 0: plain 'a' then bold-red "bc". Row 1: "xyz" (must stay intact).
        terminal.advance(b"a\x1b[1;31mbc\x1b[0m\r\nxyz");
        terminal.advance(b"\x1b[1;2H"); // back to row 0, column index 1 ('b')
        terminal.advance(b"\x1b[2X"); // erase the bold-red 'b','c'

        // Row 1 untouched (row-local).
        assert_eq!(terminal.screen().plain_text(), "a\nxyz");
        // Erased cells carry DEFAULT attrs, not the prior bold-red.
        let erased = terminal.screen().cell(0, 1).unwrap();
        assert_eq!(erased.ch, ' ');
        assert_eq!(erased.attrs, Attrs::default());
    }

    #[test]
    fn erase_chars_clears_pending_wrap() {
        let mut terminal = Terminal::new(4, 2);

        // Fill the row to arm pending_wrap at the right edge.
        terminal.advance(b"abcd");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 3 });

        terminal.advance(b"\x1b[1X"); // ECH clears pending_wrap

        // Because pending_wrap was cleared, the next printable overwrites the
        // last column on THIS row instead of wrapping to row 1. Z lands at
        // column 3 (the cursor re-caps at columns-1 and re-arms pending_wrap);
        // crucially it stays on row 0.
        terminal.advance(b"Z");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 3 });
        // Row 1 is empty (Z did not wrap there); plain_text joins both rows.
        assert_eq!(terminal.screen().plain_text(), "abcZ\n");
    }

    #[test]
    fn erase_chars_cleans_up_orphaned_wide_continuation() {
        let mut terminal = Terminal::new(6, 1);

        // Wide glyph at cols 0-1 (lead + continuation), then "ab".
        terminal.advance("世ab".as_bytes());
        assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);

        terminal.advance(b"\x1b[1;1H"); // cursor at the wide lead
        terminal.advance(b"\x1b[1X"); // ECH 1 -> erase the lead in place

        // Erasing only the lead orphans the continuation spacer at col 1; it
        // must be cleaned to a blank, not left dangling. "ab" stays in place.
        assert_eq!(terminal.screen().plain_text(), "  ab");
        assert!(
            (0..6).all(|c| !terminal.screen().cell(0, c).unwrap().wide_continuation),
            "no orphaned wide-continuation cells should remain"
        );
    }

    // REP (CSI Ps b): repeat the last printed graphic char N times through normal
    // print processing. Baseline: repeats carry the CURRENT SGR attrs and obey
    // autowrap, exactly as if the char were typed again; omitted/zero count = 1;
    // no-op when nothing graphic has been printed.
    #[test]
    fn repeat_char_repeats_last_graphic() {
        let mut terminal = Terminal::new(8, 1);

        terminal.advance(b"a\x1b[3b"); // print 'a', then REP 3

        // One original + three repeats = four 'a'.
        assert_eq!(terminal.screen().plain_text(), "aaaa");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
    }

    #[test]
    fn repeat_char_default_and_zero_count_is_one() {
        let mut terminal = Terminal::new(8, 1);

        terminal.advance(b"x\x1bb"); // not a CSI; ensure only real REP counts
        // (ESC b is not REP; nothing should repeat from it.)
        assert_eq!(terminal.screen().plain_text(), "x");

        terminal.advance(b"\x1b[b"); // REP omitted -> 1
        assert_eq!(terminal.screen().plain_text(), "xx");

        terminal.advance(b"\x1b[0b"); // REP 0 -> 1
        assert_eq!(terminal.screen().plain_text(), "xxx");
    }

    #[test]
    fn repeat_char_is_noop_without_preceding_graphic() {
        let mut terminal = Terminal::new(8, 1);

        terminal.advance(b"\x1b[5b"); // REP before any printable char

        assert_eq!(terminal.screen().plain_text(), "");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    }

    #[test]
    fn repeat_char_preserves_current_attrs() {
        let mut terminal = Terminal::new(8, 1);

        // REP reprints the previous graphic char through normal print handling,
        // so it uses CURRENT SGR attrs rather than the original cell attrs.
        terminal.advance(b"\x1b[1;31mr\x1b[0m\x1b[2b");

        let original = terminal.screen().cell(0, 0).unwrap();
        assert_eq!(original.ch, 'r');
        assert!(original.attrs.bold);
        assert_eq!(original.attrs.foreground, Color::Indexed(1));

        for column in 1..3 {
            let repeated = terminal.screen().cell(0, column).unwrap();
            assert_eq!(repeated.ch, 'r');
            assert_eq!(repeated.attrs, Attrs::default());
        }
    }

    #[test]
    fn repeat_char_is_reset_by_ris_and_decstr() {
        let mut terminal = Terminal::new(8, 2);

        terminal.advance(b"a\x1bc\x1b[3b");
        assert_eq!(terminal.screen().plain_text(), "\n");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

        terminal.advance(b"b\x1b[!p\x1b[3b");
        assert_eq!(terminal.screen().plain_text(), "b\n");
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    }

    #[test]
    fn repeat_char_obeys_autowrap() {
        let mut terminal = Terminal::new(3, 2);

        terminal.advance(b"a\x1b[3b"); // 'a' then REP 3 -> 4 'a' total across wrap

        // Row 0 fills to width 3; the 4th 'a' wraps onto row 1.
        assert_eq!(terminal.screen().plain_text(), "aaa\na");
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 1 });
    }

    #[test]
    fn repeat_char_repeats_wide_glyph() {
        let mut terminal = Terminal::new(6, 1);

        terminal.advance("世".as_bytes()); // wide lead + continuation
        terminal.advance(b"\x1b[1b"); // REP 1 -> a second wide glyph

        // Policy (documented): REP replays a wide last char as a full wide glyph.
        assert_eq!(terminal.screen().plain_text(), "世世");
        assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);
        assert!(terminal.screen().cell(0, 3).unwrap().wide_continuation);
    }

    // Tab stops (HT / HTS / TBC): owned every-8 default model. HT advances to
    // the next stop right of the cursor or clamps to the right edge; HTS (ESC H)
    // sets a stop at the current column; TBC (CSI Ps g) clears current (0) or
    // all (3). Reset policy: RIS restores defaults; DECSTR preserves stops
    // (VT220 soft-reset definition). Resize preserves retained stops and
    // default-fills newly exposed columns.

    // Helper: column the cursor lands on after a single HT from `start`.
    fn tab_to(terminal: &mut Terminal, start: usize) -> usize {
        terminal.advance(format!("\x1b[1;{}H", start + 1).as_bytes());
        terminal.advance(b"\t");
        terminal.screen().cursor().column
    }

    #[test]
    fn default_tab_stops_advance_every_eight() {
        let mut terminal = Terminal::new(40, 1);

        assert_eq!(tab_to(&mut terminal, 0), 8);
        assert_eq!(tab_to(&mut terminal, 7), 8);
        assert_eq!(tab_to(&mut terminal, 8), 16);
        assert_eq!(tab_to(&mut terminal, 15), 16);
        assert_eq!(tab_to(&mut terminal, 23), 24);
    }

    #[test]
    fn tab_clamps_to_right_edge_when_no_later_stop() {
        let mut terminal = Terminal::new(12, 1);

        // Width 12: default stop at col 8 only. From col 9 there is no later
        // stop, so HT clamps to the right edge (col 11).
        assert_eq!(tab_to(&mut terminal, 9), 11);
        // From the right edge, HT stays clamped.
        assert_eq!(tab_to(&mut terminal, 11), 11);
    }

    #[test]
    fn hts_sets_custom_tab_stop() {
        let mut terminal = Terminal::new(20, 1);

        // Set a custom stop at column 3 via HTS.
        terminal.advance(b"\x1b[1;4H"); // move to column index 3
        terminal.advance(b"\x1bH"); // HTS at column 3

        // From column 0, HT now lands on the new stop at 3 (before the default 8).
        assert_eq!(tab_to(&mut terminal, 0), 3);
        // From column 3, HT advances to the default stop at 8.
        assert_eq!(tab_to(&mut terminal, 3), 8);
    }

    #[test]
    fn tbc_clears_current_tab_stop() {
        let mut terminal = Terminal::new(20, 1);

        // Clear the default stop at column 8.
        terminal.advance(b"\x1b[1;9H"); // column index 8
        terminal.advance(b"\x1b[0g"); // TBC current column

        // From column 0, HT now skips the cleared 8 and lands on the next
        // default stop at 16.
        assert_eq!(tab_to(&mut terminal, 0), 16);
    }

    #[test]
    fn tbc_clears_all_tab_stops() {
        let mut terminal = Terminal::new(20, 1);

        terminal.advance(b"\x1b[3g"); // TBC clear all

        // With no stops anywhere, HT from column 0 clamps to the right edge.
        assert_eq!(tab_to(&mut terminal, 0), 19);
    }

    #[test]
    fn ris_restores_default_tab_stops_decstr_preserves() {
        let mut terminal = Terminal::new(20, 2);

        // Wipe all stops, then confirm HT clamps.
        terminal.advance(b"\x1b[3g");
        assert_eq!(tab_to(&mut terminal, 0), 19);

        // DECSTR (soft reset) PRESERVES the (now empty) tab-stop table.
        terminal.advance(b"\x1b[!p");
        assert_eq!(tab_to(&mut terminal, 0), 19);

        // RIS (hard reset) RESTORES the default every-8 stops.
        terminal.advance(b"\x1bc");
        assert_eq!(tab_to(&mut terminal, 0), 8);
    }

    #[test]
    fn resize_preserves_stops_and_default_fills_growth() {
        let mut terminal = Terminal::new(10, 1);

        // Custom stop at column 3; default stop at 8 also present.
        terminal.advance(b"\x1b[1;4H\x1bH");

        // Grow to 24 columns: retained stops (3, 8) preserved; new columns get
        // default stops (16).
        terminal.resize(24, 1);
        assert_eq!(tab_to(&mut terminal, 0), 3); // custom stop retained
        assert_eq!(tab_to(&mut terminal, 3), 8); // default retained
        assert_eq!(tab_to(&mut terminal, 8), 16); // default-filled on growth

        // Shrink to 6 columns: stops beyond width are dropped; the custom 3
        // remains, and HT past it clamps to the new right edge (col 5).
        terminal.resize(6, 1);
        assert_eq!(tab_to(&mut terminal, 0), 3);
        assert_eq!(tab_to(&mut terminal, 3), 5);
    }

    // --- Resize reflow (shrink/grow content preservation) ---

    /// Visible text with trailing blank rows (fixed-height grid padding) removed,
    /// so reflow assertions focus on content rather than grid height.
    fn visible_text(terminal: &Terminal) -> String {
        terminal
            .screen()
            .plain_text()
            .trim_end_matches('\n')
            .to_string()
    }

    #[test]
    fn reflow_shrink_then_grow_recovers_wide_line() {
        // Operator bug: text that disappears into a narrowed window must
        // reappear when widened again. A 30-char line on a 20-wide grid wraps;
        // shrinking to 10 re-wraps it; widening to 40 must rejoin it intact.
        let mut terminal = Terminal::new(20, 3);
        let line = "abcdefghijklmnopqrstuvwxyz0123"; // 30 chars
        terminal.advance(line.as_bytes());

        // Width 20: soft-wrapped across two rows.
        assert_eq!(visible_text(&terminal), "abcdefghijklmnopqrst\nuvwxyz0123");

        // Shrink to 10: the logical line re-wraps to three full rows.
        terminal.resize(10, 3);
        assert_eq!(
            visible_text(&terminal),
            "abcdefghij\nklmnopqrst\nuvwxyz0123"
        );

        // Grow to 40: the soft-wrapped rows rejoin into the original line.
        terminal.resize(40, 3);
        assert_eq!(visible_text(&terminal), line);
    }

    #[test]
    fn reflow_preserves_content_through_scrollback_roundtrip() {
        // When the reflowed line is taller than the visible window, the overflow
        // goes to scrollback and is still recovered on widening.
        let mut terminal = Terminal::new(20, 2);
        let line = "abcdefghijklmnopqrstuvwxyz0123"; // 30 chars
        terminal.advance(line.as_bytes());

        // Shrink to 10 (3 rows of content, only 2 visible): top row spills into
        // scrollback rather than being truncated.
        terminal.resize(10, 2);
        assert_eq!(terminal.screen().scrollback_len(), 1);
        assert_eq!(visible_text(&terminal), "klmnopqrst\nuvwxyz0123");

        // Grow to 40: scrollback + visible rejoin into the original line.
        terminal.resize(40, 2);
        assert_eq!(terminal.screen().scrollback_len(), 0);
        assert_eq!(visible_text(&terminal), line);
    }

    #[test]
    fn reflow_does_not_join_hard_newlines() {
        // Hard line breaks (explicit newlines) must never be merged by reflow,
        // even when both lines would fit on one row at the new width.
        let mut terminal = Terminal::new(20, 3);
        terminal.advance(b"foo\r\nbar");

        terminal.resize(3, 3);
        assert_eq!(visible_text(&terminal), "foo\nbar");

        terminal.resize(20, 3);
        // Stays two separate lines, not "foobar".
        assert_eq!(visible_text(&terminal), "foo\nbar");
    }

    #[test]
    fn reflow_keeps_cursor_on_its_character() {
        // The cursor must follow its logical character through a re-wrap so an
        // active prompt stays put.
        let mut terminal = Terminal::new(20, 3);
        terminal.advance(b"$ hello"); // cursor at col 7, row 0
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 7 });

        // Shrink to 4: "$ hello" wraps to "$ he" / "llo"; the cursor sits just
        // past the last char on the second wrapped row.
        terminal.resize(4, 3);
        let cursor = terminal.screen().cursor();
        assert_eq!(cursor, Position { row: 1, column: 3 });

        // Typing continues the same logical line from the cursor; widening
        // rejoins it into the expected text.
        terminal.advance(b"!");
        terminal.resize(20, 3);
        assert_eq!(visible_text(&terminal), "$ hello!");
    }

    #[test]
    fn reflow_grow_then_shrink_is_stable_for_short_lines() {
        // Lines that always fit are unaffected by reflow (no spurious joins or
        // blank bloat) across repeated resizes.
        let mut terminal = Terminal::new(10, 3);
        terminal.advance(b"a\r\nb\r\nc");
        let before = visible_text(&terminal);
        assert_eq!(before, "a\nb\nc");

        terminal.resize(40, 3);
        assert_eq!(visible_text(&terminal), "a\nb\nc");
        terminal.resize(5, 3);
        assert_eq!(visible_text(&terminal), "a\nb\nc");
        terminal.resize(10, 3);
        assert_eq!(visible_text(&terminal), "a\nb\nc");
    }

    #[test]
    fn reflow_does_not_touch_alternate_screen_but_isolates_it() {
        // The alternate screen does not reflow (apps repaint), keeps no
        // scrollback, and primary history never leaks into it. Leaving the
        // alternate screen after a resize shows the reflowed primary content.
        let mut terminal = Terminal::new(20, 3);
        let line = "abcdefghijklmnopqrstuvwxyz0123"; // 30 chars, wraps at 20
        terminal.advance(line.as_bytes());

        // Enter the alternate screen and draw app content.
        terminal.advance(b"\x1b[?1049h");
        terminal.advance(b"TUI");
        assert_eq!(terminal.screen().scrollback_len(), 0);

        // Resize while in the alternate screen: alt grid is truncated/padded
        // (no scrollback growth), and its content is preserved within bounds.
        terminal.resize(10, 3);
        assert_eq!(terminal.screen().scrollback_len(), 0);
        assert!(terminal.screen().plain_text().contains("TUI"));

        // Leave the alternate screen: the reflowed primary line is intact at the
        // new width (re-wrapped to 10).
        terminal.advance(b"\x1b[?1049l");
        terminal.resize(40, 3);
        assert_eq!(visible_text(&terminal), line);
    }

    // Baseline: xterm, Ghostty, and xterm.js all specify that IL (CSI L) and
    // DL (CSI M) move the cursor to the left margin (column 0) and unset the
    // pending wrap state. These fixtures start the cursor at a NONZERO column
    // to prove the column-reset policy (a column-preserving impl would fail
    // them). RI (ESC M), by contrast, preserves the column — see
    // reverse_index_preserves_cursor_column.
    #[test]
    fn insert_lines_resets_cursor_to_left_margin() {
        let mut terminal = Terminal::new(8, 4);

        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
        terminal.advance(b"\x1b[2;5H"); // row index 1, column index 4 (nonzero)
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 4 });

        terminal.advance(b"\x1b[L"); // IL 1

        // Cursor homed to the left margin of the current row.
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
    }

    #[test]
    fn delete_lines_resets_cursor_to_left_margin() {
        let mut terminal = Terminal::new(8, 4);

        terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
        terminal.advance(b"\x1b[3;6H"); // row index 2, column index 5 (nonzero)
        assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 5 });

        terminal.advance(b"\x1b[M"); // DL 1

        assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
    }

    #[test]
    fn insert_lines_at_right_edge_clears_pending_wrap() {
        let mut terminal = Terminal::new(4, 3);

        // Print to the last column to arm pending_wrap, then IL. The column
        // resets to 0 and pending_wrap is cleared, so the next printable lands
        // at column 1 (not wrapped to a new row).
        terminal.advance(b"abcd"); // fills row 0, cursor parked at col 3, pending_wrap set
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 3 });

        terminal.advance(b"\x1b[L"); // IL at row 0
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

        terminal.advance(b"Z"); // lands at column 0 then advances to 1, no wrap
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
    }

    #[test]
    fn reverse_index_preserves_cursor_column() {
        let mut terminal = Terminal::new(8, 3);

        // RI is NOT IL/DL: it preserves the cursor column (only the row/scroll
        // changes). Start at a nonzero column below the top margin.
        terminal.advance(b"\x1b[3;6H"); // row index 2, column index 5
        terminal.advance(b"\x1bM"); // RI moves cursor up one row, column intact

        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 5 });
    }

    #[test]
    fn hard_reset_restores_power_on_state() {
        let mut terminal = Terminal::new(8, 3);

        // Dirty as much state as possible: scrollback, alt screen, margins,
        // saved cursor, attrs, bracketed paste, hidden cursor, pending DA reply.
        terminal.advance(b"a\r\nb\r\nc\r\nd"); // forces a scrollback line
        terminal.advance(b"\x1b[?2004h"); // bracketed paste on
        terminal.advance(b"\x1b[?25l"); // cursor hidden
        terminal.advance(b"\x1b[2;3r"); // scroll region
        terminal.advance(b"\x1b7"); // save cursor
        terminal.advance(b"\x1b[1;31m"); // bold red attrs
        terminal.advance(b"\x1b[?1049h"); // enter alt screen
        terminal.advance(b"\x1b[c"); // queue a primary DA reply in host_output

        terminal.advance(b"\x1bc"); // RIS

        assert_eq!(terminal.screen().plain_text(), "\n\n");
        assert_eq!(terminal.screen().scrollback_len(), 0);
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
        assert!(!terminal.bracketed_paste_enabled());
        assert!(terminal.take_host_output().is_empty());

        // Power-on attrs: text printed after RIS carries default attributes.
        terminal.advance(b"Z");
        let cell = terminal.screen().cell(0, 0).unwrap();
        assert_eq!(cell.ch, 'Z');
        assert_eq!(cell.attrs, Attrs::default());

        // Cursor visible again after reset (snapshot reflects it).
        assert!(terminal.snapshot().cursor_visible);

        // Scroll region cleared: a bottom-row newline now scrolls the whole
        // screen and feeds scrollback (region scroll would not).
        terminal.advance(b"\x1b[3;1H\n");
        assert_eq!(terminal.screen().scrollback_len(), 1);
    }

    #[test]
    fn soft_reset_keeps_cells_but_resets_modes() {
        let mut terminal = Terminal::new(8, 3);

        terminal.advance(b"old\r\nkeep\r\ntwo\r\nthree"); // visible content + scrollback
        assert_eq!(terminal.screen().scrollback_len(), 1);
        terminal.advance(b"\x1b[?2004h"); // bracketed paste on
        terminal.advance(b"\x1b[?25l"); // cursor hidden
        terminal.advance(b"\x1b[2;3r"); // scroll region
        terminal.advance(b"\x1b7"); // save cursor
        terminal.advance(b"\x1b[c"); // queue a primary DA reply in host_output

        terminal.advance(b"\x1b[!p"); // DECSTR soft reset

        // Visible cells and scrollback preserved (NOT cleared).
        assert_eq!(terminal.screen().plain_text(), "keep\ntwo\nthree");
        assert_eq!(terminal.screen().scrollback_len(), 1);

        // Modes reset.
        assert!(!terminal.bracketed_paste_enabled());
        assert!(terminal.snapshot().cursor_visible);
        assert!(terminal.take_host_output().is_empty());

        // Cursor policy: DECSTR homes the cursor to top-left (documented).
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

        // Saved cursor dropped: a restore after soft reset is a no-op, so the
        // cursor stays where it was moved rather than jumping to a stale save.
        terminal.advance(b"\x1b[2;5H"); // move to row 1, col 4
        terminal.advance(b"\x1b8"); // restore -> no saved cursor, no movement
        assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 4 });

        // Scroll region cleared by the soft reset.
        terminal.advance(b"\x1b[3;1H\n");
        assert_eq!(terminal.screen().scrollback_len(), 2);
    }
}
