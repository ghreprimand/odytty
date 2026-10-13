// SPDX-License-Identifier: GPL-3.0-only
//! Self-cleaning scratch directories shared by integration tests.

use crate::scratch_dirs;

use std::path::{Path, PathBuf};

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str, socket_paths: bool) -> Self {
        #[cfg(unix)]
        let path = if socket_paths {
            scratch_dirs::fresh_socket_dir(prefix)
        } else {
            scratch_dirs::fresh_temp_dir(prefix)
        };
        #[cfg(not(unix))]
        let path = {
            let _ = socket_paths;
            scratch_dirs::fresh_temp_dir(prefix)
        };
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
