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
