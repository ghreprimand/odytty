// SPDX-License-Identifier: GPL-3.0-only
//! The owned terminal state machine: the [`Screen`] grid (primary + alternate),
//! the [`Terminal`] facade that drives OdyTTY's owned parser, scrollback, scroll
//! regions, resize reflow, and the CSI/OSC/SGR dispatch helpers. This is the
//! bulk of the terminal core; it builds on [`super::types`] and is exercised by
//! `super::tests`.

use unicode_width::UnicodeWidthChar;

use crate::graphics::{ImageScene, VisiblePlacement};
use crate::parser::{OdyParser, Params, VtDispatch};

use super::button::{
    ButtonHit, ButtonIcon, ButtonScope, ButtonSignal, ButtonSpan, ButtonState, ButtonTable,
    MAX_BUTTON_SPANS_PER_LINE, line_content_end, parse_button_osc_133, parse_button_osc_1337,
    point_chip_rect,
};
use super::graphics_routing::{self, DcsCapture, GraphicsStats};
use super::hyperlink::{Hyperlink, HyperlinkTable};
use super::kitty::{decode_base64_bytes, encode_base64_bytes};
use super::placeholder;

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

mod charset;
mod ops;
mod osc;
mod query;
mod rect;
mod state;
mod terminal;
mod view;

pub use terminal::Terminal;

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
    /// Button spans anchored to this physical row, in row-local columns
    /// (Button Protocol B1). Like `prompt_mark`, this is advisory sidecar
    /// state: no render-path code reads it yet and it never reaches the
    /// [`Snapshot`]. Empty (no allocation) for the common button-free row.
    pub(in crate::core) button_spans: Vec<ButtonSpan>,
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

/// A button projected onto a viewport row for rendering (Button Protocol B2).
///
/// Resolved from a line-anchored [`ButtonSpan`] plus its interned
/// [`ButtonEntry`](super::button::ButtonEntry) at snapshot time, so the render
/// layer paints chips without reaching into the private button table. The
/// coordinates are viewport-relative and column-local, matching the cells the
/// render [`Snapshot`] draws for the same viewport offset, so a caller can
/// index straight into the flattened cell grid.
///
/// This rides a side-channel accessor ([`Screen::visible_button_spans`]) rather
/// than a field on [`Snapshot`], mirroring how prompt marks and graphics
/// placements are exposed for render without widening the text-snapshot
/// surface (see [`Screen::visible_graphics`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotButton {
    /// Viewport row, `0` at the top of the visible grid.
    pub row: usize,
    /// Row-local start column: the first label cell, or — for a Tier 1 point
    /// button — the first cell of the resolved chip rect
    /// ([`super::button::point_chip_rect`]).
    pub start_col: usize,
    /// Rect length in cells: the label-run length, or the resolved chip-rect
    /// width for a Tier 1 point button. Never `0` — a point chip with no room
    /// on its row is not emitted at all.
    pub len: usize,
    /// The terminal-composed report code this button carries.
    pub code: u32,
    /// Semantic icon for the chip (platform-neutral).
    pub icon: ButtonIcon,
    /// Live or invalidated: an invalidated button keeps painting (grayed) while
    /// visible lines still reference it, but no longer activates.
    pub state: ButtonState,
    /// `true` for a Tier 1 point button: `start_col`/`len` describe the
    /// resolved chip rect and the render layer draws the `icon code` pill
    /// into it. `false` for a label run, which re-styles its own label cells.
    pub point: bool,
}

impl Line {
    /// A row that ends a logical line (hard break / no continuation).
    pub(in crate::core) fn unwrapped(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            wrapped: false,
            prompt_mark: None,
            button_spans: Vec::new(),
        }
    }

    /// A row that soft-wraps into the next physical row.
    pub(in crate::core) fn wrapped(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            wrapped: true,
            prompt_mark: None,
            button_spans: Vec::new(),
        }
    }
}
/// How an in-place row mutation moved or destroyed cell content, for
/// transforming the row's button-span sidecars in lockstep — see
/// [`Screen::transform_row_button_spans`]. Shaped as a description of the
/// cell edit (not a per-sequence variant) so future sidecars can ride the
/// same mutation description.
#[derive(Debug, Clone, Copy)]
pub(super) enum RowButtonMutation {
    /// `count` blank cells inserted at `at` (ICH): cells at and right of `at`
    /// shift right, overflow past the right edge is discarded.
    InsertShift { at: usize, count: usize },
    /// `count` cells deleted at `at` (DCH): cells right of the deleted range
    /// shift left, blanks fill at the right edge.
    DeleteShift { at: usize, count: usize },
    /// Cells in `[start, end)` overwritten or erased in place with no
    /// shifting (ECH, EL 0/1, DECSEL, DECERA, DECSERA, DECFRA, and the
    /// DECCRA destination rectangle).
    Overwrite { start: usize, end: usize },
}

/// The pure per-span half of [`Screen::transform_row_button_spans`]: the
/// span's new coordinates after `mutation`, or `None` when the mutation
/// destroys it and its table reference must be released. `columns` bounds the
/// row; spans shifted fully past it die, spans shifted partially past it are
/// clipped to the surviving label cells.
fn transform_button_span(
    span: ButtonSpan,
    mutation: RowButtonMutation,
    columns: usize,
) -> Option<ButtonSpan> {
    match mutation {
        RowButtonMutation::InsertShift { at, count } => {
            if span.start_col >= at {
                let start_col = span.start_col.saturating_add(count);
                if start_col >= columns {
                    return None;
                }
                let len = span.len.min(columns - start_col);
                Some(ButtonSpan {
                    start_col,
                    len,
                    ..span
                })
            } else if span.len > 0 && span.start_col + span.len > at {
                // The insertion point pierces the label interior: the label is
                // torn in two and no longer matches the span.
                None
            } else {
                Some(span)
            }
        }
        RowButtonMutation::DeleteShift { at, count } => {
            // A zero-length point anchor occupies its anchor cell for
            // deletion purposes: deleting that cell deletes the anchor.
            let occupied_end = span.start_col + span.len.max(1);
            if occupied_end <= at {
                Some(span)
            } else if span.start_col >= at.saturating_add(count) {
                Some(ButtonSpan {
                    start_col: span.start_col - count,
                    ..span
                })
            } else {
                // Any overlap with the deleted range destroys label cells.
                None
            }
        }
        RowButtonMutation::Overwrite { start, end } => {
            if span.len > 0 && span.start_col < end && span.start_col + span.len > start {
                None
            } else {
                // Point anchors survive: their chips re-resolve against blank
                // cells at render and hit-test time, so overwritten cells
                // stop showing the chip without a stale clickable region.
                Some(span)
            }
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
    kitty_named_transports_enabled: bool,
    /// BEL (`0x07`) latch. Set when the host writes a bell control; drained by
    /// the native layer once per frame to drive the visual/urgency bell. The
    /// core never makes noise or touches the grid — it only records that a bell
    /// was requested.
    bell_pending: bool,
    base_colors: DynamicColors,
    /// C29: the theme's base 16 ANSI colors, seeded by the native layer via
    /// [`Self::set_base_palette`]. OSC 4 queries for indices 0..16 fall back
    /// here (then to the xterm table) so applications probing the palette see
    /// the colors actually rendered, not a hardcoded xterm table.
    base_palette: [RgbColor; 16],
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
    /// G0/G1 charset designations + SO/SI GL selection (ACS line drawing).
    /// Kept per active screen (saved/reset on alternate-screen entry, restored
    /// on exit — the kitty-flag pattern) so a TUI's graphics designation never
    /// leaks into the primary prompt. Saved/restored by DECSC/DECRC; reset by
    /// RIS and DECSTR.
    charsets: CharsetModes,
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
    /// on a click on the live input region, synthesizes cursor-key presses
    /// (F2). Default-off means the emit path is inert until the app opts in.
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
    /// Interned button entries (Button Protocol B1). Bounded by construction:
    /// span refcounts couple entry lifetime to the scrollback ring, with a
    /// hard refuse-new-at-ceiling entry cap. See [`super::button`].
    buttons: ButtonTable,
    /// Master gate for the button protocol. Off (the default): both button
    /// spellings are parsed and consumed — keeping the parser total and the
    /// no-grid-write invariant exercised — but create no table entry, no span,
    /// and no observable state, so feature-off output is byte-identical. The
    /// native layer will also gate the future pointer arm on this (both
    /// chokepoints, so no partial-gate hole).
    buttons_enabled: bool,
    /// Sub-gate: accept the iTerm2 `OSC 1337 ; Button=` spelling. Inert while
    /// `buttons_enabled` is off.
    buttons_iterm_compat: bool,
    /// Sub-gate: honor `scope=sticky`. When off, every definition is
    /// block-scoped regardless of the emitter's request.
    buttons_sticky: bool,
    /// An open Tier 2 bracketed button run: definition received, `end` not
    /// yet. The label cells printed in between become the button's span(s).
    active_button_run: Option<ActiveButtonRun>,
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
    modify_other_keys: u8,
    /// Charset designations + GL selection saved with the primary screen
    /// (same per-screen isolation rationale as the kitty keyboard flags: an
    /// alternate-screen TUI's `ESC ( 0` must not leak line-drawing mode into
    /// the primary prompt on exit).
    charsets: CharsetModes,
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
    attrs: Attrs,
    origin_mode: bool,
    auto_wrap: bool,
    protected: bool,
    active_hyperlink: Option<LinkId>,
    /// DECSC saves the G0/G1 designations and GL selection alongside the
    /// cursor (VT100 behavior); DECRC restores them.
    charsets: CharsetModes,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollRegion {
    top: usize,
    bottom: usize,
}

/// An open Tier 2 button run (`odytty-button;code=…` received, `end` pending).
/// The start is anchored in absolute physical rows (scrollback height plus
/// visible row, the `active_prompt_input_start` convention) so intervening
/// scrolling is detected; a start that scrolls out of the visible grid (or a
/// resize, alternate-screen switch, or block boundary) cancels the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveButtonRun {
    id: super::button::ButtonId,
    /// Start row in "pushed rows + visible row" units: a monotonic scroll
    /// counter plus the visible row, so intervening scroll-out is detected
    /// without projecting scrollback (the projection-free sibling of the
    /// absolute-row convention).
    start_abs_row: u64,
    start_col: usize,
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
                return;
            }
            // OdyTTY-private button run (`133;P;odytty-button;…`, Button
            // Protocol B1). Parsed totally; acted on only behind the master
            // gate. Signal names the parser does not know are ignored, so
            // both private namespaces stay forward-versioned.
            if let Some(signal) = parse_button_osc_133(parts) {
                self.handle_button_signal(signal);
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
                // A command-block boundary ends every block-scoped button's
                // life (D = command done). `C` (output start) belongs to the
                // same command the buttons were defined in, so only `D`
                // invalidates here; `A` handles the new-prompt boundary below.
                if code == Some(b'D') {
                    self.end_button_block();
                }
            }
            Some(b'A') => {
                self.end_button_block();
                self.active_prompt_input_start = None;
                self.active_edit_region = None;
                // A primary-screen prompt boundary means no TUI still owns a
                // partial DECSTBM region. Clear leaked margins here while
                // preserving full-screen and alternate-screen application state.
                if self.primary_screen.is_none()
                    && self.scroll_region.is_some_and(|region| {
                        region.top != 0 || region.bottom != self.dimensions.rows - 1
                    })
                {
                    self.scroll_region = None;
                }
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
            // Merge, not overwrite: shells emit `D` and the next prompt's `A`
            // back to back on one row, and the prompt stamp must preserve the
            // displaced exit status (see `prompt_marks::merge_mark`).
            line.prompt_mark = Some(prompt_marks::merge_mark(line.prompt_mark, kind));
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

    /// Handle an OSC 1337 payload. Only `Button=` carries modeled semantics
    /// (Tier 1 of the button protocol); every other payload in the namespace
    /// is recognized and consumed with no state. Parsing always runs (parser
    /// totality is exercised regardless of configuration); state changes are
    /// gated on the master `buttons` gate AND the iTerm2-compat sub-gate.
    fn handle_osc1337(&mut self, parts: &[&[u8]]) {
        let Some(signal) = parse_button_osc_1337(parts) else {
            return;
        };
        if !self.buttons_enabled || !self.buttons_iterm_compat {
            return;
        }
        match signal {
            ButtonSignal::Define { code, icon, scope } => {
                // Tier 1 buttons carry no label run: anchor a zero-length span
                // at the cursor on the current physical row (rendered later as
                // an overlay chip at the line's content end).
                if self.primary_screen.is_some() {
                    // v1 refuses button definitions on the alternate screen:
                    // TUIs there own the mouse via real mouse reporting.
                    return;
                }
                let scope = self.effective_scope(scope);
                let row = self.cursor.row.min(self.rows.len().saturating_sub(1));
                if self.rows[row].button_spans.len() >= MAX_BUTTON_SPANS_PER_LINE {
                    return;
                }
                let Some(id) = self.buttons.define(code, icon, scope) else {
                    return;
                };
                self.rows[row].button_spans.push(ButtonSpan {
                    id,
                    start_col: self.cursor.column,
                    len: 0,
                });
                self.buttons.attach(id);
            }
            // The empty-code form invalidates ALL buttons (iTerm2 semantics);
            // a Tier 1-only emitter has no narrower spelling.
            ButtonSignal::InvalidateAll => self.buttons.invalidate_all(),
            ButtonSignal::End | ButtonSignal::InvalidateCode(_) | ButtonSignal::Ignored => {}
        }
    }

    /// Act on a parsed Tier 2 button signal (`133;P;odytty-button`). Master
    /// gate enforced here — the OSC chokepoint; the future pointer arm gates
    /// independently so no partial-gate hole exists.
    fn handle_button_signal(&mut self, signal: ButtonSignal) {
        if !self.buttons_enabled {
            return;
        }
        match signal {
            ButtonSignal::Define { code, icon, scope } => {
                // A new definition supersedes any run left open (defensive:
                // a well-formed emitter closes runs with `end`).
                self.cancel_button_run();
                if self.primary_screen.is_some() {
                    return;
                }
                let scope = self.effective_scope(scope);
                let Some(id) = self.buttons.define(code, icon, scope) else {
                    return;
                };
                // Anchor at the position the NEXT printed cell will occupy: a
                // pending-wrap cursor sits past the right edge, so the label
                // begins on the following row.
                let base = self.scrollback.pushed_row_count();
                let (row, col) = if self.pending_wrap {
                    (self.cursor.row + 1, 0)
                } else {
                    (self.cursor.row, self.cursor.column)
                };
                self.active_button_run = Some(ActiveButtonRun {
                    id,
                    start_abs_row: base + row as u64,
                    start_col: col,
                });
            }
            ButtonSignal::End => self.finish_button_run(),
            ButtonSignal::InvalidateAll => {
                self.cancel_button_run();
                self.buttons.invalidate_all();
            }
            ButtonSignal::InvalidateCode(code) => self.buttons.invalidate_code(code),
            ButtonSignal::Ignored => {}
        }
    }

    /// Downgrade a requested scope to `Block` when the sticky sub-gate is off.
    fn effective_scope(&self, scope: ButtonScope) -> ButtonScope {
        if scope == ButtonScope::Sticky && !self.buttons_sticky {
            ButtonScope::Block
        } else {
            scope
        }
    }

    /// Close the open Tier 2 run: stamp the bracketed cells as button spans on
    /// the physical rows between the run's start and the cursor, one row-local
    /// segment per row, each holding one table reference. Degenerate runs
    /// (empty label) anchor a zero-length span, matching the Tier 1 shape. A
    /// run whose start scrolled out of the visible grid (a label taller than
    /// the window — pathological) is canceled rather than half-stamped.
    fn finish_button_run(&mut self) {
        let Some(run) = self.active_button_run.take() else {
            return;
        };
        let base = self.scrollback.pushed_row_count();
        let width = self.dimensions.columns;
        let end_abs = base + self.cursor.row as u64;
        let end_col = if self.pending_wrap {
            self.cursor.column + 1
        } else {
            self.cursor.column
        };
        if run.start_abs_row < base || run.start_abs_row > end_abs {
            self.buttons.release_if_unreferenced(run.id);
            return;
        }
        let start_row = (run.start_abs_row - base) as usize;
        let end_row = self.cursor.row.min(self.rows.len().saturating_sub(1));
        let mut attached_any = false;
        for row in start_row..=end_row {
            let seg_start = if row == start_row { run.start_col } else { 0 };
            let seg_end = if row == end_row { end_col } else { width };
            if seg_end <= seg_start {
                continue;
            }
            let line = &mut self.rows[row];
            if line.button_spans.len() >= MAX_BUTTON_SPANS_PER_LINE {
                continue;
            }
            line.button_spans.push(ButtonSpan {
                id: run.id,
                start_col: seg_start,
                len: seg_end - seg_start,
            });
            self.buttons.attach(run.id);
            attached_any = true;
        }
        if !attached_any {
            // Empty label: keep the button as a point anchor so the emitter's
            // definition is not silently lost.
            let row = start_row.min(self.rows.len().saturating_sub(1));
            if self.rows[row].button_spans.len() < MAX_BUTTON_SPANS_PER_LINE {
                self.rows[row].button_spans.push(ButtonSpan {
                    id: run.id,
                    start_col: run.start_col.min(width.saturating_sub(1)),
                    len: 0,
                });
                self.buttons.attach(run.id);
            } else {
                self.buttons.release_if_unreferenced(run.id);
            }
        }
    }

    /// Abandon an open Tier 2 run (resize, alternate-screen switch, block
    /// boundary, superseding definition, reset). The interned entry is freed
    /// only if nothing else references it.
    fn cancel_button_run(&mut self) {
        if let Some(run) = self.active_button_run.take() {
            self.buttons.release_if_unreferenced(run.id);
        }
    }

    /// OSC 133 `A`/`D` boundary: every block-scoped button's life ends and any
    /// open run is abandoned. Cheap no-op while the table is empty.
    fn end_button_block(&mut self) {
        self.cancel_button_run();
        if !self.buttons.is_empty() {
            self.buttons.invalidate_block_scoped();
        }
    }

    /// Surrender the button-span references of a line leaving canonical
    /// storage by discard (region scrolls, IL/DL, wholesale erase) — the
    /// counterpart of the scrollback drain for rows that never reach the ring.
    pub(super) fn release_line_buttons(&mut self, line: &Line) {
        if line.button_spans.is_empty() {
            return;
        }
        for span in &line.button_spans {
            self.buttons.release(span.id);
        }
    }

    /// Transform one row's button-span sidecars in lockstep with an in-place
    /// cell mutation, releasing the table reference of every span the
    /// mutation destroys. Every row-range mutation that moves or overwrites
    /// cells WITHOUT replacing the row wholesale must route through this
    /// helper — the wholesale paths (EL2, ED, IL/DL, region scrolls) already
    /// release via [`Self::release_line_buttons`], and reflow re-projects.
    /// Without it, a shifted or erased label leaves the button clickable over
    /// the wrong cells.
    ///
    /// Semantics per mutation, applied span-by-span:
    /// - shift (ICH/DCH): a span at or right of the edit point moves with its
    ///   cells; a shift past the right edge clips the label and releases a
    ///   span pushed fully off. A span whose interior the edit point pierces
    ///   (insert into the label, delete overlapping it) is released — its
    ///   cells no longer form the label it was anchored to.
    /// - overwrite/erase (ECH, EL 0/1, DECSEL, DECERA, DECSERA, DECFRA, and
    ///   the DECCRA destination): any labeled span overlapping the written
    ///   range is released, including the DECSERA case where protected cells
    ///   survive — a partially surviving label no longer matches its span. A
    ///   zero-length point anchor is kept: its chip re-resolves against blank
    ///   cells at render time, so overwritten cells simply stop showing it.
    /// - copy (DECCRA): spans are NOT copied to the destination — a button is
    ///   defined by its protocol sequence, and silently duplicating clickable
    ///   regions from a rectangle copy would mint activations the program
    ///   never placed. The destination is treated as a plain overwrite.
    pub(super) fn transform_row_button_spans(&mut self, row: usize, mutation: RowButtonMutation) {
        if self.rows[row].button_spans.is_empty() {
            return;
        }
        let columns = self.dimensions.columns;
        let spans = std::mem::take(&mut self.rows[row].button_spans);
        let mut kept = Vec::with_capacity(spans.len());
        for span in spans {
            match transform_button_span(span, mutation, columns) {
                Some(span) => kept.push(span),
                None => self.buttons.release(span.id),
            }
        }
        self.rows[row].button_spans = kept;
    }

    /// Decrement table refcounts for span references the scrollback store
    /// surrendered (ring eviction, front-drain, clear). Cheap `is_empty` gate
    /// so the hot scroll path pays one branch when buttons are unused.
    pub(super) fn drain_freed_button_refs(&mut self) {
        if !self.scrollback.has_freed_button_ids() {
            return;
        }
        for id in self.scrollback.take_freed_button_ids() {
            self.buttons.release(id);
        }
    }

    /// Recompute button refcounts from an authoritative walk of canonical
    /// storage (live rows + scrollback logical lines). The resize/reflow paths
    /// re-project spans wholesale, so incremental accounting is replaced by
    /// this rebuild afterwards. No-op while the table is empty.
    pub(super) fn rebuild_button_refcounts(&mut self) {
        if self.buttons.is_empty() {
            // Nothing to rebuild; discard any surrendered references too.
            if self.scrollback.has_freed_button_ids() {
                self.scrollback.take_freed_button_ids();
            }
            return;
        }
        // The rebuild supersedes the incremental drop accounting.
        self.scrollback.take_freed_button_ids();
        let mut ids = Vec::new();
        self.scrollback.collect_button_ids(&mut ids);
        for row in &self.rows {
            ids.extend(row.button_spans.iter().map(|span| span.id));
        }
        self.buttons.rebuild_refcounts(ids);
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
        // Charset seam: translate through DEC Special Graphics BEFORE width
        // computation and `last_graphic_char` capture, so the grid, wrap
        // logic, and REP all operate on the final Unicode glyph. Only
        // single-byte-range characters (`0x5F..=0x7E`) can map; multi-byte
        // UTF-8 decodes above that range and passes through untouched. The
        // map is idempotent, so a REP replay of a stored translated glyph is
        // unaffected even if the charset changed in between.
        let ch = if self.charsets.active_graphics() && matches!(ch, '\x5f'..='\x7e') {
            charset::dec_special_graphics(ch)
        } else {
            ch
        };
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
        if let Some(region) = self
            .scroll_region
            .filter(|region| self.cursor.row == region.bottom)
        {
            if region.top == 0 && region.bottom + 1 == self.dimensions.rows {
                // A full-screen region (top row 0 through the last row) is
                // equivalent to no region: lines scrolled off the top leave the
                // screen entirely, so they must feed scrollback exactly as the
                // no-region path does. Many TUIs set `ESC[1;<rows>r` and never
                // reset it; routing that through the discard path silently
                // breaks scrollback.
                self.scroll_up_full();
            } else if region.top == 0 && self.primary_screen.is_none() {
                // A TOP-ANCHORED PARTIAL region (top row 0 with a footer
                // preserved below the bottom margin) is what a full-screen TUI
                // sets when it reserves a bottom input composer, e.g.
                // `ESC[1;<rows-1>r`. The content above the margin is real
                // scrollable history, so the row leaving the top must feed
                // scrollback for wheel-up to reveal it, while the footer rows
                // below the margin stay fixed. Primary screen only; the
                // alternate screen never accumulates scrollback.
                self.scroll_up_region_into_scrollback();
            } else {
                // A partial region anchored below row 0, or the alternate
                // screen: the row leaving the top is discarded, matching xterm.
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
            // SO/SI (LS1/LS0): select G1/G0 into GL for subsequent printed
            // characters. Pure charset-state switches — no cursor, wrap, or
            // grid effect, so the grid is not marked dirty.
            b'\x0e' => self.charsets.gl_g1 = true,
            b'\x0f' => self.charsets.gl_g1 = false,
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
            // OSC 1337 = iTerm2 extension namespace. Only the Button= payload
            // is modeled (Button Protocol B1, master-gated); everything else
            // is recognized and consumed with no state. Never touches the
            // grid, never replies. See [`Self::handle_osc1337`].
            b"1337" => self.handle_osc1337(&params[1..]),
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
            // is XTMODKEYS (set modifyOtherKeys), `CSI ? Ps m` is XTQMODKEYS
            // (query), `CSI = Ps m` is another private form — none are SGR.
            // Without this gate, the `CSI > 4 ; 2 m` apps emit at startup to
            // enable modifyOtherKeys was parsed as SGR 4;2 (underline + dim)
            // and smeared those attributes across all subsequent text.
            // Private/intermediate `m` forms are never rendition changes: the
            // XTMODKEYS/XTQMODKEYS arms below own `>`/`?`, everything else is
            // ignored.
            'm' if intermediates.is_empty() => self.apply_sgr(params),
            'm' if intermediates == b">" => self.xtmodkeys_set(params),
            'm' if intermediates == b"?" => self.xtqmodkeys_report(params),
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
        if ignore {
            return;
        }

        // Charset designation: `ESC ( Final` designates G0, `ESC ) Final`
        // designates G1. Only DEC Special Graphics (`0`) is modeled; every
        // other final — `B` (ASCII) and the national replacement sets alike —
        // designates ASCII as the safe fallback, so an unknown designator can
        // never wedge the charset state or panic (parser totality).
        match intermediates {
            b"(" => {
                self.charsets.g0_graphics = byte == b'0';
                return;
            }
            b")" => {
                self.charsets.g1_graphics = byte == b'0';
                return;
            }
            [] => {}
            // Other intermediates (ESC # 8 DECALN etc.) remain unhandled.
            _ => return,
        }

        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            // IND (ESC D): move down one row; at the bottom margin, scroll the
            // active region up by one — exactly the LF motion (column
            // untouched). Routing through `line_feed` inherits the shared
            // scroll paths' C16 wrapped-flag seams and the full-screen-region
            // scrollback equivalence.
            b'D' => self.line_feed(),
            // NEL (ESC E): IND + carriage return (next line, column 0).
            b'E' => {
                self.line_feed();
                self.carriage_return();
            }
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
            self.kitty_named_transports_enabled,
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
/// bytes; protocol decoding remains in later graphics work.
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
        modify_other_keys: 0,
        charsets: CharsetModes::default(),
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
            // Buttons are session-local interned state; a restored snapshot
            // starts with no spans (the envelope carries none).
            button_spans: Vec::new(),
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
