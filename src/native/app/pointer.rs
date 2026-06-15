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

    /// The current cell size for pointer geometry. From the GPU in production;
    /// in headless tests (no GPU) a [`App::test_cell`] override stands in. In
    /// non-test builds the override does not exist, so this is exactly
    /// `self.gpu.as_ref().map(GpuState::cell)`.
    fn resolved_cell(&self) -> Option<CellSize> {
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
        if self.should_report_mouse_to_pty() {
            let _ = self.handle_reported_wheel(delta);
            return;
        }

        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        // MOUSE-WHEEL-SPEED: local scrollback honors the configured
        // per-notch multiplier (default 3 = byte-identical). The TUI
        // reporting and overlay paths above intentionally use the fixed
        // default step, so this only affects local viewport scrolling.
        let lines = wheel_lines_scaled(delta, cell_height, self.settings.scroll_wheel_step());
        if lines != 0 {
            self.scroll_viewport(lines);
        }
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
        let visible_range = selection::visible_range_from_absolute(
            range,
            self.viewport.offset(),
            scrollback_len,
            self.grid,
        )?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        selected_clipboard_text(&snapshot, visible_range)
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
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.report_button = None;
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
        }
    }
}
