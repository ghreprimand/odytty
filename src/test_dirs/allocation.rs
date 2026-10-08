// SPDX-License-Identifier: GPL-3.0-only
//! Exclusive scratch allocation shared by unit and integration tests.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Create a fresh, empty directory `<base>/<prefix><pid>-<n>-<clock>` that no
/// other test in this or any earlier run owns.
pub fn fresh_dir(base: &Path, prefix: &str) -> PathBuf {
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
pub fn fresh_temp_dir(prefix: &str) -> PathBuf {
    fresh_dir(&std::env::temp_dir(), prefix)
}

/// [`fresh_dir`] under a short base for directories that hold Unix sockets.
/// macOS's per-user temp directory alone is about 49 bytes of the 103 a socket
/// path may use, so these live under `/tmp`.
#[cfg(unix)]
pub fn fresh_socket_dir(prefix: &str) -> PathBuf {
    fresh_dir(Path::new("/tmp"), prefix)
}
