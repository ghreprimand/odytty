//! The owned terminal state machine: the [`Screen`] grid (primary + alternate),
//! the [`Terminal`] facade that drives OdyTTY's owned parser, scrollback, scroll
//! regions, resize reflow, and the CSI/OSC/SGR dispatch helpers. This is the
//! bulk of the terminal core; it builds on [`super::types`] and is exercised by
//! `super::tests`.

use unicode_width::UnicodeWidthChar;

use crate::graphics::{ImageScene, VisiblePlacement};
use crate::parser::{OdyParser, Params, VtDispatch};

use super::graphics_routing::{self, DcsCapture, GraphicsStats};

use super::reflow::resize_buffer_rows;
use super::scrollback::{Scrollback, resize_lazy};
use super::search::{SearchMatch, SearchOptions, SearchRow, search_rows};
use super::types::*;

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
pub(in crate::core) struct Line {
    pub(in crate::core) cells: Vec<Cell>,
    pub(in crate::core) wrapped: bool,
}
impl Line {
    /// A row that ends a logical line (hard break / no continuation).
    pub(in crate::core) fn unwrapped(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            wrapped: false,
        }
    }

    /// A row that soft-wraps into the next physical row.
    pub(in crate::core) fn wrapped(cells: Vec<Cell>) -> Self {
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
#[derive(Debug, Clone)]
pub struct Screen {
    dimensions: Dimensions,
    rows: Vec<Line>,
    scrollback: Scrollback,
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
    /// Window title set via OSC 0/2. `None` until a title is set; `Some("")`
    /// records an explicit empty title (distinct from never-set).
    title: Option<String>,
    /// Set whenever the title changes; cleared by `take_title_changed` so a
    /// front end can poll without re-applying an unchanged title.
    title_changed: bool,
    /// Active mouse reporting protocol (tracking mode + wire encoding).
    mouse: MouseProtocol,
    /// Active keyboard reporting modes (DECCKM cursor keys and DECKPAM keypad).
    keyboard: KeyboardModes,
    /// DECSET/DECRST 1004 focus reporting. When on, the front end emits
    /// `ESC [ I` / `ESC [ O` on focus in/out. Off at power-on; RIS resets it.
    focus_reporting: bool,
    /// Effective cursor shape (DECSCUSR `CSI Ps SP q`, or the host default).
    cursor_style: CursorStyle,
    /// Effective cursor blink policy (DECSCUSR, or the host default).
    cursor_blink: bool,
    /// Host default cursor shape, applied at power-on and on DECSCUSR 0 / RIS /
    /// DECSTR. Set once from settings via [`Screen::set_cursor_defaults`].
    default_cursor_style: CursorStyle,
    /// Host default cursor blink policy, applied alongside `default_cursor_style`.
    default_cursor_blink: bool,
    /// Terminal-owned graphics scene: CPU images, cell placements, and raw
    /// graphics protocol payloads awaiting later decoders.
    graphics: ImageScene,
    dcs_capture: Option<DcsCapture>,
    graphics_stats: GraphicsStats,
    /// Live cell pixel metrics for graphics extent calculation. Default 8×16;
    /// the native layer overrides via [`Self::set_cell_metrics`].
    cell_metrics: CellMetrics,
}
#[derive(Debug, Clone)]
struct StoredScreen {
    rows: Vec<Line>,
    scrollback: Scrollback,
    cursor: Position,
    cursor_visible: bool,
    pending_wrap: bool,
    saved_cursor: Option<SavedCursor>,
    scroll_region: Option<ScrollRegion>,
    origin_mode: bool,
    current_attrs: Attrs,
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
            scrollback: Scrollback::new(),
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
            title: None,
            title_changed: false,
            mouse: MouseProtocol::default(),
            keyboard: KeyboardModes::default(),
            focus_reporting: false,
            cursor_style: CursorStyle::default(),
            cursor_blink: true,
            default_cursor_style: CursorStyle::default(),
            default_cursor_blink: true,
            graphics: ImageScene::default(),
            dcs_capture: None,
            graphics_stats: GraphicsStats::default(),
            cell_metrics: CellMetrics::default(),
        }
    }

    /// Set the host default cursor shape and blink policy (from settings).
    ///
    /// Establishes the values DECSCUSR 0 / RIS / DECSTR reset to, and applies
    /// them immediately as the current effective cursor since no application has
    /// issued DECSCUSR yet. Intended to be called once at startup before output.
    pub fn set_cursor_defaults(&mut self, style: CursorStyle, blink: bool) {
        self.default_cursor_style = style;
        self.default_cursor_blink = blink;
        self.cursor_style = style;
        self.cursor_blink = blink;
    }

    /// Set the live cell pixel metrics used by the graphics routing layer
    /// (Sixel/Kitty extent calculation). The native layer calls this at startup
    /// and on every rescale/font-size rebuild so new placements use the real
    /// glyph cell size. Dimensions are clamped to `[1, 1024]` — zero is never
    /// stored.
    ///
    /// Existing placements are **not** recomputed — they retain the extent
    /// calculated at creation time. Only placements created after this call
    /// use the updated metrics (new-placements-only policy, documented).
    pub fn set_cell_metrics(&mut self, width_px: u32, height_px: u32) {
        self.cell_metrics = CellMetrics::new(width_px, height_px);
    }

    /// Current cell pixel metrics. See [`Self::set_cell_metrics`].
    pub fn cell_metrics(&self) -> CellMetrics {
        self.cell_metrics
    }

    /// The cursor shape currently in effect (DECSCUSR or host default).
    pub fn cursor_style(&self) -> CursorStyle {
        self.cursor_style
    }

    /// Whether the cursor's blink policy is currently enabled.
    pub fn cursor_blinking(&self) -> bool {
        self.cursor_blink
    }

    /// DECSCUSR (`CSI Ps SP q`): set the cursor shape and blink.
    ///
    /// Per the VT520/xterm convention: `0` resets to the host default policy,
    /// odd values blink and even values are steady, with `1/2` = block,
    /// `3/4` = underline, `5/6` = bar. Unknown values are ignored.
    fn set_cursor_style(&mut self, ps: usize) {
        let (style, blink) = match ps {
            0 => (self.default_cursor_style, self.default_cursor_blink),
            1 => (CursorStyle::Block, true),
            2 => (CursorStyle::Block, false),
            3 => (CursorStyle::Underline, true),
            4 => (CursorStyle::Underline, false),
            5 => (CursorStyle::Bar, true),
            6 => (CursorStyle::Bar, false),
            _ => return,
        };
        self.cursor_style = style;
        self.cursor_blink = blink;
        self.mark_dirty();
    }

    pub fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    pub fn cursor(&self) -> Position {
        self.cursor
    }

    /// Pending host-bound responses (DA/DSR replies) accumulated by dispatch.
    /// Exposed within the crate so parser golden fixtures can include host
    /// output. Test-only.
    #[cfg(test)]
    pub(crate) fn host_output_bytes(&self) -> &[u8] {
        &self.host_output
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.physical_len(self.dimensions.columns)
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
        let width_unchanged = dimensions.columns == self.dimensions.columns;

        if self.primary_screen.is_some() {
            // Alternate screen active: truncate/pad the app-managed grid (it
            // repaints), but never feed the alternate buffer into scrollback
            // (the alternate screen keeps none).
            let mut discard = Vec::new();
            resize_buffer_rows(&mut self.rows, &mut discard, dimensions, true);
            self.cursor.row = self.cursor.row.min(dimensions.rows - 1);
            self.cursor.column = self.cursor.column.min(dimensions.columns - 1);

            if let Some(mut primary) = self.primary_screen.take() {
                // The stored primary shares the (old) width, so the same
                // width-unchanged decision applies.
                primary.cursor = resize_lazy(
                    &mut primary.scrollback,
                    &mut primary.rows,
                    dimensions,
                    primary.cursor,
                    width_unchanged,
                );
                primary.pending_wrap = false;
                primary.scroll_region = clamp_scroll_region(primary.scroll_region, dimensions);
                self.primary_screen = Some(primary);
            }
        } else {
            // Lazy resize: re-wrap only the bottom of the buffer needed for the
            // new window; deep history stays logical and is projected on access.
            // The width-unchanged path uses the O(rows) keep-width fast path
            // (preserving P1-a).
            self.cursor = resize_lazy(
                &mut self.scrollback,
                &mut self.rows,
                dimensions,
                self.cursor,
                width_unchanged,
            );
        }

        self.dimensions = dimensions;
        self.pending_wrap = false;
        self.resize_tab_stops(dimensions.columns);
        self.scroll_region = clamp_scroll_region(self.scroll_region, dimensions);
        self.graphics
            .resize(self.dimensions.rows, self.dimensions.columns);
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
        let scrollback = self.scrollback.physical(columns);
        let scrollback_len = scrollback.len();
        let offset = offset_rows.min(scrollback_len);

        if offset == 0 {
            return self.snapshot();
        }

        // The viewport is `height` rows of the combined scrollback ++ live
        // buffer whose bottom edge sits `offset` rows above the live bottom.
        let total = scrollback_len + height;
        let window_start = total - offset - height;

        let mut cells = Vec::with_capacity(height * columns);
        for row in scrollback
            .iter()
            .chain(self.rows.iter())
            .skip(window_start)
            .take(height)
        {
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

    /// Terminal-owned graphics state. Render integration is intentionally
    /// separate; this lets tests and future render packets inspect placements
    /// without changing the text [`Snapshot`] surface.
    pub fn graphics(&self) -> &ImageScene {
        &self.graphics
    }

    pub fn graphics_mut(&mut self) -> &mut ImageScene {
        &mut self.graphics
    }

    /// Graphics placements visible in the current viewport. `offset_rows`
    /// follows [`Self::snapshot_with_scrollback`]: `0` is the live screen,
    /// positive values page upward into scrollback.
    pub fn visible_graphics(&self, offset_rows: usize) -> Vec<VisiblePlacement> {
        self.graphics
            .visible_placements(offset_rows, self.dimensions.rows, self.dimensions.columns)
    }

    /// Number of sixel decode failures since power-on (debug diagnostic).
    pub fn sixel_decode_errors(&self) -> u64 {
        self.graphics_stats.sixel_decode_errors
    }

    pub fn plain_text(&self) -> String {
        self.rows
            .iter()
            .map(|row| {
                let mut line = String::new();
                for cell in row.iter().filter(|cell| !cell.wide_continuation) {
                    line.push(cell.ch);
                    for &mark in cell.combining() {
                        line.push(mark);
                    }
                }
                line.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    /// The current window title, or `None` if no OSC 0/2 has set one. An
    /// explicit empty title is reported as `Some("")`.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Return whether the title changed since the last call and clear the flag.
    /// Lets a front end poll once per frame and update the OS window title only
    /// when it actually changed.
    pub fn take_title_changed(&mut self) -> bool {
        std::mem::take(&mut self.title_changed)
    }

    /// The active mouse reporting protocol (tracking mode + encoding).
    pub fn mouse_protocol(&self) -> MouseProtocol {
        self.mouse
    }

    /// Keyboard modes that affect front-end key encoding.
    pub fn keyboard_modes(&self) -> KeyboardModes {
        self.keyboard
    }

    /// Whether DECSET 1004 focus reporting is enabled. When true, a front end
    /// should emit `ESC [ I` on focus gain and `ESC [ O` on focus loss (see
    /// [`encode_focus_event`]).
    pub fn focus_reporting(&self) -> bool {
        self.focus_reporting
    }

    /// Search the combined scrollback + visible buffer for `query`, returning
    /// every match as an absolute cell range (row `0` = oldest scrollback;
    /// see [`super::search`] for the coordinate convention and limitations).
    /// Matches are returned in reading order, sorted ascending by `start`.
    pub fn search(&self, query: &str, options: SearchOptions) -> Vec<SearchMatch> {
        let scrollback = self.scrollback.physical(self.dimensions.columns);
        let rows: Vec<SearchRow<'_>> = scrollback
            .iter()
            .chain(self.rows.iter())
            .map(|line| SearchRow {
                cells: &line.cells,
                wrapped: line.wrapped,
            })
            .collect();
        search_rows(&rows, query, options)
    }

    fn set_title(&mut self, title: String) {
        self.title = Some(title);
        self.title_changed = true;
    }

    /// Before overwriting `width` cells starting at `column` on `row`, blank any
    /// wide-pair partner that sits OUTSIDE the overwrite span so no half-wide
    /// orphan survives. A wide glyph is a lead cell (printable, width 2) plus a
    /// `wide_continuation` spacer; overwriting one half must clear the other,
    /// matching xterm. O(1): only the two span boundaries can orphan a partner.
    fn clear_wide_orphans(&mut self, row: usize, column: usize, width: usize) {
        let columns = self.dimensions.columns;
        let blank = self.current_blank();
        // Left boundary: the first overwritten cell is a continuation whose lead
        // sits to its left, outside the span — blank the now-orphaned lead.
        if column > 0 && column < columns && self.rows[row][column].wide_continuation {
            self.rows[row][column - 1] = blank;
        }
        // Right boundary: the cell just past the span is a continuation whose
        // lead is the last overwritten cell — blank the orphaned continuation.
        let end = column + width;
        if end < columns && self.rows[row][end].wide_continuation {
            self.rows[row][end] = blank;
        }
    }

    /// Attach a zero-width combining mark to the base cell the cursor last
    /// advanced past, appending it to that cell's grapheme. After printing a
    /// base char the cursor sits to its right (or stays on it in pending-wrap),
    /// so the base is just left of the cursor; a wide continuation spacer is
    /// stepped back to its lead. No-op at line start or when capacity is full —
    /// never panics.
    fn attach_combining(&mut self, mark: char) {
        let row = self.cursor.row;
        let col = if self.pending_wrap {
            self.cursor.column
        } else if self.cursor.column > 0 {
            self.cursor.column - 1
        } else {
            return; // combining mark at line start: nothing to attach to.
        };
        let base_col = if self.rows[row][col].wide_continuation && col > 0 {
            col - 1
        } else {
            col
        };
        self.rows[row][base_col].push_combining(mark);
        self.mark_dirty();
    }

    fn print_char(&mut self, ch: char) {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1);
        if width == 0 {
            // Zero-width combining mark: attach to the preceding base cell
            // rather than consuming a column. No-op at line start.
            self.attach_combining(ch);
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
            // A wide glyph does not fit in the remaining columns. xterm does not
            // split it across rows: blank the trailing cell(s) and soft-wrap the
            // glyph onto the next row, marking the row wrapped so resize rejoins
            // the logical line.
            let blank = self.current_blank();
            let r = self.cursor.row;
            let c = self.cursor.column;
            self.clear_wide_orphans(r, c, self.dimensions.columns - c);
            for col in c..self.dimensions.columns {
                self.rows[r][col] = blank;
            }
            self.rows[r].wrapped = true;
            self.carriage_return();
            self.line_feed();
        }

        let row = self.cursor.row;
        let column = self.cursor.column;
        // Overwriting either half of an existing wide pair must clear its
        // partner so no half-wide orphan survives.
        self.clear_wide_orphans(row, column, width);
        self.rows[row][column] = Cell::new(ch, self.current_attrs);

        if width == 2 && column + 1 < self.dimensions.columns {
            self.rows[row][column + 1] = Cell::wide_spacer(self.current_attrs);
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
            self.scrollback.push_row(removed);
        }

        self.rows
            .push(blank_row_with_bg(self.dimensions.columns, background));
        let scrollback_rows = if self.primary_screen.is_none() {
            self.scrollback.physical_len(self.dimensions.columns)
        } else {
            0
        };
        self.graphics.scroll_full_up(1, scrollback_rows);
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
            self.graphics.scroll_region_up(region.top, region.bottom, 1);
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
        self.graphics.scroll_region_up(top, bottom, count);
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
        self.graphics.scroll_region_down(top, bottom, count);
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
            self.graphics.scroll_region_down(top, bottom, 1);
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

        self.graphics
            .scroll_region_down(self.cursor.row, bottom, count);
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

        self.graphics
            .scroll_region_up(self.cursor.row, bottom, count);
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
        self.graphics.erase_display(
            mode,
            self.cursor.row,
            self.cursor.column,
            self.dimensions.rows,
            self.dimensions.columns,
        );
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
        let row = self.cursor.row;
        for column in self.cursor.column..self.dimensions.columns {
            self.rows[row][column] = blank;
        }
        // Erasing the lead-side boundary can orphan a wide pair (a continuation
        // at the cursor whose lead is just left of it); repair the row.
        sanitize_wide_row(&mut self.rows[row], blank);
        self.mark_dirty();
    }

    fn erase_line_to_cursor(&mut self) {
        let blank = self.current_blank();
        let row = self.cursor.row;
        for column in 0..=self.cursor.column {
            self.rows[row][column] = blank;
        }
        // Erasing up to the cursor can orphan a wide lead at the cursor whose
        // continuation sits just right of it; repair the row.
        sanitize_wide_row(&mut self.rows[row], blank);
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
                2 => self.current_attrs.dim = true,
                3 => self.current_attrs.italic = true,
                4 => self.current_attrs.underline = true,
                7 => self.current_attrs.inverse = true,
                8 => self.current_attrs.hidden = true,
                9 => self.current_attrs.strikethrough = true,
                22 => {
                    self.current_attrs.bold = false;
                    self.current_attrs.dim = false;
                }
                23 => self.current_attrs.italic = false,
                24 => self.current_attrs.underline = false,
                27 => self.current_attrs.inverse = false,
                28 => self.current_attrs.hidden = false,
                29 => self.current_attrs.strikethrough = false,
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
                1 => {
                    self.keyboard.application_cursor = action == 'h';
                }
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
                // Alternate-screen modes per xterm ctlseqs:
                // 47: plain switch (no cursor save, no clear on enter or leave).
                // 1047: switch; clear alt on leave (not on enter).
                // 1048: DECSC/DECRC only (cursor save/restore, no switch).
                // 1049: DECSC + switch + clear on enter / switch + DECRC on
                //        leave. Equivalent to 1048h + 1047h on set, and
                //        1047l + 1048l on reset.
                47 => {
                    if action == 'h' {
                        self.enter_alternate_screen(false);
                    } else {
                        self.leave_alternate_screen(false, false);
                    }
                }
                1047 => {
                    if action == 'h' {
                        self.enter_alternate_screen(false);
                    } else {
                        self.leave_alternate_screen(false, true);
                    }
                }
                1048 => {
                    if action == 'h' {
                        self.save_cursor();
                    } else {
                        self.restore_cursor();
                    }
                }
                1049 => {
                    if action == 'h' {
                        self.save_cursor();
                        self.enter_alternate_screen(true);
                    } else {
                        self.leave_alternate_screen(true, false);
                        self.restore_cursor();
                    }
                }
                2004 => {
                    self.bracketed_paste = action == 'h';
                }
                // Mouse tracking modes (single active mode; later DECSET wins,
                // any DECRST returns to Off). See `MouseTracking`.
                9 => self.set_mouse_tracking(MouseTracking::X10, action),
                1000 => self.set_mouse_tracking(MouseTracking::Normal, action),
                1002 => self.set_mouse_tracking(MouseTracking::ButtonEvent, action),
                1003 => self.set_mouse_tracking(MouseTracking::AnyEvent, action),
                // 1004: focus reporting. DECSET enables, DECRST disables.
                1004 => self.focus_reporting = action == 'h',
                // Mouse encoding extensions (single active encoding; later
                // DECSET wins, any DECRST returns to Default). See `MouseEncoding`.
                1005 => self.set_mouse_encoding(MouseEncoding::Utf8, action),
                1006 => self.set_mouse_encoding(MouseEncoding::Sgr, action),
                1015 => self.set_mouse_encoding(MouseEncoding::Urxvt, action),
                _ => {}
            }
        }
    }

    fn set_mouse_tracking(&mut self, mode: MouseTracking, action: char) {
        self.mouse.tracking = if action == 'h' {
            mode
        } else {
            MouseTracking::Off
        };
    }

    fn set_mouse_encoding(&mut self, encoding: MouseEncoding, action: char) {
        self.mouse.encoding = if action == 'h' {
            encoding
        } else {
            MouseEncoding::Default
        };
    }

    fn device_attributes(&mut self, params: &Params, intermediates: &[u8]) {
        if intermediates.is_empty() && param_or(params, 0, 0) == 0 {
            self.host_output.extend_from_slice(b"\x1b[?1;2c");
        }
    }

    /// DSR (ESC [ Ps n): answer the host status queries that line editors rely
    /// on. `5n` reports "terminal OK" (`ESC [ 0 n`); `6n` reports the cursor
    /// position as `ESC [ row ; col R` (1-based). Shells such as fish issue
    /// `6n` to locate the cursor while drawing the completion pager and
    /// multi-line prompts; without a reply their screen model desyncs and
    /// completion listings render in the wrong place or fail to refresh. The
    /// reported row honors origin mode (DECOM): when set, it is relative to the
    /// scroll-region top, mirroring [`Screen::move_to_origin`]. Private-marker
    /// (DECDSR, `?`-intermediate) requests are ignored here.
    fn device_status_report(&mut self, params: &Params, intermediates: &[u8]) {
        if !intermediates.is_empty() {
            return;
        }
        match param_or(params, 0, 0) {
            5 => self.host_output.extend_from_slice(b"\x1b[0n"),
            6 => {
                let row = if self.origin_mode {
                    let (top, _) = self.effective_region();
                    self.cursor.row.saturating_sub(top) + 1
                } else {
                    self.cursor.row + 1
                };
                let column = self.cursor.column + 1;
                self.host_output
                    .extend_from_slice(format!("\x1b[{row};{column}R").as_bytes());
            }
            _ => {}
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

    /// Enter the alternate screen buffer.
    ///
    /// `clear_alt`: when true (mode 1049), the new alt buffer starts blank and
    ///   the cursor homes to (0,0). When false (modes 47/1047), the alt buffer
    ///   starts blank on first entry but its content persists across re-entries
    ///   (only mode 47 — 1047 clears on *leave*; the re-entry path is a no-op
    ///   regardless).
    fn enter_alternate_screen(&mut self, clear_alt: bool) {
        if self.primary_screen.is_some() {
            return;
        }

        let alt_rows = if clear_alt {
            vec![blank_row(self.dimensions.columns); self.dimensions.rows]
        } else {
            vec![blank_row(self.dimensions.columns); self.dimensions.rows]
        };

        let primary_screen = StoredScreen {
            rows: std::mem::replace(&mut self.rows, alt_rows),
            scrollback: std::mem::replace(&mut self.scrollback, Scrollback::new()),
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            pending_wrap: self.pending_wrap,
            saved_cursor: self.saved_cursor,
            scroll_region: self.scroll_region,
            origin_mode: self.origin_mode,
            current_attrs: self.current_attrs,
        };

        if clear_alt {
            self.cursor = Position::default();
            self.pending_wrap = false;
        }
        self.saved_cursor = None;
        self.scroll_region = None;
        self.origin_mode = false;
        self.primary_screen = Some(primary_screen);
        self.graphics.enter_alternate(clear_alt);
        self.mark_dirty();
    }

    /// Leave the alternate screen and restore the primary buffer.
    ///
    /// `restore_cursor`: when true (mode 1049), the cursor position/wrap saved
    ///   at enter time is restored. When false (modes 47/1047), the cursor
    ///   retains its current position (clamped to screen bounds).
    ///
    /// `clear_alt`: when true (mode 1047), the alt buffer is cleared before
    ///   restoring primary (so stale alt content cannot leak if re-entered via
    ///   mode 47). When false (modes 47/1049), no extra clear is performed
    ///   (1049 already cleared on enter; 47 intentionally preserves content).
    fn leave_alternate_screen(&mut self, restore_cursor: bool, _clear_alt: bool) {
        if let Some(primary_screen) = self.primary_screen.take() {
            self.rows = primary_screen.rows;
            self.scrollback = primary_screen.scrollback;
            if restore_cursor {
                self.cursor = Position {
                    row: primary_screen.cursor.row.min(self.dimensions.rows - 1),
                    column: primary_screen
                        .cursor
                        .column
                        .min(self.dimensions.columns - 1),
                };
                self.pending_wrap = primary_screen.pending_wrap;
                self.cursor_visible = primary_screen.cursor_visible;
                self.current_attrs = primary_screen.current_attrs;
            } else {
                // Modes 47/1047: cursor stays where it is (clamped).
                self.cursor.row = self.cursor.row.min(self.dimensions.rows - 1);
                self.cursor.column = self.cursor.column.min(self.dimensions.columns - 1);
                // Still restore visibility and attrs since those are screen-level
                // state that belongs to the primary, not the alt-screen app.
                self.cursor_visible = primary_screen.cursor_visible;
                self.current_attrs = primary_screen.current_attrs;
            }
            self.saved_cursor = primary_screen.saved_cursor;
            self.scroll_region = primary_screen.scroll_region;
            self.origin_mode = primary_screen.origin_mode;
            self.graphics.leave_alternate();
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
        self.graphics.hard_reset();
        self.dcs_capture = None;
        self.graphics_stats = GraphicsStats::default();
        // RIS returns mouse reporting to its power-on (off) state. The title is
        // a persistent window property and is intentionally left untouched.
        self.mouse = MouseProtocol::default();
        self.keyboard = KeyboardModes::default();
        self.focus_reporting = false;
        // RIS returns the cursor shape/blink to the host default policy.
        self.cursor_style = self.default_cursor_style;
        self.cursor_blink = self.default_cursor_blink;
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
        self.keyboard = KeyboardModes::default();
        self.current_attrs = Attrs::default();
        self.host_output.clear();
        self.last_graphic_char = None;
        // DECSTR returns the cursor shape/blink to the host default policy.
        self.cursor_style = self.default_cursor_style;
        self.cursor_blink = self.default_cursor_blink;
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
/// Shared dispatch logic for the terminal core, parameterised over the owned
/// [`crate::parser::Params`] type. The OdyTTY-owned parser drives these methods
/// through [`VtDispatch`], keeping byte parsing separate from terminal semantics.
impl Screen {
    fn dispatch_print(&mut self, c: char) {
        self.print_char(c);
    }

    fn dispatch_execute(&mut self, byte: u8) {
        match byte {
            b'\x08' => self.backspace(),
            b'\t' => self.tab(),
            b'\n' | b'\x0b' | b'\x0c' => self.line_feed(),
            b'\r' => self.carriage_return(),
            _ => {}
        }
    }

    /// OSC handler. Only the title controls (OSC 0/2 set the window title; OSC 1
    /// sets the icon name, which this model does not surface) are acted on; the
    /// numeric the parser splits out as the first parameter selects them. Every
    /// other OSC (4 palette, 7 cwd, 8 hyperlink, 10/11/12 colors, 52 clipboard,
    /// 133 shell integration, …) is consumed safely here. Because OSC payloads
    /// never flow through `print`, none of these can leak bytes into the grid.
    fn dispatch_osc(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(&ident) = params.first() else {
            return;
        };
        match ident {
            // OSC 0 = icon name + window title, OSC 2 = window title.
            b"0" | b"2" => self.set_title(osc_string(&params[1..])),
            // OSC 1 = icon name only: consume without touching the window title.
            b"1" => {}
            _ => {}
        }
    }

    fn dispatch_csi(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }

        match action {
            'A' => self.move_up(param_or_one(params, 0)),
            'B' => self.move_down(param_or_one(params, 0)),
            'C' => self.move_right(param_or_one(params, 0)),
            'D' => self.move_left(param_or_one(params, 0)),
            'G' => self.move_to(self.cursor.row + 1, param_or_one(params, 0)),
            'H' | 'f' => self.move_to_origin(param_or_one(params, 0), param_or_one(params, 1)),
            'S' => self.scroll_region_up(param_or_one(params, 0)),
            'T' => self.scroll_region_down(param_or_one(params, 0)),
            '@' => self.insert_chars(param_or_one(params, 0)),
            'b' => self.repeat_char(param_or_one(params, 0)),
            'J' => self.erase_display(param_or(params, 0, 0)),
            'K' => self.erase_line(param_or(params, 0, 0)),
            'L' => self.insert_lines(param_or_one(params, 0)),
            'M' => self.delete_lines(param_or_one(params, 0)),
            'P' => self.delete_chars(param_or_one(params, 0)),
            'X' => self.erase_chars(param_or_one(params, 0)),
            'c' => self.device_attributes(params, intermediates),
            'n' => self.device_status_report(params, intermediates),
            'd' => self.move_to_origin(param_or_one(params, 0), self.cursor.column + 1),
            'g' => self.clear_tab_stop(param_or(params, 0, 0)),
            'h' | 'l' => self.set_cursor_mode(params, intermediates, action),
            'm' => self.apply_sgr(params),
            'p' if intermediates == b"!" => self.soft_reset(),
            'q' if intermediates == b" " => self.set_cursor_style(param_or(params, 0, 0)),
            'r' => self.set_scroll_region(params),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    fn dispatch_esc(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }

        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'M' => self.reverse_index(),
            b'c' => self.hard_reset(),
            b'H' => self.set_tab_stop(),
            b'=' => self.keyboard.application_keypad = true,
            b'>' => self.keyboard.application_keypad = false,
            _ => {}
        }
    }

    fn dispatch_dcs_hook(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        self.dcs_capture = graphics_routing::dcs_hook(params, intermediates, ignore, action);
    }

    fn dispatch_dcs_put(&mut self, byte: u8) {
        if let Some(capture) = self.dcs_capture.as_mut() {
            graphics_routing::dcs_put(capture, byte);
        }
    }

    fn dispatch_dcs_unhook(&mut self) {
        let Some(capture) = self.dcs_capture.take() else {
            return;
        };
        if let Some((new_row, new_col)) = graphics_routing::dcs_unhook(
            capture,
            &mut self.graphics,
            &mut self.graphics_stats,
            self.cursor.row,
            self.cursor.column,
            self.dimensions.rows,
            self.dimensions.columns,
            self.cell_metrics,
        ) {
            self.cursor.row = new_row;
            self.cursor.column = new_col;
            self.pending_wrap = false;
            self.mark_dirty();
        }
    }

    fn dispatch_apc(&mut self, data: &[u8]) {
        if graphics_routing::apc_dispatch(&mut self.graphics, data) {
            self.mark_dirty();
        }
    }
}

/// OdyTTY-owned seam: the [`OdyParser`](crate::parser::OdyParser) drives the core
/// through this impl. Parameters already arrive as the owned [`Params`], so the
/// callbacks forward straight to the shared `dispatch_*` logic. DCS/APC
/// graphics payloads are recognized and handed to the graphics scene as raw
/// bytes; protocol decoding remains in later graphics packets.
impl VtDispatch for Screen {
    fn print(&mut self, c: char) {
        self.dispatch_print(c);
    }

    fn execute(&mut self, byte: u8) {
        self.dispatch_execute(byte);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.dispatch_osc(params, bell_terminated);
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.dispatch_csi(params, intermediates, ignore, action);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.dispatch_esc(intermediates, ignore, byte);
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.dispatch_dcs_hook(params, intermediates, ignore, action);
    }

    fn put(&mut self, byte: u8) {
        self.dispatch_dcs_put(byte);
    }

    fn unhook(&mut self) {
        self.dispatch_dcs_unhook();
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        self.dispatch_apc(data);
    }
}
pub struct Terminal {
    parser: OdyParser,
    screen: Screen,
}
impl Terminal {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            parser: OdyParser::new(),
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

    /// The current window title (OSC 0/2), or `None` if never set.
    pub fn title(&self) -> Option<&str> {
        self.screen.title()
    }

    /// Whether the title changed since the last poll; clears the flag.
    pub fn take_title_changed(&mut self) -> bool {
        self.screen.take_title_changed()
    }

    /// The active mouse reporting protocol (tracking mode + encoding).
    pub fn mouse_protocol(&self) -> MouseProtocol {
        self.screen.mouse_protocol()
    }

    /// Keyboard modes that affect front-end key encoding.
    pub fn keyboard_modes(&self) -> KeyboardModes {
        self.screen.keyboard_modes()
    }

    /// Whether DECSET 1004 focus reporting is enabled.
    pub fn focus_reporting(&self) -> bool {
        self.screen.focus_reporting()
    }

    /// The cursor shape currently in effect (DECSCUSR or host default).
    pub fn cursor_style(&self) -> CursorStyle {
        self.screen.cursor_style()
    }

    /// Whether the cursor's blink policy is currently enabled.
    pub fn cursor_blinking(&self) -> bool {
        self.screen.cursor_blinking()
    }

    /// Set the host default cursor shape and blink policy (from settings). See
    /// [`Screen::set_cursor_defaults`].
    pub fn set_cursor_defaults(&mut self, style: CursorStyle, blink: bool) {
        self.screen.set_cursor_defaults(style, blink);
    }

    /// Set the live cell pixel metrics for graphics extent calculation.
    /// See [`Screen::set_cell_metrics`].
    pub fn set_cell_metrics(&mut self, width_px: u32, height_px: u32) {
        self.screen.set_cell_metrics(width_px, height_px);
    }

    /// Current cell pixel metrics. See [`Screen::cell_metrics`].
    pub fn cell_metrics(&self) -> CellMetrics {
        self.screen.cell_metrics()
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn graphics(&self) -> &ImageScene {
        self.screen.graphics()
    }

    pub fn graphics_mut(&mut self) -> &mut ImageScene {
        self.screen.graphics_mut()
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

    pub fn visible_graphics(&self, offset_rows: usize) -> Vec<VisiblePlacement> {
        self.screen.visible_graphics(offset_rows)
    }

    /// Search the combined scrollback + visible buffer for `query`. See
    /// [`Screen::search`] for the coordinate convention and result ordering.
    pub fn search(&self, query: &str, options: SearchOptions) -> Vec<SearchMatch> {
        self.screen.search(query, options)
    }
}
pub(in crate::core) fn blank_row(columns: usize) -> Line {
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
/// Reassemble an OSC string payload (everything after the numeric selector)
/// into text. The parser splits OSC on `;`, so a title containing a semicolon
/// arrives as multiple parts; rejoin them with `;` to recover it. Invalid UTF-8
/// is replaced rather than rejected so a malformed title can never panic or
/// desync the parser. An empty payload yields an empty string.
fn osc_string(parts: &[&[u8]]) -> String {
    let mut bytes = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            bytes.push(b';');
        }
        bytes.extend_from_slice(part);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
/// Resolve a count/position CSI parameter, applying the ECMA-48 rule that an
/// omitted *or zero* parameter means 1. The parser represents an omitted
/// parameter as an explicit `0` (e.g. `ESC [ A` parses as a single `0` param),
/// so the plain [`param_or`] with a default of 1 still yields 0 for these
/// controls and turns a bare cursor move (CUU/CUD/CUF/CUB) into a no-op — the
/// bug behind fish leaving stale completion rows on screen because its `ESC [ A`
/// failed to return the cursor to the command line before `ESC [ J`. Use this
/// for movement and count controls (A/B/C/D, S/T, ICH/IL/DL/DCH/ECH, REP) and
/// for 1-based position controls (CHA/CUP/VPA). Mode-selector controls such as
/// ED, EL, SGR, DSR, DA, and tab-clear keep [`param_or`] with a `0` default
/// because a literal `0` there is a meaningful mode, not a count.
fn param_or_one(params: &Params, index: usize) -> usize {
    param_or(params, index, 1).max(1)
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
