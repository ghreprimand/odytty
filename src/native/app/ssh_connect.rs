// SPDX-License-Identifier: GPL-3.0-only
//! App-side SSH connect action.
//!
//! The connection-manager overlay owns list/filter/select presentation. This
//! module owns the side effect after selection: spawn a new session whose PTY
//! child is the system `ssh` binary. OdyTTY only constructs argv from name-only
//! fields and never handles credentials or key material.

use super::*;
use crate::connection_hosts::{
    AppendHostOutcome, ConnectionHost, append_adhoc_host, hosts_file_path,
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
        let opts = crate::ssh_connect::RemoteSshOptions {
            integration: integration_enabled,
            reuse: reuse_enabled,
            tmux: tmux_enabled,
            control_dir: Self::ssh_control_dir(integration_enabled && reuse_enabled),
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
