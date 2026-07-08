// SPDX-License-Identifier: GPL-3.0-only
//! Shared helpers for native tests.

use anyhow::Result;

use crate::core::Dimensions;
use crate::pty::PtySession;

pub(in crate::native) fn spawn_test_pause_shell(dimensions: Dimensions) -> Result<PtySession> {
    #[cfg(unix)]
    const PAUSE_COMMAND: &str = "sleep 1";
    #[cfg(windows)]
    const PAUSE_COMMAND: &str = "ping -n 2 127.0.0.1 >NUL";

    // On Windows, `timeout /t` can fail when stdin is not a console; ping gives
    // these PTY fixture tests the same short-lived hold without interactive I/O.
    PtySession::spawn_shell_command(dimensions, PAUSE_COMMAND)
}
