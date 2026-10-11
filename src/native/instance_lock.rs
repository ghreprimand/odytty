// SPDX-License-Identifier: GPL-3.0-only
//! Primary-instance file lock.
//!
//! An advisory lock on a state-directory file elects one primary odytty process
//! per user. Only the primary autosaves and restores the workspace shape; a
//! second concurrent instance marks itself secondary and does neither, so two
//! windows never race on `workspaces.json`.
//!
//! The lock is [`std::fs::File::try_lock`] (`flock` on Unix, `LockFileEx` on
//! Windows). The kernel releases it when the process exits or crashes, so there
//! is no stale-lock cleanup. The lock file is never unlinked. The descriptor is
//! opened close-on-exec so spawned shells do not inherit it; that is
//! load-bearing on Unix, where an inherited descriptor would keep the lock held.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::logging::state_log_dir;

/// Basename of the advisory lock file in the state dir.
const LOCK_FILE: &str = "instance.lock";

/// A held primary-instance lock. Keep the value alive for the process lifetime;
/// dropping it releases the OS advisory lock (also released automatically if the
/// process crashes without dropping it).
#[derive(Debug)]
pub(crate) struct PrimaryInstanceLock {
    /// Held only to keep the advisory lock open; never read. Dropping this
    /// `File` is what releases the lock.
    _file: File,
}

impl PrimaryInstanceLock {
    /// Try to become the primary instance using the state-dir lock file.
    ///
    /// `Some` means we acquired the lock and are the primary instance, so
    /// autosave and restore are enabled. `None` means another instance already
    /// holds it, or the lock file could not be created/locked at all; in either
    /// case we run as a secondary instance and do NOT autosave or restore.
    pub(crate) fn acquire() -> Option<Self> {
        Self::acquire_at(&lock_path())
    }

    /// Lock a specific path (production uses the state-dir path; tests inject a
    /// temp path so the election is exercised without touching the real state
    /// dir or process env).
    pub(crate) fn acquire_at(path: &Path) -> Option<Self> {
        if let Some(parent) = path.parent() {
            crate::state_dir::prepare_private_dir(parent).ok()?;
        }
        // Do not truncate the persistent lock path and do not follow a final
        // symlink.  The shared helper creates a new file as 0600 on Unix or
        // repairs an owned existing regular file through its handle.
        let file = crate::state_dir::open_read_write_sensitive(path).ok()?;
        // `try_lock` is non-blocking: `Ok(())` = acquired (primary); any error
        // (would-block from another holder, or a genuine IO/lock error) => run
        // as secondary rather than risk two writers.
        match file.try_lock() {
            Ok(()) => Some(Self { _file: file }),
            Err(_) => None,
        }
    }
}

fn lock_path() -> PathBuf {
    state_log_dir().join(LOCK_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock_path(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!(
            "odytty-lock-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        path.push("instance.lock");
        path
    }

    #[test]
    fn first_acquire_succeeds_and_second_is_rejected_until_release() {
        let path = temp_lock_path("election");

        let first = PrimaryInstanceLock::acquire_at(&path);
        assert!(first.is_some(), "the first acquirer must become primary");

        // A second acquirer of the SAME file (a stand-in for a concurrent
        // process) is rejected - the advisory lock conflicts across open file
        // descriptions even within one process.
        let second = PrimaryInstanceLock::acquire_at(&path);
        assert!(
            second.is_none(),
            "a second acquirer must be rejected while the lock is held"
        );

        // Releasing the primary frees the lock for the next launch (mirrors a
        // clean exit or a crash: the OS releases on `File` drop / process death).
        drop(first);
        let third = PrimaryInstanceLock::acquire_at(&path);
        assert!(
            third.is_some(),
            "the lock must be re-acquirable once the holder releases it"
        );

        drop(third);
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[cfg(unix)]
    #[test]
    fn lock_repairs_the_final_state_leaf_and_file_modes() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_lock_path("modes");
        let parent = path.parent().expect("lock parent");
        std::fs::create_dir_all(parent).expect("create lock parent");
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755))
            .expect("chmod lock parent");
        std::fs::write(&path, b"existing").expect("seed lock");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("chmod lock");

        let lock = PrimaryInstanceLock::acquire_at(&path);
        assert!(lock.is_some(), "secure repaired lock can be acquired");
        assert_eq!(
            std::fs::metadata(parent)
                .expect("parent metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("lock metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(lock);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(parent);
    }
}
