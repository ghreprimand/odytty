// SPDX-License-Identifier: GPL-3.0-only
//! Window-level pointer dispatch for the native app: the `MouseInput` /
//! `MouseWheel` button-and-wheel handlers, middle-click PRIMARY paste, the
//! selection→clipboard text helpers, and the pointer-state resets the overlay
//! and focus-loss paths run.
//!
//! Mechanically split out of `app/mod.rs` to keep that file under the
//! source-size cap and to give the pointer gestures one home as they grow; no
//! behavior or API change. These are `App` methods living in a child module so
//! they reach `App`'s private fields and the sibling methods in `app/mod.rs`
//! and `app/interaction.rs` directly. Methods the parent `app` module (or its
//! other children) call back into are marked `pub(super)`.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EditableInputSelection {
    pub(super) text: String,
    edit_bytes: Vec<u8>,
}

impl App {
    /// Handle a window-level mouse button event (the `WindowEvent::MouseInput`
    /// dispatch). Precedence is unchanged: an open overlay captures the button
    /// first, then an in-progress local selection drag, then TUI mouse
    /// reporting, then local selection / hyperlink-open / middle-click paste.
    pub(super) fn handle_mouse_input(&mut self, state: ElementState, button: WinitMouseButton) {
        // UX4-P1: an open overlay captures the pointer before any
        // selection / PTY-report / hyperlink logic — the mouse analogue
        // of the keyboard `if self.overlay.is_open()` guard. Shift and
        // the TUI mouse mode are not consulted here.
        if self.overlay.is_open() {
            self.handle_overlay_pointer_button(state, button);
            return;
        }
        // Modal pointer capture (Wave-15 foundation): a mouse-owning modal
        // (copy-mode) swallows the press beneath the overlay guard, suppressing
        // both local selection and PTY reporting. `modal_captures_pointer()` is
        // `false` today, so this is dead code and the press path is unchanged.
        if self.modal_captures_pointer() {
            return;
        }
        if self.should_show_tab_bar() {
            match (button, state, self.current_tab_bar_hit()) {
                (WinitMouseButton::Left, ElementState::Pressed, Some(TabHit::Switch(idx))) => {
                    let Some(token) = self.sessions.token_at_position(idx) else {
                        return;
                    };
                    if self.sessions.switch(token) {
                        self.on_active_session_changed();
                    }
                    return;
                }
                (WinitMouseButton::Left, ElementState::Pressed, Some(TabHit::Close(idx))) => {
                    let Some(token) = self.sessions.token_at_position(idx) else {
                        return;
                    };
                    let is_last = self.sessions.close(token);
                    if is_last {
                        self.pending_exit = true;
                    } else {
                        self.on_active_session_changed();
                    }
                    return;
                }
                (WinitMouseButton::Left, ElementState::Pressed, Some(TabHit::NewTab)) => {
                    self.handle_new_tab();
                    return;
                }
                (WinitMouseButton::Left, ElementState::Released, Some(_)) => return,
                (WinitMouseButton::Right, ElementState::Pressed, Some(hit)) => {
                    let rename_target = match hit {
                        TabHit::Switch(idx) => self.sessions.token_at_position(idx),
                        TabHit::Close(_) | TabHit::NewTab | TabHit::None => None,
                    };
                    self.open_context_menu(rename_target);
                    return;
                }
                (WinitMouseButton::Right, ElementState::Released, Some(_)) => return,
                _ => {}
            }
        }
        if self.pointer_drag.is_selecting() {
            if button == WinitMouseButton::Left && state == ElementState::Released {
                self.finish_selection();
            }
            return;
        }
        // MOUSE-SCROLLBAR: while a scroll-thumb drag is in progress, swallow
        // button events (the release ends it) so the drag never leaks a press
        // to PTY reporting or local selection. `is_selecting()` is false for the
        // `Scrollbar` variant, so this needs its own guard alongside the one
        // above.
        if self.pointer_drag.scrollbar_grab().is_some() {
            if button == WinitMouseButton::Left && state == ElementState::Released {
                self.pointer_drag = PointerDrag::None;
            }
            return;
        }

        // MOUSE-SCROLLBAR: a left press on the visible scroll thumb grabs it to
        // scrub scrollback. Gated on the `scrollbar_drag` setting and the thumb
        // being visible (`viewport offset > 0`); the hit-test returns `None` at
        // the live tail and when disabled, so this branch is inert there and the
        // press routing below stays byte-identical. Sits before the TUI-report
        // branch so grabbing the thumb wins over mouse reporting — but only when
        // the press actually lands on the thumb; every other press (including in
        // a mouse-reporting app) falls through to exactly the historical path.
        if self.settings.scrollbar_drag
            && button == WinitMouseButton::Left
            && state == ElementState::Pressed
            && let Some(grab_dy) = self.scrollbar_hit_test()
        {
            self.pointer_drag = PointerDrag::Scrollbar { grab_dy };
            return;
        }

        if (self.should_report_mouse_to_pty() || self.report_button.is_some())
            && let Some(button) = map_winit_mouse_button(button)
        {
            self.handle_reported_mouse_input(state, button);
            return;
        }

        // IN2: a right-click press opens the context menu. This sits AFTER the
        // TUI report gate above (step 6), so inside a TUI with mouse reporting
        // active the right-click is reported to the PTY and this is never
        // reached. Shift+right-click bypasses the report gate (Shift is excluded
        // from `should_report_mouse_to_pty`), so it falls through to here and
        // opens the menu even in a TUI — the same Shift override convention as
        // local selection. In a plain shell the gate is skipped and the menu
        // opens. No enable bool: the report gate IS the off switch (D-IN2-1).
        if button == WinitMouseButton::Right && state == ElementState::Pressed {
            self.open_context_menu(None);
            return;
        }

        if button == WinitMouseButton::Left {
            match state {
                ElementState::Pressed => {
                    if !self.try_open_hovered_hyperlink() {
                        self.begin_selection();
                    }
                }
                ElementState::Released => self.finish_selection(),
            }
        } else if button == WinitMouseButton::Middle {
            if state == ElementState::Pressed {
                self.handle_primary_paste();
            }
        }
    }

    /// Hit-test the last cached pointer position against the draggable scroll
    /// thumb (MOUSE-SCROLLBAR), returning the grab offset within the thumb when
    /// the press lands on the visible thumb's grab band, else `None`. The thumb
    /// is visible only while scrolled back (`viewport offset > 0`), so a press
    /// at the live tail (the default) never grabs — keeping the plain press path
    /// byte-identical. Uses `pointer_px`, the same cached coordinates the
    /// SGR-pixel report path relies on (button events carry no coordinates).
    fn scrollbar_hit_test(&self) -> Option<f32> {
        let (x_px, y_px) = self.pointer_px?;
        let cell = self.resolved_cell()?;
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let y_px = if self.should_show_tab_bar() {
            y_px - f64::from(self.tab_bar_height_px(cell))
        } else {
            y_px
        };
        let scrollback_len = self.scrollback_len();
        scroll_indicator_hit_with_padding(
            x_px as f32,
            y_px as f32,
            self.viewport.offset(),
            scrollback_len,
            self.grid,
            cell,
            padding,
        )
    }

    pub(in crate::native) fn current_tab_bar_hit(&self) -> Option<TabHit> {
        if !self.should_show_tab_bar() {
            return None;
        }
        let (x_px, y_px) = self.pointer_px?;
        let cell = self.resolved_cell()?;
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        match self.tab_bar.hit_test(
            x_px,
            y_px,
            &self.sessions,
            self.grid.columns,
            padding.as_f32(),
            cell,
            padding,
        ) {
            TabHit::None => None,
            hit => Some(hit),
        }
    }

    /// The current cell size for pointer geometry. From the GPU in production;
    /// in headless tests (no GPU) a [`App::test_cell`] override stands in. In
    /// non-test builds the override does not exist, so this is exactly
    /// `self.gpu.as_ref().map(GpuState::cell)`.
    pub(in crate::native) fn resolved_cell(&self) -> Option<CellSize> {
        #[cfg(test)]
        if let Some(cell) = self.test_cell {
            return Some(cell);
        }
        self.gpu.as_ref().map(GpuState::cell)
    }

    /// Handle a window-level wheel event (the `WindowEvent::MouseWheel`
    /// dispatch). Precedence is unchanged: an open overlay scrolls its list
    /// first, then TUI reporting, then local scrollback movement at the
    /// configured per-notch multiplier.
    pub(super) fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        // UX4-P1: an open overlay captures the wheel to scroll its list,
        // before TUI reporting or scrollback movement.
        if self.overlay.is_open() {
            self.handle_overlay_pointer_wheel(delta);
            return;
        }
        // Modal pointer capture (Wave-15 foundation): a mouse-owning modal
        // swallows the wheel beneath the overlay guard. `false` today ⇒ dead
        // code ⇒ the wheel path is unchanged.
        if self.modal_captures_pointer() {
            return;
        }
        if self.should_report_mouse_to_pty() {
            let _ = self.handle_reported_wheel(delta);
            return;
        }

        // MOUSE-WHEEL (zoom): Ctrl+wheel adjusts the font size, but only while
        // mouse reporting is off. The report gate above already returned for a
        // reporting app, so Ctrl+wheel there passes through to the PTY untouched
        // — this branch is never reached in that case. Gated on the `wheel_zoom`
        // setting (default on); when off, Ctrl+wheel falls through to the plain
        // scrollback path below, byte-identical to today. Ctrl+wheel is a zoom
        // gesture, so it is consumed here and never also scrolls scrollback
        // (the early return holds even at the clamp boundary, where the zoom is
        // a no-op).
        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        if self.settings.wheel_zoom && self.modifiers.ctrl {
            // WHEEL-SENS: coalesce the burst before mapping to a font step so
            // one physical notch is exactly one step (cap one per notch). The
            // gesture is consumed unconditionally — even when the carry is still
            // sub-notch or the size is clamped at a bound — so Ctrl+wheel never
            // falls through to scrollback (T-zoom-clamp).
            if let Some(notch) = self.wheel_accum.coalesce_zoom(delta, cell_height) {
                let steps = wheel_zoom_steps(notch);
                if steps != 0 {
                    self.adjust_font_size_by(steps);
                }
            }
            return;
        }

        // WHEEL-SENS + MOUSE-WHEEL-SPEED: coalesce the burst into discrete
        // notches, then local scrollback honors the configured per-notch
        // multiplier (default 3 = byte-identical for a clean `LineDelta(_, ±1)`).
        // The TUI reporting and overlay paths intentionally use the fixed
        // default step, so this only affects local viewport scrolling.
        if let Some(notch) = self.wheel_accum.coalesce_scroll(delta, cell_height) {
            let lines = wheel_lines_scaled(notch, cell_height, self.settings.scroll_wheel_step());
            if lines == 0 {
                return;
            }
            // ALT-SCROLL (DECSET 1007): on the alternate screen (which has no
            // scrollback) a non-mouse-tracking TUI like a pager or Claude CLI
            // expects the wheel to move via cursor keys. Reporting is already off
            // here (the report gate returned above), so translate the wheel into
            // Up/Down presses; otherwise move the local scrollback viewport.
            if self.alternate_scroll_active() {
                self.send_wheel_as_arrows(lines);
            } else {
                self.scroll_viewport(lines);
            }
        }
    }

    /// Adjust the live font size by `steps` pixels (MOUSE-WHEEL Ctrl+wheel
    /// zoom), clamped to the supported range, routed through the existing live
    /// settings-apply seam so the atlas rebuild and grid reflow run exactly as
    /// a `font_size` settings edit would — no separate resize path. A no-op when
    /// the clamp leaves the size unchanged (already at the min/max), so zooming
    /// past the bound does nothing. The change is applied live but not written
    /// to disk: the same transient behavior as dragging the overlay slider
    /// without saving.
    fn adjust_font_size_by(&mut self, steps: i32) {
        let current = self.settings.font_size_px;
        let next_px = (current + steps as f32).clamp(
            crate::settings::MIN_FONT_SIZE_PX,
            crate::settings::MAX_FONT_SIZE_PX,
        );
        if (next_px - current).abs() < f32::EPSILON {
            return;
        }
        let mut next = self.settings.clone();
        next.font_size_px = next_px;
        self.apply_overlay_settings(next);
    }

    fn handle_primary_paste(&mut self) {
        let Some(text) = self.clipboard.read_primary_text() else {
            return;
        };
        self.return_to_live();
        let _ = write_paste_text(&self.terminal, &self.writer, &text);
    }

    pub(super) fn current_selection_text(&self) -> Option<String> {
        let Some(range) = self.selection.range() else {
            return None;
        };
        let terminal = self.terminal.lock().expect("terminal mutex");
        let scrollback_len = terminal.screen().scrollback_len();
        let offset = self.viewport.offset();
        // MOUSE-RECT: a block selection copies the column band on every row
        // (`selected_text_block`), versus the wrapped path's first/last-partial
        // run. Both branches resolve the visible range and early-return on a
        // fully-off-viewport selection BEFORE snapshotting, so the wrapped path
        // stays byte-identical (same calls, same order) and the block path skips
        // the snapshot clone when nothing is visible. This single choke point is
        // shared by PRIMARY, CLIPBOARD, copy-on-select, and the keyboard copy.
        if self.selection_block {
            let visible_range = selection::visible_block_range_from_absolute(
                range,
                offset,
                scrollback_len,
                self.grid,
            )?;
            let snapshot = terminal.snapshot_with_scrollback(offset);
            let text = selection::selected_text_block(&snapshot, visible_range);
            (!text.is_empty()).then_some(text)
        } else {
            let visible_range =
                selection::visible_range_from_absolute(range, offset, scrollback_len, self.grid)?;
            let snapshot = terminal.snapshot_with_scrollback(offset);
            selected_clipboard_text(&snapshot, visible_range)
        }
    }

    pub(super) fn editable_input_selection_for_context_menu(
        &self,
    ) -> Option<EditableInputSelection> {
        if self.selection_block || self.viewport.offset() != 0 {
            return None;
        }
        let range = self.selection.range()?;
        let terminal = self.terminal.lock().ok()?;
        let modes = key_modes_from_core(terminal.keyboard_modes());
        let (input_row, input_column) = terminal.active_prompt_input_start()?;
        let scrollback_len = terminal.screen().scrollback_len();
        let cursor = terminal.screen().cursor();
        let cursor_row = scrollback_len.saturating_add(cursor.row);
        if input_row != cursor_row || input_row < scrollback_len {
            return None;
        }
        let visible_row = input_row - scrollback_len;
        if visible_row >= self.grid.rows {
            return None;
        }
        let (selected_start, selected_end) =
            selected_columns_on_row(range, input_row, self.grid.columns)?;
        let snapshot = terminal.snapshot_with_scrollback(0);
        let editable_end = editable_input_end_column(&snapshot, visible_row, input_column, cursor)?;
        let start = selected_start.max(input_column);
        let end = selected_end.min(editable_end);
        if start > end {
            return None;
        }
        let text = snapshot_row_text(&snapshot, visible_row, start, end);
        let delete_count = snapshot_row_cell_count(&snapshot, visible_row, start, end);
        if text.is_empty() || delete_count == 0 {
            return None;
        }
        let edit_bytes =
            delete_selection_bytes(&snapshot, visible_row, start, cursor, delete_count, modes)?;
        Some(EditableInputSelection { text, edit_bytes })
    }

    pub(super) fn handle_context_menu_cut(&mut self) {
        let Some(selection) = self.editable_input_selection_for_context_menu() else {
            return;
        };
        // Fail-safe: if the clipboard write fails, do not delete the editable
        // input and do not clear the selection — the text stays in-place as
        // if Cut had not been invoked. Only proceed with the delete when the
        // write actually succeeded (D-IN2-CUT-SAFE).
        if self.clipboard.write_text(&selection.text).is_none() {
            return;
        }
        self.delete_editable_input_selection(selection);
    }

    pub(super) fn handle_context_menu_delete(&mut self) {
        let Some(selection) = self.editable_input_selection_for_context_menu() else {
            return;
        };
        self.delete_editable_input_selection(selection);
    }

    fn delete_editable_input_selection(&mut self, selection: EditableInputSelection) {
        self.return_to_live();
        self.write_pty_bytes(&selection.edit_bytes);
        self.selection.clear();
        self.selection_block = false;
        self.request_selection_redraw();
    }

    pub(super) fn write_primary_selection(&mut self) {
        let Some(text) = self.current_selection_text() else {
            return;
        };
        let _ = self.clipboard.write_primary_text(text.as_str());
    }

    /// Reset terminal-grid pointer state when entering any overlay mode.
    ///
    /// An open overlay captures the pointer (UX4-P1), so any in-progress local
    /// selection — and crucially any TUI mouse-report button still held from a
    /// press before the overlay opened — must be cleared on entry. Overlay
    /// presses short-circuit before `handle_reported_mouse_input` and overlay
    /// releases are inert, so a stale `report_button` would otherwise survive
    /// the overlay and re-enter the held-button motion path after it closes.
    /// Clearing on entry is sufficient: nothing can re-arm `report_button` while
    /// the overlay is open, so it is guaranteed `None` on close.
    pub(super) fn reset_pointer_state_for_overlay(&mut self) {
        self.selection.clear();
        self.selection_block = false;
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.report_button = None;
        // WHEEL-SENS (T-reset): clear the wheel carry on overlay entry so a
        // partial grid-scroll notch does not bleed into the overlay list scroll
        // (and vice-versa) once the overlay captures the wheel.
        self.wheel_accum.reset();
    }

    /// On focus loss, abandon any in-progress overlay slider drag (UX4-P2).
    ///
    /// A press may arm a drag whose release is then delivered to another window
    /// after an alt-tab; the overlay stays open, so without this the drag
    /// survives and the next bare hover Move on focus regain commits a phantom
    /// value (the overlay-stays-open analogue of the close/reopen lost-release
    /// case). No-op unless the overlay is open with a drag armed.
    pub(super) fn cancel_overlay_drag_on_focus_loss(&mut self) {
        if self.overlay.is_open() {
            self.overlay.cancel_settings_drag();
            // SLIDER-GUARD: clear the held flag so a focus-regain move cannot
            // advance a stale drag even if the Release event was lost.
            self.overlay_left_held = false;
        }
    }
}

fn selected_columns_on_row(
    range: AbsoluteSelectionRange,
    row: usize,
    columns: usize,
) -> Option<(usize, usize)> {
    if row < range.start.row || row > range.end.row || columns == 0 {
        return None;
    }
    let start = if row == range.start.row {
        range.start.column
    } else {
        0
    };
    let end = if row == range.end.row {
        range.end.column
    } else {
        columns - 1
    };
    Some((start.min(columns - 1), end.min(columns - 1)))
}

fn editable_input_end_column(
    snapshot: &Snapshot,
    row: usize,
    input_column: usize,
    cursor: Position,
) -> Option<usize> {
    if row >= snapshot.dimensions.rows || input_column >= snapshot.dimensions.columns {
        return None;
    }
    let offset = row * snapshot.dimensions.columns;
    let row_cells = &snapshot.cells[offset..offset + snapshot.dimensions.columns];
    let last_content = row_cells
        .iter()
        .enumerate()
        .rev()
        .find(|(_, cell)| {
            !cell.wide_continuation && (cell.ch != ' ' || !cell.combining().is_empty())
        })
        .map(|(column, _)| column);
    let cursor_end = if cursor.row == row {
        cursor.column.saturating_sub(1)
    } else {
        0
    };
    let end = last_content.map_or(cursor_end, |column| column.max(cursor_end));
    (end >= input_column).then_some(end.min(snapshot.dimensions.columns - 1))
}

fn snapshot_row_text(snapshot: &Snapshot, row: usize, start: usize, end: usize) -> String {
    snapshot_row_cells(snapshot, row, start, end)
        .filter(|cell| !cell.wide_continuation)
        .map(|cell| cell.grapheme())
        .collect()
}

fn snapshot_row_cell_count(snapshot: &Snapshot, row: usize, start: usize, end: usize) -> usize {
    snapshot_row_cells(snapshot, row, start, end)
        .filter(|cell| !cell.wide_continuation)
        .count()
}

fn snapshot_row_cells(
    snapshot: &Snapshot,
    row: usize,
    start: usize,
    end: usize,
) -> impl Iterator<Item = &crate::core::Cell> {
    let columns = snapshot.dimensions.columns;
    let offset = row * columns;
    snapshot.cells[offset + start..=offset + end].iter()
}

fn delete_selection_bytes(
    snapshot: &Snapshot,
    row: usize,
    selection_start: usize,
    cursor: Position,
    delete_count: usize,
    modes: KeyModes,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    if selection_start < cursor.column {
        let move_count = snapshot_row_cell_count(snapshot, row, selection_start, cursor.column - 1);
        let left = input::encode_key_event(Key::Left, Modifiers::NONE, modes, KeyEventType::Press);
        if move_count > 0 && left.is_empty() {
            return None;
        }
        bytes.extend(left.repeat(move_count));
    } else if selection_start > cursor.column {
        let move_count = snapshot_row_cell_count(snapshot, row, cursor.column, selection_start - 1);
        let right =
            input::encode_key_event(Key::Right, Modifiers::NONE, modes, KeyEventType::Press);
        if move_count > 0 && right.is_empty() {
            return None;
        }
        bytes.extend(right.repeat(move_count));
    }
    let delete = input::encode_key_event(Key::Delete, Modifiers::NONE, modes, KeyEventType::Press);
    if delete.is_empty() {
        return None;
    }
    bytes.extend(delete.repeat(delete_count));
    Some(bytes)
}
