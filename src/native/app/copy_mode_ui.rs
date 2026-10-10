// SPDX-License-Identifier: GPL-3.0-only
//! Native copy-mode input, overlays, and clipboard extraction.
//!
//! Each pane owns its caret and anchor. App routing enforces modal exclusion,
//! follows the caret, paints snapshot copies, and uses the caret/anchor signature
//! to invalidate cached overlays. Yanks share absolute text extraction with
//! mouse copy and refuse coordinates made stale by another history eviction.

use crate::core::Snapshot;
use crate::native::copy_mode::{CopyModeContext, CopyModeKey, CopyModeResponse, CopyModeState};
use crate::selection::{self, AbsoluteSelectionRange};

use super::overlay_registry::OverlayCtx;
use super::*;

impl App {
    /// Enter keyboard scrollback selection mode. Returns `true` when the key was
    /// consumed; `false` lets the chord fall through to the PTY.
    ///
    /// The caret starts at the live cursor's absolute position (the scrollback
    /// length plus the live cursor row), then the viewport is scrolled to bring
    /// it on screen - so entry is deterministic regardless of the current scroll
    /// position.
    pub(super) fn enter_copy_mode(&mut self) -> bool {
        self.sessions.reconcile_scrollback_trims();
        // Defensive mutual-exclusion (mirrors `activate_hints`). The key ladder
        // routes overlay / search / active modals BEFORE the BindableAction
        // match, so this is unreachable while another modal owns input; the
        // guard makes the invariant explicit and the unit test meaningful.
        if self.overlay.is_open()
            || self.search.is_open()
            || self.active_modal() != ActiveModal::None
        {
            return false;
        }

        let scrollback_len = self.scrollback_len();
        let cursor = crate::native::lock_recover(&self.terminal)
            .snapshot()
            .cursor;
        let start = selection::AbsoluteCellPoint {
            row: scrollback_len + cursor.row,
            column: cursor.column,
        };
        self.copy_mode = Some(CopyModeState::new(start));
        self.settle_pointer_for_modal();
        self.follow_copy_mode_caret();
        self.request_selection_redraw();
        true
    }

    /// Handle a key while copy-mode is active. Every key is consumed by the
    /// modal: a recognized vim motion / select / yank / cancel is applied, and
    /// an unbound key is swallowed (never encoded to the PTY).
    pub(super) fn copy_mode_key(&mut self, key: &WinitKey) {
        if self.copy_mode.is_none() {
            return;
        }
        let Some(cm_key) = self.translate_copy_mode_key(key) else {
            // Unbound key: swallowed, not encoded (trap #4 - no PTY leak).
            return;
        };

        // Resolve motions against the absolute buffer: the visible
        // viewport snapshot serves on-screen cells, and an off-screen provider
        // windows the terminal at the requested row so word/line motions can
        // scroll past the viewport edges. The terminal handle is cloned so the
        // guard borrows a local, leaving `self` free for `copy_mode.as_mut()`.
        let offset = self.viewport.offset();
        let terminal = std::sync::Arc::clone(&self.terminal);
        let response = {
            let terminal = crate::native::lock_recover(&terminal);
            let snapshot = terminal.snapshot_with_scrollback(offset);
            let scrollback_len = terminal.screen().scrollback_len();
            // One-row memo: word walks visit off-screen cells sequentially, so
            // each distinct row is windowed once, not once per cell.
            let row_memo: std::cell::RefCell<Option<(usize, Vec<char>)>> =
                std::cell::RefCell::new(None);
            let offscreen = |p: selection::AbsoluteCellPoint| -> Option<char> {
                if let Some((row, chars)) = row_memo.borrow().as_ref()
                    && *row == p.row
                {
                    return chars.get(p.column).copied();
                }
                // Window placing the row at (or below) the viewport top -
                // same mapping as `absolute_selection_text`.
                let w_offset = scrollback_len.saturating_sub(p.row);
                let snap = terminal.snapshot_with_scrollback(w_offset);
                let cols = snap.dimensions.columns;
                let top = scrollback_len.saturating_sub(w_offset);
                let vrow = p.row.checked_sub(top)?;
                if vrow >= snap.dimensions.rows {
                    return None;
                }
                // Wide-continuation spacers resolve to their lead, as in the
                // visible rows, so a CJK run is one word off screen too.
                let row = &snap.cells[vrow * cols..(vrow + 1) * cols];
                let chars: Vec<char> = (0..cols)
                    .map(|column| crate::selection::row_word_char(row, column).unwrap_or(' '))
                    .collect();
                let ch = chars.get(p.column).copied();
                *row_memo.borrow_mut() = Some((p.row, chars));
                ch
            };
            let ctx = CopyModeContext {
                snapshot: &snapshot,
                viewport_offset: offset,
                scrollback_len,
                offscreen_cell: Some(&offscreen),
            };
            let cm = self.copy_mode.as_mut().expect("copy_mode is Some");
            cm.apply(cm_key, &ctx)
        };

        match response {
            CopyModeResponse::Continue => {
                self.follow_copy_mode_caret();
                self.request_selection_redraw();
            }
            CopyModeResponse::Yank => {
                // Trap #5: reuse the existing clipboard-write transport; only the
                // scrollback-spanning text extraction is local. `range()` is
                // `None` for a degenerate (single-cell) selection, so nothing is
                // copied in that case.
                // H4: reconcile any pending scrollback trim (the scrollback-epoch
                // check) before reading; surviving coordinates shift to the new
                // origin and evicted anchors are dropped, so a yank cannot read
                // different, more recent rows. Mirrors the command-output copy generation
                // guard.
                self.sessions.reconcile_scrollback_trims();
                if let Some(range) = self.copy_mode.as_ref().and_then(CopyModeState::range)
                    && let Some(text) = self.absolute_selection_text(range, false)
                {
                    let _ = self.clipboard.write_text(&text);
                }
                self.exit_copy_mode();
            }
            CopyModeResponse::Exit => self.exit_copy_mode(),
        }
    }

    /// Tear down the copy-mode modal and force a repaint so the band + caret
    /// clear. The viewport is left where the user scrolled it.
    fn exit_copy_mode(&mut self) {
        if self.copy_mode.take().is_some() {
            self.request_selection_redraw();
        }
    }

    /// Scroll the viewport so the copy-mode caret is on screen. No-op when the
    /// caret is already visible, so a horizontal-only motion never scrolls.
    fn follow_copy_mode_caret(&mut self) {
        let Some(caret_row) = self.copy_mode.as_ref().map(|cm| cm.cursor().row) else {
            return;
        };
        let rows = self.focused_grid().rows;
        if rows == 0 {
            return;
        }
        let scrollback_len = self.scrollback_len();
        let top = selection::viewport_top_absolute_row(self.viewport.offset(), scrollback_len);
        let bottom = top + rows - 1;
        let target_top = if caret_row < top {
            caret_row
        } else if caret_row > bottom {
            caret_row.saturating_sub(rows - 1)
        } else {
            return; // already visible: no scroll
        };
        let offset = scrollback_len.saturating_sub(target_top);
        if self.viewport.jump_to(offset, scrollback_len) {
            self.on_viewport_changed();
        }
    }

    /// Extract the text of an absolute selection, spanning scrollback as needed.
    ///
    /// The stored selection is in ABSOLUTE coordinates and may cover far more
    /// rows than one viewport, so this walks the range in viewport-height
    /// windows and concatenates: a selection scrolled partly off screen is
    /// copied in full, never clamped to whatever happens to be visible at copy
    /// time. Shared by the copy-mode (keyboard) yank and the mouse
    /// PRIMARY/CLIPBOARD/copy-on-select choke point so the two can never drift.
    ///
    /// `block == false` reproduces terminal selection semantics (first/last row
    /// partial, interior rows full width, and no inserted newline across a soft
    /// wrap); `block == true` reproduces [`selection::selected_text_block`] (the
    /// same inclusive column band on every row, always newline-separated). The
    /// per-row rule (trailing-trim and wide-continuation drop) is reproduced
    /// directly rather than routed through
    /// `visible_range_from_absolute`, whose `normalize_range` collapses a
    /// single-cell row span to `None` (which would silently drop a boundary
    /// row). Blank interior rows are preserved as empty strings; an entirely
    /// blank selection yields `None`.
    pub(super) fn absolute_selection_text(
        &self,
        range: AbsoluteSelectionRange,
        block: bool,
    ) -> Option<String> {
        // Poison-recover rather than abort across the AppKit/Rust FFI on this
        // copy / PRIMARY-selection choke point; byte-identical when healthy.
        let terminal = crate::native::lock_recover(&self.terminal);
        // Refuse a stale range if the pump evicted again after reconciliation.
        if terminal.scrollback_trim_epoch() != self.last_scrollback_trim_epoch {
            return None;
        }
        let dimensions = terminal.screen().dimensions();
        let rows = dimensions.rows;
        let cols = dimensions.columns;
        if rows == 0 || cols == 0 {
            return None;
        }
        let scrollback_len = terminal.screen().scrollback_len();

        // A block selection's column band is fixed on every row; its two corner
        // columns may be in either order, so min/max them once.
        let block_lo = range.start.column.min(range.end.column);
        let block_hi = range.start.column.max(range.end.column);

        let mut text = String::new();
        let mut previous_wrapped = false;
        let mut have_previous = false;
        // Bound the walk to live rows: an end row past the buffer (a range
        // built against other geometry) copies what exists and stops.
        let last_live_row = scrollback_len + rows - 1;
        let end_row = range.end.row.min(last_live_row);
        let mut abs_row = range.start.row;
        while abs_row <= end_row {
            // Window placing `abs_row` at (or below) the viewport top.
            let offset = scrollback_len.saturating_sub(abs_row);
            let visible_rows = terminal.screen().visible_search_rows(offset);
            if visible_rows.is_empty() {
                break;
            }
            let last_col = cols - 1;
            let top = scrollback_len.saturating_sub(offset);
            let window_bottom = top + rows - 1;
            let chunk_end = window_bottom.min(end_row);
            if chunk_end < abs_row {
                // No progress is possible from here; never spin.
                break;
            }

            for r in abs_row..=chunk_end {
                let vrow = r - top;
                let Some(row) = visible_rows.get(vrow) else {
                    break;
                };
                let (start_col, end_col) = if block {
                    (block_lo.min(last_col), block_hi.min(last_col))
                } else {
                    let start_col = if r == range.start.row {
                        range.start.column.min(last_col)
                    } else {
                        0
                    };
                    // `LINE_END_COLUMN` (copy-mode line-wise) clamps here to the
                    // last column, giving a full-width row.
                    let end_col = if r == range.end.row {
                        range.end.column.min(last_col)
                    } else {
                        last_col
                    };
                    (start_col, end_col)
                };
                let line = selection::selected_row_text(
                    &row.cells,
                    start_col,
                    end_col,
                    !row.wrapped || block,
                );
                if have_previous && (block || !previous_wrapped) {
                    text.push('\n');
                }
                text.push_str(&line);
                previous_wrapped = row.wrapped;
                have_previous = true;
            }
            abs_row = chunk_end + 1;
        }
        (!text.is_empty()).then_some(text)
    }

    /// Map a raw winit key (with the live modifier state) to a normalized
    /// [`CopyModeKey`], or `None` for a key the modal does not bind (which is
    /// then swallowed). Vim motions + arrows + page keys + select/yank/cancel.
    fn translate_copy_mode_key(&self, key: &WinitKey) -> Option<CopyModeKey> {
        match key {
            WinitKey::Named(NamedKey::Escape) => Some(CopyModeKey::Cancel),
            WinitKey::Named(NamedKey::Enter) => Some(CopyModeKey::Yank),
            WinitKey::Named(NamedKey::ArrowLeft) => Some(CopyModeKey::MoveLeft),
            WinitKey::Named(NamedKey::ArrowDown) => Some(CopyModeKey::MoveDown),
            WinitKey::Named(NamedKey::ArrowUp) => Some(CopyModeKey::MoveUp),
            WinitKey::Named(NamedKey::ArrowRight) => Some(CopyModeKey::MoveRight),
            WinitKey::Named(NamedKey::PageUp) => Some(CopyModeKey::PageUp),
            WinitKey::Named(NamedKey::PageDown) => Some(CopyModeKey::PageDown),
            WinitKey::Named(NamedKey::Home) => Some(CopyModeKey::ColumnZero),
            WinitKey::Named(NamedKey::End) => Some(CopyModeKey::LineEnd),
            WinitKey::Character(text) => {
                translate_copy_mode_char(text.chars().next()?, self.modifiers.ctrl)
            }
            _ => None,
        }
    }

    // --- overlay-registry / modal-gate contributor slots ---

    /// Paint the copy-mode selection band + caret onto the snapshot cells (the
    /// cell-mutation lane). No-op when copy mode is inactive, so the default
    /// frame is byte-identical.
    pub(in crate::native) fn paint_copy_mode_cells(
        &self,
        snapshot: &mut Snapshot,
        ctx: &OverlayCtx,
    ) {
        let Some(cm) = self.copy_mode.as_ref() else {
            return;
        };

        // Selection band - Char and Line both ride the wrapped highlight path
        // (line-wise spans full-width rows via the clamped `LINE_END_COLUMN`),
        // so `block = false` always.
        if let Some(range) = cm.range() {
            selection::apply_selection_highlight(
                snapshot,
                range,
                false,
                ctx.viewport_offset,
                ctx.scrollback_len,
                ctx.grid,
                self.themed_selection_style(&self.active_session_presentation_theme()),
            );
        }

        // Caret - invert the cell so the navigable cursor is visible both inside
        // the (already-inverted) band and outside it. Mapped directly from the
        // absolute caret point (NOT via `visible_range_from_absolute`, which
        // would collapse this single cell to `None`).
        let rows = ctx.grid.rows;
        let cols = ctx.grid.columns;
        if rows == 0 || cols == 0 {
            return;
        }
        let caret = cm.cursor();
        let top = selection::viewport_top_absolute_row(ctx.viewport_offset, ctx.scrollback_len);
        let bottom = top + rows - 1;
        if caret.row < top || caret.row > bottom {
            return; // caret scrolled out of view
        }
        let vrow = caret.row - top;
        let col = caret.column.min(cols - 1);
        if let Some(cell) = snapshot.cells.get_mut(vrow * cols + col) {
            let inverted = cell.attrs.inverse();
            cell.attrs.set_inverse(!inverted);
        }
    }

    /// Copy-mode render-cache fragment. `Inert` while inactive (a constant on
    /// the default path ⇒ byte-identical plain frame); a `CopyMode { caret,
    /// anchor, kind }` keyed on the absolute caret + anchor cells and the
    /// selection kind while active, so the geometry-update gate repaints on
    /// every motion / selection change (including `v` to `V` at a fixed caret,
    /// which widens the band to full rows) but does not thrash at rest.
    pub(super) fn copy_mode_overlay_signature(&self) -> OverlayFragment {
        match &self.copy_mode {
            Some(cm) => OverlayFragment::CopyMode {
                caret: (cm.cursor().row, cm.cursor().column),
                anchor: cm.anchor().map(|a| (a.row, a.column)),
                kind: cm.mode(),
            },
            None => OverlayFragment::Inert,
        }
    }

    /// Whether copy-mode is active (captures keys AND the mouse). Truthfully
    /// reflects the live field (trap #3) - it drives both the modal gate and the
    /// pointer-capture predicate, so a lying flag would desync both.
    pub(super) fn copy_mode_active(&self) -> bool {
        self.copy_mode.is_some()
    }
}

/// Translate a character key (with the ctrl flag) to a [`CopyModeKey`]. Ctrl
/// combos (`Ctrl-u/d/b/f`) page; plain letters are the vim motions. Handles both
/// winit forms of a ctrl chord: a control code (`Ctrl-u` → `0x15`) or the plain
/// letter with the modifier reported separately.
fn translate_copy_mode_char(ch: char, ctrl: bool) -> Option<CopyModeKey> {
    if ctrl {
        return match ctrl_letter(ch)? {
            'u' => Some(CopyModeKey::HalfPageUp),
            'd' => Some(CopyModeKey::HalfPageDown),
            'b' => Some(CopyModeKey::PageUp),
            'f' => Some(CopyModeKey::PageDown),
            _ => None,
        };
    }
    match ch {
        'h' => Some(CopyModeKey::MoveLeft),
        'j' => Some(CopyModeKey::MoveDown),
        'k' => Some(CopyModeKey::MoveUp),
        'l' => Some(CopyModeKey::MoveRight),
        '0' => Some(CopyModeKey::ColumnZero),
        '^' => Some(CopyModeKey::FirstNonBlank),
        '$' => Some(CopyModeKey::LineEnd),
        'w' => Some(CopyModeKey::WordForward),
        'b' => Some(CopyModeKey::WordBackward),
        'e' => Some(CopyModeKey::WordEnd),
        'g' => Some(CopyModeKey::GPrefix),
        'G' => Some(CopyModeKey::GotoBottom),
        'v' => Some(CopyModeKey::ToggleCharSelect),
        'V' => Some(CopyModeKey::ToggleLineSelect),
        'o' => Some(CopyModeKey::SwapEnds),
        'y' => Some(CopyModeKey::Yank),
        'q' => Some(CopyModeKey::Cancel),
        _ => None,
    }
}

/// Recover the lowercase letter of a ctrl chord. winit may deliver `Ctrl-u`
/// either as the control code `0x15` (`1..=26` → `a..z`) or as the plain letter
/// `'u'` with ctrl reported in the modifier state; both are normalized here.
fn ctrl_letter(ch: char) -> Option<char> {
    let code = ch as u32;
    if (1..=26).contains(&code) {
        Some((b'a' + (code as u8 - 1)) as char)
    } else if ch.is_ascii_alphabetic() {
        Some(ch.to_ascii_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Terminal;
    use crate::native::copy_mode::CopyModeState;
    use crate::selection::AbsoluteCellPoint;

    // --- pure key-translation (no App needed) -------------------------------

    #[test]
    fn vim_motions_translate() {
        assert_eq!(
            translate_copy_mode_char('h', false),
            Some(CopyModeKey::MoveLeft)
        );
        assert_eq!(
            translate_copy_mode_char('j', false),
            Some(CopyModeKey::MoveDown)
        );
        assert_eq!(
            translate_copy_mode_char('k', false),
            Some(CopyModeKey::MoveUp)
        );
        assert_eq!(
            translate_copy_mode_char('l', false),
            Some(CopyModeKey::MoveRight)
        );
        assert_eq!(
            translate_copy_mode_char('v', false),
            Some(CopyModeKey::ToggleCharSelect)
        );
        assert_eq!(
            translate_copy_mode_char('V', false),
            Some(CopyModeKey::ToggleLineSelect)
        );
        assert_eq!(
            translate_copy_mode_char('y', false),
            Some(CopyModeKey::Yank)
        );
        assert_eq!(
            translate_copy_mode_char('q', false),
            Some(CopyModeKey::Cancel)
        );
    }

    #[test]
    fn ctrl_paging_translates_from_both_winit_forms() {
        // Control-code form (Ctrl-u → 0x15) and plain-letter-with-ctrl form.
        assert_eq!(
            translate_copy_mode_char('\u{15}', true),
            Some(CopyModeKey::HalfPageUp)
        );
        assert_eq!(
            translate_copy_mode_char('u', true),
            Some(CopyModeKey::HalfPageUp)
        );
        assert_eq!(
            translate_copy_mode_char('\u{4}', true),
            Some(CopyModeKey::HalfPageDown)
        );
        assert_eq!(
            translate_copy_mode_char('d', true),
            Some(CopyModeKey::HalfPageDown)
        );
        assert_eq!(
            translate_copy_mode_char('\u{2}', true),
            Some(CopyModeKey::PageUp)
        );
        assert_eq!(
            translate_copy_mode_char('\u{6}', true),
            Some(CopyModeKey::PageDown)
        );
    }

    #[test]
    fn unbound_key_is_not_translated() {
        // An unbound letter has no mapping - the handler swallows it (trap #4).
        assert_eq!(translate_copy_mode_char('z', false), None);
        assert_eq!(translate_copy_mode_char('z', true), None);
    }

    // --- App-level integration (headless, no real PTY) ----------------------

    fn build_app() -> Option<App> {
        let d = Dimensions::new(40, 6);
        let (mut app, _terminal) = crate::native::test_support::headless_app_with(
            crate::native::options::NativeOptions::default(),
            d,
            Settings::default(),
        );
        app.set_test_cell_for_test(crate::atlas::CellSize {
            width: 8,
            height: 16,
            baseline: 0,
        });
        Some(app)
    }

    fn seed(app: &App, text: &str) {
        if let Ok(mut t) = app.terminal.lock() {
            t.advance(text.as_bytes());
        }
    }

    fn ctx_for(app: &App) -> OverlayCtx {
        app.overlay_ctx(
            app.scrollback_len(),
            crate::atlas::CellSize {
                width: 8,
                height: 16,
                baseline: 0,
            },
            crate::core::Position::default(),
            false,
            std::time::Instant::now(),
        )
    }

    // --- trap #1 / off-path identity ---

    #[test]
    fn inactive_copy_mode_paints_zero_cells() {
        let Some(app) = build_app() else {
            return;
        };
        seed(&app, "hello world copy mode off-path");
        let snapshot = app.terminal.lock().unwrap().snapshot();
        let mut painted = snapshot.clone();
        app.paint_copy_mode_cells(&mut painted, &ctx_for(&app));
        assert_eq!(
            snapshot, painted,
            "copy_mode=None must mutate zero cells (byte-identical plain path)"
        );
    }

    // --- trap #2 / signature quantization ---

    #[test]
    fn signature_inert_off_and_copymode_on() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert_eq!(
            app.copy_mode_overlay_signature(),
            OverlayFragment::Inert,
            "inert on the default path"
        );
        assert!(app.enter_copy_mode());
        assert!(
            matches!(
                app.copy_mode_overlay_signature(),
                OverlayFragment::CopyMode { .. }
            ),
            "a live modal contributes a CopyMode fragment (cache invalidation)"
        );
    }

    #[test]
    fn signature_changes_on_motion_stable_at_rest() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "abc def ghi");
        assert!(app.enter_copy_mode());
        let before = app.copy_mode_overlay_signature();
        // No-op re-read is stable.
        assert_eq!(before, app.copy_mode_overlay_signature());
        app.copy_mode_key(&WinitKey::Character("l".into()));
        assert_ne!(
            before,
            app.copy_mode_overlay_signature(),
            "a caret motion changes the fragment (repaints)"
        );
    }

    #[test]
    fn switching_char_to_line_selection_at_a_fixed_caret_repaints_full_rows() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "abc def ghi");
        assert!(app.enter_copy_mode());
        app.copy_mode_key(&WinitKey::Character("v".into()));
        let char_wise = app.copy_mode_overlay_signature();
        let caret = app.copy_mode.as_ref().map(|cm| cm.cursor()).unwrap();
        app.copy_mode_key(&WinitKey::Character("V".into()));
        let line_wise = app.copy_mode_overlay_signature();
        assert_eq!(
            app.copy_mode.as_ref().map(|cm| cm.cursor()),
            Some(caret),
            "the caret did not move"
        );
        assert_ne!(
            char_wise, line_wise,
            "v to V widens the band, so the frame cache must repaint"
        );
        let snapshot = app.terminal.lock().unwrap().snapshot();
        let mut painted = snapshot.clone();
        app.paint_copy_mode_cells(&mut painted, &ctx_for(&app));
        let cols = painted.dimensions.columns;
        let row = caret.row;
        for col in 0..cols {
            let idx = row * cols + col;
            assert_ne!(
                painted.cells[idx], snapshot.cells[idx],
                "column {col} of the caret row is painted line-wise"
            );
        }
        let next = (row + 1) * cols;
        assert_eq!(
            painted.cells[next..next + cols],
            snapshot.cells[next..next + cols],
            "the row below stays unpainted"
        );
    }

    // --- trap #3 / truthful active flag drives the gate ---

    #[test]
    fn active_flag_drives_modal_gate_and_pointer_capture() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert!(!app.copy_mode_active());
        assert_eq!(app.active_modal(), ActiveModal::None);
        assert!(app.enter_copy_mode());
        assert!(app.copy_mode_active(), "live field ⇒ active");
        assert_eq!(app.active_modal(), ActiveModal::CopyMode);
        assert!(app.modal_captures_pointer(), "copy mode owns the pointer");
    }

    // --- trap #4 / modal dead-key discipline ---

    #[test]
    fn unbound_key_does_not_exit_or_change_state() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "abc");
        assert!(app.enter_copy_mode());
        let before = app.copy_mode_overlay_signature();
        app.copy_mode_key(&WinitKey::Character("z".into()));
        assert!(
            app.copy_mode_active(),
            "an unbound key is swallowed, not exit"
        );
        assert_eq!(before, app.copy_mode_overlay_signature(), "state unchanged");
    }

    #[test]
    fn escape_exits_when_not_selecting() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert!(app.enter_copy_mode());
        app.copy_mode_key(&WinitKey::Named(NamedKey::Escape));
        assert!(
            !app.copy_mode_active(),
            "Esc with no selection exits the modal"
        );
        assert_eq!(app.active_modal(), ActiveModal::None);
    }

    #[test]
    fn escape_clears_selection_first_then_exits() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "abc def");
        assert!(app.enter_copy_mode());
        app.copy_mode_key(&WinitKey::Character("v".into())); // start selecting
        app.copy_mode_key(&WinitKey::Character("l".into())); // extend
        app.copy_mode_key(&WinitKey::Named(NamedKey::Escape)); // clear selection
        assert!(
            app.copy_mode_active(),
            "first Esc only clears the selection"
        );
        app.copy_mode_key(&WinitKey::Named(NamedKey::Escape)); // now exit
        assert!(!app.copy_mode_active(), "second Esc exits");
    }

    // --- mutual exclusion ---

    #[test]
    fn enter_rejected_while_search_open() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.search.open();
        assert!(
            !app.enter_copy_mode(),
            "copy mode is rejected while search owns input"
        );
        assert!(!app.copy_mode_active());
    }

    // --- yank: enter → select → yank extracts the expected text -------------

    #[test]
    fn yank_extracts_selected_text() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "hello");
        // Place the caret at the start of "hello", char-select, extend 4 cells.
        let scrollback_len = app.scrollback_len();
        app.copy_mode = Some(CopyModeState::new(AbsoluteCellPoint {
            row: scrollback_len,
            column: 0,
        }));
        app.copy_mode_key(&WinitKey::Character("v".into()));
        for _ in 0..4 {
            app.copy_mode_key(&WinitKey::Character("l".into()));
        }
        let range = app
            .copy_mode
            .as_ref()
            .and_then(CopyModeState::range)
            .expect("a multi-cell selection has a range");
        let text = app
            .absolute_selection_text(range, false)
            .expect("selection yields text");
        assert_eq!(text, "hello", "char-wise yank copies the exact run");
    }

    /// The live copy choke point keeps a row's real trailing space-like
    /// scalars: only the grid's own ASCII-space padding is trimmed.
    #[test]
    fn absolute_selection_keeps_trailing_non_pad_whitespace() {
        let Some(app) = build_app() else {
            return;
        };
        seed(&app, "ab\u{3000}\u{a0}");
        let scrollback_len = app.scrollback_len();
        let range = AbsoluteSelectionRange {
            start: AbsoluteCellPoint {
                row: scrollback_len,
                column: 0,
            },
            end: AbsoluteCellPoint {
                row: scrollback_len,
                column: 19,
            },
        };
        let text = app
            .absolute_selection_text(range, false)
            .expect("selection yields text");
        assert_eq!(text, "ab\u{3000}\u{a0}");
    }

    /// The live copy choke point (mouse PRIMARY/CLIPBOARD/copy-on-select and
    /// the copy-mode yank both route here) preserves stored combining marks:
    /// a decomposed cluster copies as the same decomposed bytes rather than
    /// dropping the accent. Fish emits `e + acute`, `a + diaeresis`,
    /// `n + tilde` as base-plus-combining bytes.
    #[test]
    fn absolute_selection_preserves_combining_marks() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "e\u{0301} a\u{0308} n\u{0303}");
        let scrollback_len = app.scrollback_len();
        app.copy_mode = Some(CopyModeState::new(AbsoluteCellPoint {
            row: scrollback_len,
            column: 0,
        }));
        app.copy_mode_key(&WinitKey::Character("v".into()));
        // Base cells: e, space, a, space, n → extend 4 cells to cover all five.
        for _ in 0..4 {
            app.copy_mode_key(&WinitKey::Character("l".into()));
        }
        let range = app
            .copy_mode
            .as_ref()
            .and_then(CopyModeState::range)
            .expect("a multi-cell selection has a range");
        let text = app
            .absolute_selection_text(range, false)
            .expect("selection yields text");
        assert_eq!(
            text, "e\u{0301} a\u{0308} n\u{0303}",
            "combining marks survive the live copy path"
        );
    }

    #[test]
    fn absolute_selection_does_not_insert_newlines_at_soft_wraps() {
        let Some(app) = build_app() else {
            return;
        };
        let content = "x".repeat(45);
        seed(&app, &content);
        let text = app
            .absolute_selection_text(
                AbsoluteSelectionRange {
                    start: AbsoluteCellPoint { row: 0, column: 0 },
                    end: AbsoluteCellPoint { row: 1, column: 4 },
                },
                false,
            )
            .expect("wrapped selection yields text");
        assert_eq!(text, content);
    }

    /// C24 (app-level, real provider): a word motion from a caret parked in
    /// scrollback - entirely OFF-SCREEN while the viewport sits at the live
    /// tail - resolves against the absolute buffer through the terminal-window
    /// provider and lands on the next scrollback word.
    #[test]
    fn word_forward_resolves_in_offscreen_scrollback() {
        let Some(mut app) = build_app() else {
            return;
        };
        // 10 lines on a 6-row grid → 4 scrollback rows (abs rows 0..=3 are
        // w0..w3, off-screen while the viewport is at the live tail).
        seed(
            &app,
            "w0\r\nw1\r\nw2\r\nw3\r\nw4\r\nw5\r\nw6\r\nw7\r\nw8\r\nw9",
        );
        let scrollback_len = app.scrollback_len();
        assert!(scrollback_len >= 2, "test needs scrollback rows");
        app.copy_mode = Some(CopyModeState::new(AbsoluteCellPoint { row: 0, column: 0 }));
        app.copy_mode_key(&WinitKey::Character("w".into()));
        let cursor = app.copy_mode.as_ref().expect("still active").cursor();
        assert_eq!(
            (cursor.row, cursor.column),
            (1, 0),
            "w walks across off-screen scrollback rows to the next word"
        );
    }

    /// C13: a grid reflow (window resize) closes copy mode, because the caret +
    /// anchor are absolute-buffer coords against the old layout. Before the fix
    /// copy mode survived the resize with stale coordinates.
    #[test]
    fn grid_resize_exits_copy_mode() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "w0\r\nw1\r\nw2\r\nw3\r\nw4");
        assert!(app.enter_copy_mode(), "copy mode must enter");
        assert!(app.copy_mode_active(), "precondition: copy mode active");

        // Drive the production resize path with a surface that yields a grid
        // different from the current one, so the clearing block runs.
        let cell = crate::atlas::CellSize {
            width: 8,
            height: 16,
            baseline: 0,
        };
        app.apply_grid_resize(PendingResize {
            cell,
            padding: WindowPadding::ZERO,
            width_px: 1000,
            height_px: 1600,
        });

        assert!(
            !app.copy_mode_active(),
            "a grid reflow must close copy mode (stale absolute coords)"
        );
    }

    /// NF21-3: a window resize reflows EVERY tab's panes, so the stale
    /// layout-dependent UI state (selection / copy-mode caret / hover spans)
    /// must clear on BACKGROUND tabs too - not only the active one. Before the
    /// fix the clear went through `Deref` = the active session only, so a
    /// background tab crossed the reflow keeping absolute-row coordinates mapped
    /// to the pre-reflow layout: on switch-back the selection covered the wrong
    /// text and a copy yielded the wrong bytes.
    #[test]
    fn grid_resize_clears_layout_state_on_background_tab() {
        let Some(mut app) = build_app() else {
            return;
        };
        // Build a second headless session and push it as a second tab.
        let d = Dimensions::new(40, 6);
        let writer = crate::native::test_support::headless_writer();
        let terminal = Arc::new(Mutex::new(Terminal::new(d.columns, d.rows)));
        // Position 0 is the original tab; the pushed one takes a later position.
        let second = app.push_headless_session_for_test(terminal, writer, d);
        // Seed the stale layout state on the first tab, then background it by
        // switching to the second.
        app.seed_layout_dependent_state_for_test(0);
        assert_eq!(
            app.session_layout_state_is_clear_for_test(0),
            Some(false),
            "precondition: tab 0 has live selection / copy-mode / hover state"
        );
        assert!(
            app.switch_to_session_for_test(second),
            "the second tab must activate so tab 0 is backgrounded"
        );

        let cell = crate::atlas::CellSize {
            width: 8,
            height: 16,
            baseline: 0,
        };
        app.apply_grid_resize(PendingResize {
            cell,
            padding: WindowPadding::ZERO,
            width_px: 1000,
            height_px: 1600,
        });

        assert_eq!(
            app.session_layout_state_is_clear_for_test(0),
            Some(true),
            "a reflow must clear the BACKGROUND tab's stale absolute-row state (NF21-3)"
        );
    }

    /// NF21-3 control: the active tab still clears its layout-dependent state on
    /// a resize exactly as before - the fan-out helper reproduces the former
    /// active-only block byte-for-byte, so this path is unchanged.
    #[test]
    fn grid_resize_clears_layout_state_on_active_tab_unchanged() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.seed_layout_dependent_state_for_test(0);
        assert_eq!(
            app.session_layout_state_is_clear_for_test(0),
            Some(false),
            "precondition: the active tab has live layout-dependent state"
        );

        let cell = crate::atlas::CellSize {
            width: 8,
            height: 16,
            baseline: 0,
        };
        app.apply_grid_resize(PendingResize {
            cell,
            padding: WindowPadding::ZERO,
            width_px: 1000,
            height_px: 1600,
        });

        assert_eq!(
            app.session_layout_state_is_clear_for_test(0),
            Some(true),
            "the active tab clears its layout state after a resize (byte-identical)"
        );
    }

    #[test]
    fn yank_exits_copy_mode() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "abc");
        assert!(app.enter_copy_mode());
        app.copy_mode_key(&WinitKey::Character("v".into()));
        app.copy_mode_key(&WinitKey::Character("l".into()));
        app.copy_mode_key(&WinitKey::Character("y".into()));
        assert!(!app.copy_mode_active(), "yank exits the modal");
    }

    /// `v` then `y` with the caret still on the anchor copies the one character
    /// under the caret (inclusive keyboard selection) instead of copying nothing.
    #[test]
    fn v_then_y_copies_the_character_under_the_caret() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "hello");
        let scrollback_len = app.scrollback_len();
        app.copy_mode = Some(CopyModeState::new(AbsoluteCellPoint {
            row: scrollback_len,
            column: 1,
        }));
        app.copy_mode_key(&WinitKey::Character("v".into()));
        app.copy_mode_key(&WinitKey::Character("y".into()));
        assert!(!app.copy_mode_active(), "yank exits the modal");
        assert_eq!(
            app.clipboard.last_clipboard_write.as_deref(),
            Some("e"),
            "the character under the caret reaches the clipboard"
        );
    }

    #[test]
    fn line_select_yanks_full_row() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed(&app, "a full line of text");
        let scrollback_len = app.scrollback_len();
        app.copy_mode = Some(CopyModeState::new(AbsoluteCellPoint {
            row: scrollback_len,
            column: 3,
        }));
        app.copy_mode_key(&WinitKey::Character("V".into())); // line-wise
        let range = app
            .copy_mode
            .as_ref()
            .and_then(CopyModeState::range)
            .expect("line selection has a range");
        let text = app
            .absolute_selection_text(range, false)
            .expect("line yields text");
        assert_eq!(
            text, "a full line of text",
            "line-wise yank copies the whole row, trailing-trimmed, ignoring the caret column"
        );
    }
}
