// SPDX-License-Identifier: GPL-3.0-only
//! Working-directory checks performed before filesystem or spawn calls.

use std::path::Path;

/// Windows treats every pair of leading slash/backslash separators as a
/// network or device path. Unix keeps its local double-slash semantics.
pub(crate) fn permitted(path: &Path) -> bool {
    permitted_for_platform(path, cfg!(windows))
}

fn permitted_for_platform(path: &Path, windows: bool) -> bool {
    // OsStr's self-synchronizing encoding preserves ASCII separators, including
    // on Windows paths with unpaired UTF-16 surrogates. No lossy conversion,
    // canonicalization or filesystem lookup is needed for this prefix check.
    let bytes = path.as_os_str().as_encoded_bytes();
    !(windows && bytes.len() >= 2 && separator(bytes[0]) && separator(bytes[1]))
}

fn separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'\\')
}

/// A rejected directory never reaches the supplied filesystem probe.
pub(crate) fn existing_dir(path: &Path) -> bool {
    existing_dir_with(path, cfg!(windows), |path| {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_dir())
    })
}

fn existing_dir_with(path: &Path, windows: bool, probe: impl FnOnce(&Path) -> bool) -> bool {
    permitted_for_platform(path, windows) && probe(path)
}

/// An invalid explicit directory falls back to a permitted home directory.
/// No directory means the child inherits the default directory unchanged.
#[cfg(windows)]
pub(crate) fn spawn_directory(
    directory: Option<&Path>,
    home: Option<&Path>,
) -> Option<std::path::PathBuf> {
    directory
        .and_then(|path| {
            if permitted(path) {
                Some(path)
            } else {
                home.filter(|path| permitted(path))
            }
        })
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_network_and_device_cwds_never_reach_a_probe() {
        for path in [
            r"//fixture.invalid/share",
            r"\\fixture.invalid\share",
            r"/\fixture.invalid/share",
            r"\/fixture.invalid/share",
            r"\\?\C:\fixture",
            r"\\.\pipe\fixture",
            r"///fixture",
        ] {
            assert!(!existing_dir_with(Path::new(path), true, |_| panic!(
                "rejected cwd reached a probe"
            )));
        }
    }

    #[test]
    fn local_paths_and_unix_double_slashes_reach_the_probe() {
        for path in [r"C:\fixture", "C:/fixture", "relative", "/fixture"] {
            assert!(existing_dir_with(Path::new(path), true, |_| true));
        }
        assert!(existing_dir_with(Path::new("//fixture"), false, |_| true));
    }

    #[cfg(windows)]
    #[test]
    fn windows_spawn_directory_refuses_network_and_device_fallbacks() {
        let bad = Path::new(r"\\fixture.invalid\share");
        let home = Path::new(r"C:\fixture");
        assert_eq!(
            spawn_directory(Some(bad), Some(home)).as_deref(),
            Some(home)
        );
        assert_eq!(spawn_directory(Some(bad), Some(bad)), None);
        assert_eq!(spawn_directory(Some(bad), None), None);
        assert_eq!(spawn_directory(None, Some(home)), None);
        assert_eq!(spawn_directory(Some(home), None).as_deref(), Some(home));
    }
}
