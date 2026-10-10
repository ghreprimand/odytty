// SPDX-License-Identifier: GPL-3.0-only
//! Config/theme path resolution and theme-file lookup.

use super::*;

pub fn config_file_path() -> Option<PathBuf> {
    config_base_dir_from_env(
        std::env::var_os("APPDATA"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .map(|dir| dir.join(CONFIG_FILE_NAME))
}

/// The OdyTTY config directory a session-spawn writer (the Unix
/// shell-integration wrappers) puts its files in: the parent of
/// [`config_file_path`].
#[cfg(all(unix, not(test)))]
pub(crate) fn spawn_writer_config_dir() -> Option<PathBuf> {
    config_file_path()?.parent().map(Path::to_path_buf)
}

/// Test builds never resolve a spawn writer's directory from the live
/// environment: a session spawned by one test thread would otherwise read
/// `HOME`/`XDG_CONFIG_HOME` without the shared environment lock and write into
/// the real config directory, another test's redirected base, or a relative
/// synthetic base. A thread inside the shared config fixture uses the config
/// directory that fixture resolved while holding the lock; every other thread
/// uses one directory owned by this test process.
#[cfg(all(unix, test))]
pub(crate) fn spawn_writer_config_dir() -> Option<PathBuf> {
    static PROCESS_OWNED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let redirected = TEST_SPAWN_CONFIG_DIR.with(|dir| dir.borrow().clone());
    Some(redirected.unwrap_or_else(|| {
        PROCESS_OWNED
            .get_or_init(|| crate::test_dirs::fresh_temp_dir("odytty-test-config-"))
            .join(CONFIG_DIR_NAME)
    }))
}

#[cfg(test)]
thread_local! {
    static TEST_SPAWN_CONFIG_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Marks the current thread as redirected to `dir` for
/// [`spawn_writer_config_dir`] until dropped, then restores the previous mark.
/// Only the shared config fixture creates one, while it holds the environment
/// lock.
#[cfg(test)]
pub(crate) struct SpawnConfigDirMark(Option<PathBuf>);

#[cfg(test)]
impl SpawnConfigDirMark {
    pub(crate) fn set(dir: Option<PathBuf>) -> Self {
        Self(TEST_SPAWN_CONFIG_DIR.with(|slot| slot.replace(dir)))
    }
}

#[cfg(test)]
impl Drop for SpawnConfigDirMark {
    fn drop(&mut self) {
        let previous = self.0.take();
        TEST_SPAWN_CONFIG_DIR.with(|slot| slot.replace(previous));
    }
}

/// Resolve the OdyTTY config directory (`<base>/odytty`) from the relevant
/// environment values, following the platform base rules: on Windows
/// `%APPDATA%\\odytty` when APPDATA is set (falling through when it is not),
/// then `$XDG_CONFIG_HOME/odytty`, then `$HOME/.config/odytty`. Pure and
/// testable; the public wrappers pass the live process env and append the
/// file/dir leaf. `None` when nothing resolves.
pub(crate) fn config_base_dir_from_env(
    appdata: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let non_empty = |value: OsString| (!value.is_empty()).then(|| PathBuf::from(value));

    #[cfg(windows)]
    if let Some(base) = appdata.and_then(non_empty) {
        return Some(base.join(CONFIG_DIR_NAME));
    }
    #[cfg(not(windows))]
    let _ = &appdata;

    if let Some(base) = xdg_config_home.and_then(non_empty) {
        return Some(base.join(CONFIG_DIR_NAME));
    }

    home.and_then(non_empty)
        .map(|home| home.join(".config").join(CONFIG_DIR_NAME))
}

/// Resolved user theme directory (`<config-dir>/odytty/themes`), mirroring
/// [`config_file_path`]'s base-directory rules. `ODYTTY_THEME` values that are
/// not built-in names are looked up here (by `<name>.theme` or `<name>`).
pub fn theme_dir_path() -> Option<PathBuf> {
    config_base_dir_from_env(
        std::env::var_os("APPDATA"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .map(|dir| dir.join(THEME_DIR_NAME))
}

/// Read a user theme file for an `ODYTTY_THEME` value that is not a built-in
/// name. Resolution order:
///
/// 1. A path-like value (contains a separator or ends in `.theme`) is read
///    directly.
/// 2. Otherwise the value is looked up in `theme_dir` as `<value>.theme` and
///    then `<value>`.
///
/// Returns the file contents, or `None` when nothing resolves (caller falls
/// back to plain). All IO errors are swallowed into `None` — a bad theme value
/// must never abort startup.
pub(crate) fn resolve_theme_file(value: &str, theme_dir: Option<&Path>) -> Option<String> {
    let looks_like_path = value.contains('/') || value.ends_with(".theme");
    if looks_like_path && let Ok(contents) = fs_read::read_capped(Path::new(value)) {
        return Some(contents);
    }
    let dir = theme_dir?;
    let named = dir.join(format!("{value}.theme"));
    if let Ok(contents) = fs_read::read_capped(&named) {
        return Some(contents);
    }
    fs_read::read_capped(&dir.join(value)).ok()
}

pub fn normalize_name(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}
