// SPDX-License-Identifier: GPL-3.0-only
//! Bounded lifecycle for remote image paste-through uploads.
//!
//! Three pieces keep an upload from outliving its purpose:
//!
//! - [`UploadLedger`] is shared by a remote tab and its upload workers. Tab
//!   close marks it closed and takes the recorded remote paths for cleanup; a
//!   worker that finishes afterwards learns the tab is gone and removes its own
//!   remote file instead of appending a path nobody will clean up.
//! - [`run_bounded`] waits for a helper `ssh` with a deadline and a cancel
//!   check, killing and reaping it when either fires, so a stalled link never
//!   leaves a worker, a child process, and its temp file behind.
//! - [`UploadSlots`] caps concurrent uploads, so repeated pastes over a stalled
//!   link cannot accumulate workers.
//!
//! Upload and cleanup run the system `ssh` on every platform. Windows uses
//! the same logic: `Child::kill` terminates `ssh.exe`, and the console window
//! is suppressed at each spawn site.

use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Longest a single upload may run. The upload cap is 10 MiB, so this allows
/// roughly 85 KiB/s before a transfer is abandoned.
#[cfg_attr(test, allow(dead_code))] // used by the non-test upload worker
pub(in crate::native) const UPLOAD_DEADLINE: Duration = Duration::from_secs(120);

/// Longest a best-effort remote cleanup may run before it is killed.
#[cfg_attr(test, allow(dead_code))] // used by the non-test upload worker
pub(in crate::native) const CLEANUP_DEADLINE: Duration = Duration::from_secs(15);

/// Concurrent uploads allowed across all windows.
#[cfg_attr(test, allow(dead_code))] // used by the non-test upload worker
pub(in crate::native) const MAX_CONCURRENT_UPLOADS: usize = 2;

/// Poll interval while waiting on a helper process.
const WAIT_POLL: Duration = Duration::from_millis(50);

/// Remote paths a tab's uploads may have created, and whether the tab closed.
#[derive(Debug, Default)]
pub(in crate::native) struct UploadLedger {
    closed: bool,
    paths: Vec<String>,
}

impl UploadLedger {
    /// Record a remote path that may exist. While the tab is open the path is
    /// kept for cleanup at close and `None` is returned. After close the path
    /// is handed back: the caller owns its cleanup now.
    pub(in crate::native) fn record(&mut self, path: String) -> Option<String> {
        if self.closed {
            Some(path)
        } else {
            self.paths.push(path);
            None
        }
    }

    /// Mark the tab closed and take every recorded path for cleanup. Later
    /// [`Self::record`] calls return their path to the caller.
    pub(in crate::native) fn close(&mut self) -> Vec<String> {
        self.closed = true;
        std::mem::take(&mut self.paths)
    }

    pub(in crate::native) fn is_closed(&self) -> bool {
        self.closed
    }
}

/// How a bounded helper process ended.
#[derive(Debug)]
pub(in crate::native) enum BoundedExit {
    /// The process exited by itself.
    Exited(ExitStatus),
    /// The deadline passed; the process was killed and reaped.
    TimedOut,
    /// The cancel check fired; the process was killed and reaped.
    Cancelled,
    /// Waiting failed; the process was killed and reaped.
    #[cfg_attr(test, allow(dead_code))] // read by the non-test upload worker
    WaitFailed(std::io::Error),
}

/// Wait for `child` until it exits, `timeout` elapses, or `cancelled`
/// returns true. On timeout, cancel, or a wait error the child is killed and
/// reaped before returning, so no process outlives the call.
pub(in crate::native) fn run_bounded(
    mut child: Child,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> BoundedExit {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return BoundedExit::Exited(status),
            Ok(None) => {}
            Err(error) => {
                kill_and_reap(&mut child);
                return BoundedExit::WaitFailed(error);
            }
        }
        if cancelled() {
            kill_and_reap(&mut child);
            return BoundedExit::Cancelled;
        }
        let now = Instant::now();
        if now >= deadline {
            kill_and_reap(&mut child);
            return BoundedExit::TimedOut;
        }
        std::thread::sleep(WAIT_POLL.min(deadline - now));
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A counting limiter for concurrent uploads.
pub(in crate::native) struct UploadSlots {
    active: AtomicUsize,
    max: usize,
}

/// Process-wide upload limiter.
#[cfg_attr(test, allow(dead_code))] // used by the non-test upload worker
pub(in crate::native) static UPLOAD_SLOTS: UploadSlots = UploadSlots::new(MAX_CONCURRENT_UPLOADS);

impl UploadSlots {
    pub(in crate::native) const fn new(max: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max,
        }
    }

    /// Take a slot, or `None` when `max` uploads are already running.
    pub(in crate::native) fn try_acquire(&self) -> Option<UploadSlot<'_>> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.max).then_some(active + 1)
            })
            .ok()
            .map(|_| UploadSlot { slots: self })
    }
}

/// A held upload slot, released on drop (including a worker panic).
pub(in crate::native) struct UploadSlot<'a> {
    slots: &'a UploadSlots,
}

impl Drop for UploadSlot<'_> {
    fn drop(&mut self) {
        self.slots.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What the worker reports once an upload attempt has settled.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::native) enum UploadSettlement {
    /// The file is on the remote and the tab is open: announce the path.
    Delivered,
    /// The attempt failed while the tab is open: show the reason. The remote
    /// path is recorded, so a partial file is removed with the tab's cleanup.
    Failed(String),
    /// The tab closed first. The worker has already requested removal of the
    /// remote path; nothing is shown.
    TabClosed,
}

/// Settle one upload attempt against the tab's ledger. `outcome` is `Ok` on
/// a complete transfer or the failure reason. Whatever the outcome, the
/// remote path may exist (a failed or killed `cat` can leave a partial file),
/// so it is recorded for the tab's close cleanup, or, when the tab already
/// closed, passed to `cleanup` at once.
pub(in crate::native) fn settle_upload(
    ledger: &std::sync::Mutex<UploadLedger>,
    remote_path: String,
    outcome: Result<(), String>,
    cleanup: impl FnOnce(Vec<String>),
) -> UploadSettlement {
    let orphan = crate::native::lock_recover(ledger).record(remote_path);
    if let Some(path) = orphan {
        cleanup(vec![path]);
        return UploadSettlement::TabClosed;
    }
    match outcome {
        Ok(()) => UploadSettlement::Delivered,
        Err(reason) => UploadSettlement::Failed(reason),
    }
}

/// Spawn a best-effort remote `rm -f` for `paths` and bound its lifetime on
/// a detached thread, so tab close never waits and a stalled link cannot
/// keep the cleanup `ssh` alive past [`CLEANUP_DEADLINE`]. Only OdyTTY-minted
/// upload paths are removed (see `remote_cleanup_command`). Compiled out
/// under `cfg(test)`, where no real `ssh` is spawned.
#[cfg(not(test))]
pub(in crate::native) fn spawn_remote_cleanup(
    destination: &str,
    port: Option<u16>,
    control_dir: Option<&std::path::Path>,
    paths: &[String],
) {
    let Some(command) =
        crate::ssh_connect::remote_cleanup_command(destination, port, control_dir, paths)
    else {
        return;
    };
    let (program, args) = command.into_program_args();
    let mut cleanup = std::process::Command::new(program);
    cleanup
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Suppress the Windows console-window flash; no-op elsewhere.
    crate::native::app::win_spawn::apply_no_console_window(&mut cleanup);
    match cleanup.spawn() {
        Ok(child) => {
            let spawned = crate::spawn_util::spawn_named("odytty-upload-cleanup", move || {
                let _ = run_bounded(child, CLEANUP_DEADLINE, || false);
            });
            if let Err(error) = spawned {
                tracing::warn!("remote upload cleanup watcher unavailable: {error}");
            }
        }
        Err(error) => {
            // Best-effort by design; the remote's own /tmp reaper still
            // bounds the leak.
            tracing::warn!("remote upload cleanup spawn failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;

    fn stalled_child() -> Child {
        #[cfg(unix)]
        let mut command = {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("ping");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a stalled stand-in for ssh")
    }

    #[test]
    fn a_stalled_helper_is_killed_and_reaped_at_the_deadline() {
        let child = stalled_child();
        let start = Instant::now();
        let exit = run_bounded(child, Duration::from_millis(200), || false);
        assert!(matches!(exit, BoundedExit::TimedOut), "got {exit:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_helper_that_exits_reports_its_status() {
        #[cfg(unix)]
        let mut command = Command::new("true");
        #[cfg(windows)]
        let mut command = Command::new("hostname");
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        match run_bounded(child, Duration::from_secs(10), || false) {
            BoundedExit::Exited(status) => assert!(status.success()),
            other => panic!("expected a clean exit, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_helper_is_killed_promptly() {
        let child = stalled_child();
        let start = Instant::now();
        let exit = run_bounded(child, Duration::from_secs(30), || {
            start.elapsed() >= Duration::from_millis(100)
        });
        assert!(matches!(exit, BoundedExit::Cancelled), "got {exit:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn close_after_success_hands_the_path_to_the_close_cleanup() {
        let ledger = Mutex::new(UploadLedger::default());
        let mut worker_cleanups = Vec::new();
        let settled = settle_upload(&ledger, "/tmp/a.png".to_owned(), Ok(()), |paths| {
            worker_cleanups.push(paths)
        });
        assert_eq!(settled, UploadSettlement::Delivered);
        assert!(worker_cleanups.is_empty());
        assert_eq!(
            ledger.lock().unwrap().close(),
            vec!["/tmp/a.png".to_owned()]
        );
    }

    #[test]
    fn close_before_success_makes_the_worker_remove_its_own_file() {
        let ledger = Mutex::new(UploadLedger::default());
        assert!(ledger.lock().unwrap().close().is_empty());
        let mut worker_cleanups = Vec::new();
        let settled = settle_upload(&ledger, "/tmp/b.png".to_owned(), Ok(()), |paths| {
            worker_cleanups.push(paths)
        });
        assert_eq!(settled, UploadSettlement::TabClosed);
        assert!(ledger.lock().unwrap().is_closed());
        assert_eq!(worker_cleanups, vec![vec!["/tmp/b.png".to_owned()]]);
        assert!(
            ledger.lock().unwrap().close().is_empty(),
            "nothing is cleaned twice"
        );
    }

    #[test]
    fn a_failed_upload_still_records_its_possibly_partial_file() {
        let ledger = Mutex::new(UploadLedger::default());
        let settled = settle_upload(
            &ledger,
            "/tmp/c.png".to_owned(),
            Err("upload timed out".to_owned()),
            |_| panic!("the tab is open; cleanup waits for close"),
        );
        assert_eq!(
            settled,
            UploadSettlement::Failed("upload timed out".to_owned())
        );
        assert_eq!(
            ledger.lock().unwrap().close(),
            vec!["/tmp/c.png".to_owned()]
        );
    }

    #[test]
    fn upload_slots_cap_concurrency_and_release_on_drop() {
        let slots = UploadSlots::new(2);
        let first = slots.try_acquire().expect("first");
        let second = slots.try_acquire().expect("second");
        assert!(
            slots.try_acquire().is_none(),
            "a third upload waits for a slot"
        );
        drop(first);
        let third = slots.try_acquire().expect("a released slot is reusable");
        drop((second, third));
        assert!(slots.try_acquire().is_some());
    }
}
