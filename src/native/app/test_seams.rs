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
    /// Headless session-attach driver that uses the real WorkspaceSet connect
    /// seam, then runs the production presentation and dimension reconciliation.
    #[cfg(all(test, unix))]
    pub(in crate::native) fn attach_session_with_sink_for_test(
        &mut self,
        runtime_base: Option<&Path>,
        session_id: &str,
        sink: impl crate::native::attach::AttachEventSink,
    ) -> std::io::Result<()> {
        let token = self
            .sessions
            .attach_in_new_tab_for_test(runtime_base, session_id, sink)?;
        self.present_attached_session(token);
        Ok(())
    }

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

    /// Padding-aware sibling of [`Self::resize_grid`] (CHROME-GAP): drives the
    /// real grid-fit path with a nonzero window padding so the chrome-facing
    /// gap's whole-cell displacement is assertable headlessly.
    #[cfg(test)]
    pub(in crate::native) fn resize_grid_with_padding_for_test(
        &mut self,
        cell: CellSize,
        padding: WindowPadding,
        width_px: u32,
        height_px: u32,
    ) -> bool {
        self.resize_grid_with_padding(cell, padding, width_px, height_px)
    }

    /// Drive the same debounced model-resize path used by window resize events.
    #[cfg(test)]
    pub(in crate::native) fn record_pending_resize_for_test(
        &mut self,
        cell: CellSize,
        width_px: u32,
        height_px: u32,
        now: Instant,
    ) {
        self.record_pending_resize(
            PendingResize {
                cell,
                padding: WindowPadding::ZERO,
                width_px,
                height_px,
            },
            now,
        );
    }

    /// Test seam (UX4-P1): open the settings overlay through the production
    /// keyboard entry path (so the pointer-state reset is genuinely exercised),
    /// without a window/GPU.
    #[cfg(test)]
    pub(in crate::native) fn open_settings_overlay_for_test(&mut self) {
        self.toggle_settings_overlay();
    }

    /// Test seam (RAIL-AUTOHIDE-CTL panel coherence): the value string the open
    /// settings panel would render for `key`. Used to prove an external chevron
    /// toggle keeps the panel's Layout row in sync with the live setting.
    #[cfg(test)]
    pub(in crate::native) fn settings_panel_displayed_value_for_test(
        &self,
        key: &str,
    ) -> Option<String> {
        self.overlay.settings_panel_value_for_test(key)
    }

    /// Test seam (RAIL-AUTOHIDE-CTL panel coherence): drive the exact production
    /// external-chrome toggle the pointer arm invokes, without routing a pointer
    /// press through a possibly-open overlay. The click->toggle wiring itself is
    /// covered separately; this isolates the settings-panel reconciliation.
    #[cfg(test)]
    pub(in crate::native) fn toggle_tab_rail_autohide_for_test(&mut self) {
        self.toggle_tab_rail_autohide();
    }

    /// Test seam: open the production Layout settings target so keyboard edits
    /// can exercise the same deep-link and input route as chrome Settings.
    #[cfg(test)]
    pub(in crate::native) fn open_layout_settings_overlay_for_test(&mut self) {
        self.open_settings_overlay_target(crate::native::overlay::SettingsTarget::TabsAndPanes);
    }

    #[cfg(test)]
    pub(in crate::native) fn settings_active_section_for_test(&self) -> Option<&'static str> {
        self.overlay.settings_active_section_for_test()
    }

    /// Test seam (UX4-P1): close the overlay (Esc-equivalent), without a
    /// window/GPU.
    #[cfg(test)]
    pub(in crate::native) fn close_overlay_for_test(&mut self) {
        self.overlay.close();
    }

    /// Test seam (MENU-THEME PARITY, board 46195bed): feed bytes to the primary
    /// terminal handle the window overlay resolves its palette against, so a test
    /// can install a distinctive theme (e.g. via OSC 10/11) before asserting the
    /// overlay picked it up.
    #[cfg(test)]
    pub(in crate::native) fn advance_primary_terminal_for_test(&mut self, bytes: &[u8]) {
        crate::native::lock_recover(&self.terminal).advance(bytes);
    }

    /// Test seam (v0.14 A3): drive the live profile auto-switch poll headlessly.
    #[cfg(test)]
    pub(in crate::native) fn poll_profile_auto_switch_for_test(&mut self) {
        self.poll_profile_auto_switch();
    }

    #[cfg(test)]
    pub(in crate::native) fn active_launch_profile_for_test(&self) -> Option<String> {
        let active = self.sessions.active_id();
        self.sessions
            .get(active)
            .and_then(|session| session.launch_profile.clone())
    }

    #[cfg(test)]
    pub(in crate::native) fn set_active_remote_destination_for_test(
        &mut self,
        destination: Option<String>,
    ) {
        let active = self.sessions.active_id();
        if let Some(session) = self.sessions.get_mut(active) {
            session.remote_destination = destination;
        }
    }

    /// Test seam (MENU-THEME PARITY, board 46195bed): the `DynamicColors` the
    /// multi-pane window overlay snapshot is seeded with, alongside the live
    /// terminal palette the single-pane path resolves the same panel against.
    /// The two must match so the overlay panel is the same themed color in both
    /// pane layouts. `None` when no overlay is open or the window has no
    /// multi-pane geometry.
    #[cfg(test)]
    pub(in crate::native) fn overlay_top_colors_for_test(
        &mut self,
    ) -> Option<(crate::core::DynamicColors, crate::core::DynamicColors)> {
        let (content, cell) = self.multipane_geometry()?;
        let (snapshot, _origin) = self.build_overlay_top(content, cell)?;
        let terminal_colors = crate::native::lock_recover(&self.terminal)
            .dynamic_colors()
            .clone();
        Some((snapshot.colors, terminal_colors))
    }

    /// Test seam (multi-pane modal parity): render the topmost window-level
    /// modal through the exact production `build_overlay_top` path and return
    /// its cropped text rows. Unlike a state-only predicate, this proves the
    /// rename/name prompt has visible cells in a split frame.
    #[cfg(test)]
    pub(in crate::native) fn multipane_modal_top_rows_for_test(&mut self) -> Option<Vec<String>> {
        let (content, cell) = self.multipane_geometry()?;
        let (snapshot, _origin) = self.build_overlay_top(content, cell)?;
        Some(
            (0..snapshot.dimensions.rows)
                .map(|row| {
                    let text: String = (0..snapshot.dimensions.columns)
                        .map(|column| {
                            snapshot.cells[row * snapshot.dimensions.columns + column].grapheme()
                        })
                        .collect();
                    text.trim_end().to_owned()
                })
                .collect(),
        )
    }

    /// Test seam (PANE-PADDING, board 4c8856ae): the focused pane's TILED rect
    /// and its PADDED inner (drawable grid) rect in the active multi-pane tab,
    /// each as `[x, y, w, h]`. Lets a regression assert that every divider-facing
    /// edge is inset by exactly the window padding while outer-margin edges keep
    /// the tiled geometry. `None` on a single-pane tab / without resolved
    /// geometry.
    #[cfg(test)]
    pub(in crate::native) fn focused_pane_rects_for_test(&self) -> Option<([f32; 4], [f32; 4])> {
        let (content, _cell) = self.multipane_geometry()?;
        let focused = self.sessions.active_id();
        let tiled = self
            .sessions
            .active_pane_rects(content, super::panes::PANE_DIVIDER_PX)
            .into_iter()
            .find(|(t, _)| *t == focused)
            .map(|(_, r)| r)?;
        let (inner, _cell) = self.focused_pane_inner_rect()?;
        Some((
            [tiled.x, tiled.y, tiled.w, tiled.h],
            [inner.x, inner.y, inner.w, inner.h],
        ))
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
                integration: None,
                reuse: None,
                tmux: None,
                protocol: None,
                identity_file: None,
                persist: None,
                source: ConnectionHostSource::Odytty,
            })
            .collect();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_connections(entries, Vec::new());
        self.request_selection_redraw();
    }

    /// Test seam (C5): open the session-attach summon overlay pre-loaded with
    /// SYNTHETIC listed sessions (each `id` from `ids`), bypassing the real
    /// session-host registry scan. Synthetic data only. Lets a headless test
    /// drive the attach-overlay dedup/close path.
    #[cfg(test)]
    pub(in crate::native) fn open_session_attach_with_synthetic_sessions_for_test(
        &mut self,
        ids: &[&str],
    ) {
        use crate::session_host::ListedSession;
        let entries = ids
            .iter()
            .map(|id| ListedSession {
                id: (*id).to_owned(),
                name: format!("session-{id}"),
                state: "running",
                age_ms: 1000,
                pane_count: 1,
            })
            .collect();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_session_attach(entries);
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

    /// Test seam (THEME-CAPTURE): open the theme editor on a draft captured
    /// from the focused pane's live colors, through the production palette
    /// entry point.
    #[cfg(test)]
    pub(in crate::native) fn open_theme_capture_for_test(&mut self) {
        self.open_theme_capture_overlay();
        self.request_selection_redraw();
    }

    /// Test seam (THEME-CAPTURE): the draft the capture flow would produce
    /// right now, without opening any overlay. Lets a test assert the captured
    /// spec against the pane's live dynamic-color state directly.
    #[cfg(test)]
    pub(in crate::native) fn captured_theme_spec_for_test(&self) -> crate::theme::ThemeSpec {
        self.capture_live_theme_spec()
    }

    /// Test seam (THEME-CAPTURE): the theme editor's current working draft, or
    /// `None` when the editor is not the active overlay.
    #[cfg(test)]
    pub(in crate::native) fn theme_builder_draft_for_test(
        &self,
    ) -> Option<crate::theme::ThemeSpec> {
        self.overlay.theme_builder_draft_for_test()
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

    /// Test seam (C11): read the active session's cached pointer cell — the
    /// selection anchor `begin_selection` consumes. Lets the focus-follows-click
    /// test assert the anchor lands under the live click, not a stale per-pane
    /// coordinate.
    #[cfg(test)]
    pub(in crate::native) fn pointer_cell_for_test(&self) -> Option<CellPoint> {
        self.pointer_cell
    }

    #[cfg(test)]
    pub(in crate::native) fn pointer_over_drawable_pane_for_test(&self) -> bool {
        self.pointer_over_drawable_pane()
    }

    /// Test seam (UX4-P1): the live overlay rect for the current grid.
    #[cfg(test)]
    pub(in crate::native) fn overlay_rect_for_test(
        &self,
    ) -> Option<crate::native::overlay::OverlayRect> {
        overlay_rect(&self.overlay, self.grid.columns, self.grid.rows)
    }

    /// Test seam (PROMPT-OPACITY): the single-pane opaque cell span held opaque
    /// under a translucent window — covers an open overlay panel or, taking
    /// precedence, the rename/prompt band. `None` when neither is open (the
    /// byte-identical opaque path also passes `None`).
    #[cfg(test)]
    pub(in crate::native) fn single_pane_overlay_opaque_region_for_test(
        &self,
    ) -> Option<crate::grid::CellRegion> {
        self.single_pane_overlay_opaque_region()
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

    /// Test seam (CMD-OPEN): set the macOS `super` (Cmd) key so the
    /// platform-aware open modifier can be driven on the macOS host path.
    #[cfg(test)]
    pub(in crate::native) fn set_super_key_for_test(&mut self, super_key: bool) {
        self.super_key = super_key;
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

    /// Test seam for the reusable centered feedback surface.
    #[cfg(test)]
    pub(in crate::native) fn transient_hud_text_for_test(&self) -> Option<&str> {
        self.transient_hud.text_for_test()
    }

    /// Test seam (CTRL-WHEEL-ZOOM): drive a vertical wheel notch through the
    /// production wheel routing (`handle_mouse_wheel`), so the zoom-vs-scroll-vs
    /// -report precedence is pinned, not reimplemented. Positive = wheel up.
    #[cfg(test)]
    pub(in crate::native) fn dispatch_wheel_for_test(&mut self, vertical_notches: f32) {
        self.handle_mouse_wheel(MouseScrollDelta::LineDelta(0.0, vertical_notches));
    }

    /// Test seam (SCROLL-FEEL): drive a high-resolution `PixelDelta` wheel event
    /// (a touchpad glide) of `pos_y` physical pixels through the production wheel
    /// routing, so the continuous-lane pane resolution (pane under the pointer)
    /// is pinned. Positive = scroll up toward history.
    #[cfg(test)]
    pub(in crate::native) fn dispatch_pixel_wheel_for_test(&mut self, pos_y: f64) {
        self.handle_mouse_wheel(MouseScrollDelta::PixelDelta(
            winit::dpi::PhysicalPosition::new(0.0, pos_y),
        ));
    }

    /// Test seam (SCROLL-FEEL): the sub-cell pixel offset for a specific pane
    /// token, so a split continuous-scroll test can prove the fractional lane
    /// landed on the pane under the pointer (and left the others at rest).
    #[cfg(test)]
    pub(in crate::native) fn scroll_frac_offset_for_token_for_test(
        &self,
        token: usize,
    ) -> Option<f32> {
        self.sessions
            .get(crate::native::session::SessionToken(token as u64))
            .map(|session| session.scroll_frac_offset)
    }

    /// Test seam (SCROLL-FEEL): per-pane continuous-scroll eligibility for a
    /// token, so a split test can prove the lane is no longer single-pane-gated.
    #[cfg(test)]
    pub(in crate::native) fn continuous_scroll_eligible_of_for_test(&self, token: usize) -> bool {
        self.continuous_scroll_eligible_of(crate::native::session::SessionToken(token as u64))
    }

    /// Test seam: whether the active tab is a single pane (the byte-identical
    /// fast path). Lets a split test assert its multipane precondition.
    #[cfg(test)]
    pub(in crate::native) fn active_is_single_pane_for_test(&self) -> bool {
        self.sessions.active_is_single_pane()
    }

    /// Test seam (SCROLL-FEEL): toggle the `pixel_scroll` knob so a continuous-
    /// lane eligibility test can prove the off path falls back to notches.
    #[cfg(test)]
    pub(in crate::native) fn set_pixel_scroll_for_test(&mut self, on: bool) {
        self.settings.pixel_scroll = on;
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

    /// Test seam (F4-P3): inject the display scale factor so the DPI-aware rail
    /// reveal zone (logical→physical px) can be asserted headlessly. `None`
    /// leaves the default 1.0.
    #[cfg(test)]
    pub(in crate::native) fn set_test_scale_for_test(&mut self, scale: f32) {
        self.test_scale = Some(scale);
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

    /// Whether the top tab widget currently carries a hover hit selected by the
    /// production pointer route.
    #[cfg(test)]
    pub(in crate::native) fn top_tab_hovered_for_test(&self) -> bool {
        self.tab_bar.hover.is_some()
    }

    /// Test seam (INTERACTIVE-PATHS): the resolved path span currently under the
    /// pointer, so a test can assert the gate keeps it `None` when the feature is
    /// off and that an unresolved span never latches.
    #[cfg(test)]
    pub(in crate::native) fn hovered_path_for_test(&self) -> Option<&crate::paths::Resolved> {
        self.hovered_path.as_ref()
    }

    /// Test seam (INTERACTIVE-PATHS): the `(row, start, end)` cell extent of the
    /// hovered resolved path, so a test can assert a spaced filename's hand /
    /// underline covers the WHOLE name rather than a single split token.
    #[cfg(test)]
    pub(in crate::native) fn hovered_path_cells_for_test(&self) -> Option<(usize, usize, usize)> {
        self.hovered_path_cells
            .map(|cells| (cells.row, cells.start, cells.end))
    }

    /// Test seam (INTERACTIVE-URLS): the bare-URL string currently under the
    /// pointer, so a test can assert the gate keeps it `None` when the feature is
    /// off, that an openable bare URL latches, and that an OSC 8 cell or a
    /// non-openable scheme never latches.
    #[cfg(test)]
    pub(in crate::native) fn hovered_url_for_test(&self) -> Option<&str> {
        self.hovered_url.as_deref()
    }

    /// Test seam (UX-A / Phase 11): toggle the `interactive_paths_click_hint`
    /// setting so the hint-on / hint-silenced parity can be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_interactive_paths_click_hint_for_test(&mut self, on: bool) {
        self.settings.interactive_paths_click_hint = on;
    }

    /// Test seam (UX-A / Phase 11): whether the transient bottom-left
    /// "Ctrl+click to open" hint is currently shown.
    #[cfg(test)]
    pub(in crate::native) fn click_hint_shown_for_test(&self) -> bool {
        self.click_hint.is_shown()
    }

    #[cfg(test)]
    pub(in crate::native) fn click_hint_text_for_test(&self) -> Option<&'static str> {
        self.click_hint
            .shown_text(super::platform_opener::OpenerOs::Linux)
    }

    /// Test seam (UX-A / Phase 11): the armed-underline span (row, start, end) as
    /// the painter + cache signature see it — `Some` only when `interactive_paths`
    /// is on, Ctrl is held, and a resolved path is hovered; `None` otherwise so
    /// the plain-hover / feature-off byte-identity can be asserted.
    #[cfg(test)]
    pub(in crate::native) fn armed_underline_cells_for_test(
        &self,
    ) -> Option<(usize, usize, usize)> {
        self.armed_path_underline_cells()
            .map(|cells| (cells.row, cells.start, cells.end))
    }

    /// Test seam (MOUSE-SCROLLBAR): toggle the `scrollbar_drag` setting so the
    /// inverted-gate (off-switch) parity can be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_scrollbar_drag_for_test(&mut self, on: bool) {
        self.settings.scrollbar_drag = on;
    }

    /// Test seam (F4-P3 coexistence): force an in-progress scroll-thumb drag so
    /// the reveal-yields-to-scrollbar-drag rule can be asserted.
    #[cfg(test)]
    pub(in crate::native) fn begin_scrollbar_drag_for_test(&mut self) {
        self.pointer_drag = crate::selection::PointerDrag::Scrollbar { grab_dy: 0.0 };
        self.pointer_left_held = true;
    }

    #[cfg(test)]
    pub(in crate::native) fn scrollbar_dragging_for_test(&self) -> bool {
        self.pointer_drag.scrollbar_grab().is_some()
    }

    /// Whether the cached pointer currently intersects a live scrollbar thumb.
    #[cfg(test)]
    pub(in crate::native) fn scrollbar_hit_for_test(&self) -> bool {
        self.scrollbar_hit_test().is_some()
    }

    /// Test seam (INTERACTIVE-PATHS): toggle the `interactive_paths` setting so
    /// the gated hover-scan path (and its byte-identical off path) can be pinned.
    #[cfg(test)]
    pub(in crate::native) fn set_interactive_paths_for_test(&mut self, on: bool) {
        self.settings.interactive_paths = on;
    }

    /// Test seam (INTERACTIVE-URLS): toggle the `interactive_urls` setting so the
    /// gated bare-URL hover-scan path (and its byte-identical off path) can be
    /// pinned without an env var.
    #[cfg(test)]
    pub(in crate::native) fn set_interactive_urls_for_test(&mut self, on: bool) {
        self.settings.interactive_urls = on;
    }

    /// Test seam (INTERACTIVE-PATHS): toggle the `interactive_paths_barewords`
    /// setting so the basename-token (and spaced-filename) hover detection can be
    /// pinned without an env var.
    #[cfg(test)]
    pub(in crate::native) fn set_interactive_paths_barewords_for_test(&mut self, on: bool) {
        self.settings.interactive_paths_barewords = on;
    }

    /// Test seam (SMART-CTRLC): set the `smart_ctrl_c` policy so the plain-Ctrl+C
    /// copy-or-interrupt branch (and its byte-identical off path) can be pinned
    /// through the production `handle_key_event` path.
    #[cfg(test)]
    pub(in crate::native) fn set_smart_ctrl_c_for_test(
        &mut self,
        mode: crate::settings::SmartCtrlC,
    ) {
        self.settings.smart_ctrl_c = mode;
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
        self.window_pointer_px = Some((x, y));
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

    /// Drive the exact new-workspace spawn-failure branch without opening a PTY.
    #[cfg(test)]
    pub(in crate::native) fn new_workspace_spawn_failure_for_test(&mut self) {
        self.finish_new_workspace_spawn(Err(std::io::Error::other("forced spawn failure")));
    }

    /// Drive the exact duplicate-workspace spawn-failure branch without a PTY.
    #[cfg(test)]
    pub(in crate::native) fn duplicate_workspace_spawn_failure_for_test(&mut self) {
        self.finish_duplicate_workspace_spawn(Err(std::io::Error::other("forced spawn failure")));
    }

    /// Drive the exact split-pane spawn-failure branch without opening a PTY.
    #[cfg(test)]
    pub(in crate::native) fn split_pane_spawn_failure_for_test(&mut self) {
        self.finish_split_active_pane_spawn(Err(std::io::Error::other("forced spawn failure")));
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
        // Band-agnostic: reports the hit shape (switch/close/new) whether it
        // lands on the top tab bar or the workspace rail.
        let (_, hit) = self.current_chrome_hit()?;
        match hit {
            TabHit::Switch(_) => Some("switch"),
            TabHit::Close(_) => Some("close"),
            TabHit::NewTab => Some("new"),
            TabHit::AutohideToggle => Some("autohide"),
            TabHit::None => None,
        }
    }

    /// Test seam (W2): the chrome band the pointer hit sits on
    /// (`"tab"`/`"workspace"`), or `None` off chrome.
    pub(in crate::native) fn chrome_hit_band_for_test(&self) -> Option<&'static str> {
        match self.current_chrome_hit()? {
            (crate::native::app::ChromeBand::TopBar, _) => Some("tab"),
            (crate::native::app::ChromeBand::WorkspaceRail, _) => Some("workspace"),
        }
    }

    /// Test seam (CHROME-GAP hit routing): the empty-chrome context-menu
    /// surface the production right-press route resolves for the current
    /// pointer — `"workspace"` / `"tab"` — or `None` when the press would fall
    /// through to the content grid menu (including the padding-wide neutral
    /// gap strips between content and the chrome bands).
    pub(in crate::native) fn empty_chrome_menu_surface_for_test(&self) -> Option<&'static str> {
        match self.empty_chrome_menu_surface()? {
            super::ContextMenuSurface::WorkspaceRailEmpty => Some("workspace"),
            super::ContextMenuSurface::TabStripEmpty => Some("tab"),
            _ => None,
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
            self.decorate_snapshot_with_tab_bar(snapshot.clone(), snapshot.cursor_visible, cell);
        Some(
            decorated.cells[..decorated.dimensions.columns]
                .iter()
                .map(|cell| cell.attrs.background)
                .collect(),
        )
    }

    /// Test seam: text in each row of the decorated top-tab-bar band. This
    /// exercises the same snapshot decoration consumed by the single-pane
    /// renderer after an input-path height change.
    #[cfg(test)]
    pub(in crate::native) fn tab_bar_band_text_for_test(&self) -> Option<Vec<String>> {
        let cell = self.resolved_cell()?;
        let snapshot = self
            .terminal
            .lock()
            .ok()?
            .snapshot_with_scrollback(self.viewport.offset());
        let (decorated, _) =
            self.decorate_snapshot_with_tab_bar(snapshot.clone(), snapshot.cursor_visible, cell);
        let columns = decorated.dimensions.columns;
        let rows = self.tab_bar_rows();
        Some(
            decorated.cells[..columns * rows]
                .chunks_exact(columns)
                .map(|row| row.iter().map(|cell| cell.ch).collect())
                .collect(),
        )
    }

    /// Test seam (F4-V2): set the tab-bar placement (`"top"`/`"left"`/`"right"`)
    /// and recompute the content grid, mirroring the live-toggle path.
    #[cfg(test)]
    pub(in crate::native) fn set_tab_bar_placement_for_test(&mut self, placement: &str) {
        self.settings.tab_bar_placement = match placement {
            "left" => crate::settings::TabBarPlacement::Left,
            "right" => crate::settings::TabBarPlacement::Right,
            _ => crate::settings::TabBarPlacement::Top,
        };
        self.recompute_grid_for_tab_bar();
    }

    /// Test seam (W2): set the `workspace_rail` mode so the workspace rail's
    /// reserve / hit-test / seam / auto-hide machinery can be exercised. `auto`
    /// shows the rail at >=2 workspaces; `always`/`left`/`right` force it on
    /// (side inherited from `tab_bar_placement` for `always`).
    pub(in crate::native) fn set_workspace_rail_for_test(&mut self, mode: &str) {
        self.settings.workspace_rail = match mode {
            "always" => crate::settings::WorkspaceRail::Always,
            "left" => crate::settings::WorkspaceRail::Left,
            "right" => crate::settings::WorkspaceRail::Right,
            _ => crate::settings::WorkspaceRail::Auto,
        };
        self.recompute_grid_for_tab_bar();
    }

    /// Test seam (W2): inject a recorded session as a fresh workspace (its own
    /// single-pane tab) and SWITCH to it, so the rail has multiple slots to
    /// hit-test and the active workspace is single-tab (no top bar competes with
    /// the rail in geometry tests). Mirrors [`Self::push_session_for_test`] one
    /// level up. Returns the new workspace's rail index.
    pub(in crate::native) fn push_workspace_for_test(
        &mut self,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
    ) -> usize {
        let id = self
            .sessions
            .iter()
            .map(|s| s.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.sessions.push_workspace(Session::new(
            crate::native::session::SessionToken(id),
            terminal,
            writer,
            pty,
            None,
        ));
        // `test_cell` / `test_surface` are per-session (accessed via Deref to the
        // active session), so carry the current values onto the freshly-active
        // workspace's session — otherwise the headless geometry (resolved_cell /
        // resolved_surface) goes unset after the switch.
        let cell = self.test_cell;
        let surface = self.test_surface;
        let idx = self.sessions.workspace_count().saturating_sub(1);
        let _ = self.sessions.switch_workspace(idx);
        self.test_cell = cell;
        self.test_surface = surface;
        self.recompute_grid_for_tab_bar();
        idx
    }

    /// Headless variant of [`Self::push_workspace_for_test`]: push a workspace
    /// backed by a [`crate::native::session::HeadlessSession`] instead of a real
    /// PTY, so a pure rail/workspace UI test creates no OS child.
    #[cfg(test)]
    pub(in crate::native) fn push_headless_workspace_for_test(
        &mut self,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        dimensions: crate::core::Dimensions,
    ) -> usize {
        let id = self
            .sessions
            .iter()
            .map(|s| s.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let headless = Arc::new(crate::native::session::HeadlessSession::new(dimensions));
        self.sessions.push_workspace(Session::new_headless(
            crate::native::session::SessionToken(id),
            terminal,
            writer,
            headless,
        ));
        let cell = self.test_cell;
        let surface = self.test_surface;
        let idx = self.sessions.workspace_count().saturating_sub(1);
        let _ = self.sessions.switch_workspace(idx);
        self.test_cell = cell;
        self.test_surface = surface;
        self.recompute_grid_for_tab_bar();
        idx
    }

    /// Test seam (W2): rename the workspace at rail index `idx`.
    pub(in crate::native) fn rename_workspace_for_test(&mut self, idx: usize, name: &str) {
        self.sessions.rename_workspace(idx, name.to_owned());
    }

    /// Test seam (F4-P4): pin a fixed manual rail width (cells) so the
    /// reserve/decoration/hit-test geometry tests stay deterministic regardless
    /// of tab titles, then recompute the grid. Auto-width behavior is exercised
    /// separately at the `Settings::rail_width_cols` seam and the widget.
    #[cfg(test)]
    pub(in crate::native) fn set_tab_rail_width_manual_for_test(&mut self, cols: u16) {
        self.settings.tab_rail_width = crate::settings::TabRailWidth::Manual(cols);
        self.recompute_grid_for_tab_bar();
    }

    /// Test seam (F4-P4): the live rail width mode.
    #[cfg(test)]
    pub(in crate::native) fn tab_rail_width_for_test(&self) -> crate::settings::TabRailWidth {
        self.settings.tab_rail_width
    }

    /// Test seam (F4-P4): point the settings reloader at a hermetic temp config
    /// file so the seam-drag persistence path writes there, not the real config.
    #[cfg(test)]
    pub(in crate::native) fn set_config_path_for_test(&mut self, path: std::path::PathBuf) {
        self.settings_reloader.set_config_path_for_test(Some(path));
    }

    /// Test seam (F4-P4): whether a rail seam drag is currently in progress.
    #[cfg(test)]
    pub(in crate::native) fn rail_seam_dragging_for_test(&self) -> bool {
        self.rail_seam_drag
    }

    /// Test seam: map a physical pointer x through the live reveal-aware rail
    /// width geometry.
    #[cfg(test)]
    pub(in crate::native) fn rail_width_from_pointer_for_test(&self, x: f64) -> Option<u16> {
        self.rail_width_from_pointer(x, self.resolved_cell()?)
    }

    /// Test seam: force a manual tab-bar height in rows and reflow, so the
    /// bottom-seam drag / reservation tests start from a deterministic band.
    #[cfg(test)]
    pub(in crate::native) fn set_tab_bar_height_manual_for_test(&mut self, rows: u16) {
        self.settings.tab_bar_height = crate::settings::TabBarHeight::Manual(rows);
        self.recompute_grid_for_tab_bar();
    }

    /// Test seam: the live tab-bar height mode.
    #[cfg(test)]
    pub(in crate::native) fn tab_bar_height_for_test(&self) -> crate::settings::TabBarHeight {
        self.settings.tab_bar_height
    }

    /// Test seam: the resolved tab-bar height in rows this frame.
    #[cfg(test)]
    pub(in crate::native) fn tab_bar_rows_for_test(&self) -> usize {
        self.tab_bar_rows()
    }

    /// Test seam: whether a tab-bar height seam drag is currently in progress.
    #[cfg(test)]
    pub(in crate::native) fn tab_bar_seam_dragging_for_test(&self) -> bool {
        self.tab_bar_seam_drag
    }

    /// Exact horizontal span shared by the drawn top seam and its RowResize
    /// hit target.
    #[cfg(test)]
    pub(in crate::native) fn top_panel_span_for_test(&self) -> Option<[f32; 2]> {
        let cell = self.resolved_cell()?;
        let (surface_w, _, padding) = self.resolved_surface()?;
        Some(
            self.top_panel_span(cell, surface_w as f32, padding)
                .unwrap_or([0.0, surface_w as f32]),
        )
    }

    /// Exact physical Y of the top seam used by drawing and hit-testing.
    #[cfg(test)]
    pub(in crate::native) fn tab_bar_seam_y_for_test(&self) -> Option<f32> {
        self.tab_bar_seam_y_px(self.resolved_cell()?)
    }

    // --- F4-P3 rail auto-hide seams ---

    /// Test seam (F4-P3): toggle rail auto-hide and reflow the grid (the single
    /// reflow at toggle time), mirroring the live setting-change path.
    #[cfg(test)]
    pub(in crate::native) fn set_tab_rail_autohide_for_test(&mut self, on: bool) {
        self.settings.tab_rail_autohide = on;
        self.recompute_grid_for_tab_bar();
    }

    /// Test seam (F4-P3): whether rail auto-hide is active this frame.
    #[cfg(test)]
    pub(in crate::native) fn rail_autohide_active_for_test(&self) -> bool {
        self.rail_autohide_active()
    }

    /// Test seam (RAIL-AUTOHIDE-CTL): the raw `tab_rail_autohide` setting value,
    /// independent of whether the rail is currently shown.
    #[cfg(test)]
    pub(in crate::native) fn tab_rail_autohide_setting_for_test(&self) -> bool {
        self.settings.tab_rail_autohide
    }

    /// Test seam (RAIL-AUTOHIDE-CTL): center pixel of the rail's bottom-edge
    /// auto-hide toggle control this frame, or `None` when the rail (or its
    /// revealed overlay) is not present. Built from the same geometry the live
    /// pointer hit path uses, so a click placed here exercises the real arm.
    #[cfg(test)]
    pub(in crate::native) fn rail_autohide_center_px_for_test(&self) -> Option<(f64, f64)> {
        let cell = self.resolved_cell()?;
        let geom = self.rail_geom_px(cell)?;
        let rect = geom.autohide?;
        Some((rect.x + rect.width / 2.0, rect.y + rect.height / 2.0))
    }

    /// Test seam (F4-P3 reveal-paint gate): read/reset the frame-rebuild flag the
    /// `should_rebuild_frame` gate consults. `self.needs_rebuild` Derefs to the
    /// ACTIVE pane's flag — the same one the production rail-reveal paths must set
    /// so a visibility flip actually rebuilds and paints the overlay (a redraw
    /// request alone is dropped by the rebuild gate).
    #[cfg(test)]
    pub(in crate::native) fn needs_rebuild_for_test(&self) -> bool {
        self.needs_rebuild
    }

    /// Test seam (F4-P3 reveal-paint gate): clear the frame-rebuild flag so a test
    /// can prove a rail visibility flip re-sets it.
    #[cfg(test)]
    pub(in crate::native) fn clear_needs_rebuild_for_test(&mut self) {
        self.needs_rebuild = false;
    }

    /// Test seam (RAIL-DRAG): presentation-cache generation used to prove that
    /// drag arm/move/release invalidate retained chrome geometry immediately.
    #[cfg(test)]
    pub(in crate::native) fn presentation_epoch_for_test(&self) -> u64 {
        self.presentation_epoch
    }

    /// Test seam (F4-P3): force the revealed phase so overlay geometry / hit
    /// routing can be asserted without simulating the debounce clock.
    #[cfg(test)]
    pub(in crate::native) fn force_rail_reveal_for_test(&mut self) {
        self.rail_autohide.force_revealed();
    }

    /// Test seam (F4-P3): whether the floating rail overlay is drawn this frame.
    #[cfg(test)]
    pub(in crate::native) fn rail_overlay_visible_for_test(&self) -> bool {
        self.rail_overlay_visible()
    }

    /// Test seam (F4-P3): the overlay band width (cells) under auto-hide.
    #[cfg(test)]
    pub(in crate::native) fn rail_overlay_cols_for_test(&self) -> usize {
        self.rail_overlay_cols()
    }

    /// Test seam (F4-P3): the reveal `(in_edge, in_band)` contact for a raw
    /// pointer x — including the scrollbar-drag yield — or `None` off a
    /// rail / autohide. The point-only contact (no previous sample), so it
    /// asserts the static trigger/band geometry; the motion-aware segment path is
    /// exercised through the live feed (`feed_rail_pointer_for_test`). Set
    /// `pointer_px` / a scrollbar grab first as needed.
    #[cfg(test)]
    pub(in crate::native) fn reveal_contact_for_test(&self, x: f64) -> Option<(bool, bool)> {
        let side = self.rail_autohide_side()?;
        let cell = self.resolved_cell()?;
        Some(self.reveal_pointer_contact(x, None, cell, side))
    }

    /// Test seam (F4-P3 / NF20-B): drive the reveal machine through the REAL
    /// live feed (`update_rail_autohide_pointer`) with an injected clock, so the
    /// full reveal → hold → hide sequence is exercised through the production
    /// contact geometry deterministically. Mirrors a `CursorMoved` at `x_px`.
    #[cfg(test)]
    pub(in crate::native) fn feed_rail_pointer_for_test(
        &mut self,
        x_px: f64,
        now: std::time::Instant,
    ) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        self.update_rail_autohide_pointer(x_px, cell, now);
    }

    /// Test seam (F4-P3 / NF20-B): advance the reveal machine's timers with an
    /// injected clock (the about-to-wait `poll`), returning whether visibility
    /// changed. Lets a test cross the show-debounce / hide-grace boundaries.
    #[cfg(test)]
    pub(in crate::native) fn poll_rail_autohide_for_test(
        &mut self,
        now: std::time::Instant,
    ) -> bool {
        self.rail_autohide.poll(now)
    }

    /// Test seam (F4-P3 / NF20-B): whether the reveal machine currently reports
    /// visible at `now` (drives the overlay draw + hit-test gate).
    #[cfg(test)]
    pub(in crate::native) fn rail_autohide_is_visible_for_test(
        &self,
        now: std::time::Instant,
    ) -> bool {
        self.rail_autohide.is_visible(now)
    }

    /// Test seam (F4-P3): whether the pointer x is over the active seam grab
    /// band. For auto-hide this resolves only while the floating rail is visible.
    #[cfg(test)]
    pub(in crate::native) fn pointer_over_rail_seam_for_test(&self, x: f64) -> Option<bool> {
        let cell = self.resolved_cell()?;
        Some(self.pointer_over_rail_seam(x, cell))
    }

    /// Test seam (F4-P4): a left press routed through the real
    /// `handle_mouse_input` dispatch (covers the seam grab / double-click wiring,
    /// not just the handler in isolation). Set `pointer_px` first.
    #[cfg(test)]
    pub(in crate::native) fn mouse_left_press_for_test(&mut self) {
        self.handle_mouse_input(ElementState::Pressed, WinitMouseButton::Left);
    }

    /// Route a seam press through the real dispatch with a controlled monotonic
    /// timestamp for deterministic double-click detection.
    #[cfg(test)]
    pub(in crate::native) fn mouse_left_press_at_for_test(&mut self, at: std::time::Instant) {
        self.seam_click_at_for_test = Some(at);
        self.mouse_left_press_for_test();
    }

    /// Test seam (F4-P4): the left release that ends a seam drag, routed through
    /// the real `handle_mouse_input` dispatch.
    #[cfg(test)]
    pub(in crate::native) fn mouse_left_release_for_test(&mut self) {
        self.handle_mouse_input(ElementState::Released, WinitMouseButton::Left);
    }

    /// Test seam (F4-P3 regression): a right press routed through the real
    /// `handle_mouse_input` dispatch, so the context-menu open path (and any
    /// autohide interference with it) can be asserted end-to-end. Set
    /// `pointer_px` first.
    #[cfg(test)]
    pub(in crate::native) fn mouse_right_press_for_test(&mut self) {
        self.handle_mouse_input(ElementState::Pressed, WinitMouseButton::Right);
    }

    /// Test seam (F4-V2): the decorated single-pane snapshot's `(columns, rows)`
    /// after tab-chrome decoration. A left rail grows columns by `rail_cols`; the
    /// top bar grows rows by `TAB_BAR_ROWS`.
    #[cfg(test)]
    pub(in crate::native) fn decorated_snapshot_dims_for_test(&self) -> Option<(usize, usize)> {
        let cell = self.resolved_cell()?;
        let snapshot = self
            .terminal
            .lock()
            .ok()?
            .snapshot_with_scrollback(self.viewport.offset());
        let (decorated, _) =
            self.decorate_snapshot_with_tab_bar(snapshot.clone(), snapshot.cursor_visible, cell);
        Some((decorated.dimensions.columns, decorated.dimensions.rows))
    }

    /// Test seam (F4-P3): the UNDECORATED content snapshot's `(columns, rows)`.
    /// Paired with `decorated_snapshot_dims_for_test` so the phantom-top-bar
    /// regression (auto-hide leaking a top bar into a side-placed decoration) is
    /// pinned as `decorated == raw` — no rows grown off the top, no columns off
    /// the side.
    #[cfg(test)]
    pub(in crate::native) fn raw_snapshot_dims_for_test(&self) -> Option<(usize, usize)> {
        let snapshot = self
            .terminal
            .lock()
            .ok()?
            .snapshot_with_scrollback(self.viewport.offset());
        Some((snapshot.dimensions.columns, snapshot.dimensions.rows))
    }

    /// Drive the single-pane cursor-effect ordering through the production
    /// snapshot preparation seam. The returned pair is `(decorated, content)`:
    /// only the first may include pinned tab or workspace-rail coordinates.
    #[cfg(test)]
    pub(in crate::native) fn advance_single_pane_cursor_effects_with_chrome_for_test(
        &mut self,
        now: std::time::Instant,
        snapshot: &Snapshot,
        cell: CellSize,
    ) -> (
        Snapshot,
        Snapshot,
        CursorRenderParams,
        Option<crate::native::gpu::CursorStreakRequest>,
    ) {
        self.update_cursor_motion(now, snapshot, cell);
        self.update_cursor_streak(now, snapshot, crate::core::CursorStyle::Block, cell);

        let (chrome_dx, chrome_dy) = self.tab_chrome_offset_px(cell);
        let pad = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO)
            .as_f32();
        let content_x0 = pad + chrome_dx as f32;
        let content_y0 = pad + chrome_dy as f32;
        let streak = self.cursor_streak_request(
            now,
            [
                content_x0,
                content_y0,
                content_x0 + snapshot.dimensions.columns as f32 * cell.width as f32,
                content_y0 + snapshot.dimensions.rows as f32 * cell.height as f32,
            ],
        );
        let content = snapshot.clone();
        let (decorated, _, comparison) =
            self.prepare_single_pane_snapshots(snapshot.clone(), snapshot.cursor_visible, cell);
        let mut held = decorated.clone();
        held.cursor_visible = snapshot.cursor_visible;
        self.last_presented_snapshot = Some(held);
        self.last_cursor_comparison_snapshot = Some(comparison);
        self.last_presented_cursor_style = crate::core::CursorStyle::Block;
        (decorated, content, self.cursor_render_params(), streak)
    }

    #[cfg(test)]
    pub(in crate::native) fn held_snapshot_geometry_for_test(
        &self,
    ) -> Option<(Dimensions, Position)> {
        self.last_presented_snapshot
            .as_ref()
            .map(|snapshot| (snapshot.dimensions, snapshot.cursor))
    }

    /// Test seam (F4-P3 / NF20): whether the rail auto-hide machine schedules a
    /// wake at `now` — the state machine's contribution to `next_wake_deadline`.
    /// `false` means the machine is idle (steady Hidden, or Revealed with the
    /// pointer parked) and adds no self-wake, so an idle auto-hidden window
    /// cannot spin the event loop. Clock-injected for determinism.
    #[cfg(test)]
    pub(in crate::native) fn rail_autohide_wants_wake_for_test(
        &self,
        now: std::time::Instant,
    ) -> bool {
        self.rail_autohide.wake_deadline(now).is_some()
    }

    /// Test seam (NF20-B): arm the ACTIVE pane's cursor blink (blinking +
    /// focused) at `now`, so its activity-hold deadline enters the wake set — the setup
    /// for the per-session deadline fan-out regression (a pane whose blink is
    /// armed and then backgrounded must not strand a stale wake).
    #[cfg(test)]
    pub(in crate::native) fn arm_active_cursor_blink_for_test(&mut self, now: std::time::Instant) {
        self.cursor_blink.poll(now, true, true);
    }

    /// Test seam (NF20-B): the aggregate next event-loop wake deadline. Exposes
    /// the private `next_wake_deadline` so the multi-pane deadline fan-out
    /// regression can assert the loop parks (no stale past instant) after a tab /
    /// pane switch + maintenance.
    #[cfg(test)]
    pub(in crate::native) fn next_wake_deadline_for_test(&self) -> Option<std::time::Instant> {
        self.next_wake_deadline()
    }

    /// Test seam (NF20-B): arm the ACTIVE pane's cursor-animation (ease + slide)
    /// deadlines at `now`, so both enter the wake set — the setup for asserting
    /// they do not strand a stale wake once the pane is backgrounded.
    #[cfg(test)]
    pub(in crate::native) fn arm_active_cursor_anim_for_test(&mut self, now: std::time::Instant) {
        self.cursor_ease_deadline = Some(now + std::time::Duration::from_millis(200));
        self.cursor_slide_deadline = Some(now + std::time::Duration::from_millis(150));
    }

    /// Test seam for the focused split-pane cursor consumer. Enables every
    /// cursor effect without changing production defaults.
    #[cfg(test)]
    pub(in crate::native) fn enable_cursor_effects_for_test(&mut self) {
        self.settings.cursor_motion = true;
        self.settings.cursor_trail = true;
        self.settings.cursor_glow = true;
        self.settings.cursor_easing = true;
    }

    /// Disable every cursor effect so a split test can prove the identity path
    /// emits no effect geometry and arms no frame-paced cursor wake.
    #[cfg(test)]
    pub(in crate::native) fn disable_cursor_effects_for_test(&mut self) {
        self.settings.cursor_motion = false;
        self.settings.cursor_trail = false;
        self.settings.cursor_glow = false;
        self.settings.cursor_easing = false;
    }

    #[cfg(test)]
    pub(in crate::native) fn focused_cursor_animation_deadline_for_test(
        &self,
    ) -> Option<std::time::Instant> {
        self.focused_cursor_animation_deadline()
    }

    #[cfg(test)]
    pub(in crate::native) fn cursor_streak_deadline_for_test(&self) -> Option<std::time::Instant> {
        self.cursor_streak_deadline()
    }

    /// Drive the same focused cursor consumer used by `rebuild_multipane` and
    /// return its pane-local quads plus the live render parameters.
    #[cfg(test)]
    pub(in crate::native) fn advance_multipane_cursor_effects_for_test(
        &mut self,
        now: std::time::Instant,
        snapshot: &mut Snapshot,
        cell: CellSize,
        origin: [f32; 2],
    ) -> (
        Vec<SolidQuad>,
        Option<crate::native::gpu::CursorGlowRequest>,
        Option<crate::native::gpu::CursorStreakRequest>,
        CursorRenderParams,
    ) {
        let clip_rect = [
            origin[0],
            origin[1],
            origin[0] + snapshot.dimensions.columns as f32 * cell.width as f32,
            origin[1] + snapshot.dimensions.rows as f32 * cell.height as f32,
        ];
        let effects = self.advance_focused_multipane_cursor(
            now,
            snapshot,
            crate::core::CursorStyle::Block,
            true,
            cell,
            origin,
            clip_rect,
            0,
            0,
        );
        let glow = self.cursor_glow_request(clip_rect);
        let streak = self.cursor_streak_request(now, clip_rect);
        (effects, glow, streak, self.cursor_render_params())
    }

    /// Whether every non-focused session has no live cursor wake source.
    #[cfg(test)]
    pub(in crate::native) fn background_cursor_timers_parked_for_test(&self) -> bool {
        let active = self.sessions.active_id();
        self.sessions
            .iter()
            .filter(|session| session.id != active)
            .all(|session| {
                session.cursor_blink.deadline().is_none()
                    && session.cursor_ease_deadline.is_none()
                    && session.cursor_slide_deadline.is_none()
                    && session.cursor_streak.deadline().is_none()
            })
    }

    /// Test seam (NF20-B): arm the ACTIVE pane's synchronized-output hold at
    /// `now`, so its timeout deadline enters the wake set.
    #[cfg(test)]
    pub(in crate::native) fn arm_active_sync_hold_for_test(&mut self, now: std::time::Instant) {
        self.synchronized_output_hold.should_hold(true, now);
    }

    /// Test seam (NF21-7): the render gate's rebuild decision — `self.needs_rebuild`
    /// (the focused pane) ORed, when multi-pane, with any visible pane of the
    /// active tab. Lets the split-pane regression assert output into a
    /// non-focused pane drives a rebuild without a GPU.
    #[cfg(test)]
    pub(in crate::native) fn should_rebuild_frame_for_test(&self) -> bool {
        self.should_rebuild_frame()
    }

    /// Test seam (NF21-7): clear `needs_rebuild` on every visible pane of the
    /// active tab (the production multipane-rebuild sweep), so a split-pane test
    /// can establish an idle (gate-closed) baseline.
    #[cfg(test)]
    pub(in crate::native) fn clear_visible_pane_rebuild_flags_for_test(&mut self) {
        self.sessions.clear_visible_pane_rebuild_flags();
    }

    /// Test seam (NF21-7): a specific pane's `needs_rebuild` flag by token
    /// (pane-level, unlike the tab-indexed `session_needs_rebuild_for_test`),
    /// so a split-pane test can assert output marked the producing background
    /// pane dirty.
    #[cfg(test)]
    pub(in crate::native) fn pane_needs_rebuild_for_test(
        &self,
        token: crate::native::session::SessionToken,
    ) -> Option<bool> {
        self.sessions
            .get(token)
            .map(|session| session.needs_rebuild)
    }

    /// Test seam (F4-V2): the reserved `(rows_off_top, cols_off_side)` for the
    /// current placement.
    #[cfg(test)]
    pub(in crate::native) fn tab_reserve_for_test(&self) -> (usize, usize) {
        let r = self.tab_reserve();
        (r.top_rows, r.left_cols + r.right_cols)
    }

    /// Test seam (F4-P2): the pixel offset subtracted from a raw pointer / added
    /// to content-space overlays for the current tab chrome. `(0, 0)` for the top
    /// bar and a RIGHT rail (content origin unmoved); positive `x` for a LEFT
    /// rail. Proves right-edge overlays (IME candidate, click hint) are not
    /// shifted under a right rail.
    #[cfg(test)]
    pub(in crate::native) fn tab_chrome_offset_px_for_test(&self) -> Option<(f64, f64)> {
        let cell = self.resolved_cell()?;
        Some(self.tab_chrome_offset_px(cell))
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

    /// Test seam (SELECT-COPY-CLAMP): set a BLOCK (column-band) absolute
    /// selection so a rectangular copy spanning scrollback can be exercised
    /// through the exact `current_selection_text` choke point.
    #[cfg(test)]
    pub(in crate::native) fn set_block_selection_range_for_test(
        &mut self,
        start_row: usize,
        start_column: usize,
        end_row: usize,
        end_column: usize,
    ) {
        use crate::selection::{AbsoluteCellPoint, AbsoluteSelectionRange};
        self.selection_block = true;
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

    /// Test seam: run one active-pane render frame's viewport anchor exactly as
    /// the single-pane RedrawRequested path does (read the active terminal's
    /// scrollback length, then `anchor_viewport_for_render`), returning the
    /// resolved offset. GPU-free slice of the real render loop, so a viewport
    /// regression can drive the production anchor without a window.
    #[cfg(test)]
    pub(in crate::native) fn anchor_viewport_for_render_frame_for_test(&mut self) -> usize {
        let scrollback_len = crate::native::lock_recover(&self.terminal)
            .screen()
            .scrollback_len();
        self.anchor_viewport_for_render(scrollback_len)
    }

    /// Test seam: the active session's scrollback-growth baseline
    /// (`last_scrollback_len`), so a regression can prove it is reconciled on
    /// tab activation rather than left stale across a switch.
    #[cfg(test)]
    pub(in crate::native) fn last_scrollback_len_for_test(&self) -> usize {
        self.last_scrollback_len
    }

    /// Test seam (RC-16): viewport offset for a specific pane token, so a
    /// multi-pane wheel test can prove routing targets the pane under the
    /// pointer rather than the focused pane.
    #[cfg(test)]
    pub(in crate::native) fn viewport_offset_for_token_for_test(
        &self,
        token: usize,
    ) -> Option<usize> {
        self.sessions
            .get(crate::native::session::SessionToken(token as u64))
            .map(|session| session.viewport.offset())
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

    /// Test seam (FIX-A): number of synchronous clipboard `read_text` probes.
    /// A menu-open regression test asserts this stays 0 across a context-menu
    /// open, proving the open path no longer blocks the event-loop thread on a
    /// clipboard read (the ~12s Wayland right-click freeze).
    #[cfg(test)]
    pub(in crate::native) fn clipboard_read_text_calls_for_test(&self) -> usize {
        self.clipboard.read_text_calls
    }

    /// Test seam (FIX-A): backdate the context-menu open instant so the input
    /// debounce window has elapsed. Lets a test prove that presses route to the
    /// menu again after the debounce, without a real sleep.
    #[cfg(test)]
    pub(in crate::native) fn expire_context_menu_debounce_for_test(&mut self) {
        self.context_menu_opened_at =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(3600));
    }

    /// Test seam (FIX-A): whether the last `open_context_menu` ran the
    /// interactive-path scan (the PATH-GATE decision). `false` proves a chrome
    /// (rail/tab) right-click skipped the stat-probing scan that could block on a
    /// hung mount.
    #[cfg(test)]
    pub(in crate::native) fn last_menu_path_scan_for_test(&self) -> bool {
        self.last_menu_path_scan_for_test
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

    /// Headless variant of [`Self::push_session_for_test`]: push a second tab
    /// backed by a [`crate::native::session::HeadlessSession`] instead of a real
    /// PTY, so a pure multi-tab UI test creates no OS child.
    #[cfg(test)]
    pub(in crate::native) fn push_headless_session_for_test(
        &mut self,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        dimensions: crate::core::Dimensions,
    ) -> usize {
        let next_id = self
            .sessions
            .iter()
            .map(|session| session.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let headless = Arc::new(crate::native::session::HeadlessSession::new(dimensions));
        let id = self.sessions.push(Session::new_headless(
            crate::native::session::SessionToken(next_id),
            terminal,
            writer,
            headless,
        ));
        self.sessions.position_of_token(id).unwrap_or(0)
    }

    #[cfg(test)]
    pub(in crate::native) fn switch_to_session_for_test(&mut self, session: usize) -> bool {
        let Some(token) = self.sessions.token_at_position(session) else {
            return false;
        };
        self.finish_divider_drag();
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
    pub(in crate::native) fn workspace_count_for_test(&self) -> usize {
        self.sessions.workspace_count()
    }

    #[cfg(test)]
    pub(in crate::native) fn active_workspace_index_for_test(&self) -> usize {
        self.sessions.active_workspace_index()
    }

    /// Test seam (RAIL-REORDER): drive the App-side workspace reorder exactly as
    /// a context-menu "Move Up"/"Move Down" activation would (via the same
    /// `move_workspace_at` the interaction layer calls).
    #[cfg(test)]
    pub(in crate::native) fn move_workspace_at_for_test(&mut self, idx: usize, up: bool) {
        self.move_workspace_at(idx, up);
    }

    /// Test seam (RAIL-DRAG): the in-flight workspace-rail drag as
    /// `(armed, drop_idx)`, or `None` when no drag is active. Lets a test assert
    /// the click-vs-drag threshold and the live drop-target index.
    #[cfg(test)]
    pub(in crate::native) fn rail_ws_drag_for_test(&self) -> Option<(bool, usize)> {
        self.rail_ws_drag.map(|d| (d.armed, d.drop_idx))
    }

    /// Widget overlay quads retained by the revealed auto-hide rail path.
    #[cfg(test)]
    pub(in crate::native) fn rail_overlay_widget_quads_for_test(&self) -> Vec<SolidQuad> {
        let Some(cell) = self.resolved_cell() else {
            return Vec::new();
        };
        self.build_rail_overlay(cell)
            .map_or_else(Vec::new, |overlay| overlay.widget_quads)
    }

    /// Test seam (TOP-TAB-DRAG): armed state and live insertion index.
    #[cfg(test)]
    pub(in crate::native) fn top_tab_drag_for_test(&self) -> Option<(bool, usize)> {
        self.top_tab_drag.map(|drag| (drag.armed, drag.drop_idx))
    }

    /// Frame-level top-strip geometry probe. Returns the hit slot, drop index,
    /// insertion boundary, and hit-slot start in window pixels.
    #[cfg(test)]
    pub(in crate::native) fn top_chrome_geometry_probe_for_test(
        &self,
        x: f64,
        y: f64,
        drag_origin: usize,
    ) -> Option<(usize, usize, f64, f64)> {
        let geometry = self.top_strip_geom(self.resolved_cell()?)?;
        let hit_idx = match geometry.hit(PxPoint::new(x, y)) {
            TabHit::Switch(idx) | TabHit::Close(idx) => idx,
            TabHit::NewTab | TabHit::AutohideToggle | TabHit::None => return None,
        };
        let drop_idx = geometry.drop_index(x, drag_origin)?;
        let boundary = geometry.insertion_boundary_px(drop_idx, drag_origin);
        let slot_start = geometry
            .slots
            .iter()
            .find(|slot| slot.idx == hit_idx)?
            .rect
            .x;
        Some((hit_idx, drop_idx, boundary, slot_start))
    }

    /// Test seam (RAIL-DRAG): whether a rail-anchored surface (a drag, menu, or
    /// rename) is currently holding the auto-hide rail revealed — the
    /// autohide-hold assertion for a workspace drag.
    #[cfg(test)]
    pub(in crate::native) fn rail_pinned_open_for_test(&self) -> bool {
        self.rail_pinned_open()
    }

    /// Test seam (WP1): capture the current window shape as a serializable
    /// snapshot, exercising the persistence capture path end-to-end against a
    /// headless multi-workspace / multi-pane `App`.
    #[cfg(test)]
    pub(in crate::native) fn capture_shape_for_test(
        &self,
    ) -> crate::native::persistence::ShapeSnapshot {
        self.sessions.capture_shape()
    }

    /// Test seam (RESTORE-THEME): the dynamic default `(foreground, background)`
    /// of a session's terminal, in arena order. A live-created session carries
    /// the theme colors; a session spawned by snapshot restore / layout append
    /// carries `DynamicColors::default()` until the app seeds it. Lets a test
    /// assert the seed actually ran.
    #[cfg(test)]
    pub(in crate::native) fn session_dynamic_colors_for_test(
        &self,
        session: usize,
    ) -> Option<(crate::core::RgbColor, crate::core::RgbColor)> {
        self.sessions.iter().nth(session).and_then(|session| {
            session.terminal.lock().ok().map(|terminal| {
                let colors = terminal.dynamic_colors();
                (colors.foreground, colors.background)
            })
        })
    }

    /// Test seam (RESTORE-THEME): apply the current app-global presentation state
    /// (theme colors / palette / cursor defaults / scrollback cap) to every
    /// session's terminal — the same sweep restore-on-launch and layout append
    /// run so snapshot-spawned sessions stop rendering in the default palette.
    #[cfg(test)]
    pub(in crate::native) fn apply_model_state_to_all_sessions_for_test(&mut self) {
        self.apply_model_state_to_all_sessions();
    }

    /// Test seam (RESTORE-THEME): drive the layout-append-and-seed path with a
    /// HEADLESS leaf spawner (no event-loop proxy), so CI EXERCISES the seed
    /// instead of skipping. A proxy-backed variant would need a real winit
    /// `EventLoop`, which cannot be
    /// built where no display exists (CI, most agent shells) — there it returns
    /// early and asserts nothing. This variant spawns each appended leaf through
    /// the headless append seam, so the seed actually runs and is asserted
    /// everywhere the suite runs, including macOS.
    #[cfg(test)]
    pub(in crate::native) fn append_snapshot_headless_for_test(
        &mut self,
        snapshot: &crate::native::persistence::ShapeSnapshot,
    ) -> crate::native::session::RestoreReport {
        use crate::native::persistence::restore_home_dir;
        let home = restore_home_dir();
        let report = self
            .sessions
            .append_from_snapshot_headless_for_test(snapshot, home.as_deref());
        if matches!(
            report,
            crate::native::session::RestoreReport::Restored { .. }
        ) {
            self.apply_model_state_to_all_sessions();
        }
        report
    }

    /// Test seam (WP2): mark this headless `App` as the primary instance so the
    /// debounced shape autosave runs (it is inert on non-primary instances).
    #[cfg(test)]
    pub(in crate::native) fn set_primary_instance_for_test(&mut self, primary: bool) {
        self.set_primary_instance(primary);
    }

    /// Test seam (SECONDARY-INSTANCE-NOTICE): flip the `restore_workspaces`
    /// setting so the notice gate can be exercised without a config file.
    #[cfg(test)]
    pub(in crate::native) fn set_restore_workspaces_for_test(&mut self, on: bool) {
        self.settings.restore_workspaces = on;
    }

    /// Test seam (SECONDARY-INSTANCE-NOTICE): drive the startup notice gate.
    #[cfg(test)]
    pub(in crate::native) fn notice_secondary_instance_for_test(&mut self) {
        self.notice_secondary_instance_if_suppressed();
    }

    /// Test seam (WP2): number of shape writes the autosave has emitted. Under
    /// `cfg(test)` `write_shape_snapshot` bumps a counter instead of touching
    /// disk, so the debounce-coalescing tests assert exactly-once without I/O.
    #[cfg(test)]
    pub(in crate::native) fn autosave_saves_for_test(&self) -> u32 {
        self.autosave_saves
    }

    /// Test seam (WP2): whether a debounced autosave write is currently pending.
    #[cfg(test)]
    pub(in crate::native) fn autosave_pending_for_test(&self) -> bool {
        self.autosave_deadline.is_some()
    }

    /// Test seam (NF21-6): run one arena-wide bell + prompt-marks drain and
    /// return `(focused_bell, background_bell, focused_prompt_changed)` — so a
    /// test can assert routing without a real window (urgency is a no-op
    /// headlessly). The per-tab activity latch is applied as a side effect.
    #[cfg(test)]
    pub(in crate::native) fn drain_bells_for_test(&mut self) -> (bool, bool, bool) {
        let sweep = self.sessions.drain_bells(
            self.settings.command_status_gutter,
            std::time::Instant::now(),
        );
        (
            sweep.focused_bell,
            sweep.background_bell,
            sweep.focused_prompt_changed,
        )
    }

    /// Test seam (NF21-6): the unseen-activity latch of a tab.
    #[cfg(test)]
    pub(in crate::native) fn tab_activity_for_test(&self, ws_idx: usize, tab_idx: usize) -> bool {
        self.sessions.tab_activity(ws_idx, tab_idx)
    }

    /// Test seam (NF21-6): the DERIVED workspace-level activity rollup.
    #[cfg(test)]
    pub(in crate::native) fn workspace_activity_for_test(&self, ws_idx: usize) -> bool {
        self.sessions.workspace_has_activity(ws_idx)
    }

    /// Test seam (NF21-6): whether a bell visual flash is in flight.
    #[cfg(test)]
    pub(in crate::native) fn bell_flash_active_for_test(&self) -> bool {
        self.bell_flash_start.is_some()
    }

    /// Test seam (NF21-6): put the bell in Visual mode so a focused-pane bell
    /// starts a flash headlessly (default Urgent flashes nothing).
    #[cfg(test)]
    pub(in crate::native) fn set_bell_visual_for_test(&mut self) {
        self.settings.bell = crate::settings::BellMode::Visual;
    }

    #[cfg(test)]
    pub(in crate::native) fn workspace_names_for_test(&self) -> Vec<String> {
        self.sessions.workspace_names()
    }

    /// F6-W5: read the active workspace's host binding (the alias New Tab routes
    /// through), or `None` when the workspace is local.
    #[cfg(test)]
    pub(in crate::native) fn active_workspace_binding_for_test(&self) -> Option<String> {
        self.sessions
            .active_workspace_default_profile()
            .map(str::to_owned)
    }

    /// F6-W5: directly set the active workspace's host binding, bypassing the
    /// host-list resolution — so a headless test can exercise the New Tab
    /// routing without a configured `hosts.conf`.
    #[cfg(test)]
    pub(in crate::native) fn set_workspace_binding_for_test(&mut self, alias: Option<String>) {
        self.sessions.set_active_workspace_default_profile(alias);
    }

    /// The tab count of the ACTIVE workspace (the tab strip's length).
    #[cfg(test)]
    pub(in crate::native) fn active_workspace_tab_count_for_test(&self) -> usize {
        self.sessions.tab_count()
    }

    /// Open a fresh tab in the active workspace (the production `New Tab` path).
    #[cfg(test)]
    pub(in crate::native) fn new_tab_for_test(&mut self) {
        self.handle_new_tab();
    }

    /// Split the active pane into side-by-side columns (production path), so a
    /// test can build a multi-pane tab (SHELL-EXIT-CLOSES granularity).
    #[cfg(test)]
    pub(in crate::native) fn split_active_columns_for_test(&mut self) {
        self.split_active_pane(crate::native::layout::SplitAxis::Columns);
    }

    /// Set the exit-behavior setting to App mode (SHELL-EXIT-CLOSES): a shell
    /// exit that would close a workspace quits OdyTTY instead.
    #[cfg(test)]
    pub(in crate::native) fn set_shell_exit_closes_app_for_test(&mut self) {
        self.settings.shell_exit_closes = crate::settings::ShellExitCloses::App;
    }

    /// Toggle the running-job close confirmation (SHELL-EXIT-CLOSES tests set it
    /// off to make the App-mode quit deterministic regardless of PTY job state).
    #[cfg(test)]
    pub(in crate::native) fn set_confirm_close_for_test(&mut self, on: bool) {
        self.settings.confirm_close = on;
    }

    /// Move the tab holding `token` into the workspace at `dest_ws` (W4-v2),
    /// driving the same App handler the "Move to Workspace" picker accept
    /// dispatches.
    #[cfg(test)]
    pub(in crate::native) fn move_tab_to_workspace_for_test(
        &mut self,
        token: crate::native::session::SessionToken,
        dest_ws: usize,
    ) {
        self.move_tab_to_workspace(token, dest_ws);
    }

    /// Open the "Move to Workspace" destination picker for the tab holding
    /// `token` (W4-v2), driving the same App handler the tab context-menu item
    /// dispatches. Returns the picker's seeded destination count so a test can
    /// assert exclusion/visibility without reaching into the overlay.
    #[cfg(test)]
    pub(in crate::native) fn open_move_tab_workspace_picker_for_test(
        &mut self,
        token: crate::native::session::SessionToken,
    ) -> usize {
        let count = self.sessions.move_tab_destinations(token).len();
        self.open_move_tab_workspace_picker(token);
        count
    }

    /// Drive the workspace `BindableAction`s exactly as the key-dispatch match
    /// arms do, so a test exercises the real handler wiring (W3).
    #[cfg(test)]
    pub(in crate::native) fn dispatch_workspace_action_for_test(
        &mut self,
        action: crate::settings::BindableAction,
    ) {
        use crate::settings::BindableAction as BA;
        match action {
            BA::NewWorkspace => self.handle_new_workspace(),
            BA::CloseWorkspace => self.close_active_workspace(),
            BA::RenameWorkspace => {
                self.enter_rename_workspace(self.sessions.active_workspace_index())
            }
            BA::NextWorkspace => self.switch_to_next_workspace(),
            BA::PrevWorkspace => self.switch_to_prev_workspace(),
            BA::WorkspacePicker => self.open_command_palette_overlay(),
            _ => {}
        }
    }

    /// Route a command-palette action id through the production dispatch (W3
    /// workspace rows: `workspace-switch-<idx>`, `workspace-new`,
    /// `workspace-rename`).
    #[cfg(test)]
    pub(in crate::native) fn handle_palette_action_for_test(&mut self, id: &str) {
        self.handle_palette_action(id.to_owned());
    }

    /// Commit the in-flight rename overlay with `text` (types the string then
    /// presses Enter) so a test can assert the workspace/tab label lands.
    #[cfg(test)]
    pub(in crate::native) fn commit_rename_for_test(&mut self, text: &str) {
        use winit::keyboard::{Key as WinitKey, NamedKey};
        // Replace whatever seed the field carried.
        while self
            .rename_state
            .as_ref()
            .is_some_and(|s| !s.text.is_empty())
        {
            self.rename_key(&WinitKey::Named(NamedKey::Backspace));
        }
        for ch in text.chars() {
            self.rename_key(&WinitKey::Character(ch.to_string().into()));
        }
        self.rename_key(&WinitKey::Named(NamedKey::Enter));
    }

    #[cfg(test)]
    pub(in crate::native) fn rename_overlay_open_for_test(&self) -> bool {
        self.rename_state.is_some()
    }

    /// Test seam (LAYOUT-OPEN-MODE): drive the production `open_layout` gating —
    /// onto a pristine window it opens directly, onto real state it raises the
    /// Replace/Add/Cancel dialog.
    #[cfg(test)]
    pub(in crate::native) fn open_layout_for_test(&mut self, name: &str) {
        self.open_layout(name);
    }

    /// Test seam (LAYOUT-OPEN-MODE): whether the open-layout mode dialog is up.
    #[cfg(test)]
    pub(in crate::native) fn confirm_open_layout_open_for_test(&self) -> bool {
        self.overlay.is_confirm_open_layout()
    }

    /// Test seam (RAIL-PIN): open the right-click context menu anchored to the
    /// workspace rail slot at `idx`, exactly as a rail right-click would.
    #[cfg(test)]
    pub(in crate::native) fn open_workspace_rail_menu_for_test(&mut self, idx: usize) {
        self.open_context_menu(super::ContextMenuSurface::WorkspaceSlot(idx));
    }

    #[cfg(test)]
    pub(in crate::native) fn open_empty_tab_strip_menu_for_test(&mut self) {
        self.open_context_menu(super::ContextMenuSurface::TabStripEmpty);
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

    /// NF21-3: seed the layout-dependent UI state (a live selection, an active
    /// copy-mode caret, and a hovered-URL span) onto the session at `session`,
    /// so a test can drive `apply_grid_resize` and assert that a BACKGROUND tab
    /// gets these coordinates cleared — not only the active one.
    #[cfg(test)]
    pub(in crate::native) fn seed_layout_dependent_state_for_test(&mut self, session: usize) {
        let Some(token) = self.sessions.token_at_position(session) else {
            return;
        };
        if let Some(s) = self.sessions.get_mut(token) {
            s.selection
                .begin(crate::selection::AbsoluteCellPoint { row: 0, column: 0 });
            s.selection
                .update(crate::selection::AbsoluteCellPoint { row: 0, column: 3 });
            s.copy_mode = Some(crate::native::copy_mode::CopyModeState::new(
                crate::selection::AbsoluteCellPoint { row: 0, column: 0 },
            ));
            s.hovered_url = Some("https://example.com".to_owned());
        }
    }

    /// NF21-3 companion: `true` when every layout-dependent field seeded by
    /// [`Self::seed_layout_dependent_state_for_test`] has been cleared on the
    /// session at `session`, `None` when no such session exists.
    #[cfg(test)]
    pub(in crate::native) fn session_layout_state_is_clear_for_test(
        &self,
        session: usize,
    ) -> Option<bool> {
        let token = self.sessions.token_at_position(session)?;
        let s = self.sessions.get(token)?;
        Some(s.selection.range().is_none() && s.copy_mode.is_none() && s.hovered_url.is_none())
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

    /// Drive a named key with explicit ctrl/shift modifiers through the
    /// production key path (e.g. `Ctrl+Shift+PageDown` for workspace cycling).
    /// Restores the prior modifier state after.
    #[cfg(test)]
    pub(in crate::native) fn drive_named_key_with_mods_for_test(
        &mut self,
        key: NamedKey,
        ctrl: bool,
        shift: bool,
    ) {
        let prev = self.modifiers;
        self.modifiers = crate::input::Modifiers {
            ctrl,
            shift,
            ..crate::input::Modifiers::default()
        };
        let logical = WinitKey::Named(key);
        self.handle_key_event(
            logical.clone(),
            logical,
            PhysicalKey::Code(KeyCode::Enter),
            KeyEventType::Press,
        );
        self.modifiers = prev;
    }

    /// Drive a raw winit key identity through the production routing and
    /// PTY-write path with an explicit modifier snapshot.
    #[cfg(test)]
    pub(in crate::native) fn drive_raw_key_event_for_test(
        &mut self,
        logical: WinitKey,
        binding_key: WinitKey,
        physical: PhysicalKey,
        modifiers: crate::input::Modifiers,
        event_type: KeyEventType,
    ) {
        let previous = self.modifiers;
        self.modifiers = modifiers;
        self.handle_key_event(logical, binding_key, physical, event_type);
        self.modifiers = previous;
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

    /// Test seam (C22): drive a character chord through `handle_key_event` with
    /// an explicit `KeyEventType` (Press vs Repeat), so a test can prove a held
    /// (auto-repeating) Settings/ThemePicker chord does not repeat-toggle its
    /// overlay. Restores the prior modifier state after.
    #[cfg(test)]
    pub(in crate::native) fn drive_char_with_mods_typed_for_test(
        &mut self,
        ch: char,
        ctrl: bool,
        shift: bool,
        event_type: KeyEventType,
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
            event_type,
        );
        self.modifiers = prev;
    }

    /// Test seam (C22/C12/C5): whether any overlay is currently open — the
    /// predicate keyboard dispatch gates full-overlay key routing on.
    #[cfg(test)]
    pub(in crate::native) fn overlay_open_for_test(&self) -> bool {
        self.overlay.is_open()
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

    /// Unix-only attached-session sibling of [`Self::seed_split_pane_for_test`].
    /// It grafts the supplied socket-backed production session into the active
    /// pane tree so drag/release tests can observe real `ClientFrame::Resize`
    /// traffic rather than a mock counter.
    #[cfg(all(test, unix))]
    pub(in crate::native) fn seed_attached_split_pane_for_test(
        &mut self,
        columns: bool,
        session: crate::native::session::Session,
    ) -> usize {
        let axis = if columns {
            crate::native::layout::SplitAxis::Columns
        } else {
            crate::native::layout::SplitAxis::Rows
        };
        let token = self.sessions.split_active_for_test(axis, session);
        token.0 as usize
    }

    /// Whether a pane-divider gesture still owns a future release. Used only to
    /// assert that modal/session transitions settle rather than discard or
    /// strand the ownership latch.
    #[cfg(test)]
    pub(in crate::native) fn divider_drag_active_for_test(&self) -> bool {
        self.divider_drag.is_some()
    }

    /// Invoke the shared completion seam a second time to pin idempotence across
    /// duplicate release/focus/leave/resize ordering.
    #[cfg(test)]
    pub(in crate::native) fn finish_divider_drag_for_test(&mut self) -> bool {
        self.finish_divider_drag()
    }

    /// Drive the production cursor-leave divider boundary without a live winit
    /// window. The event arm delegates to this same seam before rail maintenance.
    #[cfg(test)]
    pub(in crate::native) fn settle_divider_for_cursor_leave_for_test(&mut self) {
        self.settle_divider_for_cursor_leave();
    }

    /// Drive the production surface-resize / scale-change divider boundary
    /// before its debounced grid reconciliation.
    #[cfg(test)]
    pub(in crate::native) fn settle_divider_for_surface_change_for_test(&mut self) {
        self.settle_divider_for_surface_change();
    }

    /// Headless variant of [`Self::seed_split_pane_for_test`]: split off a second
    /// pane backed by a [`crate::native::session::HeadlessSession`] instead of a
    /// real PTY, so a pure split-UI test creates no OS child. `dimensions` sizes
    /// the new pane's headless resize state.
    #[cfg(test)]
    pub(in crate::native) fn seed_headless_split_pane_for_test(
        &mut self,
        columns: bool,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        dimensions: crate::core::Dimensions,
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
        let headless = Arc::new(crate::native::session::HeadlessSession::new(dimensions));
        let session = crate::native::session::Session::new_headless(
            crate::native::session::SessionToken(next_id),
            terminal,
            writer,
            headless,
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

    /// Test seam (RAIL-PIN): open the workspace rename prompt on rail index
    /// `idx` and leave it open (no commit), so a test can observe the modal's
    /// effect on the auto-hide rail.
    #[cfg(test)]
    pub(in crate::native) fn enter_rename_workspace_for_test(&mut self, idx: usize) {
        self.enter_rename_workspace(idx);
    }

    /// Open each workspace-layout name prompt through its production entry
    /// point so split-render tests cover the rename state's sibling modal uses.
    #[cfg(test)]
    pub(in crate::native) fn enter_save_layout_prompt_for_test(&mut self, idx: usize) {
        self.enter_save_layout_prompt(idx);
    }

    #[cfg(test)]
    pub(in crate::native) fn enter_save_all_layout_prompt_for_test(&mut self) {
        self.enter_save_all_layout_prompt();
    }

    #[cfg(test)]
    pub(in crate::native) fn begin_rename_tab_for_test(&mut self, session: usize) -> bool {
        let Some(token) = self.sessions.token_at_position(session) else {
            return false;
        };
        self.enter_rename_tab(token);
        self.rename_state.is_some()
    }

    /// Test seam (W2): drive the shared rename field on the workspace at rail
    /// index `idx` — open it, replace its text with `new`, and commit (Enter).
    /// Returns the workspace's name afterward so a test can assert the field
    /// re-targets `Workspace.name`.
    pub(in crate::native) fn rename_workspace_via_field_for_test(
        &mut self,
        idx: usize,
        new: &str,
    ) -> Option<String> {
        self.enter_rename_workspace(idx);
        let state = self.rename_state.as_mut()?;
        state.text = new.to_owned();
        state.cursor = new.chars().count();
        self.rename_key(&winit::keyboard::Key::Named(
            winit::keyboard::NamedKey::Enter,
        ));
        self.sessions.workspace_name(idx).map(str::to_owned)
    }

    #[cfg(test)]
    pub(in crate::native) fn rename_active_for_test(&self) -> bool {
        self.rename_state.is_some()
    }

    #[cfg(test)]
    pub(in crate::native) fn modal_captures_pointer_for_test(&self) -> bool {
        self.modal_captures_pointer()
    }

    #[cfg(test)]
    pub(in crate::native) fn rename_text_for_test(&self) -> Option<String> {
        self.rename_state.as_ref().map(|state| state.text.clone())
    }

    /// F4-RENAME-MOUSE: the rename caret position (character index).
    #[cfg(test)]
    pub(in crate::native) fn rename_cursor_for_test(&self) -> Option<usize> {
        self.rename_state.as_ref().map(|state| state.cursor)
    }

    /// F4-RENAME-MOUSE: the active rename selection span `[lo, hi)`, or `None`.
    #[cfg(test)]
    pub(in crate::native) fn rename_selection_for_test(&self) -> Option<(usize, usize)> {
        let state = self.rename_state.as_ref()?;
        let anchor = state.anchor?;
        (anchor != state.cursor).then(|| (anchor.min(state.cursor), anchor.max(state.cursor)))
    }

    /// F4-RENAME-MOUSE: simulate a left press on the rename field at a grid
    /// cell, routed through the real `handle_mouse_input` dispatch so the
    /// modal-capture wiring is covered (not just the handler in isolation).
    #[cfg(test)]
    pub(in crate::native) fn rename_pointer_press_for_test(&mut self, row: usize, column: usize) {
        self.pointer_cell = Some(CellPoint { row, column });
        self.handle_mouse_input(ElementState::Pressed, WinitMouseButton::Left);
    }

    /// F4-RENAME-MOUSE: simulate pointer motion during a live rename drag,
    /// routed through the real `update_pointer_cell` dispatch. Uses cell-sized
    /// pixels so the mapped cell equals `(row, column)` on the single-pane path.
    #[cfg(test)]
    pub(in crate::native) fn rename_pointer_drag_for_test(&mut self, row: usize, column: usize) {
        self.pointer_cell = Some(CellPoint { row, column });
        if self.rename_dragging {
            self.rename_drag_extend();
        }
    }

    /// F4-RENAME-MOUSE: simulate the left-button release that ends a drag,
    /// routed through the real `handle_mouse_input` dispatch.
    #[cfg(test)]
    pub(in crate::native) fn rename_pointer_release_for_test(&mut self) {
        self.handle_mouse_input(ElementState::Released, WinitMouseButton::Left);
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

    /// Test seam (NF21-4): feed `query` to the `session`-th session's terminal
    /// and return whatever it emits to the host — used to prove a background
    /// session answers OSC 4/10/11 with the CURRENT theme after a flip.
    #[cfg(test)]
    pub(in crate::native) fn session_osc_answer_for_test(
        &self,
        session: usize,
        query: &[u8],
    ) -> Vec<u8> {
        self.sessions
            .iter()
            .nth(session)
            .and_then(|session| {
                session.terminal.lock().ok().map(|mut terminal| {
                    terminal.advance(query);
                    terminal.take_host_output()
                })
            })
            .unwrap_or_default()
    }

    /// Test seam (NF21-5): drive the production OSC 52 clipboard-request drain
    /// over every session (the per-redraw call site).
    #[cfg(test)]
    pub(in crate::native) fn drain_clipboard_requests_for_test(&mut self) {
        self.handle_terminal_clipboard_requests();
    }

    #[cfg(test)]
    pub(in crate::native) fn osc52_background_empty_replies_for_test(&self) -> usize {
        self.osc52_background_empty_replies_for_test
    }

    /// Test seam (NF21-5): the last text a clipboard write path handed to the
    /// (test-stubbed) system clipboard, and a reset so a test can distinguish a
    /// focused write (records) from a discarded non-focused write (does not).
    #[cfg(test)]
    pub(in crate::native) fn last_clipboard_write_for_test(&self) -> Option<String> {
        self.clipboard.last_clipboard_write.clone()
    }

    #[cfg(test)]
    pub(in crate::native) fn reset_last_clipboard_write_for_test(&mut self) {
        self.clipboard.last_clipboard_write = None;
    }

    #[cfg(test)]
    pub(in crate::native) fn set_osc52_write_policy_for_test(
        &mut self,
        policy: crate::settings::Osc52WritePolicy,
    ) {
        self.settings.osc52_write = policy;
    }

    #[cfg(test)]
    pub(in crate::native) fn resolve_osc52_prompt_for_test(
        &mut self,
        decision: super::osc52::PromptDecision,
    ) {
        self.resolve_osc52_prompt(decision);
    }

    #[cfg(test)]
    pub(in crate::native) fn reload_osc52_write_policy_for_test(
        &mut self,
        policy: crate::settings::Osc52WritePolicy,
    ) {
        let mut next = self.settings.clone();
        next.osc52_write = policy;
        self.apply_settings_through_reload_seam(next, SettingsApplySource::ConfigReload);
    }

    /// Enable the production OSC 52 read policy for every live session and
    /// inject hermetic clipboard text for request-drain tests.
    #[cfg(test)]
    pub(in crate::native) fn enable_osc52_read_for_test(&mut self, text: &str) {
        self.settings.osc52_read = true;
        self.clipboard.injected_clipboard_text = Some(text.to_owned());
        self.clipboard.read_text_calls = 0;
        for session in self.sessions.iter() {
            if let Ok(mut terminal) = session.terminal.lock() {
                terminal.set_osc52_read_enabled(true);
            }
        }
    }

    /// Test seam (C41): set the OS window-focus authority the OSC 52 read/write
    /// gates consult. `true` also records a confirmed focus observation (as a
    /// real `WindowEvent::Focused(true)` would) so the gate grants authority;
    /// `false` drops window focus while leaving any prior observation intact.
    #[cfg(test)]
    pub(in crate::native) fn set_window_focus_for_test(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.observe_osc52_window_focus();
        }
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

    #[cfg(all(test, unix))]
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

    /// Test seam (C5): tag the active session as attached to `session_id` so
    /// `find_attached_tab` (attach dedup) resolves it, without spinning up a real
    /// session host. Lets a headless test drive the already-attached branch of
    /// `route_attach_session`.
    #[cfg(test)]
    pub(in crate::native) fn mark_active_session_attached_for_test(&mut self, session_id: &str) {
        self.sessions.active_mut().attached_session_id = Some(session_id.to_owned());
    }

    /// Test seam (C5): drive the production `route_attach_session` dedup/attach
    /// router directly, as the AttachSession overlay outcome does.
    #[cfg(test)]
    pub(in crate::native) fn route_attach_session_for_test(&mut self, session_id: &str) {
        self.route_attach_session(session_id.to_owned());
    }

    #[cfg(test)]
    pub(in crate::native) fn search_open_for_test(&self) -> bool {
        self.search.is_open()
    }

    /// Test seam (C9): the live search-box query string, so a test can prove an
    /// IME commit routes into the search field instead of leaking to the PTY.
    #[cfg(test)]
    pub(in crate::native) fn search_query_for_test(&self) -> String {
        self.search.render_signature().query
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

    /// Test seam (NF21-8): the current grid selection held-button flag. Lets a
    /// regression prove the motion-path guard refuses to extend once release (or
    /// an active-session change) has dropped the flag.
    #[cfg(test)]
    pub(in crate::native) fn grid_left_held_for_test(&self) -> bool {
        self.grid_left_held
    }

    /// Test seam (NF21-8): drive the production single-pane grid `CursorMoved`
    /// path so a regression can assert whether a bare move extends the selection
    /// (button held) or is refused (latch stale after a lost release / switch).
    #[cfg(test)]
    pub(in crate::native) fn grid_pointer_moved_for_test(&mut self, x_px: f64, y_px: f64) {
        self.update_pointer_cell(x_px, y_px);
    }

    /// Test seam (NF21-8/9): switch to the next tab through the production path,
    /// firing the active-session-change seam under test.
    #[cfg(test)]
    pub(in crate::native) fn switch_to_next_tab_for_test(&mut self) {
        self.switch_to_next_tab();
    }

    /// Test seam (NF21-8): drive the production focus-change handler so a
    /// regression can prove focus loss drops the grid held-button flag.
    #[cfg(test)]
    pub(in crate::native) fn on_window_focus_changed_for_test(&mut self, focused: bool) {
        self.on_window_focus_changed(focused);
    }

    /// Test seam (NF21-11): the current App-level IME preedit string.
    #[cfg(test)]
    pub(in crate::native) fn ime_preedit_for_test(&self) -> &str {
        &self.ime_preedit
    }

    /// Test seam (NF21-11): seed an in-flight IME preedit so a regression can
    /// prove an active-session change drops it (no cross-surface commit/paint).
    #[cfg(test)]
    pub(in crate::native) fn set_ime_preedit_for_test(&mut self, text: &str) {
        self.ime_preedit = text.to_owned();
    }

    /// Test seam (F1): drain and clear the argv vectors that
    /// [`App::handle_new_window`] recorded instead of actually spawning a second
    /// OdyTTY instance. Lets a chord/menu dispatch test assert a New Window
    /// request reached the spawn boundary without launching a real process.
    #[cfg(test)]
    pub(in crate::native) fn drain_new_window_spawns_for_test(&self) -> Vec<Vec<String>> {
        NEW_WINDOW_SPAWN_ARGV.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
    }

    /// Test seam (F1): the argv the App would spawn for a new window. Exposes the
    /// pure `new_window_argv` builder (with the optional cwd propagation) so a
    /// test can assert the argv shape — bare exe, or exe +
    /// `--working-directory <cwd>` — without driving a full dispatch.
    #[cfg(test)]
    pub(in crate::native) fn new_window_argv_for_test(cwd: Option<&str>) -> Option<Vec<String>> {
        Self::new_window_argv(cwd)
    }

    // --- F6-i7 image paste-through seams ---

    /// Seed the synthetic clipboard image the image paste-through flow reads,
    /// bypassing the real system clipboard.
    #[cfg(test)]
    pub(in crate::native) fn set_clipboard_image_for_test(&mut self, png: Option<Vec<u8>>) {
        self.clipboard.injected_clipboard_image = png;
    }

    /// Mark the active session as a remote *integrated* upload target (as the
    /// connect path does), so image paste-through engages for it.
    #[cfg(test)]
    pub(in crate::native) fn set_active_remote_upload_for_test(&mut self, destination: &str) {
        self.sessions.set_active_upload_for_test(destination);
    }

    /// Force the `remote_image_paste` setting to a given enabled/disabled state.
    #[cfg(test)]
    pub(in crate::native) fn set_remote_image_paste_enabled_for_test(&mut self, enabled: bool) {
        self.settings.remote_image_paste = if enabled {
            crate::settings::RemoteImagePaste::Ask
        } else {
            crate::settings::RemoteImagePaste::Off
        };
    }

    /// Drive the paste shortcut (the same entry the Paste keybind hits).
    #[cfg(test)]
    pub(in crate::native) fn handle_paste_shortcut_for_test(&mut self) {
        self.handle_paste_shortcut();
    }

    /// Whether an image paste is currently awaiting the confirm prompt.
    #[cfg(test)]
    pub(in crate::native) fn image_paste_pending_for_test(&self) -> bool {
        self.pending_image_paste.is_some()
    }

    /// Enable DEC focus reporting in the active terminal and drain the routed
    /// session-transition observations recorded by the test build.
    #[cfg(test)]
    pub(in crate::native) fn enable_focus_reporting_for_test(&mut self) {
        crate::native::lock_recover(&self.terminal).advance(b"\x1b[?1004h");
    }

    #[cfg(test)]
    pub(in crate::native) fn take_focus_reports_for_test(&mut self) -> Vec<(SessionToken, bool)> {
        std::mem::take(&mut self.focus_reports_for_test)
    }

    /// Confirm the pending image paste (Enter), returning what the upload worker
    /// would have shipped (session id, PNG byte length) — recorded instead of a
    /// real `ssh` under `cfg(test)`.
    #[cfg(test)]
    pub(in crate::native) fn confirm_image_paste_for_test(
        &mut self,
    ) -> Option<(SessionToken, usize)> {
        self.commit_image_paste();
        self.last_image_upload.take()
    }

    /// Cancel the pending image paste (Esc).
    #[cfg(test)]
    pub(in crate::native) fn cancel_image_paste_for_test(&mut self) {
        self.cancel_image_paste();
    }
}

#[path = "test_seams/notifications.rs"]
mod notifications;
