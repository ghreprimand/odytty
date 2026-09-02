// SPDX-License-Identifier: GPL-3.0-only
//! Session, tab, workspace, pane, and window commands for the native app.
//!
//! Every entry point keeps its existing call site, ordering, and effects; `App`
//! remains the single state owner and the arena keyed by `SessionToken` remains
//! the single session owner. Command bodies moved unchanged from the parent
//! module.

use super::*;

impl App {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn initialize_session_with(
        session: &mut Session,
        effective_theme: Theme,
        themed_ui_roles: bool,
        osc52_read: bool,
        kitty_named_transports: bool,
        cursor_style: crate::core::CursorStyle,
        cursor_blink: crate::settings::CursorBlink,
        cell: Option<CellSize>,
        scrollback_limit: usize,
        button_gates: ButtonGates,
    ) {
        if let Ok(mut terminal) = session.terminal.lock() {
            let cursor_default = if themed_ui_roles {
                rgb(effective_theme.cursor)
            } else {
                rgb(effective_theme.foreground)
            };
            terminal.set_base_colors(
                rgb(effective_theme.foreground),
                rgb(effective_theme.background),
                cursor_default,
            );
            // C29: OSC 4 replies report the theme palette, not the xterm table.
            terminal.set_base_palette(effective_theme.palette.map(rgb));
            terminal.set_osc52_read_enabled(osc52_read);
            terminal.set_kitty_named_transports_enabled(kitty_named_transports);
            terminal.set_scrollback_limit(scrollback_limit);
            terminal.set_cursor_defaults(cursor_style, cursor_blink.enabled());
            button_gates.apply(&mut terminal);
            if let Some(cell) = cell {
                terminal.set_cell_metrics(cell.width, cell.height);
            }
        }
    }

    /// New Tab dispatcher (F6-W5 / ODP-9): when the active workspace is bound to
    /// a host alias this routes New Tab through the remote SSH connect path;
    /// otherwise it spawns a local shell — byte-identical to the pre-W5 path, so
    /// an unbound workspace is unaffected. The "New Local Tab" escape hatch
    /// bypasses the binding by calling [`Self::handle_new_local_tab`] directly.
    pub(super) fn handle_new_tab(&mut self) {
        if let Some(alias) = self
            .sessions
            .active_workspace_default_profile()
            .map(str::to_owned)
        {
            self.new_tab_for_bound_host(&alias);
            return;
        }
        let cwd = self.validated_spawn_cwd();
        let workspace_profile = self
            .sessions
            .active_workspace_launch_profile()
            .map(str::to_owned);
        if let Some(effective) = super::profile_launch::resolve_default_launch_for_new_tab(
            &self.settings,
            workspace_profile.as_deref(),
            cwd,
        ) {
            if let Some(alias) = effective.connection.clone() {
                self.new_tab_for_bound_host(&alias);
                for warning in effective.warnings {
                    tracing::warn!(warning = %warning, "profile launch notice");
                }
                return;
            }
            self.spawn_local_tab_from_effective(effective);
            return;
        }
        self.handle_new_local_tab_plain();
    }

    /// Open a New Tab against the active workspace's bound host `alias`,
    /// resolving it against the live host list and routing through the SSH
    /// connect path. A stale binding (alias removed from `hosts.conf`) or a
    /// connect failure falls back to a local tab and raises a one-line notice —
    /// a bad binding never blocks opening a tab.
    pub(super) fn new_tab_for_bound_host(&mut self, alias: &str) {
        let host = self
            .load_connection_entries()
            .into_iter()
            .find(|entry| entry.alias == alias);
        match host {
            Some(host) => {
                // A connect failure raises its own one-line notice inside
                // connect_or_notice (LOW-03); fall back to a local tab so New Tab
                // never dead-ends on a bad binding.
                if self.connect_or_notice(&host).is_none() {
                    self.handle_new_local_tab_plain();
                }
            }
            None => {
                self.raise_open_notice(format!(
                    "Host \"{alias}\" is no longer configured; opened a local tab"
                ));
                self.handle_new_local_tab_plain();
            }
        }
    }

    /// Spawn a local tab from a fully resolved named-profile launch context.
    pub(super) fn spawn_local_tab_from_effective(
        &mut self,
        effective: crate::profiles::EffectiveLaunch,
    ) {
        self.finish_divider_drag();
        if let Some(alias) = effective.connection.clone() {
            self.new_tab_for_bound_host(&alias);
            return;
        }
        let session_theme = crate::native::cvd_theme::effective_theme(
            &effective.settings.theme,
            effective.settings.cvd_mode,
            effective.settings.cvd_strength,
        );
        let themed_ui_roles = effective.settings.themed_ui_roles;
        let osc52_read = effective.settings.osc52_read;
        let kitty_named_transports = effective.settings.kitty_named_transports;
        let cursor_style = effective.settings.cursor_style;
        let cursor_blink = effective.settings.cursor_blink;
        let scrollback_limit = effective.settings.scrollback_limit();
        let button_gates = ButtonGates {
            enabled: effective.settings.buttons,
            iterm_compat: effective.settings.buttons_iterm_compat,
            sticky: effective.settings.buttons_sticky,
        };
        let cwd = effective.working_directory.clone();
        match self
            .sessions
            .spawn_with_effective(self.grid, cwd, Some(&effective))
        {
            Ok(session_id) => {
                let cell = self.gpu.as_ref().map(GpuState::cell);
                if let Some(session) = self.sessions.get_mut(session_id) {
                    Self::initialize_session_with(
                        session,
                        session_theme,
                        themed_ui_roles,
                        osc52_read,
                        kitty_named_transports,
                        cursor_style,
                        cursor_blink,
                        cell,
                        scrollback_limit,
                        button_gates,
                    );
                }
                let _ = self.sessions.switch(session_id);
                self.on_active_session_changed();
                for warning in &effective.warnings {
                    tracing::warn!(warning = %warning, "profile launch notice");
                }
                if let Some(warning) = effective.warnings.first()
                    && self.open_notice.is_none()
                {
                    self.raise_open_notice(warning.clone());
                }
            }
            Err(err) => {
                if self.open_notice.is_none() {
                    self.raise_open_notice(format!("Could not open a new tab: {err}"));
                }
            }
        }
    }

    /// New Tab with an explicit named profile from the palette or connection UI.
    pub(super) fn handle_new_tab_with_profile(&mut self, profile_name: &str) {
        let cwd = self.validated_spawn_cwd();
        let effective = super::profile_launch::resolve_for_new_local_tab(
            &self.settings,
            None,
            cwd,
            Some(profile_name),
        );
        self.spawn_local_tab_from_effective(effective);
    }

    /// Spawn a local shell in a new tab regardless of any workspace host binding
    /// (F6-W5 escape hatch). This is the exact pre-W5 New Tab behavior; the
    /// binding-aware [`Self::handle_new_tab`] delegates here when the active
    /// workspace is unbound.
    pub(super) fn handle_new_local_tab(&mut self) {
        self.handle_new_local_tab_plain();
    }

    /// Plain local tab spawn without workspace named-profile binding.
    pub(super) fn handle_new_local_tab_plain(&mut self) {
        self.finish_divider_drag();
        // F1 cwd inheritance: seed the new tab's shell in the active pane's OSC 7
        // cwd when known, so New Tab opens where you already are. A pane with no
        // tracked cwd (None) spawns in the default directory, unchanged. Works on
        // Windows too — ConPTY honors the working directory and drive-letter OSC 7
        // cwds are already normalized.
        // D-1: validate the tracked cwd (stat + home fallback) before it seeds
        // the spawn, so a bogus / non-filesystem OSC 7 cwd can never reach the
        // PTY spawn's working directory.
        let cwd = self.validated_spawn_cwd();
        match self.sessions.spawn(self.grid, cwd) {
            Ok(session_id) => {
                let effective_theme = self.effective_theme;
                let themed_ui_roles = self.themed_ui_roles;
                let osc52_read = self.settings.osc52_read;
                let kitty_named_transports = self.settings.kitty_named_transports;
                let cursor_style = self.settings.cursor_style;
                let cursor_blink = self.settings.cursor_blink;
                let cell = self.gpu.as_ref().map(GpuState::cell);
                let scrollback_limit = self.settings.scrollback_limit();
                let button_gates = self.button_gates();
                if let Some(session) = self.sessions.get_mut(session_id) {
                    Self::initialize_session_with(
                        session,
                        effective_theme,
                        themed_ui_roles,
                        osc52_read,
                        kitty_named_transports,
                        cursor_style,
                        cursor_blink,
                        cell,
                        scrollback_limit,
                        button_gates,
                    );
                }
                let _ = self.sessions.switch(session_id);
                self.on_active_session_changed();
            }
            Err(err) => {
                // D-1: surface a spawn failure instead of swallowing it (the old
                // `if let Ok(..)` silently no-oped). A validated cwd makes a
                // bogus-directory failure unreachable, but a genuine spawn error
                // (exhausted PTYs, missing shell) now leaves a visible notice.
                // Don't clobber a more specific notice a caller already raised
                // (e.g. the stale-host-binding local fallback), which explains
                // the same failed open in user terms.
                if self.open_notice.is_none() {
                    self.raise_open_notice(format!("Could not open a new tab: {err}"));
                }
            }
        }
    }

    /// F1: launch another top-level OdyTTY window — a fresh process instance,
    /// not a tab. Spawned from [`std::env::current_exe`] with no extra args, so
    /// the child inherits this process's environment (theme/env overrides carry
    /// over naturally). Routed through the reaper-backed [`spawn_detached`]
    /// (never a bare `Command::spawn`), so the child is reaped and never left a
    /// zombie. Best-effort: an unresolvable executable path or a spawn failure
    /// is logged and dropped — a new-window request must never crash the
    /// running window (consistent with the C6 log-and-drop philosophy). F1 cwd
    /// inheritance: when the focused pane has a tracked OSC 7 cwd, the new window
    /// is launched with `--working-directory <cwd>` so it opens where the active
    /// pane is; a pane with no tracked cwd launches in the default directory,
    /// unchanged. Cross-platform — `--working-directory` is honored on Windows,
    /// and drive-letter OSC 7 cwds are already normalized upstream.
    pub(in crate::native) fn handle_new_window(&mut self) {
        // D-1: validate the tracked cwd before threading it into
        // `--working-directory`, so a bogus / non-filesystem OSC 7 cwd cannot make
        // the new window die with `CreateProcessW` rejecting `lpCurrentDirectory`.
        let cwd = self
            .validated_spawn_cwd()
            .and_then(|dir| dir.into_os_string().into_string().ok());
        let Some(argv) = Self::new_window_argv(cwd.as_deref()) else {
            tracing::warn!(
                "new-window: could not resolve the current executable; not launching a window"
            );
            return;
        };
        #[cfg(test)]
        {
            // Test seam: record the argv that WOULD be spawned instead of
            // launching a real second instance, so the chord/menu dispatch can
            // be asserted at the spawn boundary without side effects.
            NEW_WINDOW_SPAWN_ARGV.with(|cell| cell.borrow_mut().push(argv));
        }
        #[cfg(not(test))]
        {
            if let Err(err) = interactive_paths::spawn_detached(&argv) {
                // D-1: a new-window spawn failure is surfaced, not just logged --
                // otherwise the window silently never appears. Still non-fatal:
                // a failed new-window request must never crash the running window.
                tracing::warn!(error = %err, "new-window: failed to launch a new OdyTTY window");
                self.raise_open_notice(format!("Could not open a new window: {err}"));
            }
        }
    }

    /// The argv that opens a new OdyTTY window: the current executable, plus
    /// `["--working-directory", cwd]` when `cwd` is `Some` (F1 cwd inheritance).
    /// The child otherwise inherits the environment. Pure — returns `None` when
    /// the current-exe path cannot be resolved or is not valid UTF-8 (the argv
    /// seam is `String`-based). Split out so the dispatch decision (and the cwd
    /// propagation) is unit-testable without spawning. `cwd == None` yields the
    /// bare-exe argv, byte-identical to the pre-F1 behavior.
    pub(super) fn new_window_argv(cwd: Option<&str>) -> Option<Vec<String>> {
        let exe = std::env::current_exe().ok()?;
        let mut argv = vec![exe.into_os_string().into_string().ok()?];
        if let Some(cwd) = cwd.filter(|dir| !dir.is_empty()) {
            argv.push("--working-directory".to_owned());
            argv.push(cwd.to_owned());
        }
        Some(argv)
    }

    /// Attach to a detached, session-host-backed session by id and present it as
    /// a new live tab in this window — the production "reopen by id, full
    /// scrollback intact" path. The mirror terminal is restored from the host
    /// snapshot inside [`WorkspaceSet::attach_in_new_tab`]; here we apply the window's
    /// presentation policy (theme/cursor/scrollback cap) so the attached tab
    /// renders consistently, switch focus to it, then reconcile the mirror to
    /// this window's dimensions. A mismatched host snapshot sends one `Resize`
    /// frame to the host; an already matching mirror remains untouched.
    /// `runtime_base` is `None` in production (derived from `XDG_RUNTIME_DIR`);
    /// tests pass a base.
    #[cfg(unix)]
    pub(in crate::native) fn attach_session_in_new_tab(
        &mut self,
        runtime_base: Option<&Path>,
        session_id: &str,
    ) -> std::io::Result<()> {
        self.finish_divider_drag();
        let token = self.sessions.attach_in_new_tab(runtime_base, session_id)?;
        self.present_attached_session(token);
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn present_attached_session(&mut self, token: SessionToken) {
        #[cfg(test)]
        let test_geometry = (self.test_cell, self.test_surface);
        let effective_theme = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let osc52_read = self.settings.osc52_read;
        let kitty_named_transports = self.settings.kitty_named_transports;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink;
        let cell = self.gpu.as_ref().map(GpuState::cell);
        let scrollback_limit = self.settings.scrollback_limit();
        let button_gates = self.button_gates();
        if let Some(session) = self.sessions.get_mut(token) {
            Self::initialize_session_with(
                session,
                effective_theme,
                themed_ui_roles,
                osc52_read,
                kitty_named_transports,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
                button_gates,
            );
        }
        let _ = self.sessions.switch(token);
        #[cfg(test)]
        {
            // The headless geometry overrides live on the active session via
            // App's Deref target, so carry them across the same way the test
            // workspace/session insertion seams do before reconciling.
            self.test_cell = test_geometry.0;
            self.test_surface = test_geometry.1;
        }
        self.on_active_session_changed();
        self.reconcile_pane_dims_to_window();
    }

    /// Windows stub: the detached session-host (Unix-domain socket transport) is
    /// not available, so attach-by-id is rejected cleanly. Callers already treat
    /// an `Err` as "attach failed" (surface a notice / leave panes untouched), so
    /// the overlay-outcome paths and the startup `attach_session` hook stay
    /// well-behaved on Windows without panicking.
    #[cfg(not(unix))]
    pub(in crate::native) fn attach_session_in_new_tab(
        &mut self,
        _runtime_base: Option<&Path>,
        _session_id: &str,
    ) -> std::io::Result<()> {
        Err(std::io::Error::other(
            "resumable sessions are not supported on Windows yet",
        ))
    }

    /// Route an accepted session from the Attach-Session overlay (Phase 14).
    /// Dedup first: if the session is already open in a tab in this window,
    /// switch to that tab — no duplicate, no prompt (this kills the reported
    /// triple-open bug). Otherwise open the attach-choice dialog so the user
    /// picks New tab vs Replace current.
    pub(in crate::native) fn route_attach_session(&mut self, session_id: String) {
        if let Some(token) = self.sessions.find_attached_tab(&session_id) {
            // C5: close the summon overlay before switching. Unlike the
            // not-yet-attached branch below (which re-opens the overlay in
            // AttachChoice mode), this early return left the SessionAttach
            // overlay open==true, so keyboard dispatch kept routing every key
            // into its type-to-filter box instead of the switched-to session
            // until Esc was pressed.
            self.finish_divider_drag();
            self.overlay.close();
            if self.sessions.switch(token) {
                self.on_active_session_changed();
            }
            return;
        }
        self.overlay.open_attach_choice(session_id);
    }

    /// Attach `session_id` in a new tab and then close the tab that was active
    /// when the Attach manager opened — the "Replace current" choice (Phase 14).
    /// Opening an overlay does not change the active session, so `active_id()`
    /// captured here is the correct replace target. Order: capture old active →
    /// attach new (appends + focuses it) → close the old tab via the existing
    /// whole-tab close path. That path routes each session through
    /// `Session::close`, which cleanly `Detach`es a hosted/attached tab (the host
    /// keeps the PTY, so it stays reattachable) and closes a local PTY tab
    /// directly — no nested confirm-close dialog, since the user explicitly chose
    /// Replace. A stale id (nothing attached) leaves the current tab untouched.
    pub(super) fn attach_session_replacing_current(&mut self, session_id: String) {
        self.finish_divider_drag();
        let replace_target = self.sessions.active_id();
        if self.attach_session_in_new_tab(None, &session_id).is_err() {
            return;
        }
        if let Some(tab_idx) = self.sessions.position_of_token(replace_target) {
            // Intentional Session Navigator reopen-history exemption: Replace
            // turns a hosted tab back into a detached registry session, so a
            // fresh local shell descriptor would be misleading. The user
            // can attach the surviving hosted process from the navigator.
            let _ = self.sessions.close_tab_at(tab_idx);
        }
        // A surviving single-pane tab may return input to the plain fast path;
        // clear any pending multiplexer prefix so stale state can't swallow keys
        // (mirrors `close_active_tab`).
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.on_active_session_changed();
    }

    pub(super) fn switch_to_next_tab(&mut self) {
        self.finish_divider_drag();
        if self.sessions.next() {
            self.on_active_session_changed();
        }
    }

    pub(super) fn switch_to_prev_tab(&mut self) {
        self.finish_divider_drag();
        if self.sessions.prev() {
            self.on_active_session_changed();
        }
    }

    pub(super) fn close_active_tab(&mut self) -> bool {
        self.finish_divider_drag();
        // "Close Tab" reaps the ENTIRE active tab — every leaf session in its
        // layout tree — and removes the tab, regardless of pane count. This is
        // distinct from "Close Pane" (`close_focused_pane`), which collapses a
        // single leaf and deliberately keeps a multi-pane tab alive.
        //
        // Exit keys on the last tab of the LAST workspace, never on the last
        // pane: closing the sole tab of the sole workspace — even a multi-pane
        // one — signals app exit. We guard on that case first and return without
        // reaping, preserving the existing shutdown path exactly (the app tears
        // down sessions on exit; reaping here would empty the `WorkspaceSet` and
        // make any `active()` Deref before exit panic). With a single workspace
        // this is byte-identical to the old last-close path.
        //
        // Closing the last tab of a NON-last workspace instead closes that
        // workspace and switches to a neighbor (`WorkspaceSet::close_active_tab`);
        // that is not app exit, so it falls through to the reap branch below.
        if self.sessions.tab_count() <= 1 && self.sessions.workspace_count() <= 1 {
            self.pending_exit = true;
            return true;
        }
        self.record_navigator_closed_tab(self.sessions.active_id());
        // Another tab in this workspace, or another workspace, survives: reap the
        // whole active tab (every pane), removing the workspace too if it was its
        // last tab.
        let ws_before = self.sessions.workspace_count();
        let _ = self.sessions.close_active_tab();
        // ODP-2: pure tab actions no longer flash the workspace rail — only a
        // change to the WORKSPACE list does. Closing the last tab of a non-last
        // workspace closes that workspace, so flash iff the list shrank.
        if self.sessions.workspace_count() < ws_before {
            self.flash_rail_autohide();
        }
        // Switching to a surviving tab may return the input path to the plain
        // single-pane fast path; clear any pending multiplexer prefix so a
        // stale state can't swallow the next key.
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.on_active_session_changed();
        false
    }

    /// Close the tab that holds `token` — the whole tab, every pane — from a
    /// tab-slot right-click (NF-F7-1). Distinct from [`Self::close_active_tab`]:
    /// the right-clicked tab may be a background one, so this resolves its strip
    /// index (within the active workspace) and reaps THAT tab. Exits the app on
    /// the last tab of the last workspace, mirroring the active-close guard;
    /// closing the last tab of a non-last workspace closes that workspace.
    pub(super) fn close_tab_by_token(&mut self, token: SessionToken) {
        self.finish_divider_drag();
        let Some(tab_idx) = self.sessions.position_of_token(token) else {
            return;
        };
        if self.sessions.tab_count() <= 1 && self.sessions.workspace_count() <= 1 {
            self.pending_exit = true;
            return;
        }
        self.record_navigator_closed_tab(token);
        let ws_before = self.sessions.workspace_count();
        let _ = self.sessions.close_tab_at(tab_idx);
        if self.sessions.workspace_count() < ws_before {
            self.flash_rail_autohide();
        }
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.on_active_session_changed();
    }

    /// Close every tab except the one holding `token` (F7 "Close Other Tabs").
    /// Reaps from the highest strip index downward so each removal leaves the
    /// remaining indices stable, then lands on the kept tab. A no-op when the
    /// kept tab is the only one open (the menu item is disabled there anyway).
    pub(super) fn close_other_tabs(&mut self, token: SessionToken) {
        self.finish_divider_drag();
        if self.sessions.position_of_token(token).is_none() || self.sessions.tab_count() <= 1 {
            return;
        }
        // Reap top-down so surviving indices never shift under the loop.
        while let Some(keep_idx) = self.sessions.position_of_token(token) {
            let Some(victim) = (0..self.sessions.tab_count())
                .rev()
                .find(|&i| i != keep_idx)
            else {
                break;
            };
            let token = self.sessions.workspaces[self.sessions.active_workspace_index()].tabs
                [victim]
                .focused;
            self.record_navigator_closed_tab(token);
            let _ = self.sessions.close_tab_at(victim);
        }
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.on_active_session_changed();
    }

    /// Create a fresh workspace — its own single-pane tab — and switch to it.
    /// Driven by the rail `+` slot, the "New Workspace" context-menu item, and
    /// (later work) the keyboard action. The new session is initialized
    /// exactly like New Tab (theme / cursor / scrollback), and the switch may
    /// make the rail auto-appear, so the content grid is reflowed and the
    /// auto-hidden rail is flashed to confirm the change.
    /// Switch the active workspace to rail index `idx` (rail click / picker /
    /// keyboard). Reflows the content grid (chrome can change) and flashes the
    /// auto-hidden rail. No-op when already active or out of range.
    pub(super) fn activate_workspace(&mut self, idx: usize) {
        self.finish_divider_drag();
        if self.sessions.switch_workspace(idx) {
            self.flash_rail_autohide();
            self.recompute_grid_for_tab_bar();
            self.on_active_session_changed();
        }
    }

    /// Close the ENTIRE workspace at rail index `idx` — every tab, every pane.
    /// The rail slot `×` / "Close Workspace" menu item. Closing the last
    /// workspace signals app exit WITHOUT emptying the arena first (mirroring the
    /// `close_active_tab` guard, so no `active()` Deref panics during teardown).
    pub(super) fn close_workspace_at(&mut self, idx: usize) {
        self.finish_divider_drag();
        // Exit keys on the last workspace: guard before reaping so the shutdown
        // path tears sessions down exactly as it does for the last tab.
        if self.sessions.workspace_count() <= 1 {
            self.pending_exit = true;
            return;
        }
        self.record_navigator_closed_workspace(idx);
        // Close-active-workspace acts on the active one; switch to the target
        // first so a background slot's `×` closes THAT workspace.
        if idx != self.sessions.active_workspace_index() {
            let _ = self.sessions.switch_workspace(idx);
        }
        let _ = self.sessions.close_active_workspace();
        self.flash_rail_autohide();
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.recompute_grid_for_tab_bar();
        self.on_active_session_changed();
    }

    /// Reorder the workspace at rail index `idx` one slot in the rail
    /// (RAIL-REORDER): `up` moves it toward the front, otherwise toward the
    /// back. The model follows the active workspace by identity, so the focused
    /// workspace never changes under the user; the rail flashes so the move is
    /// visible, and the reorder shifts the shape autosave fingerprint so the new
    /// order is persisted. A rejected move (already at an end / bad index) is a
    /// silent no-op. Pure rail bookkeeping -- no grid reflow or focus change,
    /// so no `on_active_session_changed`.
    pub(super) fn move_workspace_at(&mut self, idx: usize, up: bool) {
        if self.sessions.move_workspace(idx, up) {
            self.flash_rail_autohide();
            self.request_selection_redraw();
        }
    }

    /// Open the "Move to Workspace" destination picker for the right-clicked tab
    /// (W4-v2). Seeds the picker with every workspace EXCEPT the one that owns
    /// `token`, carrying the token so the accepted destination moves the clicked
    /// tab (not the active one). A no-op when there is no other workspace to move
    /// to — the picker never opens with an empty list.
    pub(super) fn open_move_tab_workspace_picker(&mut self, token: SessionToken) {
        let destinations = self.sessions.move_tab_destinations(token);
        if destinations.is_empty() {
            return;
        }
        let entries = destinations
            .into_iter()
            .map(
                |(index, name)| crate::native::workspace_picker::WorkspacePickerEntry {
                    index,
                    name,
                },
            )
            .collect();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_workspace_picker(entries, token);
        self.request_selection_redraw();
    }

    /// Move the tab holding `token` into the workspace at `dest_ws` (W4-v2, the
    /// destination chosen from the picker). A `Tab` value splice — the sessions
    /// stay in the arena. Moves WITHOUT following: the active workspace is
    /// unchanged and the rail flashes so the departure is visible, unless moving
    /// the last tab out closes the source workspace, which necessarily shifts
    /// focus to a neighbor. Reconciles focus only when the move actually changed
    /// the active workspace, so a same-workspace no-op stays byte-identical.
    pub(super) fn move_tab_to_workspace(&mut self, token: SessionToken, dest_ws: usize) {
        self.finish_divider_drag();
        let active_before = self.sessions.active_workspace_index();
        let (moved, _source_closed) = self.sessions.move_tab_to_workspace(token, dest_ws);
        if !moved {
            return;
        }
        self.flash_rail_autohide();
        // Removing an emptied source workspace can shift the active workspace
        // onto a neighbor; only then does focus/geometry need reconciling.
        if self.sessions.active_workspace_index() != active_before {
            if self.sessions.active_is_single_pane() {
                self.prefix_engine.cancel();
            }
            self.on_active_session_changed();
        }
    }

    /// Create a fresh workspace (one single-pane tab) and switch to it — the
    /// "New Workspace" action / palette entry / rail `+` slot (ODP-3/-5). Mirrors
    /// [`Self::handle_new_tab`] one level up: spawn the new workspace's shell,
    /// apply this window's presentation policy to it, then flash the rail so the
    /// new workspace is confirmed. The new session becomes the `Deref` target, so
    /// `on_active_session_changed` reconciles focus/geometry.
    pub(super) fn handle_new_workspace(&mut self) {
        self.finish_divider_drag();
        let cwd = self.validated_spawn_cwd();
        if let Some(effective) =
            super::profile_launch::resolve_default_launch_for_new_tab(&self.settings, None, cwd)
        {
            if let Some(alias) = effective.connection.clone() {
                let host = self
                    .load_connection_entries()
                    .into_iter()
                    .find(|entry| entry.alias == alias);
                let Some(host) = host else {
                    self.raise_open_notice(format!(
                        "Host \"{alias}\" is no longer configured; opened a plain workspace"
                    ));
                    let result = self.sessions.new_workspace(self.grid);
                    self.finish_new_workspace_spawn(result);
                    return;
                };
                let result = self.sessions.new_workspace(self.grid);
                self.finish_new_workspace_spawn(result);
                let placeholder = self.sessions.active_id();
                if self.connect_or_notice(&host).is_some() {
                    self.close_tab_by_token(placeholder);
                    self.on_active_session_changed();
                }
                for warning in effective.warnings {
                    tracing::warn!(warning = %warning, "profile launch notice");
                }
                return;
            }
            match self
                .sessions
                .new_workspace_from_effective(self.grid, &effective)
            {
                Ok(token) => self.finish_new_workspace_with_effective(token, &effective),
                Err(error) => {
                    if self.open_notice.is_none() {
                        self.raise_open_notice(format!("Could not create a workspace: {error}"));
                    }
                }
            }
            return;
        }
        self.handle_new_workspace_plain();
    }

    pub(super) fn handle_new_workspace_plain(&mut self) {
        let result = self.sessions.new_workspace(self.grid);
        self.finish_new_workspace_spawn(result);
    }

    pub(super) fn finish_new_workspace_spawn(&mut self, result: std::io::Result<SessionToken>) {
        let token = match result {
            Ok(token) => token,
            Err(error) => {
                if self.open_notice.is_none() {
                    self.raise_open_notice(format!("Could not create a workspace: {error}"));
                }
                return;
            }
        };
        let effective_theme = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let osc52_read = self.settings.osc52_read;
        let kitty_named_transports = self.settings.kitty_named_transports;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink;
        let cell = self.gpu.as_ref().map(GpuState::cell);
        let scrollback_limit = self.settings.scrollback_limit();
        let button_gates = self.button_gates();
        if let Some(session) = self.sessions.get_mut(token) {
            Self::initialize_session_with(
                session,
                effective_theme,
                themed_ui_roles,
                osc52_read,
                kitty_named_transports,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
                button_gates,
            );
        }
        self.flash_rail_autohide();
        // A new workspace can make the auto rail appear (>=2 workspaces), which
        // changes the content reservation — reflow so the grid matches.
        self.recompute_grid_for_tab_bar();
        self.on_active_session_changed();
    }

    /// Duplicate the active workspace: create a fresh workspace whose single
    /// shell spawns in the active pane's OSC 7 cwd (F1 cwd inheritance), then
    /// switch to it. Mirrors [`Self::handle_new_workspace`] but threads the
    /// captured cwd through the cwd-aware `new_workspace_in`, exactly as
    /// Duplicate Tab reuses the cwd-aware local-tab spawn one level down. HONEST
    /// framing: a fresh shell in the same directory, not a process fork —
    /// scrollback and the running program are not copied. A pane with no tracked
    /// cwd (`None`) spawns in the default directory, unchanged. Windows: the cwd
    /// flows through the same spawn path New Tab's cwd inheritance uses, so
    /// ConPTY honors it and drive-letter OSC 7 cwds are already normalized.
    pub(super) fn handle_duplicate_workspace(&mut self) {
        self.finish_divider_drag();
        // D-1: validate the tracked cwd (stat + home fallback) before it seeds
        // the duplicated workspace's shell spawn.
        let cwd = self.validated_spawn_cwd();
        let result = self.sessions.new_workspace_in(self.grid, cwd);
        self.finish_duplicate_workspace_spawn(result);
    }

    pub(super) fn finish_duplicate_workspace_spawn(
        &mut self,
        result: std::io::Result<SessionToken>,
    ) {
        let token = match result {
            Ok(token) => token,
            Err(error) => {
                if self.open_notice.is_none() {
                    self.raise_open_notice(format!("Could not duplicate the workspace: {error}"));
                }
                return;
            }
        };
        let effective_theme = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let osc52_read = self.settings.osc52_read;
        let kitty_named_transports = self.settings.kitty_named_transports;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink;
        let cell = self.gpu.as_ref().map(GpuState::cell);
        let scrollback_limit = self.settings.scrollback_limit();
        let button_gates = self.button_gates();
        if let Some(session) = self.sessions.get_mut(token) {
            Self::initialize_session_with(
                session,
                effective_theme,
                themed_ui_roles,
                osc52_read,
                kitty_named_transports,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
                button_gates,
            );
        }
        self.flash_rail_autohide();
        // A new workspace can make the auto rail appear (>=2 workspaces), which
        // changes the content reservation — reflow so the grid matches.
        self.recompute_grid_for_tab_bar();
        self.on_active_session_changed();
    }

    /// Close the entire active workspace — every tab, every pane (ODP-3). Closing
    /// the last remaining workspace exits the app, exactly like closing the last
    /// tab of the last workspace: we guard on that case first and set
    /// `pending_exit` without reaping, so the arena is not emptied before
    /// teardown (a `Deref` on the emptied set would panic). Otherwise the reap
    /// removes the workspace, switches to a neighbor, and reconciles focus.
    pub(super) fn close_active_workspace(&mut self) {
        self.finish_divider_drag();
        if self.sessions.workspace_count() <= 1 {
            self.pending_exit = true;
            return;
        }
        self.record_navigator_closed_workspace(self.sessions.active_workspace_index());
        let _ = self.sessions.close_active_workspace();
        self.flash_rail_autohide();
        // The neighbor workspace's active tab may be single-pane, returning input
        // to the plain fast path; clear any pending multiplexer prefix.
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.on_active_session_changed();
    }

    /// Switch to the next workspace in rail order (wrapping) — the "Next
    /// Workspace" action / palette entry. A no-op with a single workspace. Flashes
    /// the auto-hidden rail so the workspace change is visible even with the
    /// pointer away from the edge (ODP-2: workspace chords flash the rail).
    pub(super) fn switch_to_next_workspace(&mut self) {
        self.finish_divider_drag();
        if self.sessions.next_workspace() {
            self.flash_rail_autohide();
            if self.sessions.active_is_single_pane() {
                self.prefix_engine.cancel();
            }
            self.on_active_session_changed();
        }
    }

    /// Switch to the previous workspace in rail order (wrapping). A no-op with a
    /// single workspace.
    pub(super) fn switch_to_prev_workspace(&mut self) {
        self.finish_divider_drag();
        if self.sessions.prev_workspace() {
            self.flash_rail_autohide();
            if self.sessions.active_is_single_pane() {
                self.prefix_engine.cancel();
            }
            self.on_active_session_changed();
        }
    }

    /// Switch directly to the workspace at rail index `idx` — the command
    /// palette's per-workspace "switch to …" rows (ODP-5). A no-op when `idx` is
    /// the active workspace or out of range.
    pub(super) fn switch_to_workspace(&mut self, idx: usize) {
        self.finish_divider_drag();
        if self.sessions.switch_workspace(idx) {
            self.flash_rail_autohide();
            if self.sessions.active_is_single_pane() {
                self.prefix_engine.cancel();
            }
            self.on_active_session_changed();
        }
    }

    /// Create a fresh workspace from the command palette's "New Workspace" row.
    pub(super) fn new_workspace_from_palette(&mut self) {
        self.handle_new_workspace();
    }

    /// Dispatch a multiplexer pane action resolved on the prefix (§7, K2). The
    /// prefix engine only ever returns pane actions here; the catch-all is for
    /// exhaustiveness. Each op routes onto the `WorkspaceSet` pane methods built in
    /// Phase 1c, then reflows pane geometry and repaints as needed.
    pub(super) fn apply_pane_action(&mut self, action: BindableAction) {
        self.finish_divider_drag();
        match action {
            BindableAction::SplitColumns => self.split_active_pane(SplitAxis::Columns),
            BindableAction::SplitRows => self.split_active_pane(SplitAxis::Rows),
            BindableAction::FocusPaneLeft => self.focus_pane_dir(FocusDir::Left),
            BindableAction::FocusPaneRight => self.focus_pane_dir(FocusDir::Right),
            BindableAction::FocusPaneUp => self.focus_pane_dir(FocusDir::Up),
            BindableAction::FocusPaneDown => self.focus_pane_dir(FocusDir::Down),
            BindableAction::FocusPaneNext => {
                if self.sessions.focus_next_pane() {
                    self.on_active_session_changed();
                }
            }
            BindableAction::ClosePane => self.close_focused_pane(),
            BindableAction::EqualizePanes => {
                self.sessions.equalize_active();
                // Equalize changes split ratios, so each pane's cell dimensions
                // change — reflow before repaint.
                self.reflow_active_panes_and_redraw();
            }
            BindableAction::ZoomPane => {
                // Zoom / toggle-fullscreen-pane (tmux `Ctrl-b z`). Flips the
                // active tab's zoom flag (a no-op on a single-pane tab) without
                // mutating the layout tree, then reflows: the focused pane sizes
                // to the full content rect on zoom and back to its split sub-rect
                // on un-zoom, and the render path draws only that pane full-bleed
                // with no dividers (see `rebuild_multipane`).
                let toggled = self.sessions.toggle_active_zoom();
                if toggled {
                    self.reflow_active_panes_and_redraw();
                }
            }
            // The prefix engine only returns pane actions; other variants never
            // reach here.
            _ => {}
        }
    }

    /// Split the focused pane along `axis` (tmux `Ctrl-b %` / `"`). Spawns and
    /// initializes a new session for the new pane, then reflows every pane to
    /// its new sub-rect and repaints.
    pub(super) fn split_active_pane(&mut self, axis: SplitAxis) {
        let result = self.sessions.split_active(axis, self.grid);
        self.finish_split_active_pane_spawn(result);
    }

    pub(super) fn finish_split_active_pane_spawn(&mut self, result: std::io::Result<SessionToken>) {
        let new_token = match result {
            Ok(token) => token,
            Err(error) => {
                if self.open_notice.is_none() {
                    self.raise_open_notice(format!("Could not split the active pane: {error}"));
                }
                return;
            }
        };
        let effective_theme = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let osc52_read = self.settings.osc52_read;
        let kitty_named_transports = self.settings.kitty_named_transports;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink;
        let cell = self.gpu.as_ref().map(GpuState::cell);
        let scrollback_limit = self.settings.scrollback_limit();
        let button_gates = self.button_gates();
        if let Some(session) = self.sessions.get_mut(new_token) {
            Self::initialize_session_with(
                session,
                effective_theme,
                themed_ui_roles,
                osc52_read,
                kitty_named_transports,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
                button_gates,
            );
        }
        self.reflow_active_panes_and_redraw();
    }

    /// Directional pane focus (tmux `Ctrl-b` arrows). A no-op in a single-pane
    /// tab (`multipane_geometry` is `None`), so the single-pane path is
    /// unaffected.
    pub(super) fn focus_pane_dir(&mut self, dir: FocusDir) {
        if let Some((content, _cell)) = self.multipane_geometry()
            && self
                .sessions
                .focus_move_active(content, PANE_DIVIDER_PX, dir)
        {
            self.on_active_session_changed();
        }
    }

    /// Close the focused pane (tmux `Ctrl-b x`). Collapses the split into its
    /// sibling; closing the last pane of the last tab exits, mirroring
    /// [`Self::close_active_tab`].
    pub(super) fn close_focused_pane(&mut self) {
        let focused = self.sessions.active_id();
        if self.sessions.close(focused) {
            self.pending_exit = true;
        } else {
            // If closing collapsed the active tab back to a single pane, cancel
            // any pending multiplexer prefix. Once single-pane, the prefix
            // engine is gated out of the input path (byte-identical), so a
            // stale pending state must not linger to swallow the next key. The
            // safe, least-surprising boundary: dropping to one pane returns the
            // tab to the plain single-pane input path immediately.
            if self.sessions.active_is_single_pane() {
                self.prefix_engine.cancel();
            }
            self.reflow_active_panes_and_redraw();
        }
    }

    /// Reflow every pane of the active tab to its laid-out sub-rect (after a
    /// structural change: split, close, equalize) and request a repaint. A
    /// single-pane tab resizes its lone pane to the full content rect, matching
    /// the window-resize path.
    pub(super) fn reflow_active_panes_and_redraw(&mut self) {
        if let Some((content, cell)) = self.multipane_geometry() {
            let pad = self.window_pad_px();
            self.sessions
                .resize_all_panes(content, cell.width, cell.height, PANE_DIVIDER_PX, pad);
        } else if let (Some(cell), Some((width_px, height_px, padding))) =
            (self.resolved_cell(), self.resolved_surface())
        {
            // Collapsed back to a single pane (the common case: closing one half
            // of a split). `multipane_geometry()` returns `None` once the tab is
            // single-pane, so the branch above is skipped — without this arm the
            // lone survivor keeps the narrow sub-grid it had as a split pane, and
            // text wrapping + selection stay clipped to the old half-width until
            // the next real window resize.
            //
            // Resize the survivor to the full content rect explicitly:
            // `resize_all_panes` over the full content sizes the tab's lone leaf
            // to the full grid (its single-pane arm). We can't lean on
            // `resize_grid_with_padding` alone here — `self.grid` is only ever the
            // *window* content grid, so it is already full at close time and that
            // call early-returns a no-op without ever resizing the (narrow)
            // survivor session. We still call it afterward to keep `self.grid`
            // current (wrapping + selection read it); it no-ops when unchanged,
            // so a genuinely single-pane tab is byte-identical here.
            let content = pane_content_rect(width_px, height_px, cell, padding, self.tab_reserve());
            self.sessions.resize_all_panes(
                content,
                cell.width,
                cell.height,
                PANE_DIVIDER_PX,
                padding.as_f32(),
            );
            let _ = self.resize_grid_with_padding(cell, padding, width_px, height_px);
        }
        self.on_active_session_changed();
    }

    /// Whole-app shutdown teardown, run once after the event loop exits.
    /// CLOSE-HANG: delegates to [`WorkspaceSet::shutdown_all`], which kills every
    /// child promptly then reaps + joins OFF the main thread under a bounded
    /// deadline. The previous serial per-session `pty.wait()` + `pump_thread.join()`
    /// on the main thread blocked indefinitely when a remote `ssh` link was
    /// wedged, which surfaced as a Super+Q not-responding stall with several
    /// remote workspaces open.
    pub(in crate::native) fn close_all_sessions(&mut self) {
        self.sessions
            .shutdown_all(crate::native::session::SHUTDOWN_REAP_DEADLINE);
    }
}
