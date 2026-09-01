// SPDX-License-Identifier: GPL-3.0-only
//! App-side connection-manager integration (Phase 4).
//!
//! The overlay owns list/filter/select presentation. This module owns only the
//! act of opening it: it loads the merged local connection list through the
//! [`crate::connection_hosts`] data layer and hands a frozen clone to the
//! overlay. Opening is presentation-only — it never writes to the PTY and never
//! mutates the live terminal model.
//!
//! Privacy: the OpenSSH-config path (`~/.ssh/config`) is only ever resolved when
//! the `ssh_config_hosts` opt-in is enabled. With the opt-in off this module
//! never even forms the `~/.ssh` path, so the overlay shows OdyTTY-owned hosts
//! only and nothing under `~/.ssh` is read. Import, when enabled, is name-only
//! through the bounded parser in the data layer; no key material is ever read.

use std::path::Path;

use super::*;
use crate::connection_hosts::{
    ConnectionHost, ConnectionHostPaths, hosts_file_path, load_connection_hosts,
};

impl App {
    /// Open the connection-manager overlay over the merged local connection
    /// list. The entry list is a frozen clone, so it stays stable while the
    /// overlay is open even if the underlying files change. When no hosts are
    /// configured the list is empty and the overlay shows a hint rather than
    /// failing to open.
    pub(super) fn open_connection_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        let entries = self.load_connection_entries();
        let catalog = super::profile_launch::load_profile_catalog();
        let profile_rows = super::profile_launch::connection_profile_rows_for_manager(&catalog);
        self.reset_pointer_state_for_overlay();
        self.overlay.open_connections(entries, profile_rows);
        self.request_selection_redraw();
    }

    /// Load the merged connection list from the OdyTTY-owned `hosts.conf` and,
    /// only when the opt-in is enabled, the name-only OpenSSH-config import.
    pub(super) fn load_connection_entries(&self) -> Vec<ConnectionHost> {
        let Some(config_dir) = self
            .settings_reloader
            .config_path()
            .and_then(|path| path.parent().map(Path::to_path_buf))
        else {
            return Vec::new();
        };
        // Route through `restore_home_dir` so the OpenSSH-config import finds
        // `%USERPROFILE%\\.ssh\\config` on Windows, not a never-set `$HOME`.
        let home = crate::native::persistence::restore_home_dir();
        let paths =
            resolve_connection_paths(&config_dir, self.settings.ssh_config_hosts, home.as_deref());
        load_connection_hosts(&self.settings, &paths)
    }
}

/// Resolve the two local source paths.
///
/// The OdyTTY-owned hosts file always resolves under the config dir. The
/// OpenSSH-config path is resolved **only** when `ssh_config_hosts` is true —
/// with the opt-in off this returns `ssh_config: None` so OdyTTY never even
/// forms a path under `~/.ssh`. This is the App-side half of the privacy
/// guarantee; the data-layer half refuses to read the path unless the same
/// opt-in is set.
fn resolve_connection_paths(
    config_dir: &Path,
    ssh_config_hosts: bool,
    home: Option<&Path>,
) -> ConnectionHostPaths {
    let odytty_hosts = hosts_file_path(config_dir);
    let ssh_config = if ssh_config_hosts {
        home.map(|home| home.join(".ssh").join("config"))
    } else {
        None
    };
    ConnectionHostPaths::new(odytty_hosts, ssh_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_hosts::CONNECTION_HOSTS_FILE_NAME;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("create synthetic temp dir");
        path
    }

    #[test]
    fn opt_in_off_never_forms_ssh_config_path() {
        // PRIVACY: with the opt-in off, the resolver returns ssh_config: None
        // even when HOME is present, so OdyTTY never references ~/.ssh at all.
        let home = PathBuf::from("/home/synthetic-user");
        let paths = resolve_connection_paths(Path::new("/cfg"), false, Some(&home));
        assert!(paths.ssh_config.is_none());
        assert_eq!(paths.odytty_hosts, PathBuf::from("/cfg/hosts.conf"));
    }

    #[test]
    fn opt_in_on_forms_name_only_ssh_config_path() {
        let home = PathBuf::from("/home/synthetic-user");
        let paths = resolve_connection_paths(Path::new("/cfg"), true, Some(&home));
        assert_eq!(
            paths.ssh_config,
            Some(PathBuf::from("/home/synthetic-user/.ssh/config"))
        );
    }

    #[test]
    fn opt_in_off_loads_only_odytty_hosts() {
        // OPT-IN-OFF-SHOWS-ONLY-ODYTTY-HOSTS: even with a synthetic ssh config on
        // disk, the disabled opt-in path never reads it.
        let dir = temp_dir("odytty-connections-ui");
        let hosts_path = dir.join(CONNECTION_HOSTS_FILE_NAME);
        let ssh_path = dir.join(".ssh-config-synthetic");
        fs::write(&hosts_path, b"Host owned\nHostName owned.example.invalid\n")
            .expect("write synthetic owned hosts");
        fs::write(&ssh_path, b"Host remote\nHostName remote.example.invalid\n")
            .expect("write synthetic ssh config");

        // Disabled: resolver yields no ssh path, so only OdyTTY hosts load.
        let off = resolve_connection_paths(&dir, false, Some(&dir));
        let off_entries = load_connection_hosts(&crate::settings::Settings::default(), &off);
        assert_eq!(
            off_entries
                .iter()
                .map(|entry| entry.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["owned"]
        );

        fs::remove_dir_all(dir).ok();
    }
}
