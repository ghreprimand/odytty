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
//!   cleanups the same way. A helper reported [`BoundedExit::Stuck`] may still
//!   be running, so its limiter keeps it counted (parked, with its process
//!   handle) until a later admission proves it exited; a worker returning does
//!   not free the capacity its helper still occupies.
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
/// [`BoundedExit::Stuck`], never an unbounded wait. A stuck child is parked
/// with `slots` rather than dropped, so it keeps counting against that limiter
/// until it is seen to have exited.
pub(in crate::native) fn run_bounded(
    child: Child,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
    slots: &UploadSlots,
) -> BoundedExit {
    run_bounded_process(child, timeout, cancelled, slots, REAP_GRACE)
}

/// [`run_bounded`] over any [`Terminable`], with the reap `grace` explicit so a
/// test can stand in for a process that cannot be stopped.
fn run_bounded_process<P: Terminable + 'static>(
    mut process: P,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
    slots: &UploadSlots,
    grace: Duration,
) -> BoundedExit {
    let deadline = Instant::now() + timeout;
    let stop = |process: &mut P, exit: BoundedExit| match terminate_and_reap(process, grace) {
        Ok(()) => exit,
        Err(stuck) => BoundedExit::Stuck(stuck),
    };
    let exit = loop {
        match process.poll_exit() {
            Ok(Some(status)) => return BoundedExit::Exited(status),
            Ok(None) => {}
            Err(error) => break stop(&mut process, BoundedExit::WaitFailed(error)),
        }
        if cancelled() {
            break stop(&mut process, BoundedExit::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            break stop(&mut process, BoundedExit::TimedOut);
        }
        std::thread::sleep(WAIT_POLL.min(deadline - now));
    };
    if matches!(exit, BoundedExit::Stuck(_)) {
        slots.park(Box::new(process));
    }
    exit
}

/// A process that can be killed and polled for its exit. Implemented by
/// [`Child`]; the seam lets a test stand in for a process that survives a kill.
trait Terminable: Send {
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

/// Whether the process is known to be gone. A poll error counts: `Child`
/// reports one only when the process can no longer be waited for (it was
/// already reaped elsewhere), and retrying it would never change that.
fn exit_proved(process: &mut dyn Terminable) -> bool {
    !matches!(process.poll_exit(), Ok(None))
}

/// Ask a process to die without waiting for it, for a caller that must not
/// block (the window thread). The kill is sent once and the exit checked once;
/// a process not yet gone is parked with `slots`, which keeps it counted and
/// reaps it on a later admission.
fn kill_without_waiting(mut process: Box<dyn Terminable>, slots: &UploadSlots) {
    if let Err(error) = process.terminate() {
        tracing::warn!("remote upload helper could not be killed: {error}");
    }
    if !exit_proved(&mut *process) {
        slots.park(process);
    }
}

/// A counting limiter for concurrent helper processes.
pub(in crate::native) struct UploadSlots {
    active: AtomicUsize,
    max: usize,
    /// Helpers that may still be running after the worker that owned them
    /// returned. Each counts against `max` until it is seen to have exited, so
    /// processes that cannot be stopped never exceed the cap. Never longer than
    /// `max`: only an admitted slot's helper is parked.
    parked: std::sync::Mutex<Vec<Box<dyn Terminable>>>,
}

/// Process-wide upload limiter.
#[cfg_attr(test, allow(dead_code))] // used by the non-test upload worker
pub(in crate::native) static UPLOAD_SLOTS: UploadSlots = UploadSlots::new(MAX_CONCURRENT_UPLOADS);

impl UploadSlots {
    pub(in crate::native) const fn new(max: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max,
            parked: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Take a slot, or `None` when `max` uploads are already running or
    /// unstoppable helpers still occupy the capacity. Parked helpers are
    /// checked first: each is asked to die again, and the ones seen to have
    /// exited free their share.
    pub(in crate::native) fn try_acquire(&self) -> Option<UploadSlot<'_>> {
        let mut parked = crate::native::lock_recover(&self.parked);
        parked.retain_mut(|process| {
            let _ = process.terminate();
            !exit_proved(&mut **process)
        });
        let unresolved = parked.len();
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active.saturating_add(unresolved) < self.max).then_some(active + 1)
            })
            .ok()
            .map(|_| UploadSlot { slots: self })
    }

    /// Keep a helper that may still be running counted against the cap.
    fn park(&self, process: Box<dyn Terminable>) {
        crate::native::lock_recover(&self.parked).push(process);
    }

    /// How many parked helpers have not been seen to exit.
    #[cfg(test)]
    fn parked_count(&self) -> usize {
        crate::native::lock_recover(&self.parked).len()
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
/// killed here instead of being dropped, which neither kills nor reaps it. That
/// runs on the caller, which is the window thread when a tab closes, so it
/// never waits: a child not yet gone is parked with the limiter and reaped by a
/// later admission. A child the watcher could not stop is parked too and
/// reported. Returns whether the watcher was started.
fn watch_cleanup(
    child: Child,
    slot: UploadSlot<'static>,
    deadline: Duration,
    start: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) -> bool {
    watch_process(child, slot, deadline, REAP_GRACE, start)
}

/// [`watch_cleanup`] over any [`Terminable`], with the reap `grace` explicit.
fn watch_process<P: Terminable + 'static>(
    child: P,
    slot: UploadSlot<'static>,
    deadline: Duration,
    grace: Duration,
    start: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) -> bool {
    let cell = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
    let watcher_cell = std::sync::Arc::clone(&cell);
    let slots = slot.slots;
    let watcher = Box::new(move || {
        let _slot = slot;
        let child = crate::native::lock_recover(&watcher_cell).take();
        if let Some(child) = child
            && let BoundedExit::Stuck(stuck) =
                run_bounded_process(child, deadline, || false, slots, grace)
        {
            tracing::warn!("remote upload cleanup could not be stopped: {stuck}");
        }
    });
    match start(watcher) {
        Ok(()) => true,
        Err(error) => {
            // The watcher never ran, so the child is still in the cell.
            let child = crate::native::lock_recover(&cell).take();
            if let Some(child) = child {
                kill_without_waiting(Box::new(child), slots);
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
    let mut cleanup = match crate::native::app::win_spawn::helper_command(program) {
        Ok(command) => command,
        Err(error) => {
            tracing::warn!("remote upload cleanup spawn failed: {error}");
            return;
        }
    };
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
    use std::process::Stdio;
    use std::sync::Mutex;

    /// Limiter for tests whose helper is expected to exit or be reaped.
    static TEST_SLOTS: UploadSlots = UploadSlots::new(1);

    fn stalled_child() -> Child {
        // Windows resolves `ping` to an absolute file (no relative `PATH`
        // lookup); elsewhere the name is passed through unchanged.
        #[cfg(unix)]
        let mut command = {
            let mut command =
                crate::native::app::win_spawn::helper_command("sleep").expect("sleep resolves");
            command.arg("30");
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = crate::native::app::win_spawn::helper_command("ping")
                .expect("ping.exe is in System32");
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
        let exit = run_bounded(child, Duration::from_millis(200), || false, &TEST_SLOTS);
        assert!(matches!(exit, BoundedExit::TimedOut), "got {exit:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_helper_that_exits_reports_its_status() {
        #[cfg(unix)]
        let mut command =
            crate::native::app::win_spawn::helper_command("true").expect("true resolves");
        #[cfg(windows)]
        let mut command = crate::native::app::win_spawn::helper_command("hostname")
            .expect("hostname.exe is in System32");
        #[cfg(windows)]
        crate::native::app::win_spawn::apply_no_console_window(&mut command);
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        match run_bounded(child, Duration::from_secs(10), || false, &TEST_SLOTS) {
            BoundedExit::Exited(status) => assert!(status.success()),
            other => panic!("expected a clean exit, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_helper_is_killed_promptly() {
        let child = stalled_child();
        let start = Instant::now();
        let exit = run_bounded(
            child,
            Duration::from_secs(30),
            || start.elapsed() >= Duration::from_millis(100),
            &TEST_SLOTS,
        );
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
        // The kill is sent without waiting, so the child may not be gone yet:
        // it stays counted until an admission finds it reaped.
        let freed = Instant::now() + Duration::from_secs(30);
        while SLOTS.try_acquire().is_none() {
            assert!(Instant::now() < freed, "the killed child was never reaped");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(SLOTS.parked_count(), 0);
        // Signal 0 probes for the process: a killed but unreaped child is a
        // zombie and still answers, so a failure here means it was reaped too.
        #[cfg(unix)]
        {
            let alive = std::process::Command::new("kill")
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

    #[cfg(unix)]
    fn exit_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn exit_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    /// A stand-in helper whose exit the test controls and whose kills and
    /// polls are counted.
    #[derive(Clone, Default)]
    struct Handle {
        exited: std::sync::Arc<std::sync::atomic::AtomicBool>,
        kills: std::sync::Arc<AtomicUsize>,
        polls: std::sync::Arc<AtomicUsize>,
    }

    impl Handle {
        fn exit(&self) {
            self.exited.store(true, Ordering::Release);
        }
    }

    impl Terminable for Handle {
        fn terminate(&mut self) -> std::io::Result<()> {
            self.kills.fetch_add(1, Ordering::AcqRel);
            Err(std::io::Error::other("kill refused"))
        }

        fn poll_exit(&mut self) -> std::io::Result<Option<ExitStatus>> {
            self.polls.fetch_add(1, Ordering::AcqRel);
            Ok(self.exited.load(Ordering::Acquire).then(exit_status))
        }
    }

    const NO_GRACE: Duration = Duration::from_millis(1);

    /// A helper that cannot be stopped keeps counting against the cap after its
    /// worker returned and released its slot, and is freed only once a later
    /// admission sees it exit. Covers the upload path (`run_bounded_process`).
    #[test]
    fn a_stuck_upload_helper_keeps_counting_until_it_is_seen_to_exit() {
        let slots = UploadSlots::new(2);
        let helper = Handle::default();
        let worker_slot = slots.try_acquire().expect("the worker's slot");
        let exit = run_bounded_process(helper.clone(), Duration::ZERO, || false, &slots, NO_GRACE);
        assert!(matches!(exit, BoundedExit::Stuck(_)), "got {exit:?}");
        drop(worker_slot);
        assert_eq!(slots.parked_count(), 1, "the helper is retained");
        // One of the two places is still occupied, however often it is asked.
        let other = slots.try_acquire().expect("one place is left");
        for _ in 0..3 {
            assert!(slots.try_acquire().is_none(), "the stuck helper counts");
        }
        assert!(
            helper.kills.load(Ordering::Acquire) >= 3,
            "each ask retries the kill"
        );
        drop(other);
        helper.exit();
        let first = slots.try_acquire().expect("the exit frees its place");
        let second = slots.try_acquire().expect("both places are free again");
        assert!(slots.try_acquire().is_none(), "the cap itself is unchanged");
        assert_eq!(slots.parked_count(), 0);
        drop((first, second));
    }

    /// The same for the cleanup watcher: it returns, its slot is released, and
    /// the cleanup helper that could not be stopped still occupies the cap.
    #[test]
    fn a_stuck_cleanup_helper_keeps_counting_after_its_watcher_returns() {
        static SLOTS: UploadSlots = UploadSlots::new(1);
        let helper = Handle::default();
        let slot = SLOTS.try_acquire().expect("slot");
        let started = watch_process(helper.clone(), slot, Duration::ZERO, NO_GRACE, |watcher| {
            std::thread::spawn(watcher).join().expect("watcher");
            Ok(())
        });
        assert!(started);
        assert_eq!(SLOTS.parked_count(), 1);
        assert!(
            SLOTS.try_acquire().is_none(),
            "the cleanup past the cap is skipped while its helper may still run"
        );
        helper.exit();
        assert!(SLOTS.try_acquire().is_some(), "freed once the exit is seen");
        assert_eq!(SLOTS.parked_count(), 0);
    }

    /// Tab close runs the watcher admission on the window thread: when the
    /// watcher cannot start, the helper is told to die once and checked once,
    /// never polled in a wait loop, and a helper still running is parked.
    #[test]
    fn a_watcher_that_cannot_start_never_waits_for_the_helper() {
        static SLOTS: UploadSlots = UploadSlots::new(1);
        let helper = Handle::default();
        let slot = SLOTS.try_acquire().expect("slot");
        // A long grace would be waited out by the old synchronous reap.
        let started = watch_process(
            helper.clone(),
            slot,
            Duration::from_secs(30),
            Duration::from_secs(3600),
            |_watcher| Err(std::io::Error::other("thread limit")),
        );
        assert!(!started);
        assert_eq!(helper.kills.load(Ordering::Acquire), 1, "killed once");
        assert_eq!(
            helper.polls.load(Ordering::Acquire),
            1,
            "checked once, not awaited"
        );
        assert_eq!(SLOTS.parked_count(), 1, "still counted: it may be running");
        helper.exit();
        assert!(SLOTS.try_acquire().is_some());
    }

    /// A helper already gone when the watcher fails to start is not parked.
    #[test]
    fn a_helper_that_died_at_once_is_not_parked() {
        static SLOTS: UploadSlots = UploadSlots::new(1);
        let helper = Handle::default();
        helper.exit();
        let slot = SLOTS.try_acquire().expect("slot");
        watch_process(
            helper,
            slot,
            Duration::from_secs(30),
            NO_GRACE,
            |_watcher| Err(std::io::Error::other("thread limit")),
        );
        assert_eq!(SLOTS.parked_count(), 0);
        assert!(SLOTS.try_acquire().is_some());
    }
}
