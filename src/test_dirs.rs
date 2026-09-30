// SPDX-License-Identifier: GPL-3.0-only
//! Collision-free scratch directories for tests.
//!
//! Test threads in one process share a pid, CI runs them in parallel, and a
//! killed run can leave a directory behind. A name is therefore the pid, a
//! process-wide counter, and the low clock digits, and creation uses
//! `create_dir` (never `create_dir_all` or remove-then-create), retrying with
//! the next counter value when the name is already taken. Names stay short
//! because some callers bind Unix sockets inside, and macOS limits a socket
//! path to 103 bytes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Create a fresh, empty directory `<base>/<prefix><pid>-<n>-<clock>` that no
/// other test in this or any earlier run owns.
pub(crate) fn fresh_dir(base: &Path, prefix: &str) -> PathBuf {
    for _ in 0..1024 {
        let clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos() % 1_000_000);
        let path = base.join(format!(
            "{prefix}{}-{}-{clock}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("create test directory {}: {error}", path.display()),
        }
    }
    panic!("no free test directory name under {}", base.display());
}

/// [`fresh_dir`] under the platform temp directory.
pub(crate) fn fresh_temp_dir(prefix: &str) -> PathBuf {
    fresh_dir(&std::env::temp_dir(), prefix)
}

/// [`fresh_dir`] under a short base for directories that hold Unix sockets.
/// macOS's per-user temp directory alone is about 49 bytes of the 103 a socket
/// path may use, so these live under `/tmp`.
#[cfg(unix)]
pub(crate) fn fresh_socket_dir(prefix: &str) -> PathBuf {
    fresh_dir(Path::new("/tmp"), prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_callers_get_distinct_directories() {
        let base = fresh_temp_dir("otd");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let base = base.clone();
                std::thread::spawn(move || fresh_dir(&base, "x"))
            })
            .collect();
        let mut dirs: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("worker"))
            .collect();
        dirs.sort();
        dirs.dedup();
        assert_eq!(dirs.len(), 8, "every caller owns its own directory");
        for dir in &dirs {
            assert!(dir.is_dir());
        }
        std::fs::remove_dir_all(&base).expect("clean up");
    }
}
