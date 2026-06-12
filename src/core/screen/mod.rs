//! The owned terminal state machine: the [`Screen`] grid (primary + alternate),
//! the [`Terminal`] facade that drives OdyTTY's owned parser, scrollback, scroll
//! regions, resize reflow, and the CSI/OSC/SGR dispatch helpers. This is the
//! bulk of the terminal core; it builds on [`super::types`] and is exercised by
//! `super::tests`.

use unicode_width::UnicodeWidthChar;

use crate::graphics::{ImageScene, VisiblePlacement};
use crate::parser::{OdyParser, Params, VtDispatch};

use super::graphics_routing::{self, DcsCapture, GraphicsStats};
use super::hyperlink::{Hyperlink, HyperlinkTable};
use super::kitty::{decode_base64_bytes, encode_base64_bytes};

use super::reflow::resize_buffer_rows;
use super::scrollback::{Scrollback, resize_lazy};
use super::search::{SearchMatch, SearchOptions, SearchRow, search_rows};
use super::types::*;

mod ops;
mod query;
mod rect;

pub const OSC52_CLIPBOARD_MAX_BYTES: usize = 64 * 1024;

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
    /// DECAWM (private mode 7). When set, printing past the right edge wraps to
    /// the next row; when reset, the rightmost cell is overwritten in place.
    auto_wrap: bool,
    bracketed_paste: bool,
    current_attrs: Attrs,
    current_protected: bool,
    active_hyperlink: Option<LinkId>,
    dirty: DirtyRegion,
    render_revision: u64,
    host_output: Vec<u8>,
    clipboard_requests: Vec<ClipboardRequest>,
    osc52_read_enabled: bool,
    base_colors: DynamicColors,
    dynamic_colors: DynamicColors,
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
    /// Saved Kitty keyboard protocol flags for CSI > / CSI <. Bounded in
    /// `ops.rs`; kept per active screen so alternate-screen apps cannot leak
    /// their negotiated keyboard protocol into the primary prompt.
    kitty_keyboard_stack: Vec<u16>,
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
    hyperlinks: HyperlinkTable,
    dcs_capture: Option<DcsCapture>,
    dcs_query: Option<query::DcsQueryCapture>,
    graphics_stats: GraphicsStats,
    /// Live cell pixel metrics for graphics extent calculation. Default 8×16;
    /// the native layer overrides via [`Self::set_cell_metrics`].
    cell_metrics: CellMetrics,
    /// DECSDM (private mode 80): sixel display mode / sixel scrolling.
    /// `false` (default, reset): after a sixel image the cursor moves to the
    /// row below the image, column 0 (xterm DECSDM-off behavior).
    /// `true` (set): sixel image anchors at the cursor; cursor does NOT move.
    sixel_display_mode: bool,
    /// DECSET/DECRST 2026 synchronized output. When set, front ends should keep
    /// feeding the model but may defer presenting new grid content until reset.
    synchronized_output: bool,
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
    auto_wrap: bool,
    current_attrs: Attrs,
    current_protected: bool,
    active_hyperlink: Option<LinkId>,
    kitty_keyboard_flags: u16,
    kitty_keyboard_stack: Vec<u16>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultColorSlot {
    Foreground,
    Background,
    Cursor,
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
            auto_wrap: true,
            bracketed_paste: false,
            current_attrs: Attrs::default(),
            current_protected: false,
            active_hyperlink: None,
            dirty: DirtyRegion::Full,
            render_revision: 0,
            host_output: Vec::new(),
            clipboard_requests: Vec::new(),
            osc52_read_enabled: false,
            base_colors: DynamicColors::default(),
            dynamic_colors: DynamicColors::default(),
            last_graphic_char: None,
            tab_stops: default_tab_stops(dimensions.columns),
            title: None,
            title_changed: false,
            mouse: MouseProtocol::default(),
            keyboard: KeyboardModes::default(),
            kitty_keyboard_stack: Vec::new(),
            focus_reporting: false,
            cursor_style: CursorStyle::default(),
            cursor_blink: true,
            default_cursor_style: CursorStyle::default(),
            default_cursor_blink: true,
            graphics: ImageScene::default(),
            hyperlinks: HyperlinkTable::default(),
            dcs_capture: None,
            dcs_query: None,
            graphics_stats: GraphicsStats::default(),
            cell_metrics: CellMetrics::default(),
            sixel_display_mode: false,
            synchronized_output: false,
        }
    }

    /// Set the host default cursor shape and blink policy (from settings).
    ///
    /// Establishes the values DECSCUSR 0 / RIS / DECSTR reset to, and applies
    /// them immediately as the current effective cursor since no application has
    /// issued DECSCUSR yet. Intended to be called once at startup before output.
    pub fn set_cursor_defaults(&mut self, style: CursorStyle, blink: bool) {
        let changed = self.default_cursor_style != style
            || self.default_cursor_blink != blink
            || self.cursor_style != style
            || self.cursor_blink != blink;
        self.default_cursor_style = style;
        self.default_cursor_blink = blink;
        self.cursor_style = style;
        self.cursor_blink = blink;
        if changed {
            self.mark_dirty();
        }
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

    /// Monotonic counter for visible terminal-state changes.
    ///
    /// The native renderer uses this as an additive invalidation seam: every
    /// path that calls [`Self::mark_dirty`] changes the text/graphics/cursor
    /// pixels that a future snapshot can produce. Title-only and host-response
    /// changes deliberately do not bump it because they do not affect the cell
    /// framebuffer.
    pub fn render_revision(&self) -> u64 {
        self.render_revision
    }

    pub fn dynamic_colors(&self) -> &DynamicColors {
        &self.dynamic_colors
    }

    pub fn set_base_colors(
        &mut self,
        foreground: RgbColor,
        background: RgbColor,
        cursor: RgbColor,
    ) {
        self.base_colors.foreground = foreground;
        self.base_colors.background = background;
        self.base_colors.cursor = cursor;
        self.dynamic_colors.foreground = foreground;
        self.dynamic_colors.background = background;
        self.dynamic_colors.cursor = cursor;
        self.mark_dirty();
    }

    pub fn set_osc52_read_enabled(&mut self, enabled: bool) {
        self.osc52_read_enabled = enabled;
    }

    pub fn take_clipboard_requests(&mut self) -> Vec<ClipboardRequest> {
        std::mem::take(&mut self.clipboard_requests)
    }

    pub fn answer_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        self.host_output.extend_from_slice(b"\x1b]52;");
        self.host_output
            .extend_from_slice(osc52_selection_bytes(selection));
        self.host_output.push(b';');
        self.host_output
            .extend_from_slice(encode_base64_bytes(text.as_bytes()).as_bytes());
        self.host_output.extend_from_slice(b"\x1b\\");
    }

    /// DECSDM (private mode 80): when `true`, sixel images anchor at the cursor
    /// and the cursor does NOT move after display. When `false` (default),
    /// the cursor moves below the image.
    pub fn sixel_display_mode(&self) -> bool {
        self.sixel_display_mode
    }

    /// DECSET 2026 synchronized-output mode. The core owns the mode state and
    /// DECRQM/RIS/DECSTR behavior; native presentation owns the safety timeout
    /// so a crashed app cannot freeze the visible frame.
    pub fn synchronized_output_enabled(&self) -> bool {
        self.synchronized_output
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

    fn osc_default_color(&mut self, params: &[&[u8]], slot: DefaultColorSlot) {
        let Some(&value) = params.get(1) else {
            return;
        };
        if value == b"?" {
            self.host_output.extend_from_slice(b"\x1b]");
            self.host_output
                .extend_from_slice(default_color_osc_code(slot));
            self.host_output.push(b';');
            self.host_output
                .extend_from_slice(format_xterm_rgb(self.default_color(slot)).as_bytes());
            self.host_output.extend_from_slice(b"\x1b\\");
            return;
        }
        let Some(color) = parse_xterm_rgb(value) else {
            return;
        };
        match slot {
            DefaultColorSlot::Foreground => self.dynamic_colors.foreground = color,
            DefaultColorSlot::Background => self.dynamic_colors.background = color,
            DefaultColorSlot::Cursor => self.dynamic_colors.cursor = color,
        }
        self.mark_dirty();
    }

    fn reset_default_color(&mut self, slot: DefaultColorSlot) {
        match slot {
            DefaultColorSlot::Foreground => {
                self.dynamic_colors.foreground = self.base_colors.foreground;
            }
            DefaultColorSlot::Background => {
                self.dynamic_colors.background = self.base_colors.background;
            }
            DefaultColorSlot::Cursor => {
                self.dynamic_colors.cursor = self.base_colors.cursor;
            }
        }
        self.mark_dirty();
    }

    fn default_color(&self, slot: DefaultColorSlot) -> RgbColor {
        match slot {
            DefaultColorSlot::Foreground => self.dynamic_colors.foreground,
            DefaultColorSlot::Background => self.dynamic_colors.background,
            DefaultColorSlot::Cursor => self.dynamic_colors.cursor,
        }
    }

    fn osc_palette(&mut self, params: &[&[u8]]) {
        for pair in params[1..].chunks(2) {
            let [index, value] = pair else {
                return;
            };
            let Ok(index) = std::str::from_utf8(index).unwrap_or("").parse::<u16>() else {
                continue;
            };
            if index > 255 {
                continue;
            }
            let index = index as u8;
            if *value == b"?" {
                self.host_output.extend_from_slice(b"\x1b]4;");
                self.host_output
                    .extend_from_slice(index.to_string().as_bytes());
                self.host_output.push(b';');
                self.host_output
                    .extend_from_slice(format_xterm_rgb(self.palette_color(index)).as_bytes());
                self.host_output.extend_from_slice(b"\x1b\\");
            } else if let Some(color) = parse_xterm_rgb(value) {
                self.dynamic_colors.palette[index as usize] = Some(color);
                self.mark_dirty();
            }
        }
    }

    fn osc_reset_palette(&mut self, params: &[&[u8]]) {
        if params.len() == 1 {
            if self.dynamic_colors.palette.iter().any(Option::is_some) {
                self.dynamic_colors.palette = [None; 256];
                self.mark_dirty();
            }
            return;
        }

        let mut changed = false;
        for raw in &params[1..] {
            let Ok(index) = std::str::from_utf8(raw).unwrap_or("").parse::<u16>() else {
                continue;
            };
            if index <= 255 {
                changed |= self.dynamic_colors.palette[index as usize].take().is_some();
            }
        }
        if changed {
            self.mark_dirty();
        }
    }

    fn palette_color(&self, index: u8) -> RgbColor {
        self.dynamic_colors.palette[index as usize].unwrap_or_else(|| indexed_srgb(index))
    }

    fn osc_clipboard(&mut self, params: &[&[u8]]) {
        let selectors = params
            .get(1)
            .copied()
            .and_then(osc52_selections)
            .unwrap_or_else(|| vec![ClipboardSelection::Clipboard]);
        let Some(&payload) = params.get(2) else {
            return;
        };

        if payload == b"?" {
            if self.osc52_read_enabled {
                self.clipboard_requests.extend(
                    selectors
                        .into_iter()
                        .map(|selection| ClipboardRequest::Read { selection }),
                );
            }
            return;
        }

        let Some(decoded) = decode_base64_bytes(payload, OSC52_CLIPBOARD_MAX_BYTES) else {
            return;
        };
        let Ok(text) = String::from_utf8(decoded) else {
            return;
        };
        self.clipboard_requests
            .extend(
                selectors
                    .into_iter()
                    .map(|selection| ClipboardRequest::Write {
                        selection,
                        text: text.clone(),
                    }),
            );
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
            colors: self.dynamic_colors.clone(),
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
            colors: self.dynamic_colors.clone(),
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

    pub fn hyperlink(&self, id: LinkId) -> Option<&Hyperlink> {
        self.hyperlinks.get(id)
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

    fn set_hyperlink(&mut self, params: &[&[u8]]) {
        if params.len() < 3 {
            return;
        }
        if params[2..].iter().all(|part| part.is_empty()) {
            self.active_hyperlink = None;
            return;
        }
        self.active_hyperlink = self.hyperlinks.open(params[1], &params[2..]);
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

        if self.auto_wrap && self.cursor.column + width > self.dimensions.columns {
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
        let attrs = self.current_print_attrs();
        self.rows[row][column] = Cell::new_protected(ch, attrs, self.current_protected);

        if width == 2 && column + 1 < self.dimensions.columns {
            self.rows[row][column + 1] = Cell::wide_spacer_protected(attrs, self.current_protected);
        }

        if self.auto_wrap && self.cursor.column + width >= self.dimensions.columns {
            self.cursor.column = self.dimensions.columns - 1;
            self.pending_wrap = true;
        } else if self.cursor.column + width >= self.dimensions.columns {
            self.cursor.column = self.dimensions.columns - 1;
            self.pending_wrap = false;
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

    /// OSC handler. Title controls (OSC 0/2) set the window title; OSC 8 opens
    /// or closes hyperlink state for subsequently printed cells; OSC 4/10/11/12
    /// update/query runtime colors; OSC 52 queues clipboard requests for the
    /// native layer. Unknown OSCs are consumed safely here. Because OSC payloads
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
            b"4" => self.osc_palette(params),
            b"8" => self.set_hyperlink(params),
            b"10" => self.osc_default_color(params, DefaultColorSlot::Foreground),
            b"11" => self.osc_default_color(params, DefaultColorSlot::Background),
            b"12" => self.osc_default_color(params, DefaultColorSlot::Cursor),
            b"52" => self.osc_clipboard(params),
            b"104" => self.osc_reset_palette(params),
            b"110" => self.reset_default_color(DefaultColorSlot::Foreground),
            b"111" => self.reset_default_color(DefaultColorSlot::Background),
            b"112" => self.reset_default_color(DefaultColorSlot::Cursor),
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
            'J' if intermediates == b"?" => self.selective_erase_display(param_or(params, 0, 0)),
            'J' if intermediates.is_empty() => self.erase_display(param_or(params, 0, 0)),
            'K' if intermediates == b"?" => self.selective_erase_line(param_or(params, 0, 0)),
            'K' if intermediates.is_empty() => self.erase_line(param_or(params, 0, 0)),
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
            'p' if intermediates == b"$" || intermediates == b"?$" => {
                self.request_mode_report(params, intermediates)
            }
            'p' if intermediates == b"!" => self.soft_reset(),
            'q' if intermediates == b">" => self.xtversion_report(params),
            'q' if intermediates == b"\"" => self.set_char_protection(param_or(params, 0, 0)),
            'q' if intermediates == b" " => self.set_cursor_style(param_or(params, 0, 0)),
            'r' => self.set_scroll_region(params),
            's' => self.save_cursor(),
            't' if intermediates.is_empty() => self.window_ops_report(params),
            'u' if intermediates == b"?" => self.kitty_keyboard_query(params, intermediates),
            'u' if intermediates == b">" => self.kitty_keyboard_push(params, intermediates),
            'u' if intermediates == b"<" => self.kitty_keyboard_pop(params, intermediates),
            'u' if intermediates == b"=" => self.kitty_keyboard_set(params, intermediates),
            'u' => self.restore_cursor(),
            'v' if intermediates == b"$" => self.copy_rect(params),
            'x' if intermediates == b"$" => self.fill_rect(params),
            'z' if intermediates == b"$" => self.erase_rect(params),
            '{' if intermediates == b"$" => self.selective_erase_rect(params),
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
        self.dcs_query = query::dcs_query_hook(intermediates, ignore, action);
        if self.dcs_query.is_none() {
            self.dcs_capture = graphics_routing::dcs_hook(params, intermediates, ignore, action);
        }
    }

    fn dispatch_dcs_put(&mut self, byte: u8) {
        if let Some(capture) = self.dcs_query.as_mut() {
            query::dcs_query_put(capture, byte);
            return;
        }
        if let Some(capture) = self.dcs_capture.as_mut() {
            graphics_routing::dcs_put(capture, byte);
        }
    }

    fn dispatch_dcs_unhook(&mut self) {
        if let Some(capture) = self.dcs_query.take() {
            self.dispatch_dcs_query(capture);
            return;
        }
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
            self.sixel_display_mode,
        ) {
            self.cursor.row = new_row;
            self.cursor.column = new_col;
            self.pending_wrap = false;
            self.mark_dirty();
        }
    }

    fn dispatch_apc(&mut self, data: &[u8]) {
        let outcome = graphics_routing::apc_dispatch(
            &mut self.graphics,
            &mut self.graphics_stats,
            &mut self.host_output,
            data,
            self.cursor.row,
            self.cursor.column,
            self.dimensions.rows,
            self.dimensions.columns,
            self.cell_metrics,
        );
        if let Some((row, column)) = outcome.cursor {
            self.cursor.row = row;
            self.cursor.column = column;
            self.pending_wrap = false;
        }
        if outcome.dirty || outcome.cursor.is_some() {
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

    pub fn take_clipboard_requests(&mut self) -> Vec<ClipboardRequest> {
        self.screen.take_clipboard_requests()
    }

    pub fn set_osc52_read_enabled(&mut self, enabled: bool) {
        self.screen.set_osc52_read_enabled(enabled);
    }

    pub fn answer_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        self.screen.answer_clipboard_read(selection, text);
    }

    pub fn set_base_colors(
        &mut self,
        foreground: RgbColor,
        background: RgbColor,
        cursor: RgbColor,
    ) {
        self.screen.set_base_colors(foreground, background, cursor);
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

    pub fn hyperlink(&self, id: LinkId) -> Option<&Hyperlink> {
        self.screen.hyperlink(id)
    }

    /// The cursor shape currently in effect (DECSCUSR or host default).
    pub fn cursor_style(&self) -> CursorStyle {
        self.screen.cursor_style()
    }

    /// Whether the cursor's blink policy is currently enabled.
    pub fn cursor_blinking(&self) -> bool {
        self.screen.cursor_blinking()
    }

    /// Monotonic counter for visible terminal-state changes. See
    /// [`Screen::render_revision`].
    pub fn render_revision(&self) -> u64 {
        self.screen.render_revision()
    }

    /// Whether DECSET 2026 synchronized output is currently enabled.
    pub fn synchronized_output_enabled(&self) -> bool {
        self.screen.synchronized_output_enabled()
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

fn osc52_selections(raw: &[u8]) -> Option<Vec<ClipboardSelection>> {
    if raw.is_empty() {
        return Some(vec![ClipboardSelection::Clipboard]);
    }
    let mut selections = Vec::new();
    for &byte in raw {
        let selection = match byte {
            b'c' => ClipboardSelection::Clipboard,
            b'p' => ClipboardSelection::Primary,
            _ => continue,
        };
        if !selections.contains(&selection) {
            selections.push(selection);
        }
    }
    (!selections.is_empty()).then_some(selections)
}

fn osc52_selection_bytes(selection: ClipboardSelection) -> &'static [u8] {
    match selection {
        ClipboardSelection::Clipboard => b"c",
        ClipboardSelection::Primary => b"p",
    }
}

fn parse_xterm_rgb(raw: &[u8]) -> Option<RgbColor> {
    let raw = std::str::from_utf8(raw).ok()?;
    let components = raw.strip_prefix("rgb:")?;
    let mut parts = components.split('/');
    let red = parse_xterm_rgb_component(parts.next()?)?;
    let green = parse_xterm_rgb_component(parts.next()?)?;
    let blue = parse_xterm_rgb_component(parts.next()?)?;
    parts
        .next()
        .is_none()
        .then(|| RgbColor::new(red, green, blue))
}

fn parse_xterm_rgb_component(component: &str) -> Option<u8> {
    if component.is_empty() || component.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(component, 16).ok()?;
    let max = (1u32 << (component.len() * 4)) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

fn format_xterm_rgb(color: RgbColor) -> String {
    format!(
        "rgb:{:04x}/{:04x}/{:04x}",
        color.red as u16 * 257,
        color.green as u16 * 257,
        color.blue as u16 * 257
    )
}

fn default_color_osc_code(slot: DefaultColorSlot) -> &'static [u8] {
    match slot {
        DefaultColorSlot::Foreground => b"10",
        DefaultColorSlot::Background => b"11",
        DefaultColorSlot::Cursor => b"12",
    }
}

fn indexed_srgb(index: u8) -> RgbColor {
    let (red, green, blue) = match index {
        0 => (0x00, 0x00, 0x00),
        1 => (0xCD, 0x00, 0x00),
        2 => (0x00, 0xCD, 0x00),
        3 => (0xCD, 0xCD, 0x00),
        4 => (0x00, 0x00, 0xEE),
        5 => (0xCD, 0x00, 0xCD),
        6 => (0x00, 0xCD, 0xCD),
        7 => (0xE5, 0xE5, 0xE5),
        8 => (0x7F, 0x7F, 0x7F),
        9 => (0xFF, 0x00, 0x00),
        10 => (0x00, 0xFF, 0x00),
        11 => (0xFF, 0xFF, 0x00),
        12 => (0x5C, 0x5C, 0xFF),
        13 => (0xFF, 0x00, 0xFF),
        14 => (0x00, 0xFF, 0xFF),
        15 => (0xFF, 0xFF, 0xFF),
        16..=231 => {
            let i = index - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let level = |v: u8| -> u8 { if v == 0 { 0 } else { 55 + v * 40 } };
            (level(r), level(g), level(b))
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    };
    RgbColor::new(red, green, blue)
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
fn is_sgr_private_marker(value: u16) -> bool {
    value == b'?' as u16 || value == b'>' as u16 || value == b'<' as u16 || value == b'=' as u16
}

fn sgr_params(params: &Params) -> Vec<&[u16]> {
    params
        .iter()
        .filter(|param| {
            param
                .first()
                .is_some_and(|value| !is_sgr_private_marker(*value))
        })
        .collect()
}

fn clamp_u8(value: u16) -> u8 {
    value.min(255) as u8
}

fn parse_colon_extended_color(param: &[u16]) -> Option<Color> {
    match param {
        [_, 5, index] => Some(Color::Indexed(clamp_u8(*index))),
        [_, 2, red, green, blue] => Some(Color::Rgb(
            clamp_u8(*red),
            clamp_u8(*green),
            clamp_u8(*blue),
        )),
        // Xterm accepts an optional color-space id in colon truecolor form:
        // `38:2:<space>:r:g:b`. The parser stores a missing field as zero, so
        // `38:2::10:20:30` arrives as `[38, 2, 0, 10, 20, 30]`.
        [_, 2, _color_space, red, green, blue] => Some(Color::Rgb(
            clamp_u8(*red),
            clamp_u8(*green),
            clamp_u8(*blue),
        )),
        _ => None,
    }
}

fn parse_semicolon_extended_color(params: &[&[u16]]) -> Option<(Color, usize)> {
    let single = |index: usize| -> Option<u16> {
        params
            .get(index)
            .and_then(|param| (param.len() == 1).then_some(param[0]))
    };

    match single(1)? {
        5 => Some((Color::Indexed(clamp_u8(single(2)?)), 3)),
        2 => Some((
            Color::Rgb(
                clamp_u8(single(2)?),
                clamp_u8(single(3)?),
                clamp_u8(single(4)?),
            ),
            5,
        )),
        _ => None,
    }
}

fn parse_extended_color(params: &[&[u16]]) -> Option<(Color, usize)> {
    let first = params.first()?;
    if first.len() > 1 {
        parse_colon_extended_color(first).map(|color| (color, 1))
    } else {
        parse_semicolon_extended_color(params)
    }
}
