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
use winit::keyboard::KeyCode;

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

    /// Test seam (OVERLAY-SMALL-WINDOW): open the connection-manager overlay
    /// pre-loaded with `count` SYNTHETIC OdyTTY-owned hosts, bypassing the real
    /// `~/.ssh` / `hosts.conf` load path entirely. Synthetic data only — no real
    /// host or key material is ever read (privacy rule). Lets a small-window
    /// scroll test overflow the list deterministically without files.
    #[cfg(test)]
    pub(in crate::native) fn open_connections_with_synthetic_hosts_for_test(
        &mut self,
        count: usize,
    ) {
        use crate::connection_hosts::{ConnectionHost, ConnectionHostSource};
        let entries = (0..count)
            .map(|i| ConnectionHost {
                alias: format!("synthetic-host-{i:02}"),
                host_name: Some(format!("10.0.0.{i}")),
                user: Some("tester".to_owned()),
                port: None,
                theme: None,
                font: None,
                title: None,
                source: ConnectionHostSource::Odytty,
            })
            .collect();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_connections(entries);
        self.request_selection_redraw();
    }

    /// Test seam (OVERLAY-SMALL-WINDOW): open the command palette pre-seeded
    /// with `count` SYNTHETIC history entries, bypassing the real shell-history
    /// read so a small-window scroll test overflows deterministically. Synthetic
    /// data only — no real history is read.
    #[cfg(test)]
    pub(in crate::native) fn open_palette_with_synthetic_history_for_test(&mut self, count: usize) {
        let history: Vec<String> = (0..count)
            .map(|i| format!("synthetic command {i:02}"))
            .collect();
        self.reset_pointer_state_for_overlay();
        self.overlay
            .open_command_palette_for_test(history.iter().map(String::as_str), None);
        self.request_selection_redraw();
    }

    /// Test seam (OVERLAY-SMALL-WINDOW / ThemeBuilder): open the theme builder
    /// through the production overlay entry (`open_theme_builder_overlay`), so a
    /// small-window scroll test drives its role-list selection-follow and ▲/▼
    /// affordance through the real input/render path, without a window/GPU.
    #[cfg(test)]
    pub(in crate::native) fn open_theme_builder_for_test(&mut self) {
        self.open_theme_builder_overlay();
        self.request_selection_redraw();
    }

    /// Test seam (UX4-P1): inject a cached pointer cell, as `update_pointer_cell`
    /// would after a `CursorMoved`, so a press has coordinates.
    #[cfg(test)]
    pub(in crate::native) fn set_pointer_cell_for_test(&mut self, row: usize, column: usize) {
        self.pointer_cell = Some(CellPoint { row, column });
    }

    /// Test seam: clear the cached pointer cell to exercise button events that
    /// arrive without usable cursor coordinates.
    #[cfg(test)]
    pub(in crate::native) fn clear_pointer_cell_for_test(&mut self) {
        self.pointer_cell = None;
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

    #[cfg(test)]
    pub(in crate::native) fn editable_input_selection_text_for_test(&self) -> Option<String> {
        self.editable_input_selection_for_context_menu()
            .map(|selection| selection.text)
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

    /// Test seam (SH-CLICK): toggle the `sh_click` setting so the click-to-
    /// position default-off path and the enabled path can both be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_sh_click_for_test(&mut self, on: bool) {
        self.settings.sh_click = on;
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

    /// Test seam (OVERLAY-SMALL-WINDOW): composite the open overlay into a blank
    /// `cols`×`rows` snapshot via the exact production `apply_overlay` path and
    /// return the painted text of every row (grapheme per cell, trailing blanks
    /// trimmed). Lets an end-to-end test assert that the *rendered* rows shift
    /// when scroll/focus changes — proving the live repaint, not just geometry.
    #[cfg(test)]
    pub(in crate::native) fn render_overlay_rows_for_test(
        &mut self,
        cols: usize,
        rows: usize,
    ) -> Vec<String> {
        let mut snap = Snapshot {
            dimensions: Dimensions::new(cols, rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); cols * rows],
        };
        crate::native::overlay::apply_overlay(&mut snap, &mut self.overlay);
        (0..rows)
            .map(|r| {
                let line: String = (0..cols)
                    .map(|c| snap.cells[r * cols + c].grapheme())
                    .collect();
                line.trim_end().to_owned()
            })
            .collect()
    }

    /// Test seam (UX4-P2): absolute down/up button cells for the first visible
    /// numeric stepper, so a test can drive real clicks through the App.
    #[cfg(test)]
    pub(in crate::native) fn overlay_first_stepper_button_cells_for_test(
        &self,
    ) -> Option<(CellPoint, CellPoint)> {
        self.overlay
            .first_stepper_button_cells(self.grid.columns, self.grid.rows)
    }

    /// Test seam (UX4-P2): whether an overlay drag is in progress.
    #[cfg(test)]
    pub(in crate::native) fn overlay_is_dragging_for_test(&self) -> bool {
        self.overlay.is_settings_dragging()
    }

    /// Test seam (UX4-P2 review): drive the exact focus-loss drag-cancel the
    /// `WindowEvent::Focused(false)` arm runs, so a regression can prove a lost
    /// release on focus loss cannot leave a drag armed while the overlay
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

    /// Test seam (INTERACTIVE-PATHS): inject the synthetic stat-gate the hover
    /// path resolves against, so headless tests classify path spans from a fixed
    /// `(absolute_path, kind)` map instead of reaching the real filesystem.
    #[cfg(test)]
    pub(in crate::native) fn set_test_path_probe_for_test(
        &mut self,
        probe: super::interactive_paths::MapProbe,
    ) {
        self.test_path_probe = probe;
    }

    /// Test seam (CURSOR-ICON / divider hover): inject the surface size and
    /// window padding so `multipane_geometry()` — and the divider resize-cursor
    /// path it feeds — resolves headlessly without a GPU. See
    /// [`App::test_surface`].
    #[cfg(test)]
    pub(in crate::native) fn set_test_surface_for_test(
        &mut self,
        width_px: u32,
        height_px: u32,
        padding: WindowPadding,
    ) {
        self.test_surface = Some(((width_px, height_px), padding));
    }

    /// Test seam (CURSOR-ICON): drive the production `CursorMoved` handler
    /// headlessly so the cursor-shape selection (I-beam / hand / arrow) can be
    /// asserted without a window or GPU.
    #[cfg(test)]
    pub(in crate::native) fn pointer_move_for_test(&mut self, x_px: f64, y_px: f64) {
        self.update_pointer_cell(x_px, y_px);
    }

    /// Test seam (CURSOR-ICON): the mouse-cursor shape last selected by the
    /// pointer path. `apply_cursor_icon` updates this even with no window, so it
    /// reflects the decision the production code would push to `set_cursor`.
    #[cfg(test)]
    pub(in crate::native) fn cursor_icon_for_test(&self) -> winit::window::CursorIcon {
        self.cursor_icon
    }

    /// Test seam (INTERACTIVE-PATHS): the resolved path span currently under the
    /// pointer, so a test can assert the gate keeps it `None` when the feature is
    /// off and that an unresolved span never latches.
    #[cfg(test)]
    pub(in crate::native) fn hovered_path_for_test(&self) -> Option<&crate::paths::Resolved> {
        self.hovered_path.as_ref()
    }

    /// Test seam (MOUSE-SCROLLBAR): toggle the `scrollbar_drag` setting so the
    /// inverted-gate (off-switch) parity can be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_scrollbar_drag_for_test(&mut self, on: bool) {
        self.settings.scrollbar_drag = on;
    }

    /// Test seam (INTERACTIVE-PATHS): toggle the `interactive_paths` setting so
    /// the gated hover-scan path (and its byte-identical off path) can be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_interactive_paths_for_test(&mut self, on: bool) {
        self.settings.interactive_paths = on;
    }

    /// Test seam (INTERACTIVE-PATHS / C3): set the `interactive_paths_editor`
    /// override so the editor-matrix dispatch can be pinned without an env var.
    #[cfg(test)]
    pub(in crate::native) fn set_interactive_paths_editor_for_test(&mut self, spec: &str) {
        self.settings.interactive_paths_editor = spec.to_owned();
    }

    /// Test seam (INTERACTIVE-PATHS / C3): drive the Ctrl+click open gate
    /// directly and report whether it fired. Returns `false` (no spawn) when the
    /// feature is off, the Ctrl gate is unmet, or no path is hovered — the cases
    /// the gate tests assert. The success branch spawns, so tests only exercise
    /// the false branches.
    #[cfg(test)]
    pub(in crate::native) fn try_open_hovered_path_for_test(&mut self) -> bool {
        self.try_open_hovered_path()
    }

    /// Test seam (INTERACTIVE-PATHS / C3): the argv vector the Ctrl+click /
    /// menu-Open path would spawn for the currently hovered path, or `None` when
    /// nothing is hovered. Pure (reads `$EDITOR`/`$VISUAL`, never spawns), so a
    /// test can assert the dispatch vector without launching a process.
    #[cfg(test)]
    pub(in crate::native) fn path_open_argv_for_test(&self) -> Option<Vec<String>> {
        self.hovered_path
            .clone()
            .map(|resolved| self.path_open_argv_for(&resolved))
    }

    /// Test seam (MOUSE-SCROLLBAR): set the cached raw pointer pixel position the
    /// button handlers hit-test against (button events carry no coordinates).
    #[cfg(test)]
    pub(in crate::native) fn set_pointer_px_for_test(&mut self, x: f64, y: f64) {
        self.pointer_px = Some((x, y));
    }

    /// Test seam (OPEN-NOTICE / P0-2): drive the production open-or-notice path
    /// for an explicit argv, so a test can assert that a FAILED spawn raises a
    /// visible notice and a SUCCESSFUL spawn does not — without going through the
    /// pointer/menu plumbing.
    #[cfg(test)]
    pub(in crate::native) fn spawn_open_or_notice_for_test(&mut self, argv: &[String]) {
        self.spawn_open_or_notice(argv);
    }

    /// Test seam (OPEN-NOTICE / P0-2): the current transient notice message, or
    /// `None` when no notice is in flight (the success / default path).
    #[cfg(test)]
    pub(in crate::native) fn open_notice_message_for_test(&self) -> Option<String> {
        self.open_notice
            .as_ref()
            .map(|notice| notice.message_for_test().to_owned())
    }

    /// Test seam (multi-pane overlay geometry): the window-overlay grid dims and
    /// window-space pointer cell the overlay handlers use. In a single-pane tab
    /// these MUST equal `(grid dims, pointer_cell)` so the single-pane overlay
    /// path is byte-identical; the multi-pane mapping math is unit-tested in
    /// `panes::tests`.
    #[cfg(test)]
    pub(in crate::native) fn overlay_geometry_for_test(
        &self,
    ) -> ((usize, usize), Option<CellPoint>) {
        (self.overlay_grid_dims(), self.overlay_pointer_cell())
    }

    /// Test seam: the active window grid dimensions (`columns`, `rows`).
    #[cfg(test)]
    pub(in crate::native) fn grid_dims_for_test(&self) -> (usize, usize) {
        (self.grid.columns, self.grid.rows)
    }

    #[cfg(test)]
    pub(in crate::native) fn tab_bar_visible_for_test(&self) -> bool {
        self.should_show_tab_bar()
    }

    #[cfg(test)]
    pub(in crate::native) fn tab_bar_hit_for_test(&self) -> Option<&'static str> {
        match self.current_tab_bar_hit()? {
            TabHit::Switch(_) => Some("switch"),
            TabHit::Close(_) => Some("close"),
            TabHit::NewTab => Some("new"),
            TabHit::None => None,
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn tab_bar_row_backgrounds_for_test(&self) -> Option<Vec<Color>> {
        let cell = self.resolved_cell()?;
        let snapshot = self
            .terminal
            .lock()
            .ok()?
            .snapshot_with_scrollback(self.viewport.offset());
        let (decorated, _) =
            self.decorate_snapshot_with_tab_bar(&snapshot, snapshot.cursor_visible, cell);
        Some(
            decorated.cells[..decorated.dimensions.columns]
                .iter()
                .map(|cell| cell.attrs.background)
                .collect(),
        )
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

    /// Test seam (1c-3c): set a wrapped (non-block) absolute selection range on
    /// the focused session, so the multi-pane focused-pane overlay paint has a
    /// real selection to map. Coordinates are absolute cell points.
    #[cfg(test)]
    pub(in crate::native) fn set_selection_range_for_test(
        &mut self,
        start_row: usize,
        start_column: usize,
        end_row: usize,
        end_column: usize,
    ) {
        use crate::selection::{AbsoluteCellPoint, AbsoluteSelectionRange};
        self.selection_block = false;
        self.selection.set_range(AbsoluteSelectionRange {
            start: AbsoluteCellPoint {
                row: start_row,
                column: start_column,
            },
            end: AbsoluteCellPoint {
                row: end_row,
                column: end_column,
            },
        });
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

    /// Test seam (IN2): drive a mouse button edge through the exact production
    /// window-level routing (`handle_mouse_input`), so the right-click context
    /// menu's precedence against the TUI report gate, the Shift override, and an
    /// already-open overlay are pinned through the real path — not reimplemented.
    #[cfg(test)]
    pub(in crate::native) fn dispatch_mouse_button_for_test(
        &mut self,
        pressed: bool,
        button: WinitMouseButton,
    ) {
        let state = if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        };
        self.handle_mouse_input(state, button);
    }

    /// Test seam (IN2): whether the context menu is the active overlay mode.
    #[cfg(test)]
    pub(in crate::native) fn context_menu_open_for_test(&self) -> bool {
        self.overlay.is_context_menu()
    }

    /// Test seam (IN2): force a non-empty absolute selection so the menu's Copy
    /// gating (selection present ⇒ enabled) can be exercised deterministically.
    #[cfg(test)]
    pub(in crate::native) fn force_selection_for_test(
        &mut self,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) {
        self.selection.set_range(AbsoluteSelectionRange {
            start: selection::AbsoluteCellPoint {
                row: start_row,
                column: start_col,
            },
            end: selection::AbsoluteCellPoint {
                row: end_row,
                column: end_col,
            },
        });
    }

    /// Test seam (IN2): the live absolute selection range as
    /// `(start_row, start_col, end_row, end_col)`, so Select All can be proven to
    /// span the whole buffer.
    #[cfg(test)]
    pub(in crate::native) fn selection_range_for_test(
        &self,
    ) -> Option<(usize, usize, usize, usize)> {
        self.selection.range().map(|range| {
            (
                range.start.row,
                range.start.column,
                range.end.row,
                range.end.column,
            )
        })
    }

    /// Test seam (KB-REMAP): open the key-binding remap modal through the
    /// production entry path (so the pointer-state reset and overlay open are
    /// genuinely exercised), without a window/GPU.
    #[cfg(test)]
    pub(in crate::native) fn open_key_bindings_overlay_for_test(&mut self) {
        self.open_key_bindings_overlay();
    }

    /// Test seam (KB-REMAP R2): drive a winit logical key through the EXACT
    /// production overlay-key path (`handle_overlay_key`), with the given
    /// modifiers, so a test proves the chord-capture bypass fires before the
    /// lossy `overlay_input_from_winit` mapper. Sets `self.modifiers` to match
    /// the chord, exactly as the real `KeyboardInput` arm does.
    #[cfg(test)]
    pub(in crate::native) fn drive_overlay_key_for_test(
        &mut self,
        logical: winit::keyboard::Key,
        ctrl: bool,
        shift: bool,
    ) {
        self.modifiers.ctrl = ctrl;
        self.modifiers.shift = shift;
        self.handle_overlay_key(&logical, KeyEventType::Press);
    }

    /// Test seam (SETTINGS-SLIDER-LAG): drive a key-repeat event through the
    /// overlay path so high-frequency numeric edits can be tested separately
    /// from one-shot presses.
    #[cfg(test)]
    pub(in crate::native) fn drive_overlay_repeat_key_for_test(
        &mut self,
        logical: winit::keyboard::Key,
        ctrl: bool,
        shift: bool,
    ) {
        self.modifiers.ctrl = ctrl;
        self.modifiers.shift = shift;
        self.handle_overlay_key(&logical, KeyEventType::Repeat);
    }

    /// Test seam (SETTINGS-SLIDER-LAG): whether an overlay live-apply is queued
    /// for coalescing.
    #[cfg(test)]
    pub(in crate::native) fn pending_overlay_settings_for_test(&self) -> bool {
        self.pending_overlay_settings.is_some()
    }

    /// Test seam (SETTINGS-SLIDER-LAG): flush the coalesced overlay apply as the
    /// redraw/about-to-wait/release paths do in production.
    #[cfg(test)]
    pub(in crate::native) fn flush_pending_overlay_settings_for_test(&mut self) {
        self.flush_pending_overlay_settings();
    }

    /// Test seam (SETTINGS-SLIDER-LAG): run the non-render idle maintenance the
    /// winit loop performs in `about_to_wait`. This must not flush a coalesced
    /// slider/key-repeat apply; redraw/release/close are the flush points.
    #[cfg(test)]
    pub(in crate::native) fn run_about_to_wait_maintenance_for_test(&mut self, now: Instant) {
        self.run_about_to_wait_maintenance(now);
    }

    /// Test seam (KB-REMAP R2): whether the remap modal is armed to capture a
    /// raw chord — the predicate the production key path gates its bypass on.
    #[cfg(test)]
    pub(in crate::native) fn overlay_capturing_chord_for_test(&self) -> bool {
        self.overlay.is_capturing_chord()
    }

    /// Test seam (KB-REMAP R2): the live action a chord resolves to after the
    /// remap apply, read from the production `KeyBindings` table the renderer and
    /// key dispatch both use.
    #[cfg(test)]
    pub(in crate::native) fn live_action_for_chord_for_test(
        &self,
        logical: &winit::keyboard::Key,
        ctrl: bool,
        shift: bool,
    ) -> Option<crate::settings::BindableAction> {
        let mods = crate::input::Modifiers {
            ctrl,
            shift,
            ..crate::input::Modifiers::default()
        };
        self.key_bindings.action_for(logical, mods, false)
    }

    /// Test seam (OS-THEME): the live active (authored-for-renderer) theme. This
    /// is the theme OS following overrides; the CVD-adapted publish derives from
    /// it.
    #[cfg(test)]
    pub(in crate::native) fn active_theme_for_test(&self) -> Theme {
        self.theme
    }

    /// Test seam (OS-THEME): configure the follow knob, the dark/light theme
    /// names, and the current OS appearance signal, then re-resolve and publish
    /// through the production override path. Returns the resolved active theme so
    /// a test can assert the switch (or the off-path identity) directly.
    #[cfg(test)]
    pub(in crate::native) fn apply_os_theme_for_test(
        &mut self,
        follow: bool,
        dark: Option<&str>,
        light: Option<&str>,
        os: Option<winit::window::Theme>,
    ) -> Theme {
        self.settings.follow_os_theme = follow;
        self.settings.os_theme_dark = dark.map(str::to_owned);
        self.settings.os_theme_light = light.map(str::to_owned);
        self.os_theme = os;
        self.apply_os_theme_override();
        self.resolve_active_theme()
    }

    /// Test seam (OS-THEME): resolve the active theme WITHOUT publishing — proves
    /// the pure off-path identity (`follow_os_theme = false` ⇒ authored theme).
    #[cfg(test)]
    pub(in crate::native) fn resolve_active_theme_for_test(&self) -> Theme {
        self.resolve_active_theme()
    }

    /// Test seam (CLOSE-CONFIRM): open the confirmation dialog through the same
    /// path the `CloseRequested` handler uses.
    #[cfg(test)]
    pub(in crate::native) fn open_confirm_close_for_test(&mut self) {
        self.overlay.open_confirm_close();
    }

    /// Test seam (CLOSE-CONFIRM): whether the confirmation dialog is the active
    /// overlay mode.
    #[cfg(test)]
    pub(in crate::native) fn confirm_close_open_for_test(&self) -> bool {
        self.overlay.is_confirm_close()
    }

    /// Test seam (CLOSE-CONFIRM): the pending-exit flag the `window_event` loop
    /// consults to perform the actual close after a confirmed dialog.
    #[cfg(test)]
    pub(in crate::native) fn pending_exit_for_test(&self) -> bool {
        self.pending_exit
    }

    #[cfg(test)]
    pub(in crate::native) fn active_session_id_for_test(&self) -> usize {
        self.sessions.active_position()
    }

    #[cfg(test)]
    pub(in crate::native) fn active_session_token_for_test(
        &self,
    ) -> crate::native::session::SessionToken {
        self.sessions.active_id()
    }

    #[cfg(test)]
    pub(in crate::native) fn session_token_at_position_for_test(
        &self,
        session: usize,
    ) -> Option<crate::native::session::SessionToken> {
        self.sessions.token_at_position(session)
    }

    #[cfg(test)]
    pub(in crate::native) fn session_count_for_test(&self) -> usize {
        self.sessions.iter().count()
    }

    #[cfg(test)]
    pub(in crate::native) fn push_session_for_test(
        &mut self,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
    ) -> usize {
        let id = self.sessions.push(Session::new(
            crate::native::session::SessionToken(
                self.sessions
                    .iter()
                    .map(|session| session.id.0)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1),
            ),
            terminal,
            writer,
            pty,
            None,
        ));
        self.sessions.position_of_token(id).unwrap_or(0)
    }

    #[cfg(test)]
    pub(in crate::native) fn switch_to_session_for_test(&mut self, session: usize) -> bool {
        let Some(token) = self.sessions.token_at_position(session) else {
            return false;
        };
        if !self.sessions.switch(token) {
            return false;
        }
        self.on_active_session_changed();
        true
    }

    #[cfg(test)]
    pub(in crate::native) fn close_active_tab_for_test(&mut self) -> bool {
        self.close_active_tab()
    }

    #[cfg(test)]
    pub(in crate::native) fn handle_palette_type_text_for_test(&mut self, text: String) {
        self.handle_palette_type_text(text);
    }

    #[cfg(test)]
    pub(in crate::native) fn close_all_sessions_for_test(&mut self) {
        self.close_all_sessions();
    }

    #[cfg(test)]
    pub(in crate::native) fn dispatch_user_event_for_test(&mut self, event: UserEvent) -> bool {
        self.apply_user_event(event)
    }

    #[cfg(test)]
    pub(in crate::native) fn session_needs_rebuild_for_test(&self, session: usize) -> Option<bool> {
        self.sessions
            .iter()
            .nth(session)
            .map(|session| session.needs_rebuild)
    }

    #[cfg(test)]
    pub(in crate::native) fn set_session_needs_rebuild_for_test(
        &mut self,
        session: usize,
        needs_rebuild: bool,
    ) {
        let Some(token) = self.sessions.token_at_position(session) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(token) {
            session.needs_rebuild = needs_rebuild;
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn drive_text_key_for_test(&mut self, text: &str) {
        let logical = WinitKey::Character(text.to_owned().into());
        self.handle_key_event(
            logical.clone(),
            logical,
            PhysicalKey::Code(KeyCode::KeyA),
            KeyEventType::Press,
        );
    }

    #[cfg(test)]
    pub(in crate::native) fn drive_named_key_for_test(&mut self, key: NamedKey) {
        let logical = WinitKey::Named(key);
        self.handle_key_event(
            logical.clone(),
            logical,
            PhysicalKey::Code(KeyCode::Enter),
            KeyEventType::Press,
        );
    }

    /// Test seam (§7 K2): drive a character key with explicit ctrl/shift
    /// modifiers through the production `handle_key_event` path (so the prefix
    /// engine sees the real chord). Restores the prior modifier state after.
    #[cfg(test)]
    pub(in crate::native) fn drive_char_with_mods_for_test(
        &mut self,
        ch: char,
        ctrl: bool,
        shift: bool,
    ) {
        let prev = self.modifiers;
        self.modifiers = crate::input::Modifiers {
            ctrl,
            shift,
            ..crate::input::Modifiers::default()
        };
        let logical = WinitKey::Character(ch.to_string().into());
        self.handle_key_event(
            logical.clone(),
            logical,
            PhysicalKey::Code(KeyCode::KeyB),
            KeyEventType::Press,
        );
        self.modifiers = prev;
    }

    /// Test seam (§7 K2): the number of panes in the active tab (1 ⇒ the
    /// single-pane byte-identical path).
    #[cfg(test)]
    pub(in crate::native) fn active_pane_count_for_test(&self) -> usize {
        self.sessions.active_pane_count()
    }

    /// Test seam (§7 K2): the focused pane's session token id, to assert focus
    /// moves across panes.
    #[cfg(test)]
    pub(in crate::native) fn focused_pane_id_for_test(&self) -> usize {
        self.sessions.active_id().0 as usize
    }

    /// Test seam (§7 K2-zoom): whether the active tab is rendering one pane
    /// full-bleed (zoom mode), so a chord-driven toggle can be asserted through
    /// the production `handle_key_event` path.
    #[cfg(test)]
    pub(in crate::native) fn active_is_zoomed_for_test(&self) -> bool {
        self.sessions.active_is_zoomed()
    }

    /// Test seam (§7 K2): seed the active tab into a two-pane split headlessly
    /// (the production split spawns a PTY, which needs an event-loop proxy the
    /// test App lacks). Splits the focused leaf along `columns ? Columns : Rows`
    /// using the supplied recorded session as the new pane. Returns the new
    /// pane's focused-pane id.
    #[cfg(test)]
    pub(in crate::native) fn seed_split_pane_for_test(
        &mut self,
        columns: bool,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
    ) -> usize {
        let axis = if columns {
            crate::native::layout::SplitAxis::Columns
        } else {
            crate::native::layout::SplitAxis::Rows
        };
        let next_id = self
            .sessions
            .iter()
            .map(|session| session.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let session = crate::native::session::Session::new(
            crate::native::session::SessionToken(next_id),
            terminal,
            writer,
            pty,
            None,
        );
        let token = self.sessions.split_active_for_test(axis, session);
        token.0 as usize
    }

    /// Test seam (pane-close reflow): drive the production structural-reflow
    /// path (`split` / `equalize` / `close` all funnel through it) headlessly so
    /// a test can establish the narrow split sub-grids before closing a pane.
    #[cfg(test)]
    pub(in crate::native) fn reflow_active_panes_for_test(&mut self) {
        self.reflow_active_panes_and_redraw();
    }

    /// Test seam (pane-close reflow): close the focused pane via the production
    /// `close_focused_pane` path so a regression test can prove the survivor
    /// reflows back to the full content width after a split collapses.
    #[cfg(test)]
    pub(in crate::native) fn close_focused_pane_for_test(&mut self) {
        self.close_focused_pane();
    }

    /// Test seam (pane-close reflow): the active (focused) session's terminal
    /// grid dimensions `(columns, rows)`. After a split collapses this is the
    /// surviving pane, so a test can assert it returned to the full content
    /// width rather than keeping its narrow split sub-grid.
    #[cfg(test)]
    pub(in crate::native) fn active_session_grid_dims_for_test(&self) -> (usize, usize) {
        let terminal = self.sessions.active().terminal.lock().expect("terminal");
        let dims = terminal.screen().dimensions();
        (dims.columns, dims.rows)
    }

    #[cfg(test)]
    pub(in crate::native) fn set_session_tab_title_for_test(
        &mut self,
        session: usize,
        title: &str,
    ) {
        let Some(token) = self.sessions.token_at_position(session) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(token) {
            session.tab_title = title.to_owned();
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn set_session_title_override_for_test(
        &mut self,
        session: usize,
        title: Option<&str>,
    ) {
        let Some(token) = self.sessions.token_at_position(session) else {
            return;
        };
        self.sessions
            .set_title_override(token, title.map(ToOwned::to_owned));
    }

    #[cfg(test)]
    pub(in crate::native) fn session_tab_title_for_test(&self, session: usize) -> Option<String> {
        self.sessions
            .token_at_position(session)
            .map(|token| self.sessions.effective_tab_title(token))
    }

    #[cfg(test)]
    pub(in crate::native) fn begin_rename_tab_for_test(&mut self, session: usize) -> bool {
        let Some(token) = self.sessions.token_at_position(session) else {
            return false;
        };
        self.enter_rename_tab(token);
        self.rename_state.is_some()
    }

    #[cfg(test)]
    pub(in crate::native) fn rename_active_for_test(&self) -> bool {
        self.rename_state.is_some()
    }

    #[cfg(test)]
    pub(in crate::native) fn rename_text_for_test(&self) -> Option<String> {
        self.rename_state.as_ref().map(|state| state.text.clone())
    }

    #[cfg(test)]
    pub(in crate::native) fn advance_session_bytes_for_test(
        &mut self,
        session: usize,
        bytes: &[u8],
    ) {
        let Some(token) = self.sessions.token_at_position(session) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(token)
            && let Ok(mut terminal) = session.terminal.lock()
        {
            terminal.advance(bytes);
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn session_plain_text_for_test(&self, session: usize) -> Option<String> {
        self.sessions.iter().nth(session).and_then(|session| {
            session
                .terminal
                .lock()
                .ok()
                .map(|terminal| terminal.screen().plain_text())
        })
    }

    #[cfg(test)]
    pub(in crate::native) fn session_dimensions_for_test(
        &self,
        session: usize,
    ) -> Option<Dimensions> {
        self.sessions.iter().nth(session).and_then(|session| {
            session
                .terminal
                .lock()
                .ok()
                .map(|terminal| terminal.screen().dimensions())
        })
    }

    #[cfg(test)]
    pub(in crate::native) fn session_pty_dimensions_for_test(
        &self,
        session: usize,
    ) -> Option<Dimensions> {
        self.sessions.iter().nth(session).and_then(|session| {
            session
                .local_pty()
                .and_then(|pty| pty.lock().ok())
                .and_then(|pty| pty.dimensions_for_test().ok())
        })
    }

    #[cfg(test)]
    pub(in crate::native) fn active_window_title_for_test(&self) -> String {
        self.active_window_title()
    }

    #[cfg(test)]
    pub(in crate::native) fn open_search_for_test(&mut self) {
        self.toggle_search();
    }

    #[cfg(test)]
    pub(in crate::native) fn search_open_for_test(&self) -> bool {
        self.search.is_open()
    }

    /// Test seam (1c-3c): open search on the focused session, type `query`, and
    /// refresh matches against the focused terminal (the production
    /// `refresh_search_matches` path). Lets a test exercise focused-pane search
    /// highlighting in the multi-pane render path.
    #[cfg(test)]
    pub(in crate::native) fn drive_search_for_test(&mut self, query: &str) {
        self.search.open();
        for ch in query.chars() {
            self.search.push_char(ch);
        }
        self.refresh_search_matches();
    }

    /// Test seam (1c-3c): the focused session's current search-match count.
    #[cfg(test)]
    pub(in crate::native) fn search_match_count_for_test(&self) -> usize {
        self.search.match_count()
    }

    /// Test seam (FONT-SAVE-CORRECTNESS BUG 2): drive the post-write live-apply
    /// step that `save_overlay_settings` now performs — re-applying the
    /// just-written config through the shared `OverlayEdit` reload seam so a
    /// picked font / theme / panel edit takes effect immediately, not at the
    /// next restart. Wraps the exact production `apply_overlay_settings` the save
    /// path calls (not a parallel reimplementation), decoupled from the real
    /// `config_file_path()` so the test is hermetic.
    #[cfg(test)]
    pub(in crate::native) fn apply_saved_settings_live_for_test(&mut self, reloaded: Settings) {
        self.apply_overlay_settings(reloaded);
    }

    /// Test seam (D-IN2-CUT-SAFE): force clipboard `write_text` to return `None`
    /// so the Cut fail-safe path can be exercised without needing the system
    /// clipboard to actually fail. The flag is consumed by `NativeClipboard`'s
    /// `write_text` in `#[cfg(test)]` mode only.
    #[cfg(test)]
    pub(in crate::native) fn force_clipboard_write_fail_for_test(&mut self) {
        self.clipboard.force_write_fail = true;
    }

    /// Test seam (D-SLIDER-GUARD): the current `overlay_left_held` flag value.
    /// Lets regression tests prove that release clears the flag and subsequent
    /// moves cannot advance stale overlay adjustment state.
    #[cfg(test)]
    pub(in crate::native) fn overlay_left_held_for_test(&self) -> bool {
        self.overlay_left_held
    }
}
