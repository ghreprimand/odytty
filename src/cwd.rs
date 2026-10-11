// SPDX-License-Identifier: GPL-3.0-only
//! Working-directory checks performed before filesystem or spawn calls.

use std::path::Path;

/// Windows treats every pair of leading slash/backslash separators as a
/// network or device path. Unix keeps its local double-slash semantics.
pub(crate) fn permitted(path: &Path) -> bool {
    permitted_for_platform(path, cfg!(windows))
}

/// [`permitted`] for an explicit platform, for callers that inject it so the
/// Windows rule is testable everywhere.
pub(crate) fn permitted_for(path: &Path, windows: bool) -> bool {
    permitted_for_platform(path, windows)
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

/// The message for a working directory that failed [`existing_dir`]: a network
/// or device path is refused by policy even when it exists, so it says so
/// instead of claiming the directory is missing.
pub(crate) fn unusable_message(path: &Path, shown: &str) -> String {
    unusable_message_for_platform(path, shown, cfg!(windows))
}

fn unusable_message_for_platform(path: &Path, shown: &str, windows: bool) -> String {
    if permitted_for_platform(path, windows) {
        format!("working directory {shown:?} does not exist")
    } else {
        format!("working directory {shown:?} is a network or device path, which is not allowed")
    }
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
    fn a_refused_directory_is_not_reported_as_missing() {
        for path in [
            r"\\wsl.localhost\Ubuntu\home",
            r"//wsl$/Ubuntu",
            r"\\?\C:\fixture",
        ] {
            let message = unusable_message_for_platform(Path::new(path), path, true);
            assert!(message.contains("network or device path"), "{message}");
            assert!(!message.contains("does not exist"), "{message}");
        }
        // A permitted path that failed the probe really is missing, and a
        // double slash is an ordinary path off Windows.
        assert!(
            unusable_message_for_platform(Path::new(r"C:\fixture"), "C:", true)
                .contains("does not exist")
        );
        assert!(
            unusable_message_for_platform(Path::new("//fixture"), "//fixture", false)
                .contains("does not exist")
        );
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
