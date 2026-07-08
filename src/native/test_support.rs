// SPDX-License-Identifier: GPL-3.0-only
//! Shared helpers for native tests.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;

use crate::core::Dimensions;
use crate::pty::PtySession;

/// Serializes the real-shell spawn window across parallel test threads.
///
/// The native suite runs multi-threaded on Linux and Windows (`cargo test`
/// default; only macOS is pinned to `--test-threads=1`). Opening a real
/// pseudoconsole on Windows is sensitive to concurrent spawn/teardown: under
/// parallelism two ConPTY starts occasionally collide and one child dies at
/// startup ("the pseudoconsole could not start a usable shell"), which surfaced
/// as an intermittent CI failure spread across the ~40 native test files that
/// share `spawn_test_pause_shell`. Holding this gate only for the duration of
/// the spawn — the guard drops as this function returns, well before the test
/// body's pause-shell wait — removes the contention while leaving each test
/// fully parallel, so total suite time is unchanged. The lock guards no data,
/// so a guard poisoned by a panic mid-spawn is recovered and ignored rather
/// than cascading a poison error into every later spawn.
static SPAWN_GATE: Mutex<()> = Mutex::new(());

pub(in crate::native) fn spawn_test_pause_shell(dimensions: Dimensions) -> Result<PtySession> {
    #[cfg(unix)]
    const PAUSE_COMMAND: &str = "sleep 1";
    #[cfg(windows)]
    const PAUSE_COMMAND: &str = "ping -n 2 127.0.0.1 >NUL";

    // On Windows, `timeout /t` can fail when stdin is not a console; ping gives
    // these PTY fixture tests the same short-lived hold without interactive I/O.

    // Serialize concurrent real-shell spawns (see `SPAWN_GATE`). A poisoned lock
    // protects nothing here, so recover rather than propagate it.
    let _gate = SPAWN_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Windows ConPTY startup stays transiently flaky even with spawns
    // serialized, so absorb a spurious startup error with a small bounded retry;
    // Unix spawning is reliable and takes a single shot. The retry is expressed
    // with a runtime `cfg!` count (not a `#[cfg]` block) so the exact loop that
    // runs on Windows is compiled and lint-checked on every platform.
    let max_attempts = if cfg!(windows) { 3 } else { 1 };
    let mut result = PtySession::spawn_shell_command(dimensions, PAUSE_COMMAND);
    let mut attempts = 1;
    while result.is_err() && attempts < max_attempts {
        std::thread::sleep(Duration::from_millis(50));
        result = PtySession::spawn_shell_command(dimensions, PAUSE_COMMAND);
        attempts += 1;
    }
    result
}
