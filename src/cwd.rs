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

/// Where a launch working directory came from. A directory the user typed is
/// honoured even when it is a Windows network or device path (a UNC share or
/// `\\wsl.localhost` distribution); a directory reported by a program or
/// recovered from earlier state is never trusted that far, because a terminal
/// program, a pasted escape sequence or a tampered state file could otherwise
/// aim a child, or the probe that precedes it, at an attacker-chosen share.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirectorySource {
    /// A profile's starting directory or the command-line working directory.
    Explicit,
    /// OSC 7, restored layouts and inherited (new tab, duplicate) directories.
    #[default]
    Derived,
}

impl DirectorySource {
    /// Whether `path` from this source may reach a spawn on `windows`.
    #[cfg(any(windows, test))]
    fn admits(self, path: &Path, windows: bool) -> bool {
        self == Self::Explicit || permitted_for_platform(path, windows)
    }
}

/// Whether a launch directory is usable for a spawn: an explicit network or
/// device directory is taken as typed and never probed (a probe is itself a
/// network access, and the spawn reports a missing share); every other
/// directory must be permitted and exist.
pub(crate) fn launch_dir_usable(path: &Path, source: DirectorySource) -> bool {
    launch_dir_usable_with(path, source, cfg!(windows), |path| {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_dir())
    })
}

/// [`launch_dir_usable`] for an explicit platform and probe, for callers that
/// inject them so the Windows rule is testable everywhere.
#[cfg(test)]
pub(crate) fn launch_dir_usable_for(
    path: &Path,
    source: DirectorySource,
    windows: bool,
    probe: impl FnOnce(&Path) -> bool,
) -> bool {
    launch_dir_usable_with(path, source, windows, probe)
}

fn launch_dir_usable_with(
    path: &Path,
    source: DirectorySource,
    windows: bool,
    probe: impl FnOnce(&Path) -> bool,
) -> bool {
    if source == DirectorySource::Explicit && !permitted_for_platform(path, windows) {
        return true;
    }
    existing_dir_with(path, windows, probe)
}

/// The directory a Windows spawn uses. An explicit directory is kept as typed;
/// a derived one that is refused falls back to a permitted home directory. No
/// directory means the child inherits the default directory unchanged.
#[cfg(windows)]
pub(crate) fn spawn_directory(
    directory: Option<&Path>,
    source: DirectorySource,
    home: Option<&Path>,
) -> Option<std::path::PathBuf> {
    spawn_directory_for(directory, source, home, cfg!(windows))
}

#[cfg(any(windows, test))]
fn spawn_directory_for(
    directory: Option<&Path>,
    source: DirectorySource,
    home: Option<&Path>,
    windows: bool,
) -> Option<std::path::PathBuf> {
    directory
        .and_then(|path| {
            if source.admits(path, windows) {
                Some(path)
            } else {
                home.filter(|path| permitted_for_platform(path, windows))
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

    const NETWORK_PATHS: [&str; 4] = [
        r"\\fixture.invalid\share",
        r"//fixture.invalid/share",
        r"\\wsl.localhost\Distro\home",
        r"\\?\C:\fixture",
    ];

    #[test]
    fn an_explicit_network_directory_is_usable_without_a_probe() {
        for path in NETWORK_PATHS {
            assert!(launch_dir_usable_with(
                Path::new(path),
                DirectorySource::Explicit,
                true,
                |_| panic!("an explicit network directory reached a probe")
            ));
        }
    }

    #[test]
    fn a_derived_network_directory_is_refused_without_a_probe() {
        for path in NETWORK_PATHS {
            assert!(!launch_dir_usable_with(
                Path::new(path),
                DirectorySource::Derived,
                true,
                |_| panic!("a derived network directory reached a probe")
            ));
        }
    }

    #[test]
    fn explicit_local_directories_still_need_to_exist() {
        let local = Path::new(r"C:\fixture");
        assert!(launch_dir_usable_with(
            local,
            DirectorySource::Explicit,
            true,
            |_| true
        ));
        assert!(!launch_dir_usable_with(
            local,
            DirectorySource::Explicit,
            true,
            |_| false
        ));
        // Off Windows a double slash is an ordinary path in either case.
        assert!(!launch_dir_usable_with(
            Path::new("//fixture"),
            DirectorySource::Explicit,
            false,
            |_| false
        ));
    }

    #[test]
    fn the_spawn_sink_keeps_an_explicit_network_directory_only() {
        let home = Path::new(r"C:\fixture");
        for path in NETWORK_PATHS {
            let bad = Path::new(path);
            assert_eq!(
                spawn_directory_for(Some(bad), DirectorySource::Explicit, Some(home), true)
                    .as_deref(),
                Some(bad),
                "{path}"
            );
            assert_eq!(
                spawn_directory_for(Some(bad), DirectorySource::Derived, Some(home), true)
                    .as_deref(),
                Some(home),
                "{path}"
            );
            assert_eq!(
                spawn_directory_for(Some(bad), DirectorySource::Derived, Some(bad), true),
                None,
                "{path}"
            );
            assert_eq!(
                spawn_directory_for(Some(bad), DirectorySource::Derived, None, true),
                None,
                "{path}"
            );
        }
        assert_eq!(
            spawn_directory_for(None, DirectorySource::Derived, Some(home), true),
            None
        );
        assert_eq!(
            spawn_directory_for(Some(home), DirectorySource::Derived, None, true).as_deref(),
            Some(home)
        );
        // Unix keeps its double-slash semantics for every source.
        let unix = Path::new("//fixture");
        assert_eq!(
            spawn_directory_for(Some(unix), DirectorySource::Derived, None, false).as_deref(),
            Some(unix)
        );
    }
}
