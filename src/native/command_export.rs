// SPDX-License-Identifier: GPL-3.0-only
//! Private atomic writer for explicit plain-text command-output export.

use std::path::Path;

pub(super) const MAX_COMMAND_EXPORT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandExportError {
    TooLarge,
    InvalidDestination,
    WriteFailed,
}

impl CommandExportError {
    pub(super) fn user_message(self) -> &'static str {
        match self {
            Self::TooLarge => "Command output exceeds the 32 MiB export limit.",
            Self::InvalidDestination => {
                "Command output was not exported: the selected destination is unsafe."
            }
            Self::WriteFailed => "Command output could not be exported.",
        }
    }
}

pub(super) fn write_plain_text(path: &Path, text: &str) -> Result<(), CommandExportError> {
    validate_text(text)?;
    if path.file_name().is_none() || path.parent().is_none() {
        return Err(CommandExportError::InvalidDestination);
    }
    crate::state_dir::write_atomic(path, text.as_bytes(), crate::state_dir::WriteMode::Export)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::NotADirectory
            | std::io::ErrorKind::NotFound => CommandExportError::InvalidDestination,
            _ => CommandExportError::WriteFailed,
        })
}

fn validate_text(text: &str) -> Result<(), CommandExportError> {
    if text.len() > MAX_COMMAND_EXPORT_BYTES {
        Err(CommandExportError::TooLarge)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-command-export-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&path).expect("temp dir");
        path
    }

    #[test]
    fn writes_complete_private_plain_text_atomically() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;

        let dir = temp_dir("new");
        #[cfg(unix)]
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("parent mode");
        let path = dir.join("output.txt");
        write_plain_text(&path, "alpha\nbeta\n").expect("export");
        assert_eq!(fs::read_to_string(&path).expect("read"), "alpha\nbeta\n");
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&dir)
                    .expect("parent metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o755,
                "an export must not change its selected parent directory"
            );
            assert_eq!(
                fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_cap_is_accepted_and_cap_plus_one_is_refused_whole() {
        let exact = "x".repeat(MAX_COMMAND_EXPORT_BYTES);
        assert_eq!(validate_text(&exact), Ok(()));
        let over = format!("{exact}x");
        assert_eq!(validate_text(&over), Err(CommandExportError::TooLarge));
    }

    #[test]
    fn refuses_a_missing_parent_instead_of_creating_it() {
        let dir = temp_dir("missing-parent");
        let missing = dir.join("missing");
        let path = missing.join("output.txt");
        assert_eq!(
            write_plain_text(&path, "keep"),
            Err(CommandExportError::InvalidDestination)
        );
        assert!(!missing.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn replaces_an_owned_regular_target_with_a_private_complete_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = temp_dir("replace");
        let path = dir.join("output.txt");
        fs::write(&path, "old trailing bytes").expect("seed target");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("target mode");

        write_plain_text(&path, "new").expect("replace target");

        assert_eq!(fs::read_to_string(&path).expect("read"), "new");
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_final_component_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("symlink");
        let target = dir.join("target.txt");
        fs::write(&target, "keep").expect("target");
        let path = dir.join("output.txt");
        symlink(&target, &path).expect("link");
        assert!(write_plain_text(&path, "replace").is_err());
        assert_eq!(fs::read_to_string(&target).expect("target read"), "keep");
        let _ = fs::remove_dir_all(dir);
    }
}
