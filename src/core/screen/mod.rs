// SPDX-License-Identifier: GPL-3.0-only
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

use super::prompt_marks::{self, PromptKind};
use super::reflow::resize_buffer_rows;
use super::reflow_trace::{ResizeTrace, trace_resize};
use super::scrollback::{ResizeOptions, Scrollback, resize_lazy_with_options};
use super::search::{SearchMatch, SearchOptions, SearchRow, search_rows};
use super::snapshot_envelope::{
    SnapshotBasicModes, SnapshotEnvelope, SnapshotEnvelopeError, SnapshotLayoutState,
    SnapshotPromptMark, SnapshotRow, SnapshotScrollRegion, SnapshotTerminalState,
};
use super::types::*;

mod ops;
mod osc;
mod query;
mod rect;

use osc::*;

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
///
/// `prompt_mark` is the optional OSC 133 semantic boundary (SH1) anchored to
/// this row's logical line; it is `None` for every row except the first physical
/// row of a logical line that a shell-integration sequence marked. It rides the
/// row purely as advisory state for the poll API — no render-path code reads it
/// and it never reaches the [`Snapshot`] (see [`super::prompt_marks`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core) struct Line {
    pub(in crate::core) cells: Vec<Cell>,
    pub(in crate::core) wrapped: bool,
    pub(in crate::core) prompt_mark: Option<PromptKind>,
}
/// An owned physical row of the visible viewport, produced by
/// [`Screen::visible_search_rows`]. It owns its cells and carries the soft-wrap
/// `wrapped` flag the hint / quick-select scanner needs to join logical lines
/// across wrapped rows.
///
/// It is owned (rather than a borrowed [`SearchRow`] tied to `&self`) because a
/// scrolled-back viewport can include scrollback rows projected through a
/// `RefCell` cache, whose borrow guard cannot outlive the accessor — so a
/// borrowed view spanning scrollback is not soundly returnable. Borrow it as a
/// [`SearchRow`] for the scanners via [`VisibleRow::as_search_row`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleRow {
    /// The row's cells, exactly as stored (not padded to the column count),
    /// mirroring the [`Screen::search`] row-build.
    pub cells: Vec<Cell>,
    /// Whether this physical row soft-wrapped into the next (the logical-line
    /// continuation flag the scanner joins on).
    pub wrapped: bool,
}

impl VisibleRow {
    /// Borrow this owned row as a [`SearchRow`] for the search / hint scanners.
    pub fn as_search_row(&self) -> SearchRow<'_> {
        SearchRow {
            cells: &self.cells,
            wrapped: self.wrapped,
        }
    }
}

impl Line {
    /// A row that ends a logical line (hard break / no continuation).
    pub(in crate::core) fn unwrapped(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            wrapped: false,
            prompt_mark: None,
        }
    }

    /// A row that soft-wraps into the next physical row.
    pub(in crate::core) fn wrapped(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            wrapped: true,
            prompt_mark: None,
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
    /// Discriminator for the `preserve_cursor_physical_line` resize override:
    /// `true` once the shell has applied output (printed a cell) since the last
    /// width-changing resize. The override re-anchors the cursor to its old
    /// physical row offset on the bet that a SIGWINCH-driven shell repaint will
    /// immediately follow and correct it (the Linux contract). That bet only
    /// holds when a repaint is actually coming. For BACK-TO-BACK resizes with no
    /// intervening output (the Windows pane split/close-without-typing case,
    /// where ConPTY/PSReadLine does not repaint on a bare `ResizePseudoConsole`),
    /// honoring the override clamps the cursor column and — because the model
    /// re-derives the logical offset from the displaced physical cursor each
    /// resize — RATCHETS it toward the prompt start. Set `true` in `print_char`;
    /// cleared at the end of `Screen::resize`. When `false`, the override is
    /// skipped and the content-accurate cursor is kept (lossless logical
    /// position), breaking the ratchet without changing any Linux behavior
    /// (every interactive Linux resize is followed by a repaint, so this is
    /// `true` on the next resize there).
    output_since_last_resize: bool,
    /// Whether the backend's shell authoritatively repaints with absolute
    /// positioning on resize, so this terminal defers cursor placement to the
    /// shell rather than translating the cursor itself. Set once by the native
    /// layer from the PTY backend capability ([`Self::set_shell_owns_cursor_on_resize`]);
    /// false for a POSIX PTY (Linux/macOS), true for the ConPTY (Windows)
    /// backend. Passed into every resize as `ReflowOptions::shell_owns_cursor_on_resize`.
    shell_owns_cursor_on_resize: bool,
    saved_cursor: Option<SavedCursor>,
    primary_screen: Option<StoredScreen>,
    scroll_region: Option<ScrollRegion>,
    /// DECOM (origin mode, private mode 6). When set, CUP/HVP/VPA addressing is
    /// relative to the active scroll region top and constrained within it.
    origin_mode: bool,
    /// DECAWM (private mode 7). When set, printing past the right edge wraps to
    /// the next row; when reset, the rightmost cell is overwritten in place.
    auto_wrap: bool,
    /// IRM (ANSI mode 4, `CSI 4 h` / `CSI 4 l`). When set, a printed glyph
    /// shifts the cells at and right of the cursor toward the right edge instead
    /// of overwriting in place (insert mode); when reset (the default), printing
    /// overwrites (replace mode). Reset by RIS and DECSTR. Some line editors
    /// (notably Apple's `pico`/`nano` on macOS) rely on IRM for incremental line
    /// redraw, so without it their on-screen text is corrupted.
    insert_mode: bool,
    bracketed_paste: bool,
    /// DECSET 1007 (alternate scroll mode). When set and the alternate screen is
    /// active, the host layer translates wheel events into cursor-key presses so
    /// full-screen TUIs that do not track the mouse still scroll. Default on, to
    /// match xterm/iTerm2/Terminal.app/Ghostty.
    alternate_scroll: bool,
    current_attrs: Attrs,
    current_protected: bool,
    rect_attr_extent: RectAttributeExtent,
    active_hyperlink: Option<LinkId>,
    dirty: DirtyRegion,
    render_revision: u64,
    host_output: Vec<u8>,
    clipboard_requests: Vec<ClipboardRequest>,
    osc52_read_enabled: bool,
    /// BEL (`0x07`) latch. Set when the host writes a bell control; drained by
    /// the native layer once per frame to drive the visual/urgency bell. The
    /// core never makes noise or touches the grid — it only records that a bell
    /// was requested.
    bell_pending: bool,
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
    /// Working directory reported via OSC 7 (`file://host/path`). `None` until a
    /// well-formed OSC 7 sets it. Stores the percent-decoded path only; the host
    /// is validated (empty / "localhost" / injected local hostname) then
    /// dropped. The core performs NO filesystem access — this is advisory string
    /// state for the front end (e.g. open-new-tab-in-same-directory). See
    /// [`parse_osc7_cwd`] for the parse and hostname policy.
    working_directory: Option<String>,
    /// Local hostname injected by the front end so OSC 7 URLs such as
    /// `file://workstation/path` can be recognized as local without the core
    /// performing a syscall. Live config only: it is intentionally absent from
    /// [`SnapshotEnvelope`] encode/decode.
    local_hostname: Option<String>,
    /// Set whenever the working directory changes; cleared by
    /// [`Screen::take_working_directory_changed`] so a front end can poll once
    /// per frame without re-reading an unchanged value.
    working_directory_changed: bool,
    /// Set whenever an OSC 133 prompt mark is stamped (SH1); cleared by
    /// [`Screen::take_prompt_marks_changed`] so a front end can poll once per
    /// frame and rebuild any per-command UI only when the marks actually moved.
    prompt_marks_changed: bool,
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
    /// OSC 133 click-to-position enable (SH-CLICK). A cooperating shell sets this
    /// per prompt via a `click_events=1` attribute (and clears it with `=0`); a
    /// plain prompt leaves it unchanged. Off at power-on; RIS resets it. Advisory
    /// state only — core never acts on it; the native pointer layer reads it and,
    /// on a click on the active prompt line, maps [`prompt_marks::click_report`]
    /// to cursor-key presses. Default-off means the emit path is inert until the
    /// app opts in.
    click_events_enabled: bool,
    /// Column where the active OSC 133 `B` input boundary was reported, in the
    /// absolute row coordinate space used by prompt marks. This is advisory
    /// live-prompt state for native input editing; it never reaches snapshots.
    active_prompt_input_start: Option<(usize, usize)>,
    /// Latest OdyTTY-private edit-region report
    /// (`OSC 133;P;odytty-edit;len;cur[;nl]`) from a cooperating shell's line
    /// editor: authoritative buffer length + cursor in runes. Same advisory
    /// lifecycle as `active_prompt_input_start` (cleared by `A`/`C`/`D` and
    /// reset); consumed by [`Screen::input_region`] to make the input region's
    /// right edge exact. `None` until a shell emits the private OSC.
    active_edit_region: Option<super::input_region::EditRegionSignal>,
    /// Absolute row of the active OSC 133 `A` prompt-start boundary. During a
    /// width-changing resize, rows from this anchor through the cursor belong
    /// to the shell's live prompt repaint and must not grow extra wrap rows.
    active_prompt_start: Option<usize>,
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
    rect_attr_extent: RectAttributeExtent,
    active_hyperlink: Option<LinkId>,
    kitty_keyboard_flags: u16,
    kitty_keyboard_stack: Vec<u16>,
    /// OSC 133 `B` input-start from the primary screen, saved on entering the
    /// alternate screen and restored on leaving it. Alternate-screen apps have
    /// no prompt input boundary of their own — storing and clearing the primary
    /// value on enter prevents the native input-editing layer from reading stale
    /// primary state while an alternate-screen TUI is running.
    active_prompt_input_start: Option<(usize, usize)>,
    /// Private edit-region report saved with the primary buffer (same
    /// isolation rationale as `active_prompt_input_start`).
    active_edit_region: Option<super::input_region::EditRegionSignal>,
    /// OSC 133 `A` prompt-start anchor saved with the primary buffer.
    active_prompt_start: Option<usize>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RectAttributeExtent {
    #[default]
    Stream,
    Exact,
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
            output_since_last_resize: false,
            shell_owns_cursor_on_resize: false,
            saved_cursor: None,
            primary_screen: None,
            scroll_region: None,
            origin_mode: false,
            auto_wrap: true,
            insert_mode: false,
            bracketed_paste: false,
            alternate_scroll: true,
            current_attrs: Attrs::default(),
            current_protected: false,
            rect_attr_extent: RectAttributeExtent::default(),
            active_hyperlink: None,
            dirty: DirtyRegion::Full,
            render_revision: 0,
            host_output: Vec::new(),
            clipboard_requests: Vec::new(),
            osc52_read_enabled: false,
            bell_pending: false,
            base_colors: DynamicColors::default(),
            dynamic_colors: DynamicColors::default(),
            last_graphic_char: None,
            tab_stops: default_tab_stops(dimensions.columns),
            title: None,
            title_changed: false,
            working_directory: None,
            local_hostname: None,
            working_directory_changed: false,
            prompt_marks_changed: false,
            mouse: MouseProtocol::default(),
            keyboard: KeyboardModes::default(),
            kitty_keyboard_stack: Vec::new(),
            focus_reporting: false,
            click_events_enabled: false,
            active_prompt_input_start: None,
            active_edit_region: None,
            active_prompt_start: None,
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

    /// Set whether the backend's shell owns cursor placement on resize (it
    /// repaints with absolute positioning, e.g. ConPTY/conhost). When true, the
    /// resize reflow keeps the incoming cursor clamped to the new dims and lets
    /// the shell's repaint own placement, instead of translating the cursor (the
    /// `preserve_cursor_physical_line` override + content map). Wired once from
    /// the PTY backend capability when the native layer creates a local session.
    pub fn set_shell_owns_cursor_on_resize(&mut self, value: bool) {
        self.shell_owns_cursor_on_resize = value;
    }

    /// Whether the backend's shell owns cursor placement on resize.
    /// See [`Self::set_shell_owns_cursor_on_resize`]. Test-only: production code
    /// only ever sets this (from the backend capability); the getter exists to
    /// tie the flag to real resize behavior in the cross-platform guard tests.
    #[cfg(test)]
    pub(crate) fn shell_owns_cursor_on_resize(&self) -> bool {
        self.shell_owns_cursor_on_resize
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

    /// Set the scrollback retention cap in logical lines (`0` = unbounded),
    /// trimming any excess immediately. Applies to the active buffer and, when a
    /// TUI is on the alternate screen, the stored primary scrollback as well, so
    /// a live config reload during a full-screen app takes effect on return.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.scrollback.set_limit(limit);
        if let Some(stored) = self.primary_screen.as_mut() {
            stored.scrollback.set_limit(limit);
        }
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
        // A resize re-wraps/re-anchors marks, so absolute-row mark positions can
        // shift; flag the change for the poll API (only when marks exist).
        let had_prompt_marks = self.has_any_prompt_mark();

        // Capture the trace inputs BEFORE any mutation (old dims, incoming
        // cursor + pending-wrap, and the load-bearing discriminator). Cheap
        // plain copies; only formatted/written when ODYTTY_REFLOW_TRACE is on.
        let trace_old_cols = self.dimensions.columns;
        let trace_old_rows = self.dimensions.rows;
        let trace_output_since_last_resize = self.output_since_last_resize;
        let trace_alt_screen_active = self.primary_screen.is_some();
        let trace_cursor_in = self.cursor;
        let trace_pending_in = self.pending_wrap;

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
                let result = resize_lazy_with_options(
                    &mut primary.scrollback,
                    &mut primary.rows,
                    dimensions,
                    primary.cursor,
                    width_unchanged,
                    ResizeOptions {
                        preserve_cursor_physical_line: false,
                        cursor_pending_wrap: primary.pending_wrap,
                        collapse_prompt_start_row: None,
                        // The override never fires for the stored primary
                        // (preserve_cursor_physical_line is false here), so the
                        // discriminator is inert; pass false.
                        repaint_expected: false,
                        // Defer cursor placement to the shell on a backend that
                        // repaints absolutely (ConPTY). preserve is already
                        // false here, so this just keeps the stored-primary
                        // cursor at its incoming position clamped to new dims.
                        shell_owns_cursor_on_resize: self.shell_owns_cursor_on_resize,
                    },
                );
                primary.cursor = result.cursor;
                primary.pending_wrap = result.pending_wrap;
                primary.scroll_region = clamp_scroll_region(primary.scroll_region, dimensions);
                self.primary_screen = Some(primary);
            }
        } else {
            let old_scrollback_rows = self.scrollback.physical_len(self.dimensions.columns);
            let collapse_prompt_start_row = active_prompt_start_visible_row(
                self.active_prompt_start,
                old_scrollback_rows,
                self.cursor.row,
                self.rows.len(),
                width_unchanged,
            );
            // Lazy resize: re-wrap only the bottom of the buffer needed for the
            // new window; deep history stays logical and is projected on access.
            // The width-unchanged path uses the O(rows) keep-width fast path
            // (preserving P1-a).
            let result = resize_lazy_with_options(
                &mut self.scrollback,
                &mut self.rows,
                dimensions,
                self.cursor,
                width_unchanged,
                ResizeOptions {
                    preserve_cursor_physical_line: !width_unchanged,
                    cursor_pending_wrap: self.pending_wrap,
                    collapse_prompt_start_row,
                    repaint_expected: self.output_since_last_resize,
                    shell_owns_cursor_on_resize: self.shell_owns_cursor_on_resize,
                },
            );
            self.cursor = result.cursor;
            self.pending_wrap = result.pending_wrap;
            if let Some(row) = result.collapsed_prompt_start_row {
                let scrollback_rows = self.scrollback.physical_len(dimensions.columns);
                self.active_prompt_start = Some(scrollback_rows + row);
            }
            // Re-anchor the OSC 133 `B` input-start mark through the resize.
            // `active_prompt_input_start` caches an ABSOLUTE row
            // (`physical_len(old_columns) + cursor.row`) captured when `B`
            // arrived. A width change rewraps scrollback to a different
            // `physical_len`, so the cached row no longer equals the LIVE
            // `scrollback_len + cursor.row` the consumer gate
            // (`editable_input_selection_for_context_menu`) compares against —
            // which silently disables prompt-aware select+Delete after a
            // side-by-side split until the next prompt re-emits `A`/`B`. The
            // input line is the cursor's logical line while editing, and the
            // resize repositions the cursor faithfully, so recompute the anchor
            // from the cursor's current logical-line start at the new width.
            //
            // Semantics (decision gate → reflow-preserved mark + cursor line):
            // only re-anchor when the cursor still sits on a prompt-marked
            // logical line (marks travel with their logical line through reflow).
            // If the cursor has moved off the prompt (e.g. output without a `C`),
            // the anchor is left as-is so the gate still declines — matching
            // today's no-resize behavior. The column is preserved: the prompt
            // prefix is unchanged by a rewrap of a single short input line, and
            // `start = selected_start.max(input_column)` keeps bounding the
            // editable region. Width-unchanged resizes keep `physical_len`
            // constant, so the cached row stays valid and we skip the recompute
            // (byte-identical fast path).
            if !width_unchanged && let Some((_, input_column)) = self.active_prompt_input_start {
                let mut row = self.cursor.row.min(self.rows.len().saturating_sub(1));
                while row > 0 && self.rows[row - 1].wrapped {
                    row -= 1;
                }
                if self
                    .rows
                    .get(row)
                    .and_then(|line| line.prompt_mark)
                    .is_some()
                {
                    let scrollback_rows = self.scrollback.physical_len(dimensions.columns);
                    self.active_prompt_input_start = Some((scrollback_rows + row, input_column));
                }
            }
        }

        self.dimensions = dimensions;
        if self.primary_screen.is_some() {
            self.pending_wrap = false;
        }
        // Reset the repaint discriminator: any output that arrives AFTER this
        // resize re-arms the override for the NEXT resize. Back-to-back resizes
        // with no intervening output therefore see `false` and skip the override
        // (the no-repaint case that would otherwise ratchet the cursor).
        self.output_since_last_resize = false;
        self.resize_tab_stops(dimensions.columns);
        self.scroll_region = clamp_scroll_region(self.scroll_region, dimensions);
        self.graphics
            .resize(self.dimensions.rows, self.dimensions.columns);
        self.prompt_marks_changed |= had_prompt_marks;
        self.mark_dirty();

        // Passive, env-gated diagnostic (no-op unless ODYTTY_REFLOW_TRACE is
        // set). Emits one line capturing how this resize moved the cursor and,
        // critically, whether output had arrived since the previous resize.
        trace_resize(&ResizeTrace {
            old_cols: trace_old_cols,
            old_rows: trace_old_rows,
            new_cols: dimensions.columns,
            new_rows: dimensions.rows,
            width_unchanged,
            output_since_last_resize: trace_output_since_last_resize,
            alt_screen_active: trace_alt_screen_active,
            shell_owns_cursor_on_resize: self.shell_owns_cursor_on_resize,
            cursor_in_row: trace_cursor_in.row,
            cursor_in_col: trace_cursor_in.column,
            pending_wrap_in: trace_pending_in,
            cursor_out_row: self.cursor.row,
            cursor_out_col: self.cursor.column,
            pending_wrap_out: self.pending_wrap,
        });
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

    /// Copy the constrained Phase 2 persistence subset into owned DTOs.
    ///
    /// This is intentionally separate from the render [`Snapshot`] surface:
    /// scrollback rows and mode state are needed for resumable sessions, but
    /// private `Screen` / `Scrollback` storage remains behind this owned copy.
    pub fn snapshot_state(&self, max_scrollback_rows: usize) -> SnapshotTerminalState {
        let columns = self.dimensions.columns;
        let scrollback = self.scrollback.physical(columns);
        let start = scrollback.len().saturating_sub(max_scrollback_rows);
        let scrollback_rows = scrollback[start..]
            .iter()
            .map(|row| snapshot_row_from_line(row, columns))
            .collect();
        let visible_rows = self
            .rows
            .iter()
            .map(|row| snapshot_row_from_line(row, columns))
            .collect();

        SnapshotTerminalState {
            dimensions: self.dimensions,
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            cursor_style: self.cursor_style,
            cursor_blink: self.cursor_blink,
            basic_modes: SnapshotBasicModes {
                bracketed_paste: self.bracketed_paste,
                alternate_scroll: self.alternate_scroll,
                alternate_screen: self.primary_screen.is_some(),
                synchronized_output: self.synchronized_output,
                focus_reporting: self.focus_reporting,
                mouse: self.mouse,
                keyboard: self.keyboard,
            },
            scrollback_rows,
            visible_rows,
        }
    }

    /// Copy layout-affecting terminal state that is not part of the render
    /// [`Snapshot`] surface.
    pub fn snapshot_layout_state(&self) -> SnapshotLayoutState {
        SnapshotLayoutState {
            scroll_region: self.scroll_region.map(|region| SnapshotScrollRegion {
                top: region.top,
                bottom: region.bottom,
            }),
            tab_stops: self.tab_stops.clone(),
        }
    }

    /// Restore this screen from an owned Phase 2 snapshot envelope.
    ///
    /// The envelope carries active-buffer terminal state only. When it records
    /// `alternate_screen = true`, the active alternate grid is restored and a
    /// blank primary placeholder is installed so the mode bit remains true; the
    /// stored primary buffer is not present in v2 snapshots.
    pub fn restore_from_envelope(
        &mut self,
        envelope: &SnapshotEnvelope,
    ) -> Result<(), SnapshotEnvelopeError> {
        restore_validate_terminal_state(&envelope.terminal)?;
        envelope.layout.validate(envelope.terminal.dimensions)?;

        let columns = envelope.terminal.dimensions.columns;
        let mut scrollback_rows =
            restore_lines_from_snapshot_rows(&envelope.terminal.scrollback_rows, columns)?;
        let mut visible_rows =
            restore_lines_from_snapshot_rows(&envelope.terminal.visible_rows, columns)?;
        restore_apply_prompt_marks(
            &mut scrollback_rows,
            &mut visible_rows,
            &envelope.prompt_marks,
        )?;

        let mut restored = Screen::new(
            envelope.terminal.dimensions.columns,
            envelope.terminal.dimensions.rows,
        );
        restored.rows = visible_rows;
        restored.scrollback = Scrollback::from_physical_rows(&scrollback_rows);
        restored.cursor = envelope.terminal.cursor;
        restored.cursor_visible = envelope.terminal.cursor_visible;
        restored.cursor_style = envelope.terminal.cursor_style;
        restored.cursor_blink = envelope.terminal.cursor_blink;
        restored.bracketed_paste = envelope.terminal.basic_modes.bracketed_paste;
        restored.alternate_scroll = envelope.terminal.basic_modes.alternate_scroll;
        restored.synchronized_output = envelope.terminal.basic_modes.synchronized_output;
        restored.focus_reporting = envelope.terminal.basic_modes.focus_reporting;
        restored.mouse = envelope.terminal.basic_modes.mouse;
        restored.keyboard = envelope.terminal.basic_modes.keyboard;
        restored.dynamic_colors = envelope.dynamic_colors.clone();
        restored.title = envelope.metadata.title.clone();
        restored.title_changed = envelope.metadata.title.is_some();
        restored.working_directory = envelope.metadata.working_directory.clone();
        restored.working_directory_changed = envelope.metadata.working_directory.is_some();
        restored.prompt_marks_changed = !envelope.prompt_marks.is_empty();
        restored.scroll_region = envelope.layout.scroll_region.map(|region| ScrollRegion {
            top: region.top,
            bottom: region.bottom,
        });
        restored.tab_stops = envelope.layout.tab_stops.clone();
        if envelope.terminal.basic_modes.alternate_screen {
            restored.primary_screen = Some(blank_stored_primary(restored.dimensions));
        }
        restored.mark_dirty();

        *self = restored;
        Ok(())
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

    /// DECSET 1007 (alternate scroll mode) state. Default on; the host layer
    /// only acts on it while the alternate screen is active and the application
    /// is not tracking the mouse.
    pub fn alternate_scroll_enabled(&self) -> bool {
        self.alternate_scroll
    }

    /// Whether the alternate screen buffer is currently active (the primary
    /// buffer is stored). The alternate screen has no scrollback, so the host
    /// uses this to decide between scrollback movement and alternate-scroll
    /// cursor-key translation.
    pub fn on_alternate_screen(&self) -> bool {
        self.primary_screen.is_some()
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

    /// The current working directory reported via OSC 7, or `None` if none has
    /// been reported. The value is the percent-decoded path component of the
    /// last well-formed `file://host/path` URL (the host is validated then
    /// dropped). Advisory only — the core never touches the filesystem.
    pub fn current_working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    /// Return whether the working directory changed since the last call and
    /// clear the flag. Mirrors [`Screen::take_title_changed`] so a front end can
    /// poll once per frame and react (e.g. retitle a tab) only on change.
    pub fn take_working_directory_changed(&mut self) -> bool {
        std::mem::take(&mut self.working_directory_changed)
    }

    /// Set or clear the local hostname accepted by OSC 7. With `None`, OSC 7
    /// hostname behavior is byte-identical to the historical default: only empty
    /// host and `localhost` are accepted.
    pub fn set_local_hostname(&mut self, local_hostname: Option<String>) {
        self.local_hostname = local_hostname.filter(|host| !host.is_empty());
    }

    /// The OSC 133 prompt mark anchored to absolute row `row`, or `None` if the
    /// row carries no mark (SH1). The coordinate convention matches
    /// [`Screen::snapshot_with_scrollback`] and [`super::search`]: row `0` is the
    /// oldest physical scrollback row, counting down through scrollback into the
    /// live grid. Out-of-range rows return `None`. Advisory state only — nothing
    /// on the render path consults it; this is the sole reader of `prompt_mark`.
    pub fn prompt_mark_at(&self, row: usize) -> Option<PromptKind> {
        let scrollback = self.scrollback.physical(self.dimensions.columns);
        let scrollback_len = scrollback.len();
        if row < scrollback_len {
            scrollback[row].prompt_mark
        } else {
            self.rows
                .get(row - scrollback_len)
                .and_then(|l| l.prompt_mark)
        }
    }

    /// Every OSC 133 prompt mark in the buffer, as `(absolute_row, kind)` pairs
    /// in ascending row order. The coordinate convention matches
    /// [`Screen::prompt_mark_at`]: row `0` is the oldest physical scrollback
    /// row, counting down through scrollback into the live grid. Rows without a
    /// mark are skipped, so the result is the *set* of marked rows, not one
    /// entry per row.
    ///
    /// This is the enumeration counterpart to the point-query
    /// [`Screen::prompt_mark_at`]: a command-aware front end caches this `Vec`
    /// and rebuilds it only when [`Screen::take_prompt_marks_changed`] reports a
    /// change, rather than scanning every row each frame. Advisory read only —
    /// no mutation, no render-path effect, nothing reaches the
    /// [`super::types::Snapshot`]. When the alternate screen is active the active
    /// buffer carries no marks (they ride the stored primary), so this returns
    /// an empty `Vec`, consistent with [`Screen::prompt_mark_at`].
    pub fn prompt_marks(&self) -> Vec<(usize, PromptKind)> {
        let scrollback = self.scrollback.physical(self.dimensions.columns);
        let mut marks = Vec::new();
        for (row, line) in scrollback.iter().enumerate() {
            if let Some(kind) = line.prompt_mark {
                marks.push((row, kind));
            }
        }
        let base = scrollback.len();
        for (offset, line) in self.rows.iter().enumerate() {
            if let Some(kind) = line.prompt_mark {
                marks.push((base + offset, kind));
            }
        }
        marks
    }

    /// Return whether the set of prompt marks may have changed since the last
    /// call, and clear the flag. Mirrors [`Screen::take_working_directory_changed`]
    /// so a front end can poll once per frame and rebuild per-command UI only on
    /// change.
    ///
    /// The flag is set not only when a new mark is stamped but also whenever an
    /// operation can clear or reposition existing marks — RIS, erase-display /
    /// erase-line row replacement, resize/reflow, and alternate-screen
    /// enter/leave (which swaps the marked primary out and back) — so a consumer
    /// that trusts "rebuild only on change" never sees
    /// [`Screen::prompt_mark_at`] return a different result while this reads
    /// `false`. It is *conservative*: those
    /// clearing/repositioning paths only raise it when marks are actually
    /// present, but a raised flag does not guarantee a mark's value changed
    /// (e.g. a resize that left every mark on the same absolute row).
    pub fn take_prompt_marks_changed(&mut self) -> bool {
        std::mem::take(&mut self.prompt_marks_changed)
    }

    /// Whether any prompt mark is held anywhere in the terminal — the active
    /// screen's live rows or scrollback, *or* the stored primary screen when the
    /// alternate screen is active. Used to keep
    /// [`Self::take_prompt_marks_changed`] honest: the clear/reposition paths
    /// (RIS, resize) only raise the change flag when there is actually a mark to
    /// clear or move, and an alt-active resize re-anchors the stored primary's
    /// marks too, so they must be counted here.
    fn has_any_prompt_mark(&self) -> bool {
        if self.rows.iter().any(|l| l.prompt_mark.is_some()) || self.scrollback.any_prompt_mark() {
            return true;
        }
        if let Some(primary) = &self.primary_screen {
            return primary.rows.iter().any(|l| l.prompt_mark.is_some())
                || primary.scrollback.any_prompt_mark();
        }
        false
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

    /// Whether OSC 133 click-to-position (SH-CLICK) is currently enabled by the
    /// shell. Advisory: the native pointer layer reads this to decide whether a
    /// click on the active prompt line should emit a
    /// [`prompt_marks::click_report`]; core never acts on it. Off until a
    /// `click_events=1` prompt attribute opts in.
    pub fn click_events_enabled(&self) -> bool {
        self.click_events_enabled
    }

    /// Active OSC 133 `B` input-start boundary as `(absolute_row, column)`.
    /// Returns `None` before a cooperating shell reports `B`, after command
    /// output starts, or after reset. Advisory state only.
    pub fn active_prompt_input_start(&self) -> Option<(usize, usize)> {
        self.active_prompt_input_start
    }

    /// The live editable prompt-input region, derived in core from the OSC 133
    /// `B` mark, the soft-wrap flags, the cursor, and (when a cooperating
    /// shell emits the private edit-region OSC) the authoritative buffer
    /// geometry. `None` means no editable input is present — callers must
    /// no-op rather than guess. See [`super::input_region`] for the model and
    /// the certainty gate.
    pub fn input_region(&self) -> Option<super::input_region::InputRegion> {
        super::input_region::derive_input_region(
            &self.rows,
            self.scrollback.physical_len(self.dimensions.columns),
            self.dimensions.columns,
            self.active_prompt_input_start,
            self.cursor,
            self.active_edit_region.as_ref(),
        )
    }

    pub fn hyperlink(&self, id: LinkId) -> Option<&Hyperlink> {
        self.hyperlinks.get(id)
    }

    #[cfg(test)]
    pub(in crate::core) fn hyperlink_count(&self) -> usize {
        self.hyperlinks.len()
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

    /// The visible viewport's physical rows at scrollback `offset_rows`, as owned
    /// [`VisibleRow`]s carrying each row's `wrapped` flag — the windowed input the
    /// hint / quick-select scanner consumes (it needs the soft-wrap flags the
    /// flat [`Snapshot`] does not carry).
    ///
    /// Mirrors the [`search`](Self::search) row-build but windows to the visible
    /// viewport only — the same window as
    /// [`snapshot_with_scrollback`](Self::snapshot_with_scrollback). Offset `0` is
    /// the live screen (`self.rows`); positive offsets page upward into
    /// scrollback, clamped so callers cannot read past the oldest row. Rows are
    /// emitted top-to-bottom in screen order, so a scanner's row indices are
    /// viewport-relative (row `0` = the top visible row) — exactly the coordinate
    /// the renderer paints hint labels in. Pure; never panics.
    pub fn visible_search_rows(&self, offset_rows: usize) -> Vec<VisibleRow> {
        let height = self.dimensions.rows;
        let columns = self.dimensions.columns;
        let scrollback = self.scrollback.physical(columns);
        let scrollback_len = scrollback.len();
        // Same window as snapshot_with_scrollback: bottom edge `offset` rows above
        // the live tail. window_start = (scrollback_len + height) - offset - height.
        let offset = offset_rows.min(scrollback_len);
        let window_start = scrollback_len - offset;
        scrollback
            .iter()
            .chain(self.rows.iter())
            .skip(window_start)
            .take(height)
            .map(|line| VisibleRow {
                cells: line.cells.clone(),
                wrapped: line.wrapped,
            })
            .collect()
    }

    fn set_title(&mut self, title: String) {
        self.title = Some(title);
        self.title_changed = true;
    }

    fn set_working_directory(&mut self, cwd: String) {
        self.working_directory = Some(cwd);
        self.working_directory_changed = true;
    }

    /// Handle an OSC 133 payload (SH1): parse the `;`-split parts after `133`
    /// into a [`PromptKind`] and stamp it on the cursor's current *logical*
    /// line.
    ///
    /// Marks are **logical-line-anchored**: the mark is recorded on the FIRST
    /// physical row of the logical line the cursor is on, found by walking back
    /// over soft-wrapped predecessor rows. This keeps the live mark where the
    /// scroll-out and reflow carry will keep it (continuation rows never hold a
    /// mark), so a query before and after a resize/scroll-out agrees. A boundary
    /// reported mid-soft-wrap (e.g. a prompt that wrapped) therefore marks the
    /// start of the wrapped region, not the wrap continuation. When the logical
    /// line's true first row has already scrolled into scrollback, the walk
    /// stops at the top of the live grid; the carry paths then adopt the mark
    /// onto the open scrollback line (see [`super::scrollback::Scrollback::push_row`]).
    ///
    /// A malformed/unknown sub-command leaves the line's existing mark
    /// untouched. Pure state mutation: no grid write, no host reply, and the
    /// cursor / pending-wrap state is deliberately not touched.
    fn handle_osc133(&mut self, parts: &[&[u8]]) {
        // SH-CLICK: a prompt-start may carry a `click_events=N` directive that
        // enables (or withdraws) click-to-position. Apply it when explicitly
        // present; an absent attribute leaves the flag unchanged. This is
        // independent of the mark stamping below and never writes the grid.
        if let Some(directive) = prompt_marks::parse_click_events(parts) {
            self.click_events_enabled = matches!(directive, prompt_marks::ClickEvents::Enable);
        }
        let code = parts.first().and_then(|p| p.first()).copied();
        // OdyTTY-private edit-region report (`133;P;odytty-edit;len;cur[;nl]`,
        // B-DESIGN §3.1): a cooperating shell's line editor publishing its
        // authoritative buffer length + cursor on every redraw. Pure advisory
        // state for [`Self::input_region`]; no grid write, no mark stamping.
        // A malformed payload (or an unknown/versioned signal name) is ignored
        // and leaves existing state untouched.
        if code == Some(b'P') {
            if let Some(signal) = super::input_region::parse_edit_region_osc(parts) {
                self.active_edit_region = Some(signal);
            }
            return;
        }
        let Some(kind) = prompt_marks::parse_osc133(parts) else {
            return;
        };
        match code {
            Some(b'B') => {
                let absolute_row = self
                    .scrollback
                    .physical_len(self.dimensions.columns)
                    .saturating_add(self.cursor.row);
                self.active_prompt_input_start = Some((absolute_row, self.cursor.column));
            }
            Some(b'C' | b'D') => {
                self.active_prompt_input_start = None;
                self.active_edit_region = None;
                self.active_prompt_start = None;
            }
            Some(b'A') => {
                self.active_prompt_input_start = None;
                self.active_edit_region = None;
            }
            _ => {}
        }
        // Walk back to the first physical row of the cursor's logical line: a
        // row is a soft-wrap continuation iff its predecessor is `wrapped`.
        let mut row = self.cursor.row.min(self.rows.len().saturating_sub(1));
        while row > 0 && self.rows[row - 1].wrapped {
            row -= 1;
        }
        if let Some(line) = self.rows.get_mut(row) {
            line.prompt_mark = Some(kind);
            self.prompt_marks_changed = true;
        }
        if code == Some(b'A') {
            let absolute_row = self
                .scrollback
                .physical_len(self.dimensions.columns)
                .saturating_add(row);
            self.active_prompt_start = Some(absolute_row);
        }
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
        // The shell applied output: a width-changing resize that follows can
        // trust that a repaint is in the loop and honor the cursor-anchor
        // override (see `output_since_last_resize`).
        self.output_since_last_resize = true;

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

        if self.insert_mode {
            // IRM: open `width` blank cells at the cursor, shifting the rest of
            // the line right (cells past the edge drop off), then write into the
            // freshly cleared slot. `insert_chars` handles the right-edge
            // truncation and wide-pair sanitization.
            self.insert_chars(width);
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
            // A full-screen region (top row 0 through the last row) is
            // equivalent to no region: lines scrolled off the top leave the
            // screen entirely, so they must feed scrollback exactly as the
            // no-region path does. Many TUIs set `ESC[1;<rows>r` and never reset
            // it; routing that through the discard path silently breaks
            // scrollback. A PARTIAL region (content preserved above) still
            // discards, matching xterm.
            if self
                .scroll_region
                .is_some_and(|region| region.top == 0 && region.bottom + 1 == self.dimensions.rows)
            {
                self.scroll_up_full();
            } else {
                self.scroll_up_region();
            }
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
            b'\x07' => self.bell_pending = true,
            _ => {}
        }
    }

    /// Drain the BEL latch. Returns whether a bell was requested since the last
    /// drain and clears the flag. The native layer calls this once per frame.
    fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell_pending)
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
            // OSC 6 = report/set the per-document file URL (iTerm2 extension).
            // Accepted-and-ignored: OdyTTY tracks working directory via OSC 7
            // only, and OSC 6 carries no directory semantics we act on.
            b"6" => {}
            // OSC 7 = report the current working directory as a file:// URL.
            b"7" => {
                if let Some(cwd) = parse_osc7_cwd(&params[1..], self.local_hostname.as_deref()) {
                    self.set_working_directory(cwd);
                }
            }
            // OSC 133 = shell-integration semantic prompt marking (SH1). Parse
            // and stamp an advisory per-row mark; never touch the grid, never
            // reply. See [`Self::handle_osc133`].
            b"133" => self.handle_osc133(&params[1..]),
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
            // SGR is `CSI Ps … m` with no private-parameter prefix. `CSI > Ps ; Ps m`
            // is XTMODKEYS (set modifyOtherKeys), `CSI ? Ps m` / `CSI = Ps m` are
            // other private forms — none are SGR. Without this gate, the
            // `CSI > 4 ; 2 m` apps emit at startup to enable modifyOtherKeys was
            // parsed as SGR 4;2 (underline + dim) and smeared those attributes
            // across all subsequent text. Private/intermediate `m` forms are not
            // rendition changes; ignore them.
            'm' if intermediates.is_empty() => self.apply_sgr(params),
            'p' if intermediates == b"$" || intermediates == b"?$" => {
                self.request_mode_report(params, intermediates)
            }
            'p' if intermediates == b"!" => self.soft_reset(),
            'q' if intermediates == b">" => self.xtversion_report(params),
            'q' if intermediates == b"\"" => self.set_char_protection(param_or(params, 0, 0)),
            'q' if intermediates == b" " => self.set_cursor_style(param_or(params, 0, 0)),
            'r' if intermediates == b"$" => self.change_rect_attrs(params),
            'r' => self.set_scroll_region(params),
            's' => self.save_cursor(),
            't' if intermediates.is_empty() => self.window_ops_report(params),
            't' if intermediates == b"$" => self.reverse_rect_attrs(params),
            'u' if intermediates == b"?" => self.kitty_keyboard_query(params, intermediates),
            'u' if intermediates == b">" => self.kitty_keyboard_push(params, intermediates),
            'u' if intermediates == b"<" => self.kitty_keyboard_pop(params, intermediates),
            'u' if intermediates == b"=" => self.kitty_keyboard_set(params, intermediates),
            'u' => self.restore_cursor(),
            'v' if intermediates == b"$" => self.copy_rect(params),
            'x' if intermediates == b"*" => self.set_rect_attr_extent(param_or(params, 0, 0)),
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

    /// Drain the BEL latch (see [`Screen::take_bell`]). `true` means the host
    /// rang the bell at least once since the previous drain.
    pub fn take_bell(&mut self) -> bool {
        self.screen.take_bell()
    }

    pub fn set_osc52_read_enabled(&mut self, enabled: bool) {
        self.screen.set_osc52_read_enabled(enabled);
    }

    /// Set the scrollback retention cap in logical lines (`0` = unbounded). See
    /// [`Screen::set_scrollback_limit`].
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.screen.set_scrollback_limit(limit);
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

    /// DECSET 1007 (alternate scroll mode) state. See
    /// [`Screen::alternate_scroll_enabled`].
    pub fn alternate_scroll_enabled(&self) -> bool {
        self.screen.alternate_scroll_enabled()
    }

    /// Whether the alternate screen buffer is currently active. See
    /// [`Screen::on_alternate_screen`].
    pub fn on_alternate_screen(&self) -> bool {
        self.screen.on_alternate_screen()
    }

    /// The current window title (OSC 0/2), or `None` if never set.
    pub fn title(&self) -> Option<&str> {
        self.screen.title()
    }

    /// Whether the title changed since the last poll; clears the flag.
    pub fn take_title_changed(&mut self) -> bool {
        self.screen.take_title_changed()
    }

    /// The current working directory reported via OSC 7, or `None` if unset.
    pub fn current_working_directory(&self) -> Option<&str> {
        self.screen.current_working_directory()
    }

    /// Seed the advisory working directory from the local spawn cwd before the
    /// shell has a chance to emit OSC 7. Later OSC 7 updates still use the same
    /// parser/hostname policy and overwrite this seed.
    pub(crate) fn seed_working_directory(&mut self, cwd: String) {
        self.screen.set_working_directory(cwd);
    }

    /// Whether the working directory changed since the last poll; clears the
    /// flag.
    pub fn take_working_directory_changed(&mut self) -> bool {
        self.screen.take_working_directory_changed()
    }

    /// Set or clear the local hostname accepted by OSC 7. See
    /// [`Screen::set_local_hostname`].
    pub fn set_local_hostname(&mut self, local_hostname: Option<String>) {
        self.screen.set_local_hostname(local_hostname);
    }

    /// The OSC 133 prompt mark anchored to absolute row `row` (SH1), or `None`.
    /// Row `0` is the oldest scrollback row; see [`Screen::prompt_mark_at`].
    pub fn prompt_mark_at(&self, row: usize) -> Option<PromptKind> {
        self.screen.prompt_mark_at(row)
    }

    /// Every OSC 133 prompt mark as `(absolute_row, kind)` pairs in ascending
    /// row order (row `0` = oldest scrollback). The enumeration counterpart to
    /// [`Terminal::prompt_mark_at`]; see [`Screen::prompt_marks`].
    pub fn prompt_marks(&self) -> Vec<(usize, PromptKind)> {
        self.screen.prompt_marks()
    }

    /// Whether the set of prompt marks may have changed since the last poll
    /// (a new mark stamped, or marks cleared/repositioned by RIS, erase, resize,
    /// or an alternate-screen switch); clears the flag. See
    /// [`Screen::take_prompt_marks_changed`] for the conservative contract.
    pub fn take_prompt_marks_changed(&mut self) -> bool {
        self.screen.take_prompt_marks_changed()
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

    /// Whether OSC 133 click-to-position (SH-CLICK) is currently enabled by the
    /// shell; see [`Screen::click_events_enabled`].
    pub fn click_events_enabled(&self) -> bool {
        self.screen.click_events_enabled()
    }

    /// Active OSC 133 `B` input-start boundary as `(absolute_row, column)`;
    /// see [`Screen::active_prompt_input_start`].
    pub fn active_prompt_input_start(&self) -> Option<(usize, usize)> {
        self.screen.active_prompt_input_start()
    }

    /// The live editable prompt-input region; see [`Screen::input_region`].
    pub fn input_region(&self) -> Option<super::input_region::InputRegion> {
        self.screen.input_region()
    }

    pub fn hyperlink(&self, id: LinkId) -> Option<&Hyperlink> {
        self.screen.hyperlink(id)
    }

    #[cfg(test)]
    pub(crate) fn hyperlink_count_for_test(&self) -> usize {
        self.screen.hyperlink_count()
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

    pub fn dynamic_colors(&self) -> &DynamicColors {
        self.screen.dynamic_colors()
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

    /// Set whether the backend's shell owns cursor placement on resize.
    /// See [`Screen::set_shell_owns_cursor_on_resize`].
    pub fn set_shell_owns_cursor_on_resize(&mut self, value: bool) {
        self.screen.set_shell_owns_cursor_on_resize(value);
    }

    /// Whether the backend's shell owns cursor placement on resize.
    /// See [`Screen::shell_owns_cursor_on_resize`]. Test-only (see the Screen
    /// getter): production only sets this from the backend capability.
    #[cfg(test)]
    pub(crate) fn shell_owns_cursor_on_resize(&self) -> bool {
        self.screen.shell_owns_cursor_on_resize()
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

    /// Copy the constrained Phase 2 persistence subset into owned DTOs. See
    /// [`Screen::snapshot_state`].
    pub fn snapshot_state(&self, max_scrollback_rows: usize) -> SnapshotTerminalState {
        self.screen.snapshot_state(max_scrollback_rows)
    }

    /// Copy layout-affecting terminal state that is not part of the render
    /// [`Snapshot`] surface. See [`Screen::snapshot_layout_state`].
    pub fn snapshot_layout_state(&self) -> SnapshotLayoutState {
        self.screen.snapshot_layout_state()
    }

    pub fn visible_graphics(&self, offset_rows: usize) -> Vec<VisiblePlacement> {
        self.screen.visible_graphics(offset_rows)
    }

    /// Search the combined scrollback + visible buffer for `query`. See
    /// [`Screen::search`] for the coordinate convention and result ordering.
    pub fn search(&self, query: &str, options: SearchOptions) -> Vec<SearchMatch> {
        self.screen.search(query, options)
    }

    /// The visible viewport's physical rows (with `wrapped` flags) at scrollback
    /// `offset_rows`, for the hint / quick-select scanner. See
    /// [`Screen::visible_search_rows`] for the window and coordinate convention.
    pub fn visible_search_rows(&self, offset_rows: usize) -> Vec<VisibleRow> {
        self.screen.visible_search_rows(offset_rows)
    }

    /// Apply a decoded Phase 2 snapshot envelope into this terminal model.
    ///
    /// The parser is reset because the snapshot format stores terminal state,
    /// not an in-flight escape/DCS parser state.
    pub fn restore_from_envelope(
        &mut self,
        envelope: &SnapshotEnvelope,
    ) -> Result<(), SnapshotEnvelopeError> {
        self.screen.restore_from_envelope(envelope)?;
        self.parser = OdyParser::new();
        Ok(())
    }

    /// Construct a fresh terminal from a decoded Phase 2 snapshot envelope.
    pub fn from_snapshot_envelope(
        envelope: &SnapshotEnvelope,
    ) -> Result<Self, SnapshotEnvelopeError> {
        let mut terminal = Self::new(
            envelope.terminal.dimensions.columns,
            envelope.terminal.dimensions.rows,
        );
        terminal.restore_from_envelope(envelope)?;
        Ok(terminal)
    }
}
fn blank_stored_primary(dimensions: Dimensions) -> StoredScreen {
    StoredScreen {
        rows: vec![blank_row(dimensions.columns); dimensions.rows],
        scrollback: Scrollback::new(),
        cursor: Position::default(),
        cursor_visible: true,
        pending_wrap: false,
        saved_cursor: None,
        scroll_region: None,
        origin_mode: false,
        auto_wrap: true,
        current_attrs: Attrs::default(),
        current_protected: false,
        rect_attr_extent: RectAttributeExtent::default(),
        active_hyperlink: None,
        kitty_keyboard_flags: 0,
        kitty_keyboard_stack: Vec::new(),
        active_prompt_input_start: None,
        active_edit_region: None,
        active_prompt_start: None,
    }
}
fn restore_validate_terminal_state(
    state: &SnapshotTerminalState,
) -> Result<(), SnapshotEnvelopeError> {
    let columns = state.dimensions.columns;
    let rows = state.dimensions.rows;
    if columns == 0 || rows == 0 {
        return Err(SnapshotEnvelopeError::InvalidDimensions { columns, rows });
    }
    if state.cursor.row >= rows || state.cursor.column >= columns {
        return Err(SnapshotEnvelopeError::InvalidCursor {
            cursor: state.cursor,
        });
    }
    if state.visible_rows.len() != rows {
        return Err(SnapshotEnvelopeError::InvalidVisibleRowCount {
            count: state.visible_rows.len(),
            expected: rows,
        });
    }
    for row in state
        .scrollback_rows
        .iter()
        .chain(state.visible_rows.iter())
    {
        if row.cells.len() != columns {
            return Err(SnapshotEnvelopeError::InvalidRowWidth {
                width: row.cells.len(),
                columns,
            });
        }
    }
    let total_rows = state
        .scrollback_rows
        .len()
        .checked_add(state.visible_rows.len())
        .ok_or(SnapshotEnvelopeError::CellCapExceeded)?;
    total_rows
        .checked_mul(columns)
        .ok_or(SnapshotEnvelopeError::CellCapExceeded)?;
    Ok(())
}
fn restore_lines_from_snapshot_rows(
    rows: &[SnapshotRow],
    columns: usize,
) -> Result<Vec<Line>, SnapshotEnvelopeError> {
    let mut restored = Vec::with_capacity(rows.len());
    for row in rows {
        if row.cells.len() != columns {
            return Err(SnapshotEnvelopeError::InvalidRowWidth {
                width: row.cells.len(),
                columns,
            });
        }
        restored.push(Line {
            cells: row.cells.iter().map(|cell| cell.to_cell()).collect(),
            wrapped: row.wrapped,
            prompt_mark: None,
        });
    }
    Ok(restored)
}
fn restore_apply_prompt_marks(
    scrollback_rows: &mut [Line],
    visible_rows: &mut [Line],
    marks: &[SnapshotPromptMark],
) -> Result<(), SnapshotEnvelopeError> {
    let total_rows = scrollback_rows.len() + visible_rows.len();
    for mark in marks {
        if mark.row >= total_rows {
            return Err(SnapshotEnvelopeError::InvalidPromptMark {
                row: mark.row,
                rows: total_rows,
            });
        }
        if mark.row < scrollback_rows.len() {
            scrollback_rows[mark.row].prompt_mark = Some(mark.kind);
        } else {
            visible_rows[mark.row - scrollback_rows.len()].prompt_mark = Some(mark.kind);
        }
    }
    Ok(())
}
pub(in crate::core) fn blank_row(columns: usize) -> Line {
    Line::unwrapped(vec![Cell::blank(); columns])
}
fn snapshot_row_from_line(line: &Line, columns: usize) -> SnapshotRow {
    let mut cells: Vec<_> = line.iter().take(columns).copied().map(Into::into).collect();
    cells.resize_with(columns, || Cell::blank().into());
    SnapshotRow {
        wrapped: line.wrapped,
        cells,
    }
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
fn active_prompt_start_visible_row(
    active_prompt_start: Option<usize>,
    scrollback_rows: usize,
    cursor_row: usize,
    visible_rows: usize,
    width_unchanged: bool,
) -> Option<usize> {
    if width_unchanged {
        return None;
    }
    let start = active_prompt_start?;
    let visible = start.checked_sub(scrollback_rows)?;
    (visible < visible_rows && visible <= cursor_row).then_some(visible)
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
/// Materialize the SGR parameter groups as slices for the attribute decoder.
///
/// A CSI private-marker byte (`<`, `=`, `>`, `?`) is recorded by the parser as a
/// sequence *intermediate*, never as a parameter value (see the parser's
/// `CsiEntry`/`CsiParam` ParamMarker handling), so it never reaches this list.
/// This is therefore a straight pass-through: every group is a real numeric
/// parameter. Historically a value-equality filter dropped any group whose first
/// value equaled 60–63 (the ASCII codes of `<=>?`), which silently corrupted any
/// 24-bit color (`38;2;R;G;B` / `48` / `58`) with a channel of 60/61/62/63 —
/// fixed by removing that filter, since the marker is tracked structurally.
fn sgr_params(params: &Params) -> Vec<&[u16]> {
    params.iter().collect()
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
