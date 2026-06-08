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
        Self {
            ch: ' ',
            attrs: Attrs::default(),
            wide_continuation: false,
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
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
    rows: Vec<Vec<Cell>>,
    scrollback: Vec<Vec<Cell>>,
    cursor: Position,
    cursor_visible: bool,
    pending_wrap: bool,
    saved_cursor: Option<SavedCursor>,
    primary_screen: Option<StoredScreen>,
    scroll_region: Option<ScrollRegion>,
    bracketed_paste: bool,
    current_attrs: Attrs,
    dirty: DirtyRegion,
    host_output: Vec<u8>,
    last_graphic_char: Option<char>,
}

#[derive(Debug, Clone)]
struct StoredScreen {
    rows: Vec<Vec<Cell>>,
    scrollback: Vec<Vec<Cell>>,
    cursor: Position,
    pending_wrap: bool,
    saved_cursor: Option<SavedCursor>,
    scroll_region: Option<ScrollRegion>,
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
            bracketed_paste: false,
            current_attrs: Attrs::default(),
            dirty: DirtyRegion::Full,
            host_output: Vec::new(),
            last_graphic_char: None,
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

    pub fn resize(&mut self, columns: usize, rows: usize) {
        let dimensions = Dimensions::new(columns, rows);

        resize_buffer_rows(
            &mut self.rows,
            &mut self.scrollback,
            dimensions,
            self.primary_screen.is_some(),
        );

        self.dimensions = dimensions;
        self.cursor.row = self.cursor.row.min(self.dimensions.rows - 1);
        self.cursor.column = self.cursor.column.min(self.dimensions.columns - 1);
        self.pending_wrap = false;

        if let Some(primary) = &mut self.primary_screen {
            resize_buffer_rows(
                &mut primary.rows,
                &mut primary.scrollback,
                dimensions,
                false,
            );
            primary.cursor.row = primary.cursor.row.min(dimensions.rows - 1);
            primary.cursor.column = primary.cursor.column.min(dimensions.columns - 1);
            primary.pending_wrap = false;
            primary.scroll_region = clamp_scroll_region(primary.scroll_region, dimensions);
        }

        self.scroll_region = clamp_scroll_region(self.scroll_region, dimensions);
        self.mark_dirty();
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            dimensions: self.dimensions,
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            cells: self.rows.iter().flatten().copied().collect(),
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
            self.carriage_return();
            self.line_feed();
            self.pending_wrap = false;
        }

        if self.cursor.column + width > self.dimensions.columns {
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
        let next_tab = ((self.cursor.column / 8) + 1) * 8;
        self.cursor.column = next_tab.min(self.dimensions.columns - 1);
        self.pending_wrap = false;
        self.mark_dirty();
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

        if self.primary_screen.is_none() {
            self.scrollback.push(removed);
        }

        self.rows.push(blank_row(self.dimensions.columns));
        self.mark_dirty();
    }

    fn scroll_up_region(&mut self) {
        if let Some(region) = self.scroll_region {
            self.rows.remove(region.top);
            self.rows
                .insert(region.bottom, blank_row(self.dimensions.columns));
            self.mark_dirty();
        }
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

        if self.cursor.row == top {
            self.rows.remove(bottom);
            self.rows.insert(top, blank_row(self.dimensions.columns));
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
        for _ in 0..count {
            self.rows.remove(bottom);
            self.rows
                .insert(self.cursor.row, blank_row(self.dimensions.columns));
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
        for _ in 0..count {
            self.rows.remove(self.cursor.row);
            self.rows.insert(bottom, blank_row(self.dimensions.columns));
        }

        self.cursor.column = 0;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// ICH (CSI Ps @): insert `count` blank cells at the cursor, shifting the
    /// rest of the line right. Cells pushed past the right edge are discarded.
    /// Row-local: no wrap, no scroll, cursor stays in place. Fill blanks use
    /// the default-attribute erase policy (see `erase_line`), matching the rest
    /// of OdyTTY's erase handling; background-color-erase is a separate future
    /// change applied uniformly, not here.
    fn insert_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);

        let row = &mut self.rows[self.cursor.row];
        for _ in 0..count {
            row.insert(column, Cell::blank());
        }
        row.truncate(columns);

        sanitize_wide_row(row);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DCH (CSI Ps P): delete `count` cells at the cursor, shifting the rest of
    /// the line left and filling blanks at the right edge. Row-local: no wrap,
    /// no scroll, cursor stays in place. Fill blanks use the default-attribute
    /// erase policy, matching `erase_line`.
    fn delete_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);

        let row = &mut self.rows[self.cursor.row];
        for _ in 0..count {
            row.remove(column);
        }
        while row.len() < columns {
            row.push(Cell::blank());
        }

        sanitize_wide_row(row);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// ECH (CSI Ps X): erase `count` cells from the cursor in place, overwriting
    /// them with blanks WITHOUT shifting the rest of the line. Row-local: no
    /// wrap, no scroll, cursor stays put. Blanks use the default-attribute erase
    /// policy (Cell::blank()), uniform with erase_line/erase_display/ICH/DCH;
    /// BCE is not implemented.
    fn erase_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);

        let row = &mut self.rows[self.cursor.row];
        for cell in &mut row[column..column + count] {
            *cell = Cell::blank();
        }

        sanitize_wide_row(row);
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

    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line_from_cursor();
                for row in self.cursor.row + 1..self.dimensions.rows {
                    self.rows[row] = blank_row(self.dimensions.columns);
                }
            }
            1 => {
                for row in 0..self.cursor.row {
                    self.rows[row] = blank_row(self.dimensions.columns);
                }
                self.erase_line_to_cursor();
            }
            2 | 3 => {
                for row in &mut self.rows {
                    *row = blank_row(self.dimensions.columns);
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
            2 => self.rows[self.cursor.row] = blank_row(self.dimensions.columns),
            _ => {}
        }
        self.mark_dirty();
    }

    fn erase_line_from_cursor(&mut self) {
        for column in self.cursor.column..self.dimensions.columns {
            self.rows[self.cursor.row][column] = Cell::blank();
        }
        self.mark_dirty();
    }

    fn erase_line_to_cursor(&mut self) {
        for column in 0..=self.cursor.column {
            self.rows[self.cursor.row][column] = Cell::blank();
        }
        self.mark_dirty();
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
        };

        self.cursor = Position::default();
        self.pending_wrap = false;
        self.saved_cursor = None;
        self.scroll_region = None;
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

        self.move_to(1, 1);
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
        self.bracketed_paste = false;
        self.current_attrs = Attrs::default();
        self.host_output.clear();
        self.mark_dirty();
    }

    /// DECSTR (CSI ! p): soft reset. Resets modes and cursor state without
    /// touching the visible cells or scrollback. Cursor policy: homed to the
    /// top-left (documented in tests), matching xterm's DECSTR behaviour.
    fn soft_reset(&mut self) {
        self.cursor = Position::default();
        self.cursor_visible = true;
        self.pending_wrap = false;
        self.saved_cursor = None;
        self.scroll_region = None;
        self.bracketed_paste = false;
        self.current_attrs = Attrs::default();
        self.host_output.clear();
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
            'H' | 'f' => self.move_to(param_or(params, 0, 1), param_or(params, 1, 1)),
            '@' => self.insert_chars(param_or(params, 0, 1)),
            'b' => self.repeat_char(param_or(params, 0, 1)),
            'J' => self.erase_display(param_or(params, 0, 0)),
            'K' => self.erase_line(param_or(params, 0, 0)),
            'L' => self.insert_lines(param_or(params, 0, 1)),
            'M' => self.delete_lines(param_or(params, 0, 1)),
            'P' => self.delete_chars(param_or(params, 0, 1)),
            'X' => self.erase_chars(param_or(params, 0, 1)),
            'c' => self.device_attributes(params, intermediates),
            'd' => self.move_to(param_or(params, 0, 1), self.cursor.column + 1),
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
}

fn blank_row(columns: usize) -> Vec<Cell> {
    vec![Cell::blank(); columns]
}

/// Repair wide-character pairs broken by a row-local shift (ICH/DCH). A
/// wide glyph occupies a lead cell plus a `wide_continuation` spacer; shifting
/// can orphan either half. Blank any continuation cell whose lead is missing,
/// and any wide lead whose continuation slot no longer carries the flag
/// (including a wide lead shifted into the last column with no room to follow).
fn sanitize_wide_row(row: &mut [Cell]) {
    let columns = row.len();
    for index in 0..columns {
        if row[index].wide_continuation {
            let lead_ok = index > 0
                && !row[index - 1].wide_continuation
                && UnicodeWidthChar::width(row[index - 1].ch) == Some(2);
            if !lead_ok {
                row[index] = Cell::blank();
            }
        } else if UnicodeWidthChar::width(row[index].ch) == Some(2) {
            let cont_ok = index + 1 < columns && row[index + 1].wide_continuation;
            if !cont_ok {
                row[index] = Cell::blank();
            }
        }
    }
}

fn resize_buffer_rows(
    rows: &mut Vec<Vec<Cell>>,
    scrollback: &mut Vec<Vec<Cell>>,
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
    // cells keep their attrs, fill blanks use OdyTTY's default-attribute erase
    // policy (uniform with erase_line; bce is a separate future change).
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
    // line — it overwrites count cells with default-attribute blanks. Cursor
    // stays put, pending_wrap clears, count clamps to the row tail.
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

        terminal.advance(b"\x1b[1;31mr\x1b[2b"); // bold-red 'r', then REP 2

        for column in 0..3 {
            let cell = terminal.screen().cell(0, column).unwrap();
            assert_eq!(cell.ch, 'r');
            assert!(cell.attrs.bold, "repeat at col {column} should stay bold");
            assert_eq!(cell.attrs.foreground, Color::Indexed(1));
        }
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
