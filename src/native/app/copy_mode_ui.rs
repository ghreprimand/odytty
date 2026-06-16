// SPDX-License-Identifier: GPL-3.0-only
//! COPYMODE: vim-key keyboard scrollback selection ("copy") mode.
//!
//! This is the native wiring around the banked pure-core [`crate::native::copy_mode`]
//! model. All feature logic lives in this file; the foundation pre-wired every
//! seam (dispatch arm, modal gate, key route, pointer capture, render hook,
//! signature consumer) so this packet touches `app/mod.rs` for exactly ONE line:
//! the `copy_mode: Option<CopyModeState>` field + its `None` initializer.
//!
//! COPYMODE rides the Wave-15 overlay-registry + modal-input foundation:
//!
//! - **`ActiveModal` input gate (YES, and pointer-owning).** While active the
//!   modal captures EVERY key beneath the overlay/search guards
//!   ([`App::copy_mode_key`] is the routed handler), so nothing leaks to the
//!   PTY; [`App::copy_mode_active`] feeds both the modal gate and the pointer
//!   capture predicate, so it must reflect the live field truthfully.
//! - **`OverlayCompositeSignature.copy_mode` fragment (YES).** A
//!   [`OverlayFragment::CopyMode`] keyed on the caret + anchor cells invalidates
//!   the render cache while the selection/caret moves, and is `Inert` while
//!   inactive so the default frame bytes are byte-identical.
//! - **cell-mutation lane (YES).** The selection band + caret are painted onto a
//!   snapshot copy ([`App::paint_copy_mode_cells`], a sibling of
//!   `paint_selection_cells`), never the terminal core — copy mode is purely a
//!   presentation overlay.
//!
//! Off-path contract: when `self.copy_mode` is `None` (the default),
//! `copy_mode_active()` is `false`, `paint_copy_mode_cells` mutates zero cells,
//! and `copy_mode_overlay_signature()` is `Inert` — so `active_modal()` is
//! `None` and the frame bytes + input routing are byte-identical to before
//! COPYMODE landed.

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
    /// it on screen — so entry is deterministic regardless of the current scroll
    /// position.
    pub(super) fn enter_copy_mode(&mut self) -> bool {
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
        let cursor = self
            .terminal
            .lock()
            .map(|t| t.snapshot().cursor)
            .unwrap_or_default();
        let start = selection::AbsoluteCellPoint {
            row: scrollback_len + cursor.row,
            column: cursor.column,
        };
        self.copy_mode = Some(CopyModeState::new(start));
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
            // Unbound key: swallowed, not encoded (trap #4 — no PTY leak).
            return;
        };

        // Resolve motions against the currently-visible viewport snapshot.
        let offset = self.viewport.offset();
        let (snapshot, scrollback_len) = {
            let Ok(terminal) = self.terminal.lock() else {
                return;
            };
            (
                terminal.snapshot_with_scrollback(offset),
                terminal.screen().scrollback_len(),
            )
        };
        let ctx = CopyModeContext {
            snapshot: &snapshot,
            viewport_offset: offset,
            scrollback_len,
        };
        let response = {
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
                if let Some(range) = self.copy_mode.as_ref().and_then(CopyModeState::range)
                    && let Some(text) = self.copy_mode_selection_text(range)
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
        let rows = self.grid.rows;
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
            return; // already visible — no scroll
        };
        let offset = scrollback_len.saturating_sub(target_top);
        if self.viewport.jump_to(offset, scrollback_len) {
            self.on_viewport_changed();
        }
    }

    /// Extract the text of a copy-mode selection, spanning scrollback as needed.
    ///
    /// The pure-core selection is in ABSOLUTE coordinates and may cover far more
    /// rows than one viewport, so this walks the range in viewport-height
    /// windows and applies the SAME per-row rule as [`selection::selected_text`]
    /// (trailing-trim, wide-continuation drop, `'\n'` join) — reproduced rather
    /// than routed through `visible_range_from_absolute`, whose `normalize_range`
    /// collapses a single-cell row span to `None` (which would silently drop a
    /// boundary row). Blank interior rows are preserved as empty strings.
    fn copy_mode_selection_text(&self, range: AbsoluteSelectionRange) -> Option<String> {
        let rows = self.grid.rows;
        let cols = self.grid.columns;
        if rows == 0 || cols == 0 {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let scrollback_len = terminal.screen().scrollback_len();

        let mut lines: Vec<String> = Vec::new();
        let mut abs_row = range.start.row;
        while abs_row <= range.end.row {
            // Window placing `abs_row` at (or below) the viewport top.
            let offset = scrollback_len.saturating_sub(abs_row);
            let snapshot = terminal.snapshot_with_scrollback(offset);
            let snap_cols = snapshot.dimensions.columns;
            let snap_rows = snapshot.dimensions.rows;
            let top = scrollback_len.saturating_sub(offset);
            let window_bottom = top + rows - 1;
            let chunk_end = window_bottom.min(range.end.row);

            for r in abs_row..=chunk_end {
                let vrow = r - top;
                if vrow >= snap_rows {
                    break;
                }
                let start_col = if r == range.start.row {
                    range.start.column.min(snap_cols - 1)
                } else {
                    0
                };
                // `LINE_END_COLUMN` (line-wise selection) clamps to the last
                // column here, giving a full-width row.
                let end_col = if r == range.end.row {
                    range.end.column.min(snap_cols - 1)
                } else {
                    snap_cols - 1
                };
                let off = vrow * snap_cols;
                let line: String = snapshot.cells[off + start_col..=off + end_col]
                    .iter()
                    .filter(|cell| !cell.wide_continuation)
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned();
                lines.push(line);
            }
            abs_row = chunk_end + 1;
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
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

    // --- overlay-registry / modal-gate contributor slots (Wave-15) ---

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

        // Selection band — Char and Line both ride the wrapped highlight path
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
                self.themed_selection_style(),
            );
        }

        // Caret — invert the cell so the navigable cursor is visible both inside
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
    /// anchor }` keyed on the absolute caret + anchor cells while active, so the
    /// geometry-update gate repaints on every motion / selection change but does
    /// not thrash at rest (trap #2).
    pub(super) fn copy_mode_overlay_signature(&self) -> OverlayFragment {
        match &self.copy_mode {
            Some(cm) => OverlayFragment::CopyMode {
                caret: (cm.cursor().row, cm.cursor().column),
                anchor: cm.anchor().map(|a| (a.row, a.column)),
            },
            None => OverlayFragment::Inert,
        }
    }

    /// Whether copy-mode is active (captures keys AND the mouse). Truthfully
    /// reflects the live field (trap #3) — it drives both the modal gate and the
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
        // An unbound letter has no mapping — the handler swallows it (trap #4).
        assert_eq!(translate_copy_mode_char('z', false), None);
        assert_eq!(translate_copy_mode_char('z', true), None);
    }

    // --- App-level integration (skips when no PTY is available) -------------

    fn build_app() -> Option<App> {
        let d = Dimensions::new(40, 6);
        let session = crate::pty::PtySession::spawn_shell_command(d, "sleep 1").ok()?;
        let writer: crate::native::pty::PtyWriter =
            Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(d.columns, d.rows)));
        let pty = Arc::new(Mutex::new(session));
        let mut app = App::new(
            crate::native::options::NativeOptions::default(),
            terminal,
            writer,
            pty,
            Settings::default(),
            crate::settings::SettingsReloader::for_current_process(Instant::now()),
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
            .copy_mode_selection_text(range)
            .expect("selection yields text");
        assert_eq!(text, "hello", "char-wise yank copies the exact run");
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
            .copy_mode_selection_text(range)
            .expect("line yields text");
        assert_eq!(
            text, "a full line of text",
            "line-wise yank copies the whole row, trailing-trimmed, ignoring the caret column"
        );
    }
}
