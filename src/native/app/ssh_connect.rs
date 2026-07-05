// SPDX-License-Identifier: GPL-3.0-only
//! App-side SSH connect action.
//!
//! The connection-manager overlay owns list/filter/select presentation. This
//! module owns the side effect after selection: spawn a new session whose PTY
//! child is the system `ssh` binary. OdyTTY only constructs argv from name-only
//! fields and never handles credentials or key material.

use super::*;
use crate::connection_hosts::{
    AppendHostOutcome, ConnectionHost, HostsEditOutcome, append_adhoc_host, edit_host_block,
    hosts_file_path, remove_host_block,
};

impl App {
    /// Append an ad-hoc host to the OdyTTY-owned `hosts.conf` (ADHOC-CONNECT
    /// save offer). Resolves the same config dir the connection manager loads
    /// from; a missing config dir, an exact-alias collision, or a write error
    /// each surface a one-line notice and never disturb the just-opened
    /// connection. Windows: the config dir resolves through the same settings
    /// path logic, so the write lands under `%APPDATA%`/the platform config dir
    /// exactly as on Unix.
    pub(in crate::native) fn save_adhoc_host(&mut self, host: &ConnectionHost) {
        let Some(config_dir) = self
            .settings_reloader
            .config_path()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        else {
            self.raise_open_notice("Could not locate hosts.conf to save the host".to_owned());
            return;
        };
        let path = hosts_file_path(&config_dir);
        match append_adhoc_host(&path, host) {
            Ok(AppendHostOutcome::Appended) => {
                self.raise_open_notice(format!("Saved \"{}\" to hosts.conf", host.alias));
            }
            Ok(AppendHostOutcome::AlreadyExists) => {
                self.raise_open_notice(format!("\"{}\" is already saved", host.alias));
            }
            Err(error) => {
                tracing::warn!("failed to save ad-hoc host to hosts.conf: {error}");
                self.raise_open_notice("Could not save the host to hosts.conf".to_owned());
            }
        }
    }

    /// Persist a host built in the Add / Edit connection form (REMOTE-UX P4).
    /// `edit_target` `None` appends a new block; `Some(alias)` byte-splices over
    /// the existing block owning that alias, leaving every other block, comment,
    /// and unknown field untouched. Resolves the same config dir the connection
    /// manager loads from; a missing config dir or a write error surfaces a
    /// one-line notice and never panics. The form already guarded the
    /// alias-collision case, so an unexpected `AlreadyExists` is reported rather
    /// than silently dropped. Windows: the config dir resolves through the same
    /// settings path logic, so the write lands under the platform config dir
    /// exactly as on Unix.
    pub(in crate::native) fn persist_connection_form(
        &mut self,
        host: &ConnectionHost,
        edit_target: Option<String>,
    ) {
        let Some(config_dir) = self
            .settings_reloader
            .config_path()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        else {
            self.raise_open_notice("Could not locate hosts.conf to save the host".to_owned());
            return;
        };
        let path = hosts_file_path(&config_dir);
        match edit_target {
            Some(original) => match edit_host_block(&path, &original, host) {
                Ok(HostsEditOutcome::Written) => {
                    self.raise_open_notice(format!("Updated \"{}\"", host.alias));
                }
                Ok(HostsEditOutcome::NotFound) => {
                    self.raise_open_notice(format!("\"{original}\" is no longer in hosts.conf"));
                }
                Err(error) => {
                    tracing::warn!("failed to edit host in hosts.conf: {error}");
                    self.raise_open_notice("Could not update the host in hosts.conf".to_owned());
                }
            },
            None => match append_adhoc_host(&path, host) {
                Ok(AppendHostOutcome::Appended) => {
                    self.raise_open_notice(format!("Saved \"{}\" to hosts.conf", host.alias));
                }
                Ok(AppendHostOutcome::AlreadyExists) => {
                    self.raise_open_notice(format!("\"{}\" is already saved", host.alias));
                }
                Err(error) => {
                    tracing::warn!("failed to save host to hosts.conf: {error}");
                    self.raise_open_notice("Could not save the host to hosts.conf".to_owned());
                }
            },
        }
    }

    /// Run a Test Connection probe for a host built in the Add / Edit form
    /// (REMOTE-UX P4 / ODP-8) on a background thread, storing the receiver so
    /// `run_about_to_wait_maintenance` can drain the tri-state result into the
    /// open form. The argv is the same non-interactive `BatchMode` one-shot the
    /// probe builder produces; a build error (an invalid host) surfaces in the
    /// form rather than spawning. Windows: the probe uses `ssh.exe` the same
    /// way; no `ControlPath` is ever added.
    pub(in crate::native) fn run_connection_probe(&mut self, host: &ConnectionHost) {
        let command = match crate::ssh_connect::ssh_probe_command_for_host(host) {
            Ok(command) => command,
            Err(_) => {
                self.overlay
                    .set_connection_form_test_result(Err("invalid host for a probe".to_owned()));
                return;
            }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.connection_probe = Some(rx);
        let proxy = self.sessions.event_proxy();
        let session = self.sessions.active_id();
        super::connection_probe::spawn_connection_probe(command, session, proxy, tx);
    }

    /// Drain a completed Test Connection probe (if any) into the open form and
    /// wake a repaint. Idle when no probe is in flight. Called from the
    /// about-to-wait maintenance pass.
    pub(in crate::native) fn poll_connection_probe(&mut self) {
        let Some(rx) = self.connection_probe.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.overlay.set_connection_form_test_result(result);
                self.connection_probe = None;
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.connection_probe = None;
            }
        }
    }

    /// Hand-off seam for the connection-manager overlay: consume a resolved
    /// connection entry and present it as a focused new tab.
    pub(in crate::native) fn connect_ssh_host_in_new_tab(
        &mut self,
        host: &ConnectionHost,
    ) -> std::io::Result<SessionToken> {
        let integration_enabled = crate::ssh_connect::remote_integration_enabled(
            host.integration,
            self.settings.remote_integration,
        );
        let reuse_enabled =
            crate::ssh_connect::remote_reuse_enabled(host.reuse, self.settings.remote_reuse);
        // tmux persistence rides inside the integration bootstrap, so it is only
        // meaningful when integration is on.
        let tmux_enabled = integration_enabled
            && crate::ssh_connect::remote_tmux_enabled(host.tmux, self.settings.remote_tmux);
        // Resolve the ControlPersist window for the reuse master: a per-host
        // `Persist` override (any ssh ControlPersist value) wins, else the global
        // `remote_persist` knob. The default (`10m`) yields `600`, byte-identical
        // to the historical fixed window. Inert on a Windows client (the reuse
        // control options are compiled out there).
        let control_persist =
            Self::resolve_control_persist(host.persist.as_deref(), self.settings.remote_persist);
        let opts = crate::ssh_connect::RemoteSshOptions {
            integration: integration_enabled,
            reuse: reuse_enabled,
            tmux: tmux_enabled,
            control_dir: Self::ssh_control_dir(integration_enabled && reuse_enabled),
            control_persist: Some(control_persist),
        };
        let token = self
            .sessions
            .connect_ssh_in_new_tab(host, self.grid, &opts)?;
        let effective_theme = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let osc52_read = self.settings.osc52_read;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink;
        let cell = self.gpu.as_ref().map(GpuState::cell);
        let scrollback_limit = self.settings.scrollback_limit();
        if let Some(session) = self.sessions.get_mut(token) {
            Self::initialize_session_with(
                session,
                effective_theme,
                themed_ui_roles,
                osc52_read,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
            );
        }
        let _ = self.sessions.switch(token);
        self.on_active_session_changed();
        Ok(token)
    }

    /// Open `host` in a fresh workspace pre-bound to it (ODP-2C "Open in New
    /// Workspace"). Creates a new workspace (which spawns a placeholder local
    /// first tab), sets its `default_profile` so future New Tabs there route
    /// through the SSH connect path, then connects the host as the workspace's
    /// tab and drops the placeholder — connect-then-close, so a connect failure
    /// leaves a usable local tab rather than an empty workspace. No bind toast:
    /// the connecting remote tab is the visible confirmation. Windows: the
    /// connect uses the same `ssh.exe` path; workspace mechanics are
    /// platform-neutral.
    pub(in crate::native) fn open_host_in_new_workspace(&mut self, host: &ConnectionHost) {
        self.handle_new_workspace();
        let placeholder = self.sessions.active_id();
        self.sessions
            .set_active_workspace_default_profile(Some(host.alias.clone()));
        if self.connect_ssh_host_in_new_tab(host).is_ok() {
            self.close_tab_by_token(placeholder);
            self.on_active_session_changed();
        }
    }

    /// Delete the OdyTTY-owned `hosts.conf` block for `host` (ODP-2C "Remove…",
    /// P1 byte-splice) and reopen the connection manager so the removed row
    /// disappears — mirroring the Manage-Sessions kill-confirm reopen. Resolves
    /// the same config dir the manager loads from; a missing config dir, an
    /// already-gone alias, or a write error each surface a one-line notice and
    /// never panic. Only OdyTTY-owned rows reach here (the menu hides Remove for
    /// ssh-config rows), so `~/.ssh/config` is never touched. Windows: the
    /// config dir resolves through the same settings path logic, and the atomic
    /// remove works on both platforms.
    pub(in crate::native) fn remove_saved_host(&mut self, host: &ConnectionHost) {
        let config_dir = self
            .settings_reloader
            .config_path()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
        match config_dir {
            Some(config_dir) => {
                let path = hosts_file_path(&config_dir);
                match remove_host_block(&path, &host.alias) {
                    Ok(HostsEditOutcome::Written) => {
                        self.raise_open_notice(format!("Removed \"{}\"", host.alias));
                    }
                    Ok(HostsEditOutcome::NotFound) => {
                        self.raise_open_notice(format!(
                            "\"{}\" is no longer in hosts.conf",
                            host.alias
                        ));
                    }
                    Err(error) => {
                        tracing::warn!("failed to remove host from hosts.conf: {error}");
                        self.raise_open_notice(
                            "Could not remove the host from hosts.conf".to_owned(),
                        );
                    }
                }
            }
            None => {
                self.raise_open_notice("Could not locate hosts.conf to remove the host".to_owned());
            }
        }
        // Reopen the manager over the refreshed list so the removed row is gone.
        self.open_connection_overlay();
    }

    /// Bind the active workspace to the known host at list index `idx` (F6-W5 /
    /// ODP-9), so New Tab in that workspace routes through the SSH connect path.
    /// The index is into the same `load_connection_entries` order the palette
    /// built its rows from; an out-of-range index (host list changed under the
    /// open palette) is a no-op. A one-line notice confirms the binding and
    /// spells out what it changes — every new tab/pane in this workspace now
    /// spawns on the remote, and "New Local Tab" is the escape hatch.
    pub(in crate::native) fn bind_active_workspace_to_host_index(&mut self, idx: usize) {
        let Some(host) = self.load_connection_entries().into_iter().nth(idx) else {
            return;
        };
        self.bind_active_workspace_to_host_alias(host.alias);
    }

    /// Bind the active workspace to a host by its alias (ODP-6B). Shared tail of
    /// the palette index path and the ODP-1B host-picker path; a one-line notice
    /// spells out what the binding changes (every new tab/pane spawns on the
    /// remote; "New Local Tab" is the escape hatch).
    pub(in crate::native) fn bind_active_workspace_to_host_alias(&mut self, alias: String) {
        self.sessions
            .set_active_workspace_default_profile(Some(alias.clone()));
        self.raise_open_notice(workspace_bound_notice(&alias));
    }

    /// Open the shared host picker (ODP-1B) seeded for binding the active
    /// workspace (ODP-6B). Reuses the connection-manager list — the same
    /// OdyTTY-owned hosts plus any opt-in ssh-config names — with a tagged
    /// purpose so accepting a row binds instead of connecting. With no saved
    /// hosts the picker still opens and shows its empty-state hint.
    pub(in crate::native) fn open_bind_workspace_picker(&mut self) {
        let entries = self.load_connection_entries();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_connections_for_purpose(
            entries,
            crate::native::connection_overlay::ConnectionPickerPurpose::BindWorkspace,
        );
        self.request_selection_redraw();
    }

    /// Open the shared host picker (ODP-1B) seeded to bind the workspace at rail
    /// index `idx` (RAIL-BIND). Same as [`Self::open_bind_workspace_picker`] but
    /// the pick routes back to the CLICKED slot rather than the active
    /// workspace, so a rail context menu can bind a workspace without switching
    /// to it first.
    pub(in crate::native) fn open_bind_workspace_at_picker(&mut self, idx: usize) {
        let entries = self.load_connection_entries();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_connections_for_purpose(
            entries,
            crate::native::connection_overlay::ConnectionPickerPurpose::BindWorkspaceIndex(idx),
        );
        self.request_selection_redraw();
    }

    /// Bind the workspace at rail index `idx` to a host by alias (RAIL-BIND).
    /// The by-index counterpart to [`Self::bind_active_workspace_to_host_alias`];
    /// the one-line notice states plainly that the binding routes NEW tabs to
    /// the remote (existing tabs keep their shells).
    pub(in crate::native) fn bind_workspace_at_to_host_alias(&mut self, idx: usize, alias: String) {
        self.sessions
            .set_workspace_default_profile_at(idx, Some(alias.clone()));
        // Confirm the bind landed on a real slot (an out-of-range index is a
        // no-op) before announcing it.
        if self.sessions.workspace_default_profile_at(idx) == Some(alias.as_str()) {
            self.raise_open_notice(workspace_bound_notice(&alias));
        }
    }

    /// Clear the host binding of the workspace at rail index `idx` (RAIL-BIND),
    /// returning its New Tab to a local shell. The by-index counterpart to
    /// [`Self::unbind_active_workspace`]; unbinding an already-local workspace is
    /// a silent no-op.
    pub(in crate::native) fn unbind_workspace_at(&mut self, idx: usize) {
        if self
            .sessions
            .set_workspace_default_profile_at(idx, None)
            .is_some()
        {
            self.raise_open_notice(WORKSPACE_UNBOUND_NOTICE.to_owned());
        }
    }

    /// Open the shared host picker (ODP-1B) seeded to open a host in a new tab
    /// right after the tab that owns `token` (ODP-5D "Connect to host ▸"). The
    /// pick routes back through [`Self::connect_host_in_tab_after`]. With no
    /// saved hosts the picker still opens and shows its empty-state hint.
    pub(in crate::native) fn open_connect_tab_after_picker(&mut self, token: SessionToken) {
        let entries = self.load_connection_entries();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_connections_for_purpose(
            entries,
            crate::native::connection_overlay::ConnectionPickerPurpose::ConnectTabAfter(token),
        );
        self.request_selection_redraw();
    }

    /// Open the shared host picker (ODP-1B) seeded to REPLACE the tab that owns
    /// `token` with a saved host (ODP-5D "Replace this tab with ▸"). The pick
    /// routes back through [`Self::replace_tab_with_host`], which gates the
    /// destructive close behind a confirm when that tab is busy.
    pub(in crate::native) fn open_replace_tab_picker(&mut self, token: SessionToken) {
        let entries = self.load_connection_entries();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_connections_for_purpose(
            entries,
            crate::native::connection_overlay::ConnectionPickerPurpose::ReplaceTab(token),
        );
        self.request_selection_redraw();
    }

    /// Open `host` in a NEW tab positioned right after the tab that owns `anchor`
    /// (ODP-5D). The connect path appends the tab and switches to it; the
    /// reposition then slides it into the neighbour slot so it reads as
    /// "connect from here" without disturbing the clicked shell. A connect
    /// failure is swallowed like every other connect path.
    pub(in crate::native) fn connect_host_in_tab_after(
        &mut self,
        host: &ConnectionHost,
        anchor: SessionToken,
    ) {
        if self.connect_ssh_host_in_new_tab(host).is_ok() {
            self.sessions.reposition_active_tab_after(anchor);
            self.on_active_session_changed();
        }
    }

    /// Replace the tab that owns `target` with `host` (ODP-5D). When that tab
    /// holds a running foreground child the destructive close is gated behind a
    /// confirm dialog carrying the pending host + token; an idle tab (shell at
    /// its prompt) replaces directly with no prompt.
    pub(in crate::native) fn replace_tab_with_host(
        &mut self,
        host: Box<ConnectionHost>,
        target: SessionToken,
    ) {
        if self.sessions.tab_foreground_job_running(target) {
            self.reset_pointer_state_for_overlay();
            self.overlay.open_confirm_replace_tab(host, target);
            self.request_selection_redraw();
        } else {
            self.do_replace_tab_with_host(&host, target);
        }
    }

    /// Perform the replace (ODP-5D): open `host` adjacent to `target`, then close
    /// `target` so the remote lands in its former slot. Ordered connect-then-
    /// close so the anchor still exists when the new tab is placed, and so a
    /// connect failure leaves the original tab untouched (no data loss).
    pub(in crate::native) fn do_replace_tab_with_host(
        &mut self,
        host: &ConnectionHost,
        target: SessionToken,
    ) {
        if self.connect_ssh_host_in_new_tab(host).is_ok() {
            self.sessions.reposition_active_tab_after(target);
            self.close_tab_by_token(target);
            self.on_active_session_changed();
        }
    }

    /// Clear the active workspace's host binding (F6-W5), returning New Tab there
    /// to spawning a local shell. A one-line notice confirms new tabs are local
    /// again; unbinding an already-local workspace is a silent no-op.
    pub(in crate::native) fn unbind_active_workspace(&mut self) {
        if self
            .sessions
            .set_active_workspace_default_profile(None)
            .is_some()
        {
            self.raise_open_notice(WORKSPACE_UNBOUND_NOTICE.to_owned());
        }
    }

    /// Resolve the `ControlPersist=` token for a host's reuse master: a per-host
    /// `Persist` override (any recognized ssh ControlPersist value) wins,
    /// otherwise the global `remote_persist` knob. The global default (`10m`)
    /// resolves to `600`, so the emitted argv is byte-identical to the historical
    /// fixed window; an unparseable per-host override falls back to the global.
    /// Platform-neutral resolution — the token is only ever emitted on Unix
    /// (the reuse control options are compiled out on a Windows client).
    fn resolve_control_persist(
        host_persist: Option<&str>,
        global: crate::settings::RemotePersist,
    ) -> String {
        match host_persist {
            Some(raw) => crate::ssh_connect::parse_control_persist(raw)
                .unwrap_or_else(|| global.control_persist_value().to_owned()),
            None => global.control_persist_value().to_owned(),
        }
    }

    /// Resolve the directory OdyTTY owns for `ControlMaster` sockets, creating it
    /// with owner-only `0700` permissions. Returns `None` — disabling connection
    /// reuse — when reuse is off, the state dir is unresolvable, or the directory
    /// cannot be prepared. On a Windows client this is always `None`: OpenSSH
    /// there has no socket multiplexing, so no control options are ever emitted.
    #[cfg(unix)]
    fn ssh_control_dir(enabled: bool) -> Option<std::path::PathBuf> {
        if !enabled {
            return None;
        }
        let dir = crate::logging::state_log_dir().join("ssh");
        match crate::ssh_connect::ensure_control_dir(&dir) {
            Ok(()) => Some(dir),
            Err(error) => {
                tracing::warn!("ssh connection reuse disabled: {error}");
                None
            }
        }
    }

    #[cfg(windows)]
    fn ssh_control_dir(_enabled: bool) -> Option<std::path::PathBuf> {
        None
    }
}

/// The one-line notice raised when a workspace is bound to a host (W5-BIND-TOAST).
/// It confirms the binding AND spells out the behavior change — every new tab in
/// this workspace now spawns on the remote, and "New Local Tab" is the escape.
fn workspace_bound_notice(alias: &str) -> String {
    format!("Workspace bound to {alias} — new tabs open there; New Local Tab escapes")
}

/// The notice raised when a workspace binding is cleared (W5-BIND-TOAST). Host-
/// agnostic: it states the new behavior (new tabs are local again) rather than
/// naming the host that was unbound.
const WORKSPACE_UNBOUND_NOTICE: &str = "Workspace unbound — new tabs open locally";

#[cfg(test)]
mod bind_notice_tests {
    use super::{WORKSPACE_UNBOUND_NOTICE, workspace_bound_notice};

    #[test]
    fn bind_notice_names_the_host_and_the_escape() {
        // W5-BIND-TOAST: the bind toast must carry the host, that new tabs open
        // there, and the local-escape hatch so the surface is discoverable.
        let notice = workspace_bound_notice("prod@edge");
        assert!(notice.contains("prod@edge"), "names the host: {notice}");
        assert!(
            notice.contains("new tabs open there"),
            "states routing: {notice}"
        );
        assert!(
            notice.contains("New Local Tab"),
            "names the escape: {notice}"
        );
    }

    #[test]
    fn unbind_notice_states_local_without_a_host() {
        assert!(
            WORKSPACE_UNBOUND_NOTICE.contains("unbound")
                && WORKSPACE_UNBOUND_NOTICE.contains("local"),
            "unbind toast states new tabs are local: {WORKSPACE_UNBOUND_NOTICE}"
        );
    }
}
