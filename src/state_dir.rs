// SPDX-License-Identifier: GPL-3.0-only
//! Owner-private persistent-state filesystem helpers.
//!
//! Unix state contains local paths, saved layouts, and diagnostic records.  The
//! helpers in this module enforce the narrow boundary shared by those writers:
//! only the final OdyTTY-owned directory is normalized, and only known regular
//! files opened without following a final symlink are repaired.  Parents and
//! unknown children are deliberately left alone.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Create or repair one application-owned state leaf.  This never changes a
/// parent directory and never walks children.
pub(crate) fn prepare_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::prepare_private_dir(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Validate an existing state leaf without creating it.
pub(crate) fn validate_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::validate_private_dir(path)
    }
    #[cfg(not(unix))]
    {
        let metadata = fs::metadata(path)?;
        if metadata.is_dir() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "persistent state path is not a directory",
            ))
        }
    }
}

/// Open an existing known-sensitive regular file for reading and repair its
/// owner-only mode where Unix mode bits apply.
pub(crate) fn open_existing_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::open_existing_sensitive(path, false, false, false)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().read(true).open(path)
    }
}

/// Open a known-sensitive regular file for append, creating it owner-private
/// on Unix.  Existing objects are validated through the open handle first.
pub(crate) fn open_append_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::open_existing_sensitive(path, true, false, true)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().create(true).append(true).open(path)
    }
}

/// Open a known-sensitive regular file for read/write, creating it
/// owner-private on Unix.  The caller retains the handle for locking.
pub(crate) fn open_read_write_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::open_existing_sensitive(path, false, true, true)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
    }
}

/// Create one unique sibling file without replacing an existing object.  This
/// is used by atomic persistence writes, so a colliding temporary name can
/// never be opened or removed as though it belonged to this process.
pub(crate) fn create_new_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::create_new_sensitive(path)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().create_new(true).write(true).open(path)
    }
}

/// Validate and repair a known sensitive file if it already exists.  Absence is
/// not an error; callers that need creation use one of the open helpers above.
pub(crate) fn repair_existing_sensitive(path: &Path) -> io::Result<()> {
    match open_existing_sensitive(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    fn current_uid() -> u32 {
        // SAFETY: `geteuid` has no arguments, no memory preconditions, and no
        // failure mode.  It is queried only to enforce ownership of this
        // process's own persistent state leaf and files.
        unsafe { libc::geteuid() }
    }

    fn owned_by_current_user(uid: u32) -> bool {
        uid == current_uid()
    }

    fn invalid(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, message)
    }

    fn open_dir_no_follow(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true);
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        options.open(path)
    }

    fn validate_private_dir_handle(dir: &File) -> io::Result<()> {
        let metadata = dir.metadata()?;
        if !metadata.file_type().is_dir() {
            return Err(invalid("persistent state path is not a directory"));
        }
        if !owned_by_current_user(metadata.uid()) {
            return Err(invalid("persistent state path is not owned by this user"));
        }
        if metadata.mode() & 0o777 != PRIVATE_DIR_MODE {
            // On macOS this is an fchmod of the opened descriptor: BSD mode
            // bits are tightened without stripping any extended ACL entries.
            dir.set_permissions(fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
        }
        Ok(())
    }

    pub(super) fn prepare_private_dir(path: &Path) -> io::Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            // Parent creation is needed for a fresh XDG/HOME tree, but no parent
            // is chmodded or otherwise normalized by this helper.
            fs::create_dir_all(parent)?;
        }

        let mut builder = fs::DirBuilder::new();
        builder.mode(PRIVATE_DIR_MODE);
        match builder.create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }

        let dir = open_dir_no_follow(path)?;
        validate_private_dir_handle(&dir)
    }

    pub(super) fn validate_private_dir(path: &Path) -> io::Result<()> {
        let dir = open_dir_no_follow(path)?;
        validate_private_dir_handle(&dir)
    }

    fn validate_sensitive_file_handle(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(invalid("persistent state path is not a regular file"));
        }
        if !owned_by_current_user(metadata.uid()) {
            return Err(invalid("persistent state file is not owned by this user"));
        }
        if metadata.mode() & 0o777 != PRIVATE_FILE_MODE {
            // File::set_permissions is descriptor-based on Unix.  This repairs
            // the opened object rather than a pathname that might have changed;
            // macOS ACL entries are preserved rather than replaced.
            file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        }
        Ok(())
    }

    pub(super) fn open_existing_sensitive(
        path: &Path,
        append: bool,
        write: bool,
        create: bool,
    ) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(!append || !write);
        options.append(append).write(write).create(create);
        options.mode(PRIVATE_FILE_MODE);
        options.custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path)?;
        validate_sensitive_file_handle(&file)?;
        Ok(file)
    }

    pub(super) fn create_new_sensitive(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true).mode(PRIVATE_FILE_MODE);
        options.custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path)?;
        validate_sensitive_file_handle(&file)?;
        Ok(file)
    }

    #[cfg(test)]
    pub(super) fn owner_policy_accepts_only_the_effective_uid(uid: u32) -> bool {
        owned_by_current_user(uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-state-dir-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos()
        ));
        fs::create_dir(&path).expect("create temp root");
        path
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn private_leaf_and_known_file_repair_same_owner_modes() {
        let root = temp_dir("repair");
        let leaf = root.join("state");
        fs::create_dir(&leaf).expect("create broad leaf");
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o755)).expect("chmod leaf");
        let file = leaf.join("state.json");
        fs::write(&file, "unchanged").expect("seed file");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod file");

        prepare_private_dir(&leaf).expect("repair leaf");
        let opened = open_existing_sensitive(&file).expect("repair file");
        drop(opened);

        assert_eq!(mode(&leaf), PRIVATE_DIR_MODE);
        assert_eq!(mode(&file), PRIVATE_FILE_MODE);
        assert_eq!(fs::read_to_string(&file).expect("read file"), "unchanged");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn private_creations_ignore_a_permissive_umask_in_a_subprocess() {
        const CHILD_ENV: &str = "ODYTTY_STATE_DIR_UMASK_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            // SAFETY: this branch runs in a dedicated test subprocess, so no
            // parallel in-process test can observe the temporary umask.
            unsafe { libc::umask(0) };
            let root = temp_dir("umask-child");
            let leaf = root.join("state");
            prepare_private_dir(&leaf).expect("create private state leaf");
            let file = leaf.join("state.json");
            drop(create_new_sensitive(&file).expect("create private state file"));
            assert_eq!(mode(&leaf), PRIVATE_DIR_MODE);
            assert_eq!(mode(&file), PRIVATE_FILE_MODE);
            let _ = fs::remove_dir_all(root);
            return;
        }

        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("--exact")
            .arg("state_dir::tests::private_creations_ignore_a_permissive_umask_in_a_subprocess")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .status()
            .expect("run umask child");
        assert!(status.success(), "umask child failed with {status}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaf_and_file_are_rejected_without_following() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("symlink");
        let target = root.join("target");
        fs::create_dir(&target).expect("target dir");
        let leaf_link = root.join("state");
        symlink(&target, &leaf_link).expect("leaf link");
        assert!(prepare_private_dir(&leaf_link).is_err());

        let leaf = root.join("real-state");
        prepare_private_dir(&leaf).expect("prepare leaf");
        let target_file = root.join("target-file");
        fs::write(&target_file, "keep").expect("target file");
        let file_link = leaf.join("state.json");
        symlink(&target_file, &file_link).expect("file link");
        assert!(open_existing_sensitive(&file_link).is_err());
        assert_eq!(
            fs::read_to_string(&target_file).expect("target contents"),
            "keep"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn non_directory_state_leaf_is_rejected_without_replacement() {
        let root = temp_dir("non-directory-leaf");
        let leaf = root.join("state");
        fs::write(&leaf, "keep").expect("seed wrong leaf type");

        assert!(prepare_private_dir(&leaf).is_err());
        assert_eq!(fs::read_to_string(&leaf).expect("leaf contents"), "keep");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_known_file_is_rejected_without_repairing_it() {
        let root = temp_dir("non-regular");
        let leaf = root.join("state");
        prepare_private_dir(&leaf).expect("prepare leaf");
        let directory_at_file_path = leaf.join("workspaces.json");
        fs::create_dir(&directory_at_file_path).expect("create wrong type");

        assert!(open_existing_sensitive(&directory_at_file_path).is_err());
        assert!(
            directory_at_file_path.is_dir(),
            "wrong type stays untouched"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn owner_policy_rejects_a_different_uid_without_a_privileged_fixture() {
        let uid = unsafe { libc::geteuid() };
        assert!(unix::owner_policy_accepts_only_the_effective_uid(uid));
        assert!(!unix::owner_policy_accepts_only_the_effective_uid(
            uid.wrapping_add(1)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_ordinary_inherited_acl_operations() {
        let root = temp_dir("windows");
        let leaf = root.join("state");
        prepare_private_dir(&leaf).expect("create state leaf");
        let file = leaf.join("state.json");
        drop(open_read_write_sensitive(&file).expect("create state file"));
        assert!(file.exists());
        let _ = fs::remove_dir_all(root);
    }
}
