// SPDX-License-Identifier: GPL-3.0-only
//! COPYMODE — vim-key keyboard scrollback selection (pure-core state machine).
//!
//! This module is the headless heart of COPYMODE (Phase 5): a keyboard-driven
//! selection cursor that navigates the scrollback buffer with vim motions and
//! derives a selectable range. It is **pure logic** — no GPU, no winit, no
//! clipboard, no key routing — so it is fully unit-testable without a frame.
//!
//! ## Ownership boundary
//!
//! COPYMODE reuses the mouse-selection SSOT [`crate::selection`] **read-only**:
//! it produces an [`AbsoluteSelectionRange`] from its own cursor/anchor state
//! and re-uses the shared absolute-coordinate helpers
//! ([`normalize_absolute_range`], [`viewport_top_absolute_row`]) and the
//! word-character predicate ([`is_selection_word_char`]). It defines its own
//! state here and **edits nothing** in `selection.rs`. The host (a later wiring
//! stage) is the only thing that turns a derived range into a painted
//! selection (`AbsoluteSelectionState::set_range`) or a clipboard write.
//!
//! ## Coordinate space
//!
//! The cursor and anchor are stored in **absolute** coordinates (the same space
//! the mouse selection uses) so they stay pinned to *content* — not to screen
//! rows — while scrollback grows under an open copy-mode session. Conversion to
//! the visible viewport happens only when reading grid characters (word / blank
//! scans), via the viewport metrics carried in [`CopyModeContext`].
//!
//! ## Scope (v1 core)
//!
//! This module is **core only**: the model + motions + range derivation, wired to
//! nothing. Activation (a bindable action, key routing, mutual exclusion with
//! search/overlay), the cursor render, and the yank-to-clipboard hand-off are a
//! separate later native change. Numeric motion counts (`3w`) and `Ctrl-v`
//! block selection are deferred (the latter shares one implementation with the
//! mouse MOUSE-RECT item); a clean [`SelectKind::Block`] seam is left for it.
//!
//! The public surface here is **wired to nothing yet** (core-only);
//! the later activation + key-routing layer is its first consumer.
//! Until then these items are unreferenced from non-test code, so the module
//! opts out of `dead_code` — the allow becomes a no-op once wiring lands.
#![allow(dead_code)]

use crate::core::Snapshot;
use crate::selection::{
    AbsoluteCellPoint, AbsoluteSelectionRange, is_selection_word_char, normalize_absolute_range,
    viewport_top_absolute_row,
};

/// Sentinel `end` column meaning "to the end of the row" for line-wise
/// selection. The host clamps it to the real row width when converting the
/// absolute range to a visible range (`selection::visible_range_from_absolute`
/// already clamps `end.column` to `columns - 1`), so the pure model never needs
/// to know the grid width to express a whole-line selection.
pub const LINE_END_COLUMN: usize = usize::MAX;

/// What kind of selection the cursor is currently driving.
///
/// `Normal` = a navigable cursor with no anchored selection. `Char` grows a
/// character-wise range from the anchor; `Line` spans full rows. `Block` is the
/// reserved MOUSE-RECT seam — column/rectangular selection lands later with the
/// mouse block-select item so a single block-range implementation serves both;
/// it is intentionally not constructed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectKind {
    #[default]
    Normal,
    Char,
    Line,
    /// Reserved column/rectangular selection (MOUSE-RECT). Not constructed in
    /// v1 core; range derivation returns `None` for it until block selection is wired.
    Block,
}

/// A partially-entered multi-key motion. Only `gg` (go-to-top) needs one in v1;
/// numeric counts are deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// A leading `g` was pressed; a second `g` jumps to the top of scrollback.
    G,
}

/// A normalized copy-mode key. The host (wiring layer) maps raw winit key
/// events to these; the pure model only ever sees the semantic key, so its
/// behavior is testable headless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyModeKey {
    /// `h` / Left.
    MoveLeft,
    /// `j` / Down.
    MoveDown,
    /// `k` / Up.
    MoveUp,
    /// `l` / Right.
    MoveRight,
    /// `0` — column 0.
    ColumnZero,
    /// `^` — first non-blank column of the cursor row.
    FirstNonBlank,
    /// `$` — last non-blank column of the cursor row.
    LineEnd,
    /// `w` — start of the next word.
    WordForward,
    /// `b` — start of the previous word.
    WordBackward,
    /// `e` — end of the next word.
    WordEnd,
    /// `g` — leading key of the `gg` go-to-top motion.
    GPrefix,
    /// `G` — bottom of the buffer (live).
    GotoBottom,
    /// `Ctrl-u` — half page up.
    HalfPageUp,
    /// `Ctrl-d` — half page down.
    HalfPageDown,
    /// `Ctrl-b` / PageUp — full page up.
    PageUp,
    /// `Ctrl-f` / PageDown — full page down.
    PageDown,
    /// `v` — toggle character-wise selection.
    ToggleCharSelect,
    /// `V` — toggle line-wise selection.
    ToggleLineSelect,
    /// `o` — swap the cursor and anchor ends of the selection.
    SwapEnds,
    /// `y` / Enter — yank: the host copies [`CopyModeState::range`] then exits.
    Yank,
    /// `Esc` / `q` — clear the selection if any, else exit copy mode.
    Cancel,
}

/// The control signal a key application returns to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyModeResponse {
    /// Stay in copy mode; state was updated in place.
    Continue,
    /// Yank requested: the host should copy [`CopyModeState::range`] (which may
    /// be `None` for a degenerate range) and then exit copy mode.
    Yank,
    /// Exit copy mode without copying.
    Exit,
}

/// Read-only view of the grid + viewport the motions resolve against. Borrowed
/// per key application so the model never owns a snapshot.
pub struct CopyModeContext<'a> {
    /// The currently visible viewport snapshot (row-major cells).
    pub snapshot: &'a Snapshot,
    /// Rows scrolled back from live (0 = live bottom).
    pub viewport_offset: usize,
    /// Number of scrollback rows above the live screen.
    pub scrollback_len: usize,
    /// C24: character at an absolute cell NOT currently on screen (scrollback
    /// above the viewport, or live rows below it while scrolled back), so
    /// word/line motions resolve against the whole buffer instead of stopping
    /// at the viewport edges. `None` (or a provider returning `None`) keeps
    /// off-screen cells opaque — word scans then treat them as non-word
    /// content and stop, which is the safe degradation.
    pub offscreen_cell: Option<&'a dyn Fn(AbsoluteCellPoint) -> Option<char>>,
}

impl CopyModeContext<'_> {
    fn rows(&self) -> usize {
        self.snapshot.dimensions.rows
    }

    fn columns(&self) -> usize {
        self.snapshot.dimensions.columns
    }

    fn last_column(&self) -> usize {
        self.columns().saturating_sub(1)
    }

    /// First absolute row currently visible at the top of the viewport.
    fn top_row(&self) -> usize {
        viewport_top_absolute_row(self.viewport_offset, self.scrollback_len)
    }

    /// Last absolute row that holds content (the live screen's bottom row).
    /// Independent of the scroll offset — total content height is
    /// `scrollback_len + rows`.
    fn max_row(&self) -> usize {
        (self.scrollback_len + self.rows()).saturating_sub(1)
    }

    /// Map an absolute row to its visible viewport row, or `None` if it is not
    /// currently on screen.
    fn visible_row(&self, abs_row: usize) -> Option<usize> {
        let top = self.top_row();
        if abs_row < top {
            return None;
        }
        let v = abs_row - top;
        (v < self.rows()).then_some(v)
    }

    /// Character at an absolute cell, if the cell is in bounds. On-screen cells
    /// read the viewport snapshot; off-screen cells (C24) go through the
    /// `offscreen_cell` provider so motions can resolve against the absolute
    /// buffer. Without a provider, off-screen cells are opaque (`None`).
    fn cell_char(&self, p: AbsoluteCellPoint) -> Option<char> {
        let cols = self.columns();
        if p.column >= cols || p.row > self.max_row() {
            return None;
        }
        if let Some(vrow) = self.visible_row(p.row) {
            return self
                .snapshot
                .cells
                .get(vrow * cols + p.column)
                .map(|c| c.ch);
        }
        self.offscreen_cell.and_then(|fetch| fetch(p))
    }

    /// Next cell in row-major order within the absolute buffer, or `None` at
    /// the end of content (C24: no longer stops at the viewport bottom).
    fn step_forward(&self, p: AbsoluteCellPoint) -> Option<AbsoluteCellPoint> {
        let cols = self.columns();
        if cols == 0 {
            return None;
        }
        if p.column + 1 < cols {
            Some(AbsoluteCellPoint {
                row: p.row,
                column: p.column + 1,
            })
        } else if p.row < self.max_row() {
            Some(AbsoluteCellPoint {
                row: p.row + 1,
                column: 0,
            })
        } else {
            None
        }
    }

    /// Previous cell in row-major order within the absolute buffer, or `None`
    /// at the start of content (C24: no longer stops at the viewport top).
    fn step_back(&self, p: AbsoluteCellPoint) -> Option<AbsoluteCellPoint> {
        if p.column > 0 {
            Some(AbsoluteCellPoint {
                row: p.row,
                column: p.column - 1,
            })
        } else if p.row > 0 {
            Some(AbsoluteCellPoint {
                row: p.row - 1,
                column: self.last_column(),
            })
        } else {
            None
        }
    }
}

/// A blank cell for `^` / `$` scanning: a space or the default null glyph.
fn is_blank(ch: char) -> bool {
    ch == ' ' || ch == '\0'
}

/// The pure copy-mode model: a keyboard caret + optional anchored selection in
/// absolute coordinates. All state transitions are deterministic pure functions
/// of `(self, key, ctx)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyModeState {
    cursor: AbsoluteCellPoint,
    anchor: Option<AbsoluteCellPoint>,
    mode: SelectKind,
    pending: Option<Pending>,
}

impl CopyModeState {
    /// Enter copy mode with the caret at `cursor` (absolute coords; the host
    /// typically seeds it at the live cursor or viewport bottom). No selection
    /// is anchored yet — the caret is free to navigate first.
    pub fn new(cursor: AbsoluteCellPoint) -> Self {
        Self {
            cursor,
            anchor: None,
            mode: SelectKind::Normal,
            pending: None,
        }
    }

    /// The keyboard caret (absolute coords).
    pub fn cursor(&self) -> AbsoluteCellPoint {
        self.cursor
    }

    /// The selection anchor, if a selection has been started.
    pub fn anchor(&self) -> Option<AbsoluteCellPoint> {
        self.anchor
    }

    /// The active selection kind.
    pub fn mode(&self) -> SelectKind {
        self.mode
    }

    /// Whether a selection is currently anchored (`v` / `V` started, not yet
    /// cleared).
    pub fn is_selecting(&self) -> bool {
        self.anchor.is_some() && self.mode != SelectKind::Normal
    }

    /// The current selection range derived from `(anchor, cursor, mode)`, or
    /// `None` for no / degenerate selection. Character-wise uses the shared
    /// [`normalize_absolute_range`]; line-wise spans full rows with the
    /// [`LINE_END_COLUMN`] sentinel the host clamps. Pure function of state —
    /// it does not consult the viewport, so it survives scrollback growth.
    pub fn range(&self) -> Option<AbsoluteSelectionRange> {
        let anchor = self.anchor?;
        match self.mode {
            SelectKind::Char => normalize_absolute_range(anchor, self.cursor),
            SelectKind::Line => {
                let (top, bot) = if anchor.row <= self.cursor.row {
                    (anchor.row, self.cursor.row)
                } else {
                    (self.cursor.row, anchor.row)
                };
                Some(AbsoluteSelectionRange {
                    start: AbsoluteCellPoint {
                        row: top,
                        column: 0,
                    },
                    end: AbsoluteCellPoint {
                        row: bot,
                        column: LINE_END_COLUMN,
                    },
                })
            }
            SelectKind::Normal | SelectKind::Block => None,
        }
    }

    /// Apply one normalized key, mutating the caret / selection in place and
    /// returning the host control signal.
    pub fn apply(&mut self, key: CopyModeKey, ctx: &CopyModeContext) -> CopyModeResponse {
        // A `g` prefix is only honored when immediately followed by `g`; any
        // other key consumes (clears) it and is processed normally.
        let pending = self.pending.take();

        match key {
            CopyModeKey::GPrefix => {
                if pending == Some(Pending::G) {
                    self.cursor.row = 0;
                    self.clamp_column(ctx);
                } else {
                    self.pending = Some(Pending::G);
                }
                CopyModeResponse::Continue
            }
            CopyModeKey::MoveLeft => {
                self.cursor.column = self.cursor.column.saturating_sub(1);
                CopyModeResponse::Continue
            }
            CopyModeKey::MoveRight => {
                self.cursor.column = (self.cursor.column + 1).min(ctx.last_column());
                CopyModeResponse::Continue
            }
            CopyModeKey::MoveUp => {
                self.cursor.row = self.cursor.row.saturating_sub(1);
                self.clamp_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::MoveDown => {
                self.cursor.row = (self.cursor.row + 1).min(ctx.max_row());
                self.clamp_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::ColumnZero => {
                self.cursor.column = 0;
                CopyModeResponse::Continue
            }
            CopyModeKey::FirstNonBlank => {
                self.cursor.column = self.first_non_blank_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::LineEnd => {
                self.cursor.column = self.last_non_blank_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::WordForward => {
                self.word_forward(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::WordBackward => {
                self.word_backward(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::WordEnd => {
                self.word_end(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::GotoBottom => {
                self.cursor.row = ctx.max_row();
                self.clamp_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::HalfPageUp => {
                self.cursor.row = self.cursor.row.saturating_sub(ctx.rows() / 2);
                self.clamp_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::HalfPageDown => {
                self.cursor.row = (self.cursor.row + ctx.rows() / 2).min(ctx.max_row());
                self.clamp_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::PageUp => {
                self.cursor.row = self.cursor.row.saturating_sub(ctx.rows());
                self.clamp_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::PageDown => {
                self.cursor.row = (self.cursor.row + ctx.rows()).min(ctx.max_row());
                self.clamp_column(ctx);
                CopyModeResponse::Continue
            }
            CopyModeKey::ToggleCharSelect => {
                self.toggle_select(SelectKind::Char);
                CopyModeResponse::Continue
            }
            CopyModeKey::ToggleLineSelect => {
                self.toggle_select(SelectKind::Line);
                CopyModeResponse::Continue
            }
            CopyModeKey::SwapEnds => {
                self.swap_ends();
                CopyModeResponse::Continue
            }
            CopyModeKey::Yank => CopyModeResponse::Yank,
            CopyModeKey::Cancel => {
                if self.anchor.is_some() {
                    self.anchor = None;
                    self.mode = SelectKind::Normal;
                    CopyModeResponse::Continue
                } else {
                    CopyModeResponse::Exit
                }
            }
        }
    }

    fn clamp_column(&mut self, ctx: &CopyModeContext) {
        self.cursor.column = self.cursor.column.min(ctx.last_column());
    }

    fn first_non_blank_column(&self, ctx: &CopyModeContext) -> usize {
        let cols = ctx.columns();
        for col in 0..cols {
            let here = AbsoluteCellPoint {
                row: self.cursor.row,
                column: col,
            };
            if ctx.cell_char(here).is_some_and(|ch| !is_blank(ch)) {
                return col;
            }
        }
        0
    }

    fn last_non_blank_column(&self, ctx: &CopyModeContext) -> usize {
        let mut col = ctx.last_column();
        loop {
            let here = AbsoluteCellPoint {
                row: self.cursor.row,
                column: col,
            };
            if ctx.cell_char(here).is_some_and(|ch| !is_blank(ch)) {
                return col;
            }
            if col == 0 {
                return 0;
            }
            col -= 1;
        }
    }

    fn is_word(ctx: &CopyModeContext, p: AbsoluteCellPoint) -> bool {
        ctx.cell_char(p).is_some_and(is_selection_word_char)
    }

    /// `w` — advance to the start of the next word.
    fn word_forward(&mut self, ctx: &CopyModeContext) {
        let mut p = self.cursor;
        if Self::is_word(ctx, p) {
            // Skip the remainder of the current word.
            while Self::is_word(ctx, p) {
                match ctx.step_forward(p) {
                    Some(n) => p = n,
                    None => break,
                }
            }
        } else if let Some(n) = ctx.step_forward(p) {
            p = n;
        }
        // Skip separators to the next word start.
        while ctx
            .cell_char(p)
            .is_some_and(|ch| !is_selection_word_char(ch))
        {
            match ctx.step_forward(p) {
                Some(n) => p = n,
                None => break,
            }
        }
        self.cursor = p;
    }

    /// `b` — retreat to the start of the previous word.
    fn word_backward(&mut self, ctx: &CopyModeContext) {
        let mut p = match ctx.step_back(self.cursor) {
            Some(n) => n,
            None => return,
        };
        // Skip separators backward onto the previous word's last cell.
        while ctx
            .cell_char(p)
            .is_some_and(|ch| !is_selection_word_char(ch))
        {
            match ctx.step_back(p) {
                Some(n) => p = n,
                None => break,
            }
        }
        // Walk back to that word's first cell.
        while let Some(prev) = ctx.step_back(p) {
            if Self::is_word(ctx, prev) {
                p = prev;
            } else {
                break;
            }
        }
        self.cursor = p;
    }

    /// `e` — advance to the end of the next word.
    fn word_end(&mut self, ctx: &CopyModeContext) {
        let mut p = match ctx.step_forward(self.cursor) {
            Some(n) => n,
            None => return,
        };
        // Skip separators forward onto the next word's first cell.
        while ctx
            .cell_char(p)
            .is_some_and(|ch| !is_selection_word_char(ch))
        {
            match ctx.step_forward(p) {
                Some(n) => p = n,
                None => break,
            }
        }
        // Walk forward to that word's last cell.
        while let Some(next) = ctx.step_forward(p) {
            if Self::is_word(ctx, next) {
                p = next;
            } else {
                break;
            }
        }
        self.cursor = p;
    }

    /// Toggle a selection of `kind`: turning it off if already active, else
    /// (re)anchoring at the caret and switching to that kind (so `V` then `v`
    /// keeps the anchor and switches char-wise, like vim).
    fn toggle_select(&mut self, kind: SelectKind) {
        if self.mode == kind {
            self.mode = SelectKind::Normal;
            self.anchor = None;
        } else {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
            self.mode = kind;
        }
    }

    /// `o` — swap the caret and the anchor, jumping to the other end of the
    /// selection. No-op when nothing is anchored.
    fn swap_ends(&mut self) {
        if let Some(anchor) = self.anchor {
            self.anchor = Some(self.cursor);
            self.cursor = anchor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Attrs, Cell, Dimensions, DynamicColors, Position, Snapshot};

    /// Build a viewport snapshot from text rows (padded to `columns`).
    fn snapshot(lines: &[&str], columns: usize) -> Snapshot {
        let rows = lines.len();
        let mut cells = Vec::new();
        for line in lines {
            let mut chars = line.chars().collect::<Vec<_>>();
            chars.resize(columns, ' ');
            cells.extend(
                chars
                    .into_iter()
                    .take(columns)
                    .map(|ch| Cell::new(ch, Attrs::default())),
            );
        }
        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position::default(),
            cursor_visible: true,
            colors: DynamicColors::default(),
            cells,
        }
    }

    /// Live context (offset 0, no scrollback) so absolute == visible.
    fn ctx(snapshot: &Snapshot) -> CopyModeContext<'_> {
        CopyModeContext {
            snapshot,
            viewport_offset: 0,
            scrollback_len: 0,
            offscreen_cell: None,
        }
    }

    fn at(row: usize, column: usize) -> AbsoluteCellPoint {
        AbsoluteCellPoint { row, column }
    }

    #[test]
    fn new_state_has_no_selection() {
        let s = CopyModeState::new(at(2, 3));
        assert_eq!(s.cursor(), at(2, 3));
        assert_eq!(s.anchor(), None);
        assert_eq!(s.mode(), SelectKind::Normal);
        assert!(!s.is_selecting());
        assert_eq!(s.range(), None);
    }

    #[test]
    fn basic_moves_and_clamps() {
        let snap = snapshot(&["abcd", "efgh", "ijkl"], 4);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(1, 1));

        s.apply(CopyModeKey::MoveLeft, &c);
        assert_eq!(s.cursor(), at(1, 0));
        // Clamp at left edge.
        s.apply(CopyModeKey::MoveLeft, &c);
        assert_eq!(s.cursor(), at(1, 0));

        s.apply(CopyModeKey::MoveRight, &c);
        assert_eq!(s.cursor(), at(1, 1));
        s.apply(CopyModeKey::MoveUp, &c);
        assert_eq!(s.cursor(), at(0, 1));
        // Clamp at top.
        s.apply(CopyModeKey::MoveUp, &c);
        assert_eq!(s.cursor(), at(0, 1));

        s.apply(CopyModeKey::MoveDown, &c);
        s.apply(CopyModeKey::MoveDown, &c);
        assert_eq!(s.cursor(), at(2, 1));
        // Clamp at bottom (max_row = 2).
        s.apply(CopyModeKey::MoveDown, &c);
        assert_eq!(s.cursor(), at(2, 1));
    }

    #[test]
    fn move_right_clamps_at_last_column() {
        let snap = snapshot(&["abcd"], 4);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 2));
        s.apply(CopyModeKey::MoveRight, &c);
        assert_eq!(s.cursor(), at(0, 3));
        s.apply(CopyModeKey::MoveRight, &c);
        assert_eq!(s.cursor(), at(0, 3));
    }

    #[test]
    fn column_zero_first_and_last_non_blank() {
        let snap = snapshot(&["  hi world  "], 12);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 5));

        s.apply(CopyModeKey::ColumnZero, &c);
        assert_eq!(s.cursor(), at(0, 0));

        s.apply(CopyModeKey::FirstNonBlank, &c);
        assert_eq!(s.cursor(), at(0, 2)); // first 'h'

        s.apply(CopyModeKey::LineEnd, &c);
        assert_eq!(s.cursor(), at(0, 9)); // last 'd' before trailing spaces
    }

    #[test]
    fn first_non_blank_on_all_blank_row_is_zero() {
        let snap = snapshot(&["    "], 4);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 2));
        s.apply(CopyModeKey::FirstNonBlank, &c);
        assert_eq!(s.cursor(), at(0, 0));
        s.apply(CopyModeKey::LineEnd, &c);
        assert_eq!(s.cursor(), at(0, 0));
    }

    #[test]
    fn word_forward_backward_end() {
        // columns: 0123456789...
        //          "foo bar  baz"
        let snap = snapshot(&["foo bar  baz"], 12);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 0));

        s.apply(CopyModeKey::WordForward, &c);
        assert_eq!(s.cursor(), at(0, 4)); // 'b' of bar
        s.apply(CopyModeKey::WordForward, &c);
        assert_eq!(s.cursor(), at(0, 9)); // 'b' of baz

        s.apply(CopyModeKey::WordBackward, &c);
        assert_eq!(s.cursor(), at(0, 4)); // back to 'b' of bar
        s.apply(CopyModeKey::WordBackward, &c);
        assert_eq!(s.cursor(), at(0, 0)); // back to 'f' of foo

        s.apply(CopyModeKey::WordEnd, &c);
        assert_eq!(s.cursor(), at(0, 2)); // 'o' end of foo
        s.apply(CopyModeKey::WordEnd, &c);
        assert_eq!(s.cursor(), at(0, 6)); // 'r' end of bar
    }

    #[test]
    fn word_forward_crosses_rows() {
        // Padded to 4 cols: row 0 = "foo ", row 1 = "bar " — the trailing pad
        // is the separator, so the next word starts on the following row.
        let snap = snapshot(&["foo", "bar"], 4);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 0));
        s.apply(CopyModeKey::WordForward, &c);
        assert_eq!(s.cursor(), at(1, 0)); // wraps to 'b' on next row
    }

    /// C24: `w` at the end of the viewport's LAST visible row continues into
    /// the live row BELOW the viewport (scrolled back, live content below).
    /// Pre-fix, `step_forward` stopped at the viewport bottom and the caret
    /// stuck on the last visible word.
    #[test]
    fn word_forward_crosses_below_viewport_bottom() {
        // 2-row viewport of a 3-row buffer: scrollback_len=1, offset=1 →
        // top_row=0, visible abs rows 0..=1; abs row 2 (live bottom) is
        // off-screen below. Provider serves its content.
        let snap = snapshot(&["foo", "bar"], 4);
        let below = |p: AbsoluteCellPoint| -> Option<char> {
            (p.row == 2).then(|| "baz ".chars().nth(p.column)).flatten()
        };
        let c = CopyModeContext {
            snapshot: &snap,
            viewport_offset: 1,
            scrollback_len: 1,
            offscreen_cell: Some(&below),
        };
        let mut s = CopyModeState::new(at(1, 0)); // 'b' of bar (last visible row)
        s.apply(CopyModeKey::WordForward, &c);
        assert_eq!(s.cursor(), at(2, 0), "w continues onto the off-screen row");
    }

    /// C24: `b` at the start of the viewport's FIRST visible row continues into
    /// the scrollback row ABOVE the viewport. Pre-fix, `step_back` stopped at
    /// the viewport top.
    #[test]
    fn word_backward_crosses_above_viewport_top() {
        // scrollback_len=1, offset=0 → top_row=1, visible abs rows 1..=2;
        // abs row 0 is scrollback above the viewport.
        let snap = snapshot(&["bar", "baz"], 4);
        let above = |p: AbsoluteCellPoint| -> Option<char> {
            (p.row == 0).then(|| "foo ".chars().nth(p.column)).flatten()
        };
        let c = CopyModeContext {
            snapshot: &snap,
            viewport_offset: 0,
            scrollback_len: 1,
            offscreen_cell: Some(&above),
        };
        let mut s = CopyModeState::new(at(1, 0)); // 'b' of bar (first visible row)
        s.apply(CopyModeKey::WordBackward, &c);
        assert_eq!(
            s.cursor(),
            at(0, 0),
            "b reaches the word start in scrollback"
        );
    }

    /// C24 degradation: without a provider, off-screen cells are opaque — a
    /// word scan stops rather than walking blind.
    #[test]
    fn word_motion_without_provider_stops_at_opaque_rows() {
        let snap = snapshot(&["bar", "baz"], 4);
        let c = CopyModeContext {
            snapshot: &snap,
            viewport_offset: 0,
            scrollback_len: 1,
            offscreen_cell: None,
        };
        let mut s = CopyModeState::new(at(1, 0));
        s.apply(CopyModeKey::WordBackward, &c);
        // Steps into abs row 0 are allowed (the caret may be placed anywhere),
        // but the scan finds no word chars there and settles at its edge.
        assert!(s.cursor().row <= 1, "no panic, bounded motion");
    }

    #[test]
    fn gg_jumps_to_top_g_jumps_to_bottom() {
        let snap = snapshot(&["a", "b", "c", "d"], 1);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(2, 0));

        // Single GPrefix sets pending, does not move.
        s.apply(CopyModeKey::GPrefix, &c);
        assert_eq!(s.cursor(), at(2, 0));
        // Second GPrefix completes gg -> top.
        s.apply(CopyModeKey::GPrefix, &c);
        assert_eq!(s.cursor(), at(0, 0));

        s.apply(CopyModeKey::GotoBottom, &c);
        assert_eq!(s.cursor(), at(3, 0)); // max_row
    }

    #[test]
    fn g_prefix_cleared_by_other_key() {
        let snap = snapshot(&["a", "b", "c", "d"], 1);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(2, 0));
        s.apply(CopyModeKey::GPrefix, &c); // pending g
        s.apply(CopyModeKey::MoveUp, &c); // clears pending, moves up
        assert_eq!(s.cursor(), at(1, 0));
        // A following GPrefix must NOT complete gg (pending was cleared).
        s.apply(CopyModeKey::GPrefix, &c);
        assert_eq!(s.cursor(), at(1, 0));
    }

    #[test]
    fn page_motions_clamp_at_edges() {
        // 6 visible rows -> half page = 3, full page = 6.
        let snap = snapshot(&["0", "1", "2", "3", "4", "5"], 1);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 0));

        s.apply(CopyModeKey::HalfPageDown, &c);
        assert_eq!(s.cursor(), at(3, 0));
        s.apply(CopyModeKey::HalfPageDown, &c);
        assert_eq!(s.cursor(), at(5, 0)); // clamps at bottom
        s.apply(CopyModeKey::HalfPageUp, &c);
        assert_eq!(s.cursor(), at(2, 0));

        s.apply(CopyModeKey::PageUp, &c);
        assert_eq!(s.cursor(), at(0, 0)); // clamps at top
        s.apply(CopyModeKey::PageDown, &c);
        assert_eq!(s.cursor(), at(5, 0)); // clamps at bottom
    }

    #[test]
    fn char_selection_range_and_degenerate_none() {
        let snap = snapshot(&["abcdef"], 6);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 1));

        s.apply(CopyModeKey::ToggleCharSelect, &c);
        assert!(s.is_selecting());
        // Single cell (anchor == cursor) is degenerate -> None.
        assert_eq!(s.range(), None);

        s.apply(CopyModeKey::MoveRight, &c);
        s.apply(CopyModeKey::MoveRight, &c);
        assert_eq!(
            s.range(),
            Some(AbsoluteSelectionRange {
                start: at(0, 1),
                end: at(0, 3),
            })
        );
    }

    #[test]
    fn char_selection_range_is_order_independent() {
        let snap = snapshot(&["abcdef"], 6);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 4));
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        s.apply(CopyModeKey::MoveLeft, &c);
        s.apply(CopyModeKey::MoveLeft, &c);
        // anchor=4, cursor=2 -> normalized start=2,end=4.
        assert_eq!(
            s.range(),
            Some(AbsoluteSelectionRange {
                start: at(0, 2),
                end: at(0, 4),
            })
        );
    }

    #[test]
    fn line_selection_spans_full_rows() {
        let snap = snapshot(&["aaa", "bbb", "ccc"], 3);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 1));
        s.apply(CopyModeKey::ToggleLineSelect, &c);
        // Single row line selection is a whole line (non-degenerate).
        assert_eq!(
            s.range(),
            Some(AbsoluteSelectionRange {
                start: at(0, 0),
                end: AbsoluteCellPoint {
                    row: 0,
                    column: LINE_END_COLUMN,
                },
            })
        );
        s.apply(CopyModeKey::MoveDown, &c);
        assert_eq!(
            s.range(),
            Some(AbsoluteSelectionRange {
                start: at(0, 0),
                end: AbsoluteCellPoint {
                    row: 1,
                    column: LINE_END_COLUMN,
                },
            })
        );
    }

    #[test]
    fn toggle_off_clears_selection() {
        let snap = snapshot(&["abc"], 3);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 0));
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        assert!(s.is_selecting());
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        assert!(!s.is_selecting());
        assert_eq!(s.anchor(), None);
        assert_eq!(s.range(), None);
    }

    #[test]
    fn line_to_char_keeps_anchor() {
        let snap = snapshot(&["abcdef"], 6);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 2));
        s.apply(CopyModeKey::ToggleLineSelect, &c);
        assert_eq!(s.mode(), SelectKind::Line);
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        assert_eq!(s.mode(), SelectKind::Char);
        assert_eq!(s.anchor(), Some(at(0, 2))); // anchor preserved across switch
    }

    #[test]
    fn swap_ends_jumps_to_other_end() {
        let snap = snapshot(&["abcdef"], 6);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 1));
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        s.apply(CopyModeKey::MoveRight, &c);
        s.apply(CopyModeKey::MoveRight, &c); // cursor=3, anchor=1
        s.apply(CopyModeKey::SwapEnds, &c);
        assert_eq!(s.cursor(), at(0, 1));
        assert_eq!(s.anchor(), Some(at(0, 3)));
        // Range is unchanged by the swap.
        assert_eq!(
            s.range(),
            Some(AbsoluteSelectionRange {
                start: at(0, 1),
                end: at(0, 3),
            })
        );
    }

    #[test]
    fn swap_ends_without_selection_is_noop() {
        let snap = snapshot(&["abc"], 3);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 1));
        s.apply(CopyModeKey::SwapEnds, &c);
        assert_eq!(s.cursor(), at(0, 1));
        assert_eq!(s.anchor(), None);
    }

    #[test]
    fn cancel_clears_then_exits() {
        let snap = snapshot(&["abc"], 3);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 0));
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        // First cancel clears the selection but stays active.
        assert_eq!(s.apply(CopyModeKey::Cancel, &c), CopyModeResponse::Continue);
        assert!(!s.is_selecting());
        // Second cancel (no selection) exits.
        assert_eq!(s.apply(CopyModeKey::Cancel, &c), CopyModeResponse::Exit);
    }

    #[test]
    fn yank_signals_and_preserves_range() {
        let snap = snapshot(&["abcdef"], 6);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 0));
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        s.apply(CopyModeKey::MoveRight, &c);
        s.apply(CopyModeKey::MoveRight, &c);
        let before = s.range();
        assert_eq!(s.apply(CopyModeKey::Yank, &c), CopyModeResponse::Yank);
        // Yank does not mutate the range; the host reads it after the signal.
        assert_eq!(s.range(), before);
        assert_eq!(
            s.range(),
            Some(AbsoluteSelectionRange {
                start: at(0, 0),
                end: at(0, 2),
            })
        );
    }

    #[test]
    fn absolute_coords_survive_scrollback_growth() {
        // Selection anchored in a scrolled-up viewport.
        let snap = snapshot(&["row0", "row1", "row2"], 4);
        let c = CopyModeContext {
            snapshot: &snap,
            viewport_offset: 3,
            scrollback_len: 5,
            offscreen_cell: None,
        };
        // top_row = 5 - 3 = 2; absolute cursor seeded on the visible top row.
        let mut s = CopyModeState::new(at(2, 0));
        s.apply(CopyModeKey::ToggleCharSelect, &c);
        s.apply(CopyModeKey::MoveRight, &c);
        s.apply(CopyModeKey::MoveRight, &c);
        let before = s.range();
        assert_eq!(
            before,
            Some(AbsoluteSelectionRange {
                start: at(2, 0),
                end: at(2, 2),
            })
        );

        // Scrollback grows by 4 rows (background output) while copy mode is
        // open: the range is a pure function of absolute state, so it is
        // unchanged — the selection stays pinned to content.
        let grown = CopyModeContext {
            snapshot: &snap,
            viewport_offset: 7,
            scrollback_len: 9,
            offscreen_cell: None,
        };
        let _ = grown; // range() does not consult the context.
        assert_eq!(s.range(), before);
    }

    #[test]
    fn block_seam_is_inert() {
        // The default mode never derives a range; the Block seam is reserved.
        let snap = snapshot(&["abc"], 3);
        let c = ctx(&snap);
        let mut s = CopyModeState::new(at(0, 0));
        // Move without selecting -> still no range.
        s.apply(CopyModeKey::MoveRight, &c);
        assert_eq!(s.mode(), SelectKind::Normal);
        assert_eq!(s.range(), None);
    }
}
