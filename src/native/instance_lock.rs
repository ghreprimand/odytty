// SPDX-License-Identifier: GPL-3.0-only
//! Primary-instance file lock (WP2 sub-ODP 8d).
//!
//! A single advisory lock on a state-dir file elects ONE "primary" odytty
//! process per machine-user. Only the primary autosaves the workspace shape and
//! restores it at launch; a second concurrent instance takes the lock's
//! would-block, marks itself secondary, and neither saves nor restores — so two
//! windows never race on `workspaces.json` and a second window never clobbers
//! the first window's saved layout.
//!
//! Mechanism: [`std::fs::File::try_lock`] — a kernel advisory lock (`flock` on
//! Unix, `LockFileEx` on Windows, via one std API). Chosen over a PID file
//! because the kernel RELEASES the lock automatically when the owning process
//! exits OR crashes: there is no stale-lock class to clean up, no PID-liveness
//! probe to get right per platform, and no PID-reuse false positive. The lock is
//! held for the whole process lifetime by keeping the `File` alive inside
//! [`PrimaryInstanceLock`]; dropping it (normal exit) or the process dying
//! (crash) frees the lock for the next launch. The lock file itself is left in
//! place between runs — never unlinked — so there is no unlink/relock race; the
//! next launch simply re-locks the same file. Behavior is identical on Windows
//! through the same std API.

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
            let _ = std::fs::create_dir_all(parent);
        }
        let file = File::create(path).ok()?;
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
        // process) is rejected — the advisory lock conflicts across open file
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
}
