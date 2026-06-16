// SPDX-License-Identifier: GPL-3.0-only
//! Test seams for the native `App`: thin, behaviour-preserving accessors and
//! drivers used only by the `crate::native::tests` harness to exercise the
//! production code paths without a window or GPU.
//!
//! Mechanically split out of `app/mod.rs` to keep that file under the
//! source-size cap; no behaviour or API change. These are `App` methods in a
//! child module so they reach `App`'s private fields directly. Reachable from
//! `crate::native::tests`, so they are `pub(in crate::native)` (the move's only
//! required visibility widening, from the in-`app` `pub(super)`).

use super::*;

impl App {
    /// Resize the terminal model and PTY to fit the new physical surface size.
    ///
    /// Idempotent: when the computed whole-cell grid is unchanged (a sub-cell
    /// pixel change, or a duplicate event), no model or PTY resize is performed
    /// and `false` is returned. The GPU surface itself is reconfigured by the
    /// caller regardless, since it tracks pixel size, not the cell grid.
    ///
    /// Lock scopes are kept tight and never nested: the terminal mutex is taken
    /// and dropped for the model resize, then the PTY mutex is taken and dropped
    /// for the (non-blocking) `TIOCSWINSZ`. Neither is held across the other or
    /// across any GPU call.
    #[cfg(test)]
    pub(in crate::native) fn resize_grid(
        &mut self,
        cell: CellSize,
        width_px: u32,
        height_px: u32,
    ) -> bool {
        self.resize_grid_with_padding(cell, WindowPadding::ZERO, width_px, height_px)
    }

    /// Test seam (UX4-P1): open the settings overlay through the production
    /// keyboard entry path (so the pointer-state reset is genuinely exercised),
    /// without a window/GPU.
    #[cfg(test)]
    pub(in crate::native) fn open_settings_overlay_for_test(&mut self) {
        self.toggle_settings_overlay();
    }

    /// Test seam (UX4-P1): close the overlay (Esc-equivalent), without a
    /// window/GPU.
    #[cfg(test)]
    pub(in crate::native) fn close_overlay_for_test(&mut self) {
        self.overlay.close();
    }

    /// Test seam (UX4-P1): inject a cached pointer cell, as `update_pointer_cell`
    /// would after a `CursorMoved`, so a press has coordinates.
    #[cfg(test)]
    pub(in crate::native) fn set_pointer_cell_for_test(&mut self, row: usize, column: usize) {
        self.pointer_cell = Some(CellPoint { row, column });
    }

    /// Test seam (UX4-P1): the live overlay rect for the current grid.
    #[cfg(test)]
    pub(in crate::native) fn overlay_rect_for_test(
        &self,
    ) -> Option<crate::native::overlay::OverlayRect> {
        overlay_rect(&self.overlay, self.grid.columns, self.grid.rows)
    }

    /// Test seam (UX4-P1): whether a local text selection is in progress.
    #[cfg(test)]
    pub(in crate::native) fn selecting_for_test(&self) -> bool {
        self.pointer_drag.is_selecting()
    }

    /// Test seam (MOUSE-EXTEND): force the drag-extend feature flag so a test can
    /// exercise both the default-on path and the byte-identical off branch.
    #[cfg(test)]
    pub(in crate::native) fn set_selection_drag_extend_for_test(&mut self, on: bool) {
        self.settings.selection_drag_extend = on;
    }

    /// Test seam (MOUSE-EXTEND): drive the left-press selection dispatch (the
    /// click-count / Shift+click-extend entry the `MouseInput` arm calls).
    #[cfg(test)]
    pub(in crate::native) fn begin_selection_for_test(&mut self) {
        self.begin_selection();
    }

    /// Test seam (MOUSE-EXTEND): drive the left-release finalize the
    /// `MouseInput` arm calls.
    #[cfg(test)]
    pub(in crate::native) fn finish_selection_for_test(&mut self) {
        self.finish_selection();
    }

    /// Test seam (MOUSE-EXTEND): drive the granularity-aware drag-extend the
    /// `CursorMoved` handler runs, without a GPU/pixel path. Wraps the
    /// production `extend_drag_to` (not a parallel reimplementation).
    #[cfg(test)]
    pub(in crate::native) fn extend_drag_to_cell_for_test(&mut self, row: usize, column: usize) {
        self.extend_drag_to(CellPoint { row, column });
    }

    /// Test seam (MOUSE-EXTEND): the text the current selection would copy,
    /// through the exact `current_selection_text` path PRIMARY/CLIPBOARD use.
    #[cfg(test)]
    pub(in crate::native) fn selection_text_for_test(&self) -> Option<String> {
        self.current_selection_text()
    }

    /// Test seam (MOUSE-EXTEND): whether finishing the current drag would write
    /// PRIMARY. Lets a regression prove a plain double/triple-click (no drag)
    /// stays no-write (parity) while a drag that extended does write.
    #[cfg(test)]
    pub(in crate::native) fn drag_should_write_primary_for_test(&self) -> bool {
        self.drag_selection_should_write_primary()
    }

    /// Test seam (MOUSE-EXTEND): set the Shift modifier so a Shift+click-extend
    /// gesture can be driven through `begin_selection`.
    #[cfg(test)]
    pub(in crate::native) fn set_shift_modifier_for_test(&mut self, shift: bool) {
        self.modifiers.shift = shift;
    }

    /// Test seam (CTRL-WHEEL-ZOOM): set the Ctrl modifier so a Ctrl+wheel zoom
    /// gesture can be driven through `handle_mouse_wheel`.
    #[cfg(test)]
    pub(in crate::native) fn set_ctrl_modifier_for_test(&mut self, ctrl: bool) {
        self.modifiers.ctrl = ctrl;
    }

    /// Test seam (MOUSE-RECT): set the Alt modifier so an Alt+drag block
    /// selection can be driven through the production `begin_selection` route.
    #[cfg(test)]
    pub(in crate::native) fn set_alt_modifier_for_test(&mut self, alt: bool) {
        self.modifiers.alt = alt;
    }

    /// Test seam (MOUSE-RECT): whether the live selection is a block (column)
    /// selection, so a test can prove the Alt gesture armed block mode and a
    /// plain drag did not.
    #[cfg(test)]
    pub(in crate::native) fn selection_is_block_for_test(&self) -> bool {
        self.selection_block
    }

    /// Test seam (CTRL-WHEEL-ZOOM): toggle the `wheel_zoom` setting so the
    /// inverted-gate (off-switch) parity can be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_wheel_zoom_for_test(&mut self, on: bool) {
        self.settings.wheel_zoom = on;
    }

    /// Test seam (U4): the theme currently published to the renderer (the
    /// authored theme after CVD adaptation).
    #[cfg(test)]
    pub(in crate::native) fn effective_theme_for_test(&self) -> Theme {
        self.effective_theme
    }

    /// Test seam (U4): drive a live CVD settings change through the real
    /// `apply_settings` chokepoint (the overlay-edit path), exactly as toggling
    /// the Accessibility group would.
    #[cfg(test)]
    pub(in crate::native) fn apply_cvd_for_test(
        &mut self,
        mode: crate::settings::CvdMode,
        strength: f32,
    ) {
        let mut next = self.settings.clone();
        next.cvd_mode = mode;
        next.cvd_strength = strength;
        self.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);
    }

    /// Test seam (CTRL-WHEEL-ZOOM): the current live font size in pixels.
    #[cfg(test)]
    pub(in crate::native) fn font_size_px_for_test(&self) -> f32 {
        self.settings.font_size_px
    }

    /// Test seam (CTRL-WHEEL-ZOOM): drive a vertical wheel notch through the
    /// production wheel routing (`handle_mouse_wheel`), so the zoom-vs-scroll-vs
    /// -report precedence is pinned, not reimplemented. Positive = wheel up.
    #[cfg(test)]
    pub(in crate::native) fn dispatch_wheel_for_test(&mut self, vertical_notches: f32) {
        self.handle_mouse_wheel(MouseScrollDelta::LineDelta(0.0, vertical_notches));
    }

    /// Test seam (UX4-P1): the held mouse-report button, if any.
    #[cfg(test)]
    pub(in crate::native) fn report_button_for_test(&self) -> Option<CoreMouseButton> {
        self.report_button
    }

    /// Test seam (UX4-P1): whether the no-overlay path would report the pointer
    /// to the PTY (TUI mouse mode active and Shift not held). Lets a precedence
    /// test assert reporting is armed yet an overlay press still does not leak.
    #[cfg(test)]
    pub(in crate::native) fn would_report_mouse_to_pty_for_test(&self) -> bool {
        self.should_report_mouse_to_pty()
    }

    /// Test seam (UX4-P1): the overlay render signature (mode + panel state).
    #[cfg(test)]
    pub(in crate::native) fn overlay_signature_for_test(
        &self,
    ) -> crate::native::overlay::OverlayRenderSignature {
        self.overlay.render_signature()
    }

    /// Test seam (UX4-P2): absolute track-end cells for the first visible
    /// slider, so a test can drive a real press/drag/release through the App.
    #[cfg(test)]
    pub(in crate::native) fn overlay_first_slider_track_cells_for_test(
        &self,
    ) -> Option<(CellPoint, CellPoint)> {
        self.overlay
            .first_slider_track_cells(self.grid.columns, self.grid.rows)
    }

    /// Test seam (UX4-P2): whether a settings-panel slider drag is in progress.
    #[cfg(test)]
    pub(in crate::native) fn overlay_is_dragging_for_test(&self) -> bool {
        self.overlay.is_settings_dragging()
    }

    /// Test seam (UX4-P2 review): drive the exact focus-loss drag-cancel the
    /// `WindowEvent::Focused(false)` arm runs, so a regression can prove a lost
    /// release on focus loss cannot leave a slider drag armed while the overlay
    /// stays open. Wraps the production helper (not a parallel reimplementation).
    #[cfg(test)]
    pub(in crate::native) fn cancel_overlay_drag_on_focus_loss_for_test(&mut self) {
        self.cancel_overlay_drag_on_focus_loss();
    }

    /// Test seam (UX4-P1): arm a held TUI mouse-report button exactly as a real
    /// reported press would, so a regression test can prove overlay entry clears
    /// it. Wraps the (module-private) `handle_reported_mouse_input`.
    #[cfg(test)]
    pub(in crate::native) fn arm_reported_mouse_press_for_test(&mut self, button: CoreMouseButton) {
        self.handle_reported_mouse_input(ElementState::Pressed, button);
    }

    /// Test seam (MOUSE-SCROLLBAR): inject a cell size so the pointer hit-test
    /// can run headlessly (no GPU). See [`App::test_cell`].
    #[cfg(test)]
    pub(in crate::native) fn set_test_cell_for_test(&mut self, cell: CellSize) {
        self.test_cell = Some(cell);
    }

    /// Test seam (MOUSE-SCROLLBAR): toggle the `scrollbar_drag` setting so the
    /// inverted-gate (off-switch) parity can be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_scrollbar_drag_for_test(&mut self, on: bool) {
        self.settings.scrollbar_drag = on;
    }

    /// Test seam (MOUSE-SCROLLBAR): set the cached raw pointer pixel position the
    /// button handlers hit-test against (button events carry no coordinates).
    #[cfg(test)]
    pub(in crate::native) fn set_pointer_px_for_test(&mut self, x: f64, y: f64) {
        self.pointer_px = Some((x, y));
    }

    /// Test seam (MOUSE-SCROLLBAR): scroll the viewport up into history so the
    /// scroll thumb becomes visible (offset clamps to the scrollback length).
    #[cfg(test)]
    pub(in crate::native) fn scroll_up_for_test(&mut self, lines: usize) {
        let scrollback_len = self.scrollback_len();
        self.viewport.scroll_up(lines, scrollback_len);
    }

    /// Test seam (MOUSE-SCROLLBAR): the current scrollback length.
    #[cfg(test)]
    pub(in crate::native) fn scrollback_len_for_test(&self) -> usize {
        self.scrollback_len()
    }

    /// Test seam (MOUSE-SCROLLBAR): the live viewport offset.
    #[cfg(test)]
    pub(in crate::native) fn viewport_offset_for_test(&self) -> usize {
        self.viewport.offset()
    }

    /// Test seam (MOUSE-SCROLLBAR): enable a TUI mouse-reporting mode (DECSET
    /// 1000) on the underlying terminal, so a press routes through the report
    /// path unless the scroll-thumb grab captures it first.
    #[cfg(test)]
    pub(in crate::native) fn enable_mouse_reporting_for_test(&mut self) {
        if let Ok(mut terminal) = self.terminal.lock() {
            terminal.advance(b"\x1b[?1000h");
        }
    }

    /// Test seam (MOUSE-SCROLLBAR): drive a real left button event through the
    /// production routing and classify the outcome, so the press precedence
    /// (scroll-thumb grab vs PTY report vs local selection) can be pinned
    /// without a GPU or a winit event loop.
    #[cfg(test)]
    pub(in crate::native) fn left_button_outcome_for_test(
        &mut self,
        pressed: bool,
    ) -> &'static str {
        let state = if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        };
        self.handle_mouse_input(state, WinitMouseButton::Left);
        if self.pointer_drag.scrollbar_grab().is_some() {
            "grab"
        } else if self.report_button.is_some() {
            "report"
        } else if self.pointer_drag.is_selecting() {
            "select"
        } else {
            "idle"
        }
    }
}
