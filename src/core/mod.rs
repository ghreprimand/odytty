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
            'J' => self.erase_display(param_or(params, 0, 0)),
            'K' => self.erase_line(param_or(params, 0, 0)),
            'L' => self.insert_lines(param_or(params, 0, 1)),
            'M' => self.delete_lines(param_or(params, 0, 1)),
            'c' => self.device_attributes(params, intermediates),
            'd' => self.move_to(param_or(params, 0, 1), self.cursor.column + 1),
            'h' | 'l' => self.set_cursor_mode(params, intermediates, action),
            'm' => self.apply_sgr(params),
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
}
