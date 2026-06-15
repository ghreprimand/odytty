// SPDX-License-Identifier: GPL-3.0-only
//! Pointer-driven interaction for the native app: mouse reporting, text
//! selection, hyperlink hover/open, and scrollback viewport movement.
//!
//! Mechanically split out of `app/mod.rs` (MS3) to keep that file under the
//! source-size cap; no behavior or API change. These are `App` methods that
//! live in a child module so they can reach `App`'s private fields and the
//! sibling methods that stayed in `app/mod.rs` directly. Methods the parent
//! `app` module calls back into are marked `pub(super)`.

use super::*;

impl App {
    fn mouse_protocol(&self) -> MouseProtocol {
        self.terminal
            .lock()
            .map(|terminal| terminal.mouse_protocol())
            .unwrap_or_default()
    }

    fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_protocol().is_enabled()
    }

    /// Shift is the local-selection escape hatch while a TUI has enabled mouse
    /// reporting, matching the common xterm-family terminal convention.
    pub(super) fn should_report_mouse_to_pty(&self) -> bool {
        self.mouse_reporting_enabled() && !self.modifiers.shift
    }

    /// Route an overlay [`OverlayOutcome`] (from either the keyboard or the
    /// pointer path) through the shared App-side handlers, so the two entry
    /// points stay in lockstep (UX4-P1).
    pub(super) fn apply_overlay_outcome(&mut self, outcome: OverlayOutcome) {
        match outcome {
            OverlayOutcome::Consumed => {}
            OverlayOutcome::Close => self.overlay.close(),
            OverlayOutcome::OpenThemePicker => self.open_theme_picker_overlay(),
            OverlayOutcome::OpenThemeBuilder => self.open_theme_builder_overlay(),
            OverlayOutcome::ApplySettings(settings) => self.apply_overlay_settings(settings),
            OverlayOutcome::SaveSettings(changes) => self.save_overlay_settings(&changes),
            OverlayOutcome::SaveTheme(request) => self.save_overlay_theme(request),
        }
    }

    /// Translate a winit mouse button edge over an open overlay into an
    /// [`OverlayPointer::Press`]/`Release` and apply the outcome (UX4-P1/P2).
    /// Press drives clicks and arms a slider drag; release ends a drag. Middle/
    /// other buttons are dropped so no PRIMARY paste fires while the overlay is
    /// up and so a stray middle release cannot disturb a drag.
    pub(in crate::native) fn handle_overlay_pointer_button(
        &mut self,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        let Some(cell) = self.pointer_cell else {
            return;
        };
        let button = match button {
            WinitMouseButton::Left => PointerButton::Left,
            WinitMouseButton::Right => PointerButton::Right,
            _ => return,
        };
        let Some(rect) = overlay_rect(&self.overlay, self.grid.columns, self.grid.rows) else {
            return;
        };
        let pointer = match state {
            ElementState::Pressed => OverlayPointer::Press { cell, button },
            ElementState::Released => OverlayPointer::Release { cell, button },
        };
        let outcome = self.overlay.handle_pointer(pointer, rect);
        self.apply_overlay_outcome(outcome);
        self.request_selection_redraw();
    }

    /// Drive an in-progress slider drag from the cached pointer cell (UX4-P2).
    /// Gated on an active drag so ordinary hover over the open overlay stays a
    /// cheap no-op (no redraw, no PTY/selection work).
    pub(in crate::native) fn handle_overlay_pointer_move(&mut self) {
        if !self.overlay.is_settings_dragging() {
            return;
        }
        let Some(cell) = self.pointer_cell else {
            return;
        };
        let Some(rect) = overlay_rect(&self.overlay, self.grid.columns, self.grid.rows) else {
            return;
        };
        let outcome = self
            .overlay
            .handle_pointer(OverlayPointer::Move { cell }, rect);
        self.apply_overlay_outcome(outcome);
        self.request_selection_redraw();
    }

    /// Translate a winit wheel event over an open overlay into an
    /// [`OverlayPointer::Wheel`] free-scroll of the panel list (UX4-P1).
    pub(in crate::native) fn handle_overlay_pointer_wheel(&mut self, delta: MouseScrollDelta) {
        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        let lines = wheel_lines(delta, cell_height);
        if lines == 0 {
            return;
        }
        let Some(rect) = overlay_rect(&self.overlay, self.grid.columns, self.grid.rows) else {
            return;
        };
        // `wheel_lines` is positive for wheel-up (toward earlier content); the
        // settings list scrolls toward earlier entries (lower index), so negate.
        let outcome = self
            .overlay
            .handle_pointer(OverlayPointer::Wheel { lines: -lines }, rect);
        self.apply_overlay_outcome(outcome);
        self.request_selection_redraw();
    }

    fn update_hover_hyperlink(&mut self) {
        let hovered = self
            .pointer_cell
            .and_then(|point| self.visible_cell_hyperlink(point));
        if self.hovered_hyperlink != hovered {
            self.hovered_hyperlink = hovered;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    fn visible_cell_hyperlink(&self, point: CellPoint) -> Option<LinkId> {
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        snapshot
            .cells
            .get(point.row * snapshot.dimensions.columns + point.column)
            .and_then(|cell| cell.attrs.hyperlink)
    }

    fn hovered_hyperlink_uri(&self) -> Option<String> {
        let id = self.hovered_hyperlink?;
        self.terminal
            .lock()
            .ok()?
            .hyperlink(id)
            .map(|link| link.uri.clone())
    }

    pub(super) fn try_open_hovered_hyperlink(&mut self) -> bool {
        if !hyperlink_action_allowed(self.modifiers, self.mouse_reporting_enabled()) {
            return false;
        }
        let Some(uri) = self.hovered_hyperlink_uri() else {
            return false;
        };
        if !openable_hyperlink_uri(&uri) {
            return false;
        }

        // Security: OdyTTY never auto-opens OSC 8 links. A URI is opened only
        // after explicit Ctrl+click, scheme allowlist filtering, and direct
        // argv passing to xdg-open. No shell interpolation is involved.
        let _ = Command::new("xdg-open")
            .arg(uri)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        true
    }

    pub(super) fn write_pty_bytes(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn send_mouse_report(&mut self, button: CoreMouseButton, kind: MouseEventKind) -> bool {
        let protocol = self.mouse_protocol();
        // SGR-pixel (1016) reports true 1-based physical pixel coordinates; every
        // other encoding (legacy/UTF-8/SGR/urxvt) reports cells. Only 1016 takes
        // the pixel seam — the cell path is untouched for all other modes.
        let bytes = if protocol.encoding == MouseEncoding::SgrPixel {
            self.encode_pixel_mouse_report(protocol, button, kind)
        } else {
            self.pointer_cell.and_then(|point| {
                encode_native_mouse_report(protocol, point, button, kind, self.modifiers)
            })
        };
        let Some(bytes) = bytes else {
            return false;
        };

        self.return_to_live();
        self.write_pty_bytes(&bytes);
        true
    }

    /// Encode an SGR-pixel (1016) mouse report from the cached physical pointer
    /// position. Returns `None` until a cursor position and GPU cell metrics are
    /// available, or when the active tracking gate drops the event (the core
    /// encoder applies the same gating as the cell path). The grid is drawn at
    /// the window origin, so the cached physical position is already
    /// grid-relative; [`pixel_coords_for_report`] floors it to a 1-based pixel
    /// and clamps to the grid's pixel extent after removing any window padding.
    fn encode_pixel_mouse_report(
        &self,
        protocol: MouseProtocol,
        button: CoreMouseButton,
        kind: MouseEventKind,
    ) -> Option<Vec<u8>> {
        let (x_px, y_px) = self.pointer_px?;
        let gpu = self.gpu.as_ref()?;
        let cell = gpu.cell();
        let (px, py) = pixel_coords_for_report(x_px, y_px, cell, self.grid, gpu.window_padding());
        let mods = MouseModifiers {
            // Shift stays reserved for local selection while reporting is active,
            // matching the cell path's modifier policy.
            shift: false,
            alt: self.modifiers.alt,
            ctrl: self.modifiers.ctrl,
        };
        encode_mouse_event_pixel(protocol, button, kind, px, py, mods)
    }

    fn send_mouse_motion_report(&mut self) {
        let protocol = self.mouse_protocol();
        let Some(button) = motion_report_button(protocol, self.report_button) else {
            return;
        };
        let _ = self.send_mouse_report(button, MouseEventKind::Motion);
    }

    pub(super) fn send_focus_report(&mut self, focused: bool) {
        let Some(bytes) = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| encode_native_focus_report(&terminal, focused))
        else {
            return;
        };

        self.write_pty_bytes(&bytes);
    }

    pub(super) fn handle_reported_mouse_input(
        &mut self,
        state: ElementState,
        button: CoreMouseButton,
    ) {
        match state {
            ElementState::Pressed => {
                self.report_button = Some(button);
                let _ = self.send_mouse_report(button, MouseEventKind::Press);
            }
            ElementState::Released => {
                let _ = self.send_mouse_report(button, MouseEventKind::Release);
                if self.report_button == Some(button) {
                    self.report_button = None;
                }
            }
        }
    }

    pub(super) fn handle_reported_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let Some(button) = wheel_report_button(delta) else {
            return false;
        };
        self.send_mouse_report(button, MouseEventKind::Press)
    }

    pub(super) fn update_pointer_cell(&mut self, x_px: f64, y_px: f64) {
        let Some(cell) = self.gpu.as_ref().map(GpuState::cell) else {
            return;
        };
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let point = selection::cell_at_physical_with_padding(x_px, y_px, cell, self.grid, padding);
        self.pointer_cell = Some(point);
        self.pointer_px = Some((x_px, y_px));
        // UX4-P1/P2: while an overlay is open it owns the pointer. Keep caching
        // the coordinates above (a press needs them), but skip link hover, local
        // selection, and PTY motion reports — they belong to the terminal grid
        // beneath the panel. A move is forwarded to the overlay only to advance
        // an active slider drag (UX4-P2); non-drag hover is a no-op.
        if self.overlay.is_open() {
            self.handle_overlay_pointer_move();
            return;
        }
        self.update_hover_hyperlink();
        if self.pointer_drag.is_selecting() {
            self.autoscroll_selection_if_needed(y_px, cell, padding);
            self.extend_drag_to(point);
            self.request_selection_redraw();
        } else if self.should_report_mouse_to_pty() || self.report_button.is_some() {
            self.send_mouse_motion_report();
        }
    }

    pub(super) fn begin_selection(&mut self) {
        let Some(point) = self.pointer_cell else {
            return;
        };
        // MOUSE-EXTEND: Shift+click extends an existing selection (keep the
        // anchor, move the focus to the click) instead of starting a new one.
        // Reached only on the local path (the report decision already ran), so
        // Shift stays the selection-vs-passthrough seam untouched. Gated by the
        // feature flag and an existing selection; otherwise fall through to the
        // historical click-count dispatch.
        if self.settings.selection_drag_extend
            && self.modifiers.shift
            && self.selection.range().is_some()
        {
            let scrollback_len = self.scrollback_len();
            self.selection.update(selection::visible_to_absolute(
                point,
                self.viewport.offset(),
                scrollback_len,
            ));
            self.pointer_drag = PointerDrag::Select {
                granularity: SelectGranularity::Char,
                block: false,
            };
            self.drag_anchor_unit = None;
            self.last_selection_autoscroll = None;
            self.request_selection_redraw();
            return;
        }
        match self.clicks.register_click(point, Instant::now()) {
            1 => self.begin_drag_selection(point),
            2 => self.select_word(point),
            _ => self.select_line(point),
        }
    }

    fn begin_drag_selection(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        self.selection.begin(selection::visible_to_absolute(
            point,
            self.viewport.offset(),
            scrollback_len,
        ));
        self.pointer_drag = PointerDrag::Select {
            granularity: SelectGranularity::Char,
            block: false,
        };
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    fn select_word(&mut self, point: CellPoint) {
        let (snapshot, scrollback_len) = self.selection_snapshot();
        let Some(range) = selection::word_range_at(&snapshot, point) else {
            // No word under the pointer: clear and finalize exactly as before
            // (nothing to anchor a word-drag to), regardless of the flag.
            self.selection.clear();
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
            self.request_selection_redraw();
            return;
        };

        let absolute =
            selection::absolute_range_from_visible(range, self.viewport.offset(), scrollback_len);
        self.selection.set_range(absolute);
        self.finalize_or_arm_unit_drag(SelectGranularity::Word, absolute);
        self.request_selection_redraw();
    }

    fn select_line(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let Some(range) = selection::line_range_at(point, self.grid) else {
            return;
        };

        let absolute =
            selection::absolute_range_from_visible(range, self.viewport.offset(), scrollback_len);
        self.selection.set_range(absolute);
        self.finalize_or_arm_unit_drag(SelectGranularity::Line, absolute);
        self.request_selection_redraw();
    }

    /// MOUSE-EXTEND: after a double/triple-click sets a word/line range, either
    /// keep the drag live so a follow-on drag extends by that unit (flag on) or
    /// finalize byte-identically to the historical click-to-finish behavior
    /// (flag off). The off branch is the mandated parity path.
    fn finalize_or_arm_unit_drag(
        &mut self,
        granularity: SelectGranularity,
        anchor: AbsoluteSelectionRange,
    ) {
        if self.settings.selection_drag_extend {
            self.pointer_drag = PointerDrag::Select {
                granularity,
                block: false,
            };
            self.drag_anchor_unit = Some(anchor);
        } else {
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
        }
    }

    /// Extend the in-progress drag-selection to a visible cell, honoring the
    /// active granularity (MOUSE-EXTEND). Char follows the pointer exactly (the
    /// historical drag); Word/Line snap to and union with whole words/lines.
    pub(super) fn extend_drag_to(&mut self, point: CellPoint) {
        match self.pointer_drag {
            PointerDrag::Select {
                granularity: SelectGranularity::Char,
                ..
            } => {
                let scrollback_len = self.scrollback_len();
                self.selection.update(selection::visible_to_absolute(
                    point,
                    self.viewport.offset(),
                    scrollback_len,
                ));
            }
            PointerDrag::Select {
                granularity: SelectGranularity::Word,
                ..
            } => self.extend_word_drag(point),
            PointerDrag::Select {
                granularity: SelectGranularity::Line,
                ..
            } => self.extend_line_drag(point),
            PointerDrag::None | PointerDrag::Scrollbar => {}
        }
    }

    fn extend_word_drag(&mut self, point: CellPoint) {
        let Some(anchor) = self.drag_anchor_unit else {
            return;
        };
        let (snapshot, scrollback_len) = self.selection_snapshot();
        let offset = self.viewport.offset();
        let focus_unit = selection::word_range_at(&snapshot, point)
            .map(|range| selection::absolute_range_from_visible(range, offset, scrollback_len))
            .unwrap_or_else(|| {
                // No word under the pointer (e.g. whitespace): extend to the
                // pointer cell as a degenerate unit so the drag still grows.
                let p = selection::visible_to_absolute(point, offset, scrollback_len);
                AbsoluteSelectionRange { start: p, end: p }
            });
        self.selection
            .set_range(selection::union_absolute_ranges(anchor, focus_unit));
    }

    fn extend_line_drag(&mut self, point: CellPoint) {
        let Some(anchor) = self.drag_anchor_unit else {
            return;
        };
        let scrollback_len = self.scrollback_len();
        let offset = self.viewport.offset();
        let Some(range) = selection::line_range_at(point, self.grid) else {
            return;
        };
        let focus_unit = selection::absolute_range_from_visible(range, offset, scrollback_len);
        self.selection
            .set_range(selection::union_absolute_ranges(anchor, focus_unit));
    }

    pub(super) fn request_selection_redraw(&mut self) {
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn selection_snapshot(&self) -> (Snapshot, usize) {
        let terminal = self.terminal.lock().expect("terminal mutex");
        (
            terminal.snapshot_with_scrollback(self.viewport.offset()),
            terminal.screen().scrollback_len(),
        )
    }

    fn autoscroll_selection_if_needed(
        &mut self,
        y_px: f64,
        cell: CellSize,
        padding: WindowPadding,
    ) {
        // MOUSE-AUTOSCROLL-VEL: the step magnitude ramps with how far the pointer
        // is dragged past the edge band, up to the configured cap. `legacy` mode
        // returns a cap of 1, which makes the helper yield exactly ±1/0 —
        // byte-identical to the historical fixed one-row-per-tick autoscroll.
        let max_rows = self.settings.autoscroll_max_rows();
        let delta =
            selection::drag_autoscroll_step_with_padding(y_px, cell, self.grid, padding, max_rows);
        if delta == 0 {
            return;
        }

        let now = Instant::now();
        if self
            .last_selection_autoscroll
            .is_some_and(|last| now.saturating_duration_since(last) < SELECTION_AUTOSCROLL_INTERVAL)
        {
            return;
        }
        self.last_selection_autoscroll = Some(now);
        self.scroll_viewport(delta);
    }

    pub(super) fn finish_selection(&mut self) {
        if !self.pointer_drag.is_selecting() {
            return;
        }
        // MOUSE-EXTEND parity: a plain double/triple-click that never dragged
        // must stay byte-identical to the historical finalize, which wrote
        // nothing to PRIMARY. Only write when a char drag ran (today's behavior)
        // or a word/line drag actually grew the selection past its clicked unit.
        if self.drag_selection_should_write_primary() {
            self.write_primary_selection();
            // MOUSE-COPYSELECT: when enabled, also write the CLIPBOARD via the
            // exact copy-shortcut path. Off by default, so the historical
            // PRIMARY-only finish is byte-identical.
            if self.settings.copy_on_select {
                self.handle_copy_shortcut();
            }
        }
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    /// Whether finishing the current drag should write PRIMARY (MOUSE-EXTEND).
    /// Char drags write as before (an empty selection no-ops in
    /// `current_selection_text`, so a plain single click stays a no-op too).
    /// Word/Line drags write only when the selection grew beyond the anchored
    /// click unit, so a plain double/triple-click without a drag stays no-write
    /// — byte-identical to the historical finalize.
    pub(super) fn drag_selection_should_write_primary(&self) -> bool {
        match self.pointer_drag {
            PointerDrag::Select {
                granularity: SelectGranularity::Char,
                ..
            } => true,
            PointerDrag::Select { .. } => match (self.selection.range(), self.drag_anchor_unit) {
                (Some(current), Some(anchor)) => current != anchor,
                _ => true,
            },
            PointerDrag::None | PointerDrag::Scrollbar => false,
        }
    }

    /// Number of rows a Shift+PageUp/PageDown press scrolls: one screenful less
    /// one row of overlap for continuity (at least one row).
    pub(super) fn page_lines(&self) -> usize {
        self.grid.rows.saturating_sub(1).max(1)
    }

    /// Current scrollback length from the shared model (0 if the lock is
    /// poisoned), used to clamp upward scrolling.
    pub(super) fn scrollback_len(&self) -> usize {
        self.terminal
            .lock()
            .map(|t| t.screen().scrollback_len())
            .unwrap_or(0)
    }

    /// Adjust the scrollback viewport. `delta > 0` pages up into history,
    /// `delta < 0` pages toward the live bottom. Selections are stored against
    /// absolute rows, so moving the viewport keeps their anchors meaningful.
    /// With no scrollback this is a clamped no-op (never panics).
    pub(super) fn scroll_viewport(&mut self, delta: isize) {
        let changed = match delta.cmp(&0) {
            std::cmp::Ordering::Greater => self
                .viewport
                .scroll_up(delta as usize, self.scrollback_len()),
            std::cmp::Ordering::Less => self.viewport.scroll_down((-delta) as usize),
            std::cmp::Ordering::Equal => false,
        };
        if changed {
            self.on_viewport_changed();
        }
    }

    /// Return the viewport to the live bottom (offset 0). Called whenever input
    /// is written to the PTY so typing always jumps back to the prompt.
    pub(super) fn return_to_live(&mut self) {
        if self.viewport.reset_to_live() {
            self.on_viewport_changed();
        }
    }

    /// Shared side effects of a viewport offset change: keep absolute
    /// selections intact and request one rebuild/redraw so their visible
    /// intersection is recomputed.
    pub(super) fn on_viewport_changed(&mut self) {
        self.hovered_hyperlink = self
            .pointer_cell
            .and_then(|point| self.visible_cell_hyperlink(point));
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// Convert a physical cursor pixel position to 1-based terminal pixel
/// coordinates for SGR-pixel (1016) mouse reporting, clamped to the grid's
/// pixel extent.
///
/// `x_px`/`y_px` are the raw `winit` `CursorMoved` coordinates, which are
/// already physical pixels; `CellSize` and `padding` are likewise
/// physical-pixel sized. The result first subtracts the window padding to get
/// grid-relative coordinates, then floors to an integer pixel and shifts to the
/// 1-based convention the protocol uses. A cursor left of or above the grid
/// clamps to pixel 1; a cursor at or past the right/bottom edge (e.g. while
/// dragging outside the window) clamps to the last in-grid pixel, mirroring how
/// [`selection::cell_at_physical_with_padding`] saturates the cell path.
fn pixel_coords_for_report(
    x_px: f64,
    y_px: f64,
    cell: CellSize,
    dims: Dimensions,
    padding: WindowPadding,
) -> (usize, usize) {
    let max_px = (dims.columns as u32)
        .saturating_mul(cell.width.max(1))
        .max(1);
    let max_py = (dims.rows as u32).saturating_mul(cell.height.max(1)).max(1);
    let pad = f64::from(padding.physical_px());
    let px = ((x_px - pad).max(0.0) as u32).min(max_px - 1) as usize + 1;
    let py = ((y_px - pad).max(0.0) as u32).min(max_py - 1) as usize + 1;
    (px, py)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MouseTracking;

    // --- MS2: SGR-pixel (1016) native pixel seam ---

    fn cell_8x16() -> CellSize {
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        }
    }

    #[test]
    fn pixel_coords_origin_maps_to_one_based() {
        // Cursor at the top-left physical pixel maps to (1, 1): the protocol is
        // 1-based and zero padding keeps the grid at the window origin.
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(0.0, 0.0, cell_8x16(), dims, WindowPadding::ZERO),
            (1, 1)
        );
    }

    #[test]
    fn pixel_coords_floor_then_one_base() {
        // Sub-pixel fractions floor; 10.9px -> pixel index 10 -> 1-based 11.
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(10.9, 33.2, cell_8x16(), dims, WindowPadding::ZERO),
            (11, 34)
        );
    }

    #[test]
    fn pixel_coords_are_independent_of_cell_size() {
        // The pixel path reports raw physical pixels, NOT cells: the same cursor
        // position yields the same pixel coords regardless of cell metrics
        // (a larger cell only changes the clamp extent, not the mapping).
        let dims = Dimensions::new(80, 24);
        let small = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let large = CellSize {
            width: 20,
            height: 40,
            baseline: 30,
        };
        assert_eq!(
            pixel_coords_for_report(100.0, 100.0, small, dims, WindowPadding::ZERO),
            pixel_coords_for_report(100.0, 100.0, large, dims, WindowPadding::ZERO)
        );
    }

    #[test]
    fn pixel_coords_clamp_negative_to_one() {
        // A cursor left of / above the grid (negative physical coords during a
        // drag) saturates to pixel 1, mirroring cell_at_physical's max(0.0).
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(-50.0, -5.0, cell_8x16(), dims, WindowPadding::ZERO),
            (1, 1)
        );
    }

    #[test]
    fn pixel_coords_clamp_to_grid_extent() {
        // Grid is 80x24 cells of 8x16 px = 640x384 px. A cursor at or beyond the
        // bottom-right edge clamps to the last in-grid pixel (640, 384).
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(640.0, 384.0, cell_8x16(), dims, WindowPadding::ZERO),
            (640, 384)
        );
        assert_eq!(
            pixel_coords_for_report(9999.0, 9999.0, cell_8x16(), dims, WindowPadding::ZERO),
            (640, 384)
        );
    }

    #[test]
    fn pixel_coords_last_in_grid_pixel_is_not_clamped() {
        // 639.0px -> index 639 -> 1-based 640, the max; still inside the grid so
        // it is reported as-is (the clamp only bites at/after the extent).
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(639.0, 383.0, cell_8x16(), dims, WindowPadding::ZERO),
            (640, 384)
        );
    }

    #[test]
    fn pixel_coords_subtract_window_padding_before_reporting() {
        let dims = Dimensions::new(80, 24);
        let padding = WindowPadding::from_logical(8.0, 1.0);

        assert_eq!(
            pixel_coords_for_report(8.0, 8.0, cell_8x16(), dims, padding),
            (1, 1)
        );
        assert_eq!(
            pixel_coords_for_report(18.9, 41.2, cell_8x16(), dims, padding),
            (11, 34)
        );
    }

    #[test]
    fn sgr_pixel_encoder_emits_pixel_wire_shape() {
        // The 1016 seam feeds computed pixel coords to the core encoder, which
        // emits the SGR wire shape with those pixel values (here 101;201).
        let protocol = MouseProtocol {
            tracking: MouseTracking::Normal,
            encoding: MouseEncoding::SgrPixel,
        };
        let dims = Dimensions::new(80, 24);
        let (px, py) =
            pixel_coords_for_report(100.0, 200.0, cell_8x16(), dims, WindowPadding::ZERO);
        let mods = MouseModifiers {
            shift: false,
            alt: false,
            ctrl: false,
        };
        let bytes = encode_mouse_event_pixel(
            protocol,
            CoreMouseButton::Left,
            MouseEventKind::Press,
            px,
            py,
            mods,
        )
        .expect("1016 press encodes");
        assert_eq!(bytes, b"\x1b[<0;101;201M");
    }

    #[test]
    fn pixel_encoder_guard_rejects_non_1016_encodings() {
        // The pixel encoder only fires for SgrPixel; for every other encoding it
        // returns None, so send_mouse_report's branch leaves the cell path
        // authoritative for legacy/UTF-8/SGR/urxvt.
        let dims = Dimensions::new(80, 24);
        let (px, py) =
            pixel_coords_for_report(100.0, 200.0, cell_8x16(), dims, WindowPadding::ZERO);
        let mods = MouseModifiers {
            shift: false,
            alt: false,
            ctrl: false,
        };
        for encoding in [
            MouseEncoding::Default,
            MouseEncoding::Utf8,
            MouseEncoding::Sgr,
            MouseEncoding::Urxvt,
        ] {
            let protocol = MouseProtocol {
                tracking: MouseTracking::Normal,
                encoding,
            };
            assert!(
                encode_mouse_event_pixel(
                    protocol,
                    CoreMouseButton::Left,
                    MouseEventKind::Press,
                    px,
                    py,
                    mods,
                )
                .is_none(),
                "encoding {encoding:?} must not take the pixel seam"
            );
        }
    }
}
