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
    current_attrs: Attrs,
    dirty: DirtyRegion,
    host_output: Vec<u8>,
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

        for row in &mut self.rows {
            row.resize(dimensions.columns, Cell::blank());
        }

        if self.rows.len() > dimensions.rows {
            let removed = self.rows.len() - dimensions.rows;
            self.scrollback.extend(self.rows.drain(0..removed));
        }
        self.rows
            .resize_with(dimensions.rows, || blank_row(dimensions.columns));

        self.dimensions = dimensions;
        self.cursor.row = self.cursor.row.min(self.dimensions.rows - 1);
        self.cursor.column = self.cursor.column.min(self.dimensions.columns - 1);
        self.pending_wrap = false;
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
        if self.cursor.row + 1 == self.dimensions.rows {
            self.scroll_up();
        } else {
            self.cursor.row += 1;
            self.mark_dirty();
        }
    }

    fn scroll_up(&mut self) {
        let removed = self.rows.remove(0);
        self.scrollback.push(removed);
        self.rows.push(blank_row(self.dimensions.columns));
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
        if intermediates == b"?" && param_or(params, 0, 0) == 25 {
            self.cursor_visible = action == 'h';
            self.mark_dirty();
        }
    }

    fn device_attributes(&mut self, params: &Params, intermediates: &[u8]) {
        if intermediates.is_empty() && param_or(params, 0, 0) == 0 {
            self.host_output.extend_from_slice(b"\x1b[?1;2c");
        }
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
            'c' => self.device_attributes(params, intermediates),
            'd' => self.move_to(param_or(params, 0, 1), self.cursor.column + 1),
            'h' | 'l' => self.set_cursor_mode(params, intermediates, action),
            'm' => self.apply_sgr(params),
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

fn param_or(params: &Params, index: usize, default: usize) -> usize {
    params
        .iter()
        .nth(index)
        .and_then(|param| param.first())
        .copied()
        .map(usize::from)
        .unwrap_or(default)
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
}
