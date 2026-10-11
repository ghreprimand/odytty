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
//!   leaves a worker, a child process, and its temp file behind. The wait after
//!   the kill is itself bounded: a process that cannot be killed or does not
//!   exit is reported as [`BoundedExit::Stuck`] instead of blocking forever.
//! - [`UploadSlots`] caps concurrent uploads, so repeated pastes over a stalled
//!   link cannot accumulate workers. A second limiter caps concurrent remote
//!   cleanups the same way.
//!
//! Overflow policy (best effort by design): the ledger keeps at most
//! [`MAX_LEDGER_PATHS`] remote paths per tab and forgets the oldest beyond
//! that, and at most [`MAX_CONCURRENT_CLEANUPS`] cleanups run at once and a
//! cleanup past that is skipped. A forgotten or skipped file is left to the
//! remote's own temp reaper; nothing earlier than the cap is deleted early,
//! because its path may already be in use in the shell.
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

/// Remote cleanups allowed to run at once across all windows. Each is a short
/// lived `ssh`; a cleanup requested while all are busy is skipped.
#[cfg_attr(test, allow(dead_code))] // used by the non-test cleanup spawner
pub(in crate::native) const MAX_CONCURRENT_CLEANUPS: usize = 8;

/// Remote paths one tab's ledger retains for cleanup at close. A tab that has
/// pasted more images than this forgets the oldest paths; see the module notes.
pub(in crate::native) const MAX_LEDGER_PATHS: usize = 256;

/// Longest the wait after a kill may take before the process is reported as
/// stuck. A killed process normally exits at once.
const REAP_GRACE: Duration = Duration::from_secs(2);

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
    /// is handed back: the caller owns its cleanup now. At
    /// [`MAX_LEDGER_PATHS`] the oldest path is forgotten to make room.
    pub(in crate::native) fn record(&mut self, path: String) -> Option<String> {
        if self.closed {
            Some(path)
        } else {
            if self.paths.len() >= MAX_LEDGER_PATHS {
                self.paths.remove(0);
            }
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
    /// The deadline passed, the cancel check fired, or waiting failed, but the
    /// process could not be killed or did not exit within the grace period, so
    /// it may still be running. The error says why.
    #[cfg_attr(test, allow(dead_code))] // read by the non-test upload worker
    Stuck(std::io::Error),
}

/// Wait for `child` until it exits, `timeout` elapses, or `cancelled`
/// returns true. On timeout, cancel, or a wait error the child is killed and
/// reaped before returning, so no process outlives the call, unless it cannot
/// be killed or does not exit within [`REAP_GRACE`]: that is
/// [`BoundedExit::Stuck`], never an unbounded wait.
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
                return match kill_and_reap(&mut child) {
                    Ok(()) => BoundedExit::WaitFailed(error),
                    Err(stuck) => BoundedExit::Stuck(stuck),
                };
            }
        }
        if cancelled() {
            return match kill_and_reap(&mut child) {
                Ok(()) => BoundedExit::Cancelled,
                Err(stuck) => BoundedExit::Stuck(stuck),
            };
        }
        let now = Instant::now();
        if now >= deadline {
            return match kill_and_reap(&mut child) {
                Ok(()) => BoundedExit::TimedOut,
                Err(stuck) => BoundedExit::Stuck(stuck),
            };
        }
        std::thread::sleep(WAIT_POLL.min(deadline - now));
    }
}

/// A process that can be killed and polled for its exit. Implemented by
/// [`Child`]; the seam lets a test stand in for a process that survives a kill.
trait Terminable {
    fn terminate(&mut self) -> std::io::Result<()>;
    fn poll_exit(&mut self) -> std::io::Result<Option<ExitStatus>>;
}

impl Terminable for Child {
    fn terminate(&mut self) -> std::io::Result<()> {
        self.kill()
    }

    fn poll_exit(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.try_wait()
    }
}

/// Kill the process and wait for it to exit, for at most `grace`. `Ok` means it
/// exited (and is reaped); `Err` carries the kill error, or a timeout, when it
/// may still be running. A failed kill is not fatal by itself: the process may
/// have exited on its own.
fn terminate_and_reap(process: &mut impl Terminable, grace: Duration) -> std::io::Result<()> {
    let killed = process.terminate();
    let deadline = Instant::now() + grace;
    loop {
        match process.poll_exit() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(killed.err().unwrap_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the process did not exit after it was killed",
                )
            }));
        }
        std::thread::sleep(WAIT_POLL.min(deadline - now));
    }
}

fn kill_and_reap(child: &mut Child) -> std::io::Result<()> {
    terminate_and_reap(child, REAP_GRACE)
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

/// Cleanups currently running, across all windows.
#[cfg_attr(test, allow(dead_code))] // used by the non-test cleanup spawner
pub(in crate::native) static CLEANUP_SLOTS: UploadSlots = UploadSlots::new(MAX_CONCURRENT_CLEANUPS);

/// Run `child` to completion under [`CLEANUP_DEADLINE`] on a thread started by
/// `start`, holding `slot` for the duration. The child is owned by a shared
/// cell until the thread has been admitted: when `start` fails, the child is
/// killed and reaped here instead of being dropped, which neither kills nor
/// reaps it. Returns whether the watcher was started.
fn watch_cleanup(
    child: Child,
    slot: UploadSlot<'static>,
    deadline: Duration,
    start: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) -> bool {
    let cell = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
    let watcher_cell = std::sync::Arc::clone(&cell);
    let watcher = Box::new(move || {
        let _slot = slot;
        let child = crate::native::lock_recover(&watcher_cell).take();
        if let Some(child) = child {
            let _ = run_bounded(child, deadline, || false);
        }
    });
    match start(watcher) {
        Ok(()) => true,
        Err(error) => {
            // The watcher never ran, so the child is still in the cell.
            let child = crate::native::lock_recover(&cell).take();
            if let Some(mut child) = child
                && let Err(stuck) = kill_and_reap(&mut child)
            {
                tracing::warn!("remote upload cleanup could not be stopped: {stuck}");
            }
            tracing::warn!("remote upload cleanup watcher unavailable: {error}");
            false
        }
    }
}

/// Spawn a best-effort remote `rm -f` for `paths` and bound its lifetime on
/// a detached thread, so tab close never waits and a stalled link cannot
/// keep the cleanup `ssh` alive past [`CLEANUP_DEADLINE`]. At most
/// [`MAX_CONCURRENT_CLEANUPS`] run at once; a cleanup requested beyond that is
/// skipped and the remote's own temp reaper removes the file. Only
/// OdyTTY-minted upload paths are removed (see `remote_cleanup_command`).
/// Compiled out under `cfg(test)`, where no real `ssh` is spawned.
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
    let Some(slot) = CLEANUP_SLOTS.try_acquire() else {
        tracing::warn!("remote upload cleanup skipped: {MAX_CONCURRENT_CLEANUPS} are running");
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
            watch_cleanup(child, slot, CLEANUP_DEADLINE, |watcher| {
                crate::spawn_util::spawn_named("odytty-upload-cleanup", watcher).map(drop)
            });
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
        #[cfg(windows)]
        crate::native::app::win_spawn::apply_no_console_window(&mut command);
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
        #[cfg(windows)]
        crate::native::app::win_spawn::apply_no_console_window(&mut command);
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

    /// A process that ignores the kill and never exits.
    struct Unkillable {
        kill_error: bool,
        polls: usize,
    }

    impl Terminable for Unkillable {
        fn terminate(&mut self) -> std::io::Result<()> {
            if self.kill_error {
                Err(std::io::Error::other("kill refused"))
            } else {
                Ok(())
            }
        }

        fn poll_exit(&mut self) -> std::io::Result<Option<ExitStatus>> {
            self.polls += 1;
            Ok(None)
        }
    }

    #[test]
    fn a_process_that_survives_the_kill_is_reported_after_a_bounded_wait() {
        for kill_error in [true, false] {
            let mut process = Unkillable {
                kill_error,
                polls: 0,
            };
            let start = Instant::now();
            let result = terminate_and_reap(&mut process, Duration::from_millis(150));
            let error = result.expect_err("the process never exited");
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the wait is bounded"
            );
            assert!(process.polls >= 2, "it kept polling until the grace ended");
            if kill_error {
                assert_eq!(error.to_string(), "kill refused");
            } else {
                assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
            }
        }
    }

    #[test]
    fn a_killed_helper_is_reaped_before_the_call_returns() {
        let mut child = stalled_child();
        terminate_and_reap(&mut child, Duration::from_secs(5)).expect("killed and reaped");
        assert!(
            child.try_wait().expect("status").is_some(),
            "the child has been waited for"
        );
    }

    #[test]
    fn the_ledger_forgets_the_oldest_path_beyond_its_cap() {
        let mut ledger = UploadLedger::default();
        for index in 0..MAX_LEDGER_PATHS + 3 {
            assert_eq!(ledger.record(format!("/tmp/{index}.png")), None);
        }
        let kept = ledger.close();
        assert_eq!(kept.len(), MAX_LEDGER_PATHS);
        assert_eq!(kept.first().map(String::as_str), Some("/tmp/3.png"));
        assert_eq!(
            kept.last().map(String::as_str),
            Some(format!("/tmp/{}.png", MAX_LEDGER_PATHS + 2).as_str())
        );
    }

    #[test]
    fn a_watcher_that_cannot_start_does_not_leave_the_cleanup_child_running() {
        static SLOTS: UploadSlots = UploadSlots::new(1);
        let child = stalled_child();
        let pid = child.id();
        let slot = SLOTS.try_acquire().expect("slot");
        let started = watch_cleanup(child, slot, Duration::from_secs(30), |_watcher| {
            Err(std::io::Error::other("thread limit"))
        });
        assert!(!started);
        assert!(
            SLOTS.try_acquire().is_some(),
            "the slot is released when the watcher is not admitted"
        );
        // Signal 0 probes for the process: a killed but unreaped child is a
        // zombie and still answers, so a failure here means it was reaped too.
        #[cfg(unix)]
        {
            let alive = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .expect("probe the process")
                .success();
            assert!(!alive, "the cleanup child was killed and reaped");
        }
        #[cfg(not(unix))]
        let _ = pid;
    }

    #[test]
    fn concurrent_cleanups_are_capped_and_the_overflow_is_refused() {
        let held: Vec<_> = (0..MAX_CONCURRENT_CLEANUPS)
            .map(|_| CLEANUP_SLOTS.try_acquire().expect("a free cleanup slot"))
            .collect();
        assert!(
            CLEANUP_SLOTS.try_acquire().is_none(),
            "a cleanup past the cap is skipped, not queued without bound"
        );
        drop(held);
        assert!(CLEANUP_SLOTS.try_acquire().is_some());
    }

    #[test]
    fn an_admitted_watcher_runs_the_cleanup_to_its_deadline_and_releases_its_slot() {
        static SLOTS: UploadSlots = UploadSlots::new(1);
        let child = stalled_child();
        let slot = SLOTS.try_acquire().expect("slot");
        let (sender, receiver) = std::sync::mpsc::channel();
        let started = watch_cleanup(child, slot, Duration::from_millis(100), |watcher| {
            let handle = std::thread::spawn(move || {
                watcher();
                let _ = sender.send(());
            });
            drop(handle);
            Ok(())
        });
        assert!(started);
        receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the watcher finished at its deadline");
        assert!(SLOTS.try_acquire().is_some(), "the slot is free again");
    }
}
