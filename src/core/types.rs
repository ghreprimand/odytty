//! Core terminal data types: geometry, colors, attributes, the [`Cell`]
//! grapheme model, the mouse-reporting enums, and the rendering [`Snapshot`] /
//! [`TerminalModel`] surface. These are the leaf types the screen state machine
//! and the mouse encoders build on; this module depends on nothing else in
//! `core`.

use std::num::NonZeroU32;

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

/// Pixel dimensions of a single terminal cell, used by the graphics routing
/// layer to compute cell-extent from pixel-sized images (e.g. Sixel/Kitty).
///
/// The headless default is 8×16 px (a common fixed-width cell metric). The
/// native layer overrides this at startup and on every rescale/font rebuild
/// via [`super::screen::Screen::set_cell_metrics`]. Values are clamped to
/// `[1, 1024]` — zero is never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    pub width_px: u32,
    pub height_px: u32,
}

impl CellMetrics {
    /// Headless default: 8×16 px cell (matches the former provisional constants).
    pub const DEFAULT: Self = Self {
        width_px: 8,
        height_px: 16,
    };

    /// Construct with clamping: each dimension is clamped to `[1, 1024]`.
    pub fn new(width_px: u32, height_px: u32) -> Self {
        Self {
            width_px: width_px.clamp(1, 1024),
            height_px: height_px.clamp(1, 1024),
        }
    }
}

impl Default for CellMetrics {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub row: usize,
    pub column: usize,
}
/// Which mouse events the host has asked to receive, selected via DECSET/DECRST
/// of the private modes 9/1000/1002/1003.
///
/// xterm stores these in a single state variable rather than as independent
/// bits: each DECSET overwrites the active tracking mode (so a later DECSET
/// wins), and a DECRST of *any* tracking mode returns to [`Off`](Self::Off).
/// This mirrors xterm's `send_mouse_pos` handling and is what real apps that
/// reset their modes (`?1000l ?1002l ?1003l`) rely on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    /// No reporting (default / after any tracking DECRST).
    #[default]
    Off,
    /// Mode 9 (X10): button press only, no modifiers, no release, no motion.
    X10,
    /// Mode 1000 (normal): button press and release.
    Normal,
    /// Mode 1002 (button-event): press, release, and motion while a button is held.
    ButtonEvent,
    /// Mode 1003 (any-event): press, release, and all motion.
    AnyEvent,
}
/// How mouse coordinates and buttons are encoded on the wire, selected via
/// DECSET/DECRST of the private modes 1005/1006/1015. As with tracking, xterm
/// keeps a single active encoding: a later DECSET wins and a DECRST of any
/// extension returns to [`Default`](Self::Default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    /// Legacy X10 byte encoding: `CSI M Cb Cx Cy`, each value offset by 32.
    /// Coordinates above 223 cannot be represented and are dropped.
    #[default]
    Default,
    /// Mode 1005: legacy layout but each value encoded as UTF-8, extending the
    /// representable range.
    Utf8,
    /// Mode 1006 (SGR): `CSI < Cb ; Cx ; Cy M|m` with decimal, unbounded
    /// coordinates and a distinct release terminator (`m`).
    Sgr,
    /// Mode 1015 (urxvt): `CSI Cb ; Cx ; Cy M` with decimal values.
    Urxvt,
}
/// The active mouse reporting protocol: which events to report and how to
/// encode them. Exposed so a front end can decide what to send without
/// reaching into terminal internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseProtocol {
    pub tracking: MouseTracking,
    pub encoding: MouseEncoding,
}
impl MouseProtocol {
    /// Whether any mouse reporting is active.
    pub fn is_enabled(&self) -> bool {
        self.tracking != MouseTracking::Off
    }
}
/// Keyboard modes that affect the byte sequences a front end should send for
/// cursor and keypad keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyboardModes {
    /// DECCKM (`CSI ? 1 h/l`): unmodified cursor keys use application SS3
    /// forms while enabled.
    pub application_cursor: bool,
    /// DECKPAM/DECKPNM (`ESC =` / `ESC >`): keypad keys use application keypad
    /// SS3 forms while enabled.
    pub application_keypad: bool,
    /// Kitty keyboard protocol flags active on the current screen buffer.
    ///
    /// Zero preserves legacy DEC/xterm key encoding. Nonzero values are set by
    /// Kitty's CSI-u keyboard protocol controls and consumed by front ends when
    /// encoding key presses.
    pub kitty_keyboard_flags: u16,
}
/// A mouse button (or wheel direction) for [`encode_mouse_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// No button held. Used for any-event (1003) hover motion, which xterm
    /// encodes with the "no button" code 3 plus the motion flag. Reported only
    /// in any-event tracking; button-event (1002) drops no-button motion.
    NoButton,
}
/// The kind of mouse event being reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    /// Pointer motion (a drag in button-event mode, or any move in any-event mode).
    Motion,
}
/// Keyboard modifiers held during a mouse event. Folded into the button code
/// for every encoding except X10 tracking, which carries no modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
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
pub struct RgbColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl RgbColor {
    pub fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    pub fn tuple(self) -> (u8, u8, u8) {
        (self.red, self.green, self.blue)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicColors {
    pub foreground: RgbColor,
    pub background: RgbColor,
    pub cursor: RgbColor,
    pub palette: [Option<RgbColor>; 256],
}

impl DynamicColors {
    pub fn palette_color(&self, index: u8) -> Option<RgbColor> {
        self.palette[index as usize]
    }
}

impl Default for DynamicColors {
    fn default() -> Self {
        Self {
            foreground: RgbColor::new(0xCC, 0xCC, 0xCC),
            background: RgbColor::new(0x0B, 0x0C, 0x10),
            cursor: RgbColor::new(0xCC, 0xCC, 0xCC),
            palette: [None; 256],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    Clipboard,
    Primary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardRequest {
    Write {
        selection: ClipboardSelection,
        text: String,
    },
    Read {
        selection: ClipboardSelection,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub foreground: Color,
    pub background: Color,
    /// Interned OSC 8 hyperlink associated with this cell, if any.
    pub hyperlink: Option<LinkId>,
}

impl std::fmt::Debug for Attrs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut attrs = f.debug_struct("Attrs");
        attrs
            .field("bold", &self.bold)
            .field("dim", &self.dim)
            .field("italic", &self.italic)
            .field("underline", &self.underline)
            .field("strikethrough", &self.strikethrough)
            .field("inverse", &self.inverse)
            .field("hidden", &self.hidden)
            .field("foreground", &self.foreground)
            .field("background", &self.background);
        if let Some(hyperlink) = self.hyperlink {
            attrs.field("hyperlink", &hyperlink);
        }
        attrs.finish()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId(NonZeroU32);

impl LinkId {
    pub(crate) fn new(id: NonZeroU32) -> Self {
        Self(id)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}
/// Maximum zero-width combining marks stored per cell. Marks beyond this are
/// dropped — a bounded limitation; common diacritics use one or two marks.
pub(crate) const MAX_COMBINING: usize = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Base character of the cell's grapheme cluster. Width 1, or width 2 for a
    /// wide lead; a `wide_continuation` spacer carries `' '`. The renderer draws
    /// this glyph; zero-width combining marks attached to it are read via
    /// [`Cell::combining`] / [`Cell::grapheme`].
    pub ch: char,
    pub attrs: Attrs,
    /// True for the trailing spacer cell of a wide (two-column) glyph.
    pub wide_continuation: bool,
    /// Zero-width combining marks attached to `ch`, in arrival order. Unused
    /// slots hold `'\0'`. Private so the invariant (`combining_len` marks, the
    /// rest zeroed) stays internal; constructors keep `Cell: Copy`.
    combining: [char; MAX_COMBINING],
    combining_len: u8,
}
impl Cell {
    /// A single-width cell carrying `ch` with `attrs` and no combining marks.
    pub fn new(ch: char, attrs: Attrs) -> Self {
        Self {
            ch,
            attrs,
            wide_continuation: false,
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        }
    }

    /// The trailing spacer cell of a wide glyph, inheriting `attrs`.
    pub fn wide_spacer(attrs: Attrs) -> Self {
        Self {
            ch: ' ',
            attrs,
            wide_continuation: true,
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        }
    }

    pub fn blank() -> Self {
        Self::blank_with_bg(Color::Default)
    }

    pub fn blank_with_bg(background: Color) -> Self {
        let attrs = Attrs {
            background,
            ..Attrs::default()
        };

        Self::new(' ', attrs)
    }

    /// Zero-width combining marks attached to this cell's base char, in order.
    /// Empty for the common case. The renderer composes `ch` followed by these.
    pub fn combining(&self) -> &[char] {
        &self.combining[..self.combining_len as usize]
    }

    /// Append a combining mark. Returns `false` (dropping the mark) once the
    /// per-cell capacity is reached — a bounded limitation.
    pub(crate) fn push_combining(&mut self, mark: char) -> bool {
        let len = self.combining_len as usize;
        if len < MAX_COMBINING {
            self.combining[len] = mark;
            self.combining_len += 1;
            true
        } else {
            false
        }
    }

    /// The full grapheme cluster: the base char followed by any combining marks.
    pub fn grapheme(&self) -> String {
        let mut s = String::with_capacity(1 + self.combining_len as usize);
        s.push(self.ch);
        for &mark in self.combining() {
            s.push(mark);
        }
        s
    }
}
impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}
/// Visual shape of the text cursor, selected by DECSCUSR (`CSI Ps SP q`) or the
/// host default policy. Terminal semantics only — how each shape is drawn is the
/// renderer's concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    /// Full-cell block (the power-on default).
    #[default]
    Block,
    /// Thin horizontal bar along the cell's baseline/bottom edge.
    Underline,
    /// Thin vertical bar at the cell's left edge.
    Bar,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub dimensions: Dimensions,
    pub cursor: Position,
    pub cursor_visible: bool,
    pub colors: DynamicColors,
    pub cells: Vec<Cell>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("dimensions", &self.dimensions)
            .field("cursor", &self.cursor)
            .field("cursor_visible", &self.cursor_visible)
            .field("cells", &self.cells)
            .finish()
    }
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
