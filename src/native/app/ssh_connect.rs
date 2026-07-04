// SPDX-License-Identifier: GPL-3.0-only
//! App-side SSH connect action.
//!
//! The connection-manager overlay owns list/filter/select presentation. This
//! module owns the side effect after selection: spawn a new session whose PTY
//! child is the system `ssh` binary. OdyTTY only constructs argv from name-only
//! fields and never handles credentials or key material.

use super::*;
use crate::connection_hosts::ConnectionHost;

impl App {
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
