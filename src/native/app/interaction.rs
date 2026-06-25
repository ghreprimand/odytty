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
        self.apply_overlay_outcome_with_policy(outcome, false);
    }

    pub(super) fn apply_overlay_outcome_with_policy(
        &mut self,
        outcome: OverlayOutcome,
        coalesce_apply: bool,
    ) {
        match outcome {
            OverlayOutcome::Consumed => {}
            OverlayOutcome::Close => {
                self.flush_pending_overlay_settings();
                self.overlay.close();
            }
            // First-run onboarding dismissal: persist a marker so the welcome
            // card does not reshow next launch (best-effort; a write failure
            // must never block dismissal), then close like any other overlay.
            OverlayOutcome::CloseOnboarding => {
                self.flush_pending_overlay_settings();
                self.persist_first_run_config();
                self.overlay.close();
            }
            OverlayOutcome::OpenThemePicker => {
                self.flush_pending_overlay_settings();
                self.open_theme_picker_overlay();
            }
            OverlayOutcome::OpenThemeBuilder => {
                self.flush_pending_overlay_settings();
                self.open_theme_builder_overlay();
            }
            OverlayOutcome::OpenKeyBindings => {
                self.flush_pending_overlay_settings();
                self.open_key_bindings_overlay();
            }
            OverlayOutcome::OpenFontPicker => {
                self.flush_pending_overlay_settings();
                self.open_font_picker_overlay();
            }
            OverlayOutcome::ApplySettings(settings) => {
                if coalesce_apply {
                    self.queue_overlay_settings(*settings);
                } else {
                    self.pending_overlay_settings = None;
                    self.apply_overlay_settings(*settings);
                }
            }
            OverlayOutcome::SaveSettings(changes) => self.save_overlay_settings(&changes),
            OverlayOutcome::SaveTheme(request) => {
                self.flush_pending_overlay_settings();
                self.save_overlay_theme(request);
            }
            // IN2: the menu closed itself before emitting these; run the action.
            OverlayOutcome::ContextMenuCopy => {
                self.flush_pending_overlay_settings();
                self.handle_copy_shortcut();
            }
            OverlayOutcome::ContextMenuCut => {
                self.flush_pending_overlay_settings();
                self.handle_context_menu_cut();
            }
            OverlayOutcome::ContextMenuPaste => {
                self.flush_pending_overlay_settings();
                self.handle_paste_shortcut();
            }
            OverlayOutcome::ContextMenuDelete => {
                self.flush_pending_overlay_settings();
                self.handle_context_menu_delete();
            }
            OverlayOutcome::ContextMenuSelectAll => {
                self.flush_pending_overlay_settings();
                self.handle_select_all();
            }
            OverlayOutcome::ContextMenuNewTab => {
                self.flush_pending_overlay_settings();
                self.handle_new_tab();
            }
            OverlayOutcome::ContextMenuRenameTab(target) => {
                self.flush_pending_overlay_settings();
                self.enter_rename_tab(target);
            }
            OverlayOutcome::ContextMenuCloseTab => {
                self.flush_pending_overlay_settings();
                let _ = self.close_active_tab();
            }
            // Part B: the context menu closed itself; split the focused pane
            // through the exact same action the keyboard split chords fire
            // (`apply_pane_action` → `split_active_pane`).
            OverlayOutcome::ContextMenuSplitColumns => {
                self.flush_pending_overlay_settings();
                self.apply_pane_action(crate::settings::BindableAction::SplitColumns);
            }
            OverlayOutcome::ContextMenuSplitRows => {
                self.flush_pending_overlay_settings();
                self.apply_pane_action(crate::settings::BindableAction::SplitRows);
            }
            // The context menu closed itself; close the focused pane through the
            // same action the tmux `Ctrl-b x` prefix / palette `close-pane` fire.
            // Only emitted in a multi-pane tab (the item is hidden single-pane).
            OverlayOutcome::ContextMenuClosePane => {
                self.flush_pending_overlay_settings();
                self.apply_pane_action(crate::settings::BindableAction::ClosePane);
            }
            // D-IN2-SETTINGS: the context menu closed itself; open the settings
            // panel through the existing toggle path (same destination as
            // Ctrl+Shift+,). No extra state: the toggle path handles open/close.
            OverlayOutcome::ContextMenuSettings => {
                self.toggle_settings_overlay();
            }
            // v0.3.1 launcher section: the context menu closed itself; open each
            // overlay through the same entry the discoverability chords fire.
            OverlayOutcome::ContextMenuConnectionManager => {
                self.flush_pending_overlay_settings();
                self.open_connection_overlay();
            }
            OverlayOutcome::ContextMenuCommandPalette => {
                self.flush_pending_overlay_settings();
                self.open_command_palette_overlay();
            }
            OverlayOutcome::ContextMenuSessionReplay => {
                self.flush_pending_overlay_settings();
                self.open_replay_overlay();
            }
            OverlayOutcome::ContextMenuSessionAttach => {
                self.flush_pending_overlay_settings();
                self.open_session_attach_overlay();
            }
            // C3 file section: the menu closed itself before emitting these.
            // Open dispatches through the same argv-only path the Ctrl+click
            // open uses; copy items write text to the clipboard; reveal opens
            // the parent directory. All best-effort — a spawn/clipboard failure
            // never panics the UI.
            OverlayOutcome::ContextMenuOpenPath(resolved) => {
                self.flush_pending_overlay_settings();
                let argv = self.path_open_argv_for(&resolved);
                self.spawn_open_or_notice(&argv);
            }
            // C4: open the resolved image span in the in-terminal viewer.
            OverlayOutcome::ContextMenuOpenInOdytty(resolved) => {
                self.flush_pending_overlay_settings();
                self.open_image_view(&resolved);
            }
            // C3b: enumerate the apps that can open the resolved file and open
            // the "Open With…" picker overlay. Enumeration is read-only; a file
            // with no handlers opens the overlay with its empty-state hint.
            OverlayOutcome::ContextMenuOpenWith(resolved) => {
                self.flush_pending_overlay_settings();
                self.open_open_with_overlay(&resolved);
            }
            // C3b: launch the app chosen in the picker. The overlay closed
            // itself before emitting this; the argv was built argv-only by
            // `exec_to_argv` (path already one inert element). A spawn failure
            // never panics the UI — it surfaces a transient notice (P0-2).
            OverlayOutcome::OpenWithApp(argv) => {
                self.flush_pending_overlay_settings();
                self.spawn_open_or_notice(&argv);
            }
            OverlayOutcome::ContextMenuCopyPath(abs) => {
                self.flush_pending_overlay_settings();
                let _ = self.clipboard.write_text(&abs);
            }
            OverlayOutcome::ContextMenuCopyFile(uri) => {
                self.flush_pending_overlay_settings();
                let _ = self.clipboard.write_text(&uri);
            }
            OverlayOutcome::ContextMenuRevealPath(resolved) => {
                self.flush_pending_overlay_settings();
                let argv = super::platform_opener::reveal_argv(
                    super::platform_opener::OpenerOs::host(),
                    &resolved,
                );
                self.spawn_open_or_notice(&argv);
            }
            OverlayOutcome::PaletteTypeText(text) => {
                self.flush_pending_overlay_settings();
                self.handle_palette_type_text(text);
            }
            OverlayOutcome::PaletteAction(id) => {
                self.flush_pending_overlay_settings();
                self.handle_palette_action(id);
            }
            // Phase 4: the connection-manager overlay closed itself before
            // emitting this; spawn the chosen host through the connect action
            // (system `ssh`, name-only argv). A spawn failure must never panic
            // the UI — surface nothing for now beyond the dropped result; the
            // overlay is already closed and the user can retry.
            OverlayOutcome::Connect(host) => {
                self.flush_pending_overlay_settings();
                let _ = self.connect_ssh_host_in_new_tab(&host);
            }
            // Phase 5 / B2: the session-attach overlay closed itself before
            // emitting this; attach the chosen session into a new tab. A stale
            // id (the session ended between list and accept) returns Err from
            // the attach path; swallow it like the connect arm — the overlay is
            // already closed and the user can retry. Never panics.
            OverlayOutcome::AttachSession(id) => {
                self.flush_pending_overlay_settings();
                let _ = self.attach_session_in_new_tab(None, &id);
            }
            // CLOSE-CONFIRM: the dialog closed itself before emitting this; flag
            // the exit so `window_event` exits the loop on this same turn (the
            // outcome cannot reach `ActiveEventLoop` from here — `&mut self`).
            OverlayOutcome::ForceClose => {
                self.flush_pending_overlay_settings();
                self.pending_exit = true;
            }
        }
    }

    /// Translate a winit mouse button edge over an open overlay into an
    /// [`OverlayPointer::Press`]/`Release` and apply the outcome (UX4-P1/P2).
    /// Press drives clicks and may arm an overlay drag in modes that still
    /// capture motion (theme builder). Release ends any such drag. Middle/other
    /// buttons are dropped so no PRIMARY paste fires while the overlay is up and
    /// so a stray middle release cannot disturb a drag.
    pub(in crate::native) fn handle_overlay_pointer_button(
        &mut self,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        let pointer_button = match button {
            WinitMouseButton::Left => PointerButton::Left,
            WinitMouseButton::Right => PointerButton::Right,
            _ => return,
        };
        // SLIDER-GUARD (D-SLIDER-GUARD): track whether the left button is held so
        // `handle_overlay_pointer_move` can gate drag updates in modes that
        // still capture motion. Clear BEFORE Release is processed so the
        // Release handler never sees a stale held flag, and set AFTER Press
        // lands so the flag reflects an active drag.
        if button == WinitMouseButton::Left {
            match state {
                ElementState::Released => {
                    self.overlay_left_held = false;
                    self.overlay.cancel_settings_drag();
                }
                ElementState::Pressed => {} // set below, after overlay confirms a drag
            }
        }
        // Window-level overlays use window-overlay cell space, not the focused
        // pane's sub-grid. In a single-pane tab these are exactly
        // `self.pointer_cell` / `self.grid`, so the single-pane path is
        // unchanged; in a multi-pane tab they map to the whole content grid so
        // clicks land on the panel that renders there.
        let Some(cell) = self.overlay_pointer_cell() else {
            if state == ElementState::Released {
                self.flush_pending_overlay_settings();
                self.request_selection_redraw();
            }
            return;
        };
        let (win_cols, win_rows) = self.overlay_grid_dims();
        let Some(rect) = overlay_rect(&self.overlay, win_cols, win_rows) else {
            if state == ElementState::Released {
                self.flush_pending_overlay_settings();
                self.request_selection_redraw();
            }
            return;
        };
        let x_in_body = self.pointer_x_in_body(&rect);
        let pointer = match state {
            ElementState::Pressed => OverlayPointer::Press {
                cell,
                button: pointer_button,
                x_in_body,
            },
            ElementState::Released => OverlayPointer::Release {
                cell,
                button: pointer_button,
            },
        };
        let outcome = self.overlay.handle_pointer(pointer, rect);
        // After a left press, arm the held flag only if the overlay confirms a
        // real drag. Settings sliders are click-to-set and leave this false.
        if button == WinitMouseButton::Left && state == ElementState::Pressed {
            self.overlay_left_held = self.overlay.is_settings_dragging();
        }
        let coalesce_apply = state == ElementState::Pressed && self.overlay.is_settings_dragging();
        self.apply_overlay_outcome_with_policy(outcome, coalesce_apply);
        if state == ElementState::Released {
            self.flush_pending_overlay_settings();
        }
        self.request_selection_redraw();
    }

    /// Drive an in-progress overlay drag from the cached pointer cell (UX4-P2).
    /// Gated on an active drag AND the left-button-held flag so cursor movements
    /// after the button is released can never advance an armed drag
    /// (D-SLIDER-GUARD). Ordinary hover over the open overlay stays a cheap
    /// no-op (no redraw, no PTY/selection work).
    pub(in crate::native) fn handle_overlay_pointer_move(&mut self) {
        // A bare hover is forwarded only to advance an active overlay drag
        // (UX4-P2, only when the left button IS held — D-SLIDER-GUARD) or to
        // drive context-menu hover-to-focus (IN2); otherwise it is a cheap no-op.
        let should_route = if self.overlay.is_settings_dragging() {
            // Slider move: require the left button to be held. If the drag state
            // is somehow stale (lost Release event), cancel it and return.
            if !self.overlay_left_held {
                self.overlay.cancel_settings_drag();
                return;
            }
            true
        } else {
            self.overlay.is_context_menu()
        };
        if !should_route {
            return;
        }
        // Window-space overlay geometry (see `handle_overlay_pointer_button`):
        // identical to `self.pointer_cell` / `self.grid` in a single-pane tab,
        // mapped to the content grid in a multi-pane tab.
        let Some(cell) = self.overlay_pointer_cell() else {
            return;
        };
        let (win_cols, win_rows) = self.overlay_grid_dims();
        let Some(rect) = overlay_rect(&self.overlay, win_cols, win_rows) else {
            return;
        };
        let x_in_body = self.pointer_x_in_body(&rect);
        let outcome = self
            .overlay
            .handle_pointer(OverlayPointer::Move { cell, x_in_body }, rect);
        let coalesce_apply = self.overlay.is_settings_dragging();
        self.apply_overlay_outcome_with_policy(outcome, coalesce_apply);
        self.request_selection_redraw();
    }

    /// Translate a winit wheel event over an open overlay into an
    /// [`OverlayPointer::Wheel`] free-scroll of the panel list (UX4-P1).
    pub(in crate::native) fn handle_overlay_pointer_wheel(&mut self, delta: MouseScrollDelta) {
        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        // WHEEL-SENS (T-overlay): coalesce the high-resolution burst so the
        // settings list advances one entry per physical notch instead of flying.
        // The overlay deliberately uses the fixed default step (the user's
        // `scroll_wheel_lines` multiplier is a terminal-scroll knob), but it
        // still benefits from notch-coalescing.
        let Some(notch) = self.wheel_accum.coalesce_scroll(delta, cell_height) else {
            return;
        };
        let lines = wheel_lines(notch, cell_height);
        if lines == 0 {
            return;
        }
        // Window-space overlay dims (identical to `self.grid` single-pane).
        let (win_cols, win_rows) = self.overlay_grid_dims();
        let Some(rect) = overlay_rect(&self.overlay, win_cols, win_rows) else {
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

    /// Compute the fractional body-relative x coordinate from the cached
    /// physical pixel position. Returns `None` when pixel data or GPU cell info
    /// is unavailable (tests, headless mode).
    ///
    /// The value is body-relative: 0.0 = left edge of the first body cell,
    /// 1.0 = right edge of the first body cell, etc. Non-integer values give
    /// sub-cell precision for smooth slider tracking.
    fn pointer_x_in_body(&self, rect: &crate::native::overlay::OverlayRect) -> Option<f32> {
        let (x_px, _) = self.pointer_px?;
        let cell = self.gpu.as_ref().map(GpuState::cell)?;
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let body_left_px = rect.body_left as f32 * cell.width as f32 + padding.as_f32();
        Some((x_px as f32 - body_left_px) / cell.width.max(1) as f32)
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

    /// INTERACTIVE-PATHS (Phase 7): recompute the resolved path span under the
    /// pointer and update the hover state that drives the pointer (hand) cursor.
    ///
    /// **The byte-identity gate.** The very first thing this does is check the
    /// `interactive_paths` setting; when it is off (the default) it returns
    /// before any terminal lock, row build, `detect_paths` scan, or stat probe —
    /// so the default hover path never scans and produces byte-identical frames.
    /// When on, it dedupes exactly like [`Self::update_hover_hyperlink`]: the
    /// rebuild flag/redraw fire only when the resolved span actually changes.
    fn update_hover_path(&mut self) {
        if !self.settings.interactive_paths {
            // Clear a stale span if the setting was toggled off live while one
            // was hovered; otherwise nothing to do — the scanner never runs.
            if self.hovered_path.is_some() {
                self.hovered_path = None;
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            return;
        }
        let resolved = self.resolved_hovered_path();
        if self.hovered_path != resolved {
            self.hovered_path = resolved;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Resolve the path span (if any) under the current pointer cell against the
    /// pane's OSC 7 working directory and `$HOME`, stat-gated through the active
    /// [`crate::paths::ResolveProbe`]. Pure aside from the single probe call;
    /// `None` when no live filesystem path sits under the pointer.
    pub(super) fn resolved_hovered_path(&self) -> Option<crate::paths::Resolved> {
        let point = self.pointer_cell?;
        let (line, column, cwd) = self.hovered_row_text_and_cwd(point)?;
        // Map the pointer's cell column to a byte offset in the row string, then
        // find the detected span covering that offset. Paths are ASCII/narrow,
        // so one char per cell column keeps the column and char indices aligned.
        let target = line.char_indices().nth(column).map(|(byte, _)| byte)?;
        let span = crate::paths::detect_paths_with_options(
            &line,
            crate::paths::DetectionOptions {
                barewords: self.settings.interactive_paths_barewords,
            },
        )
        .into_iter()
        .find(|span| target >= span.start && target < span.end)?;
        self.classify_hovered_path(&span, cwd.as_deref(), self.home_dir.as_deref())
    }

    /// Single-lock fetch of the row text under `point` plus the pane's OSC 7
    /// working directory. Mirrors [`Self::visible_cell_hyperlink`]'s one-lock
    /// structure: the row string and the cwd both come from the same `terminal`
    /// lock. The row is built one char per cell column so a column index maps to
    /// a char index.
    fn hovered_row_text_and_cwd(
        &self,
        point: CellPoint,
    ) -> Option<(String, usize, Option<String>)> {
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        let cols = snapshot.dimensions.columns;
        if point.row >= snapshot.dimensions.rows {
            return None;
        }
        let start = point.row * cols;
        let row = snapshot.cells.get(start..start + cols)?;
        let line: String = row.iter().map(|cell| cell.ch).collect();
        let cwd = terminal.current_working_directory().map(str::to_owned);
        Some((line, point.column, cwd))
    }

    /// Stat-gate a candidate span through the production probe. Split on
    /// `cfg(test)` so headless hover tests resolve against an injected synthetic
    /// fs map (`test_path_probe`) and never touch the real filesystem, while
    /// production wires the real `std::fs::symlink_metadata` probe.
    #[cfg(not(test))]
    fn classify_hovered_path(
        &self,
        span: &crate::paths::PathSpan,
        cwd: Option<&str>,
        home: Option<&str>,
    ) -> Option<crate::paths::Resolved> {
        crate::paths::resolve(span, cwd, home, &super::interactive_paths::FsResolveProbe)
    }

    #[cfg(test)]
    fn classify_hovered_path(
        &self,
        span: &crate::paths::PathSpan,
        cwd: Option<&str>,
        home: Option<&str>,
    ) -> Option<crate::paths::Resolved> {
        crate::paths::resolve(span, cwd, home, &self.test_path_probe)
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
        // argv passing to the platform default opener. No shell interpolation is
        // involved. Routed through the single argv-only spawn point shared with
        // path opens; a failed/missing opener surfaces a transient notice (P0-2).
        let argv = super::platform_opener::open_default_argv(
            super::platform_opener::OpenerOs::host(),
            &uri,
        );
        self.spawn_open_or_notice(&argv);
        true
    }

    /// INTERACTIVE-PATHS (Phase 8 / C3): Ctrl+click open for a resolved path
    /// span under the pointer. Chained in the pointer Pressed arm AFTER
    /// [`Self::try_open_hovered_hyperlink`] (OSC 8 wins ties) and BEFORE
    /// `begin_selection`, so when this returns `false` the selection path is
    /// byte-identical.
    ///
    /// Returns `false` immediately — opening nothing, starting no selection
    /// change — when the feature is off, the Ctrl+click gate is not satisfied,
    /// or no live path span sits under the pointer. The gate reused is exactly
    /// the hyperlink one ([`hyperlink_action_allowed`]): Ctrl required,
    /// suppressed under mouse reporting unless Shift overrides. The open itself
    /// is an argv-only [`super::interactive_paths::spawn_detached`] of the
    /// dispatch vector ([`super::interactive_paths::path_open_argv`]) — never a
    /// shell string.
    pub(super) fn try_open_hovered_path(&mut self) -> bool {
        if !self.settings.interactive_paths {
            return false;
        }
        if !hyperlink_action_allowed(self.modifiers, self.mouse_reporting_enabled()) {
            return false;
        }
        let Some(resolved) = self.hovered_path.clone() else {
            return false;
        };
        let argv = self.path_open_argv_for(&resolved);
        self.spawn_open_or_notice(&argv);
        true
    }

    /// Build the argv vector to open a resolved path, threading the configured
    /// editor override (`interactive_paths_editor`) and the `$EDITOR`/`$VISUAL`
    /// environment (read at open time). Pure aside from the env read; the spawn
    /// is the caller's separate step. Shared by the Ctrl+click path and the
    /// context-menu Open item so both dispatch identically.
    pub(super) fn path_open_argv_for(&self, resolved: &crate::paths::Resolved) -> Vec<String> {
        let editor_env = std::env::var("EDITOR")
            .ok()
            .or_else(|| std::env::var("VISUAL").ok());
        super::interactive_paths::path_open_argv(
            resolved,
            &self.settings.interactive_paths_editor,
            editor_env.as_deref(),
            super::platform_opener::OpenerOs::host(),
        )
    }

    /// Open a resolved image span in the in-terminal viewer (Phase 9 / C4).
    /// Decodes the file through the single bounded decode point
    /// ([`super::image_decode::decode_image_rgba`], FLAG B), uploads the pixels
    /// to the GPU image layer, and opens the `ImageView` overlay. A decode that
    /// fails or is refused by the decode bound is a graceful no-op — the menu
    /// item only ever appears on an extension match, so a corrupt/oversized file
    /// simply does not open rather than erroring. Presentation-only.
    pub(super) fn open_image_view(&mut self, resolved: &crate::paths::Resolved) {
        // Only files are images; a directory span never reaches here, but guard
        // anyway so the decode is never attempted on a non-file.
        if resolved.kind != crate::paths::FsKind::File {
            return;
        }
        let Some((rgba, width, height)) =
            crate::native::image_decode::decode_image_rgba(std::path::Path::new(&resolved.abs))
        else {
            return;
        };
        // Hand the pixels to the GPU overlay slot (centered fit computed there),
        // then open the presentation-only overlay with the filename caption.
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_overlay_image(Some((rgba.as_slice(), width, height)));
        }
        let caption = resolved
            .abs
            .rsplit('/')
            .next()
            .unwrap_or(resolved.abs.as_str())
            .to_owned();
        self.overlay.open_image_view(caption);
        self.image_overlay = Some(super::interactive_paths::ImageOverlayState {
            rgba,
            width,
            height,
        });
        self.needs_rebuild = true;
    }

    /// Keep the GPU image-viewer overlay in lockstep with the overlay state
    /// (C4). Called once per frame before drawing: when the `ImageView` overlay
    /// is no longer open (dismissed via Esc, click-outside, or any mode switch),
    /// clear the decoded buffer and the GPU overlay texture so the very next
    /// frame is byte-identical to the no-viewer path. Cheap no-op while the
    /// viewer stays open or was never opened.
    pub(super) fn sync_image_overlay(&mut self) {
        if self.image_overlay.is_some() && !self.overlay.image_view_open() {
            self.image_overlay = None;
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.set_overlay_image(None);
            }
        }
    }

    /// Re-push the current image-viewer overlay image after a surface resize so
    /// its centered fit-rect is recomputed for the new dimensions (C4). No-op
    /// when the viewer is closed.
    pub(super) fn refresh_image_overlay_on_resize(&mut self) {
        if let Some(state) = self.image_overlay.as_ref() {
            let rgba = state.rgba.clone();
            let (width, height) = (state.width, state.height);
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.set_overlay_image(Some((rgba.as_slice(), width, height)));
            }
        }
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
        // WHEEL-SENS (T-overlay decision, TUI arm): coalesce the burst so a
        // high-resolution scroll emits one wheel report per physical notch
        // rather than one per sub-notch event (which would fly a TUI pager). The
        // report protocol carries only a discrete up/down button — sign, not
        // magnitude — so we emit a single report per accumulated notch and
        // deliberately do NOT apply the user's `scroll_wheel_lines` multiplier
        // (the app owns its own line count). A clean `LineDelta(_, ±1.0)` still
        // yields exactly one report, byte-identical to before.
        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        let Some(notch) = self.wheel_accum.coalesce_scroll(delta, cell_height) else {
            return false;
        };
        let Some(button) = wheel_report_button(notch) else {
            return false;
        };
        self.send_mouse_report(button, MouseEventKind::Press)
    }

    /// Push a mouse-cursor shape to the window, but only when it actually
    /// changes — winit issues a platform request on every `set_cursor` call, so
    /// the dedupe keeps `CursorMoved` (which fires on every pixel of motion)
    /// from spamming the windowing system. The terminal grid shows an I-beam
    /// (`Text`), a hovered hyperlink shows a hand (`Pointer`), and window chrome
    /// (tab bar, open overlay) plus mouse-reporting TUIs show the arrow
    /// (`Default`). Before this, OdyTTY never called `set_cursor` at all, so the
    /// pointer stayed the OS default arrow everywhere.
    pub(super) fn apply_cursor_icon(&mut self, icon: CursorIcon) {
        if self.cursor_icon == icon {
            return;
        }
        self.cursor_icon = icon;
        if let Some(window) = self.window.as_ref() {
            window.set_cursor(icon);
        }
    }

    /// The resize cursor for a divider of the given split axis: a column split
    /// (panes side-by-side, vertical divider) drags horizontally → `ColResize`
    /// (`↔`); a row split (panes stacked, horizontal divider) drags vertically →
    /// `RowResize` (`↕`). Pure mapping, shared by the hover and active-drag
    /// cursor paths so both agree.
    pub(super) fn divider_resize_icon(axis: SplitAxis) -> CursorIcon {
        match axis {
            SplitAxis::Columns => CursorIcon::ColResize,
            SplitAxis::Rows => CursorIcon::RowResize,
        }
    }

    pub(super) fn update_pointer_cell(&mut self, x_px: f64, y_px: f64) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        self.pointer_px = Some((x_px, y_px));
        // While a divider is grabbed, pointer motion reflows the split and
        // nothing else — no selection or hover work. `divider_drag` is only ever
        // `Some` in a multi-pane tab, so the single-pane motion path below is
        // byte-identical. Keep the matching resize cursor (`↔`/`↕`) for the
        // dragged divider's axis even as the pointer strays off the hairline, so
        // the affordance is stable through the whole gesture; fall back to the
        // arrow only if the divider can't be resolved.
        if let Some(idx) = self.divider_drag {
            self.drag_divider_to_pointer();
            let icon = self
                .multipane_geometry()
                .and_then(|(content, _)| {
                    self.sessions
                        .active_divider_axis(content, PANE_DIVIDER_PX, idx)
                })
                .map(Self::divider_resize_icon)
                .unwrap_or(CursorIcon::Default);
            self.apply_cursor_icon(icon);
            return;
        }
        let tab_bar_hit = if self.should_show_tab_bar() {
            let hit = self.tab_bar.hit_test(
                x_px,
                y_px,
                &self.sessions,
                self.tab_bar_grid_cols(),
                padding.as_f32(),
                cell,
                padding,
            );
            let hover = (hit != TabHit::None).then_some(hit);
            if self.tab_bar.hover != hover {
                self.tab_bar.set_hover(hover);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            hover
        } else {
            self.tab_bar.set_hover(None);
            None
        };
        if tab_bar_hit.is_some() {
            let x = (x_px as f32 - padding.as_f32()).max(0.0);
            let col = (x / cell.width as f32) as usize;
            // Clamp to the *window* content columns the strip is laid out across
            // (byte-identical to `self.grid.columns` single-pane), not the
            // focused pane's narrower sub-grid in a multi-pane tab.
            self.pointer_cell = Some(CellPoint {
                row: 0,
                column: col.min(self.tab_bar_grid_cols().saturating_sub(1)),
            });
            self.apply_cursor_icon(CursorIcon::Default);
            return;
        }
        let y_px = if self.should_show_tab_bar() {
            y_px - f64::from(self.tab_bar_height_px(cell))
        } else {
            y_px
        };
        let point = selection::cell_at_physical_with_padding(x_px, y_px, cell, self.grid, padding);
        self.pointer_cell = Some(point);
        // UX4-P1/P2: while an overlay is open it owns the pointer. Keep caching
        // the coordinates above (a press needs them), but skip link hover, local
        // selection, and PTY motion reports — they belong to the terminal grid
        // beneath the panel. A move is forwarded to the overlay only to advance
        // an active slider drag (UX4-P2); non-drag hover is a no-op.
        if self.overlay.is_open() {
            self.apply_cursor_icon(CursorIcon::Default);
            self.handle_overlay_pointer_move();
            return;
        }
        // MOUSE-SCROLLBAR: a scroll-thumb drag owns the pointer move — scrub the
        // viewport to the offset the thumb-top maps to and stop. Placed before
        // hover/selection/PTY-report so a scrollbar drag does not update link
        // hover, extend a selection, or emit PTY motion. Mutually exclusive with
        // selection (one `pointer_drag` enum); the grab decision already ran at
        // press time.
        if let Some(grab_dy) = self.pointer_drag.scrollbar_grab() {
            self.apply_cursor_icon(CursorIcon::Default);
            self.drag_scrollbar_to(y_px, grab_dy, cell, padding);
            return;
        }
        // Divider hover (multi-pane): show a resize cursor over a divider grab
        // zone so drag-to-resize is discoverable (the press path already grabs
        // the same band). Absolute pointer coords (`self.pointer_px`) match the
        // press-time hit-test basis — `content.y` already includes the tab-bar
        // offset, unlike the tab-bar-relative `y_px` shadowed above. Skipped
        // while a text selection is in progress (the gesture owns the pointer)
        // and never reached on a single-pane tab (`multipane_geometry` is
        // `None`), so the byte-identical path never shows a resize cursor.
        if !self.pointer_drag.is_selecting()
            && let Some((content, _)) = self.multipane_geometry()
            && let Some((px, py)) = self.pointer_px
            && let Some(axis) = self.sessions.active_divider_axis_at_point(
                content,
                PANE_DIVIDER_PX,
                px as f32,
                py as f32,
                DIVIDER_GRAB_PX,
            )
        {
            self.apply_cursor_icon(Self::divider_resize_icon(axis));
            return;
        }
        self.update_hover_hyperlink();
        // INTERACTIVE-PATHS (Phase 7): recompute the hovered path span. Gated on
        // the `interactive_paths` setting inside `update_hover_path`, so with the
        // feature off (the default) it returns before scanning and this call is a
        // single bool test — the hover path stays byte-identical.
        self.update_hover_path();
        // Cursor shape over the terminal grid: a hand on a hovered hyperlink OR a
        // resolved interactive path, the arrow while a TUI has mouse reporting
        // enabled (it owns clicks, so an I-beam would mislead), and the I-beam
        // over plain selectable text — the standard terminal affordance OdyTTY
        // previously never set. `hovered_path` is permanently `None` while the
        // feature is off, so the default decision is unchanged. OSC 8 wins ties
        // (cosmetically identical icon; the precedence matters for C3 click).
        let grid_icon = if self.hovered_hyperlink.is_some() || self.hovered_path.is_some() {
            CursorIcon::Pointer
        } else if self.mouse_reporting_enabled() {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        };
        self.apply_cursor_icon(grid_icon);
        if self.pointer_drag.is_selecting() {
            self.autoscroll_selection_if_needed(y_px, cell, padding);
            self.extend_drag_to(point);
            self.request_selection_redraw();
        } else if self.should_report_mouse_to_pty() || self.report_button.is_some() {
            self.send_mouse_motion_report();
        }
    }

    /// Scrub the viewport to the scrollback offset the dragged scroll thumb maps
    /// to (MOUSE-SCROLLBAR). `grab_dy` anchors the cursor to the grab point on
    /// the thumb. Locks the terminal once for the scrollback length and reuses
    /// it for both the geometry and the clamped jump.
    fn drag_scrollbar_to(
        &mut self,
        y_px: f64,
        grab_dy: f32,
        cell: CellSize,
        padding: WindowPadding,
    ) {
        let y_px = if self.should_show_tab_bar() {
            y_px - f64::from(self.tab_bar_height_px(cell))
        } else {
            y_px
        };
        let scrollback_len = self.scrollback_len();
        let Some(target) = scrollbar_offset_for_drag_with_padding(
            y_px as f32,
            grab_dy,
            scrollback_len,
            self.grid,
            cell,
            padding,
        ) else {
            return;
        };
        if self.viewport.jump_to(target, scrollback_len) {
            self.on_viewport_changed();
        }
    }

    pub(super) fn begin_selection(&mut self) {
        let Some(point) = self.pointer_cell else {
            return;
        };
        // MOUSE-RECT: Alt makes the whole gesture a rectangular/column (block)
        // selection; every non-Alt gesture is wrapped. The mode is decided once
        // here at the single selection entry point, so the word/line/drag
        // sub-paths below all inherit the right mode and a prior block selection
        // can never leak into a new wrapped one. Alt is reached only on the
        // local path (the mouse-reporting gate already returned for a reporting
        // app, where Shift is the only selection-vs-passthrough seam), so
        // Alt+drag never steals Alt+motion from a TUI that wants it. Block
        // selection is inherently char-granularity, so Alt suppresses the
        // double/triple-click word/line semantics and starts a fresh block drag.
        self.selection_block = self.modifiers.alt;
        if self.modifiers.alt {
            self.begin_block_drag(point);
            return;
        }
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
            let viewport_offset = self.viewport.offset();
            self.selection.update(selection::visible_to_absolute(
                point,
                viewport_offset,
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
        let viewport_offset = self.viewport.offset();
        self.selection.begin(selection::visible_to_absolute(
            point,
            viewport_offset,
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

    /// MOUSE-RECT: begin a rectangular/column (block) selection at `point`. The
    /// press cell is the anchor; the column band then grows as the pointer
    /// drags, reusing the existing Char-granularity `extend_drag_to` arm (a
    /// block drag follows the pointer exactly like a normal drag — only how the
    /// range renders and copies differs). `self.selection_block` is already set
    /// by the caller, so the render/copy paths treat the live selection as a
    /// block. Constructs the reserved `PointerDrag::Select { block: true }`.
    fn begin_block_drag(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let viewport_offset = self.viewport.offset();
        self.selection.begin(selection::visible_to_absolute(
            point,
            viewport_offset,
            scrollback_len,
        ));
        self.pointer_drag = PointerDrag::Select {
            granularity: SelectGranularity::Char,
            block: true,
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
                let viewport_offset = self.viewport.offset();
                self.selection.update(selection::visible_to_absolute(
                    point,
                    viewport_offset,
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
            PointerDrag::None | PointerDrag::Scrollbar { .. } => {}
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
        // SH-CLICK: a bare left click (no drag, so the selection stayed empty —
        // `range()` is `None` for a zero-width selection) on the live shell
        // prompt repositions the input cursor. Decided here at release, NOT at
        // press, so a real drag (a non-empty selection) always wins: drag-select
        // and click-to-position are mutually exclusive by construction (D-SHC-2).
        // `try_click_to_position` returns `false` whenever it does not fire
        // (feature off, shell not advertising, wrong row, same-cell, modified
        // click), so the off path falls straight through to the historical
        // finalize below — byte-identical to today (T1).
        if self.selection.range().is_none() && self.try_click_to_position() {
            // The click positioned the cursor; nothing was selected to copy.
        } else if self.drag_selection_should_write_primary() {
            // MOUSE-EXTEND parity: a plain double/triple-click that never dragged
            // must stay byte-identical to the historical finalize, which wrote
            // nothing to PRIMARY. Only write when a char drag ran (today's
            // behavior) or a word/line drag actually grew past its clicked unit.
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
            PointerDrag::None | PointerDrag::Scrollbar { .. } => false,
        }
    }

    /// SH-CLICK: whether click-to-position is live right now — the `sh_click`
    /// setting is on AND the shell has advertised OSC 133 `click_events=1` on
    /// its prompt. Doubly off by default: the setting defaults off, and a
    /// non-integrated shell never sets the core flag. When the setting is off
    /// this short-circuits before locking the terminal, so the off path does no
    /// work at all (T1 off-path identity).
    fn sh_click_enabled(&self) -> bool {
        self.settings.sh_click
            && self
                .terminal
                .lock()
                .map(|terminal| terminal.click_events_enabled())
                .unwrap_or(false)
    }

    /// SH-CLICK: emit the cursor-positioning key burst for a bare left click on
    /// the live prompt line, returning whether the click was consumed.
    ///
    /// Returns `false` (the caller falls through to the historical finalize,
    /// byte-identical to today) in every case but the narrow one the feature
    /// targets:
    /// - the feature is off or the shell has not advertised click-events (T1);
    /// - the click carries any modifier — Shift is the selection/passthrough
    ///   seam, Alt is block-select, Ctrl is hyperlink-open, so only a *plain*
    ///   click repositions (T2 — Shift seam preserved);
    /// - the viewport is scrolled off the live tail (a click in scrollback is
    ///   never a prompt edit);
    /// - the live command block is not awaiting input — i.e. there is no live
    ///   prompt because the command already executed (an `OutputStart` exists)
    ///   or there are no marks at all. This is the real prompt-context gate
    ///   (T4): the click-events flag alone can linger across a running command,
    ///   so we require the last [`crate::core::CommandBlock`] to have no output
    ///   yet;
    /// - the click is not on the cursor's own visual row — v1 is same-row
    ///   horizontal only (D-SHC-4); a click on a wrapped prompt's other row
    ///   falls through rather than emitting a wrong jump;
    /// - the click lands on the cursor's own cell, so [`crate::core::click_report`]
    ///   yields no movement (T4 same-cell ⇒ None).
    ///
    /// When it does fire, the horizontal delta from [`crate::core::click_report`]
    /// is encoded as `|delta|` Left/Right cursor keys through the live key modes
    /// ([`click_position_bytes`]) — honoring DECCKM application-cursor mode, the
    /// load-bearing encoding trap — and written through the same PTY writer a
    /// real arrow keypress uses (T5), after returning to the live tail.
    ///
    /// TUI mouse reporting (DECSET 1000/1002/1003/1006…) never reaches here: the
    /// reporting gate in [`App::handle_mouse_input`] returns earlier, so a
    /// reporting app's click is sent to the app, not to click-to-position (T3).
    fn try_click_to_position(&mut self) -> bool {
        if !self.sh_click_enabled() {
            return false;
        }
        // T2: only a plain left click repositions; any modifier defers to its
        // existing meaning (Shift=select/passthrough, Alt=block, Ctrl=open).
        if self.modifiers.shift || self.modifiers.alt || self.modifiers.ctrl || self.super_key {
            return false;
        }
        // A scrolled-back viewport is never a live-prompt edit.
        if self.viewport.offset() != 0 {
            return false;
        }
        let Some(point) = self.pointer_cell else {
            return false;
        };
        let (cursor, report, at_live_prompt) = {
            let Ok(terminal) = self.terminal.lock() else {
                return false;
            };
            let cursor = terminal.screen().cursor();
            // T4 prompt-context gate: the last command block must be awaiting
            // input (no OutputStart) for a live prompt to exist.
            let blocks = crate::core::command_blocks(&terminal.prompt_marks());
            let at_live_prompt = blocks
                .last()
                .is_some_and(|block| block.output_start.is_none());
            let report = crate::core::click_report(true, cursor.column, point.column);
            (cursor, report, at_live_prompt)
        };
        // v1 same-row only (D-SHC-4) + the prompt-context gate (T4).
        if !at_live_prompt || point.row != cursor.row {
            return false;
        }
        let Some(report) = report else {
            return false; // same-cell click ⇒ no movement (T4)
        };
        let bytes = click_position_bytes(report, self.key_modes());
        if bytes.is_empty() {
            return false;
        }
        // T5: the positioning burst goes to the host through the exact keystroke
        // writer, after snapping to the live tail like any typed input.
        self.return_to_live();
        self.write_pty_bytes(&bytes);
        true
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
        let scrollback_len = self.scrollback_len();
        let changed = match delta.cmp(&0) {
            std::cmp::Ordering::Greater => self.viewport.scroll_up(delta as usize, scrollback_len),
            std::cmp::Ordering::Less => self.viewport.scroll_down((-delta) as usize),
            std::cmp::Ordering::Equal => false,
        };
        if changed {
            // `on_viewport_changed` clears any glide (snap by default, RV4
            // D-RV4-8). Re-arm the eased glide only for a user-initiated scroll
            // while `smooth_scroll` is on; a selection drag-autoscroll
            // (`pointer_drag.is_selecting()`) must snap to avoid nested easing
            // (D-RV4-10 / T5). The integer `Viewport::offset` already moved
            // above, so the scroll TARGET is updated with zero added latency —
            // only the visual catches up.
            self.on_viewport_changed();
            if self.settings.smooth_scroll && !self.pointer_drag.is_selecting() {
                self.begin_scroll_anim(delta);
            }
        }
    }

    /// Whether wheel events should be translated into cursor keys (alternate
    /// scroll mode, DECSET 1007). True only on the alternate screen with the
    /// mode enabled; the caller has already excluded the mouse-reporting case.
    pub(super) fn alternate_scroll_active(&self) -> bool {
        self.terminal
            .lock()
            .map(|t| t.on_alternate_screen() && t.alternate_scroll_enabled())
            .unwrap_or(false)
    }

    /// Translate a wheel movement of `lines` into that many Up/Down cursor-key
    /// presses sent to the PTY (alternate scroll mode). `lines > 0` is a
    /// scroll-up (toward earlier content) → Up; `lines < 0` → Down. Arrows are
    /// encoded through the shared key encoder so DECCKM application-cursor mode
    /// gets the SS3 form (`\x1bOA`/`\x1bOB`), byte-identical to a real arrow key.
    pub(super) fn send_wheel_as_arrows(&mut self, lines: isize) {
        let key = if lines > 0 { Key::Up } else { Key::Down };
        let count = lines.unsigned_abs();
        if count == 0 {
            return;
        }
        let modes = self.key_modes();
        let arrow = input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Press);
        if arrow.is_empty() {
            return;
        }
        let mut bytes = Vec::with_capacity(arrow.len() * count);
        for _ in 0..count {
            bytes.extend_from_slice(&arrow);
        }
        self.write_pty_bytes(&bytes);
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
        // RV4: snap by default — clear any in-flight smooth-scroll glide. The
        // user `scroll_viewport` path re-arms it after this call, so every other
        // viewport change (return-to-live, search nav, scrollbar-thumb drag,
        // resize) snaps. No-op on the off path (the glide is always `None`).
        self.clear_scroll_anim();
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
/// SH-CLICK: encode the cursor-positioning key burst for a click-to-position
/// report — `|cell_delta|` repetitions of Left (negative delta) or Right
/// (positive delta), each encoded through the live [`KeyModes`] so a shell in
/// DECCKM application-cursor mode receives the SS3 form (`\x1bOC`/`\x1bOD`), not
/// the CSI form (`\x1b[C`/`\x1b[D`). This is the load-bearing encoding trap:
/// hardcoded CSI arrows would move the cursor wrong (or not at all) in zsh/zle,
/// fish, and readline shells that run in application-cursor mode, so the bytes
/// MUST be identical to a real arrow keypress in every mode.
///
/// Pure and total: returns the exact bytes the PTY writer receives. A
/// zero-delta report cannot reach here ([`crate::core::click_report`] returns
/// `None` for a same-cell click), and the delta is already saturated into
/// `i32` range by core, so `unsigned_abs` never overflows.
fn click_position_bytes(report: crate::core::ClickReport, modes: KeyModes) -> Vec<u8> {
    let (key, count) = if report.cell_delta < 0 {
        (Key::Left, report.cell_delta.unsigned_abs() as usize)
    } else {
        (Key::Right, report.cell_delta as usize)
    };
    let arrow = input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Press);
    arrow.repeat(count)
}

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
    use crate::core::{ClickReport, MouseTracking};

    // --- SH-CLICK: click-to-position arrow encoding (Finding A) ---

    fn app_cursor_modes() -> KeyModes {
        KeyModes {
            application_cursor: true,
            ..KeyModes::default()
        }
    }

    #[test]
    fn click_position_emits_right_arrows_in_csi_mode() {
        // A positive delta (click right of the cursor) emits that many Right
        // cursor keys in the default CSI form.
        let bytes = click_position_bytes(ClickReport { cell_delta: 5 }, KeyModes::default());
        assert_eq!(bytes, b"\x1b[C".repeat(5));
    }

    #[test]
    fn click_position_emits_left_arrows_in_csi_mode() {
        // A negative delta (click left of the cursor) emits Left cursor keys.
        let bytes = click_position_bytes(ClickReport { cell_delta: -3 }, KeyModes::default());
        assert_eq!(bytes, b"\x1b[D".repeat(3));
    }

    #[test]
    fn click_position_honors_decckm_application_cursor_mode() {
        // Finding A (the highest-risk encoding trap): a shell in DECCKM
        // application-cursor mode must receive the SS3 forms (\x1bOC / \x1bOD),
        // byte-identical to a real arrow keypress, NOT the CSI forms. This is
        // why the burst routes through `encode_key_event`, never hardcoded bytes.
        let right = click_position_bytes(ClickReport { cell_delta: 5 }, app_cursor_modes());
        assert_eq!(right, b"\x1bOC".repeat(5));
        let left = click_position_bytes(ClickReport { cell_delta: -2 }, app_cursor_modes());
        assert_eq!(left, b"\x1bOD".repeat(2));
    }

    #[test]
    fn click_position_burst_length_matches_delta_magnitude() {
        // The number of arrows equals |delta|; a single-cell move emits one key.
        assert_eq!(
            click_position_bytes(ClickReport { cell_delta: 1 }, KeyModes::default()),
            b"\x1b[C"
        );
        assert_eq!(
            click_position_bytes(ClickReport { cell_delta: -1 }, KeyModes::default()).len(),
            b"\x1b[D".len()
        );
        // A wide delta maps to exactly that many arrows (no off-by-one), without
        // exercising an absurd allocation.
        let wide = click_position_bytes(ClickReport { cell_delta: 200 }, KeyModes::default());
        assert_eq!(wide.len(), b"\x1b[C".len() * 200);
    }

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
