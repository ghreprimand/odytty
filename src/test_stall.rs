// SPDX-License-Identifier: GPL-3.0-only
//! Bounded waits for tests that run a real PTY child.
//!
//! A test that joins a PTY output pump or reads a child to EOF can wait
//! forever when the slave stays open (a descendant holds it, or a runner stalls
//! in process teardown), and the CI job then ends only at its step timeout with
//! no indication of where it stopped. These helpers turn such a wait into a
//! failure that names the stage, so the job fails fast and says what stalled.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::pty::PtySession;

const POLL: Duration = Duration::from_millis(20);

/// Join `handle` within `limit`, or fail the test naming `stage`. The stuck
/// thread is abandoned, not waited for.
pub(crate) fn join_within<T>(handle: JoinHandle<T>, limit: Duration, stage: &str) -> T {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() {
        assert!(
            Instant::now() < deadline,
            "stalled at stage `{stage}`: thread still running after {limit:?}"
        );
        std::thread::sleep(POLL);
    }
    match handle.join() {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// The stage a [`run_staged`] body has reached, shown when it stalls.
#[derive(Clone)]
pub(crate) struct Stage(Arc<Mutex<&'static str>>);

impl Stage {
    pub(crate) fn enter(&self, name: &'static str) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = name;
    }

    fn current(&self) -> &'static str {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Run `body` on a worker thread and fail the test, naming the last stage the
/// body entered, when it has not finished within `limit`. A panic in the body
/// is re-raised unchanged. A stalled worker is abandoned.
pub(crate) fn run_staged(limit: Duration, body: impl FnOnce(&Stage) + Send + 'static) {
    let stage = Stage(Arc::new(Mutex::new("start")));
    let worker_stage = stage.clone();
    let worker = std::thread::spawn(move || body(&worker_stage));
    let deadline = Instant::now() + limit;
    while !worker.is_finished() {
        assert!(
            Instant::now() < deadline,
            "stalled at stage `{}` after {limit:?}",
            stage.current()
        );
        std::thread::sleep(POLL);
    }
    if let Err(payload) = worker.join() {
        std::panic::resume_unwind(payload);
    }
}

/// Read a session's output to EOF within `limit`. On a stall the reader is
/// forced to EOF and the test fails with the output seen so far, instead of
/// waiting for a slave that never closes.
pub(crate) fn read_to_end_within(session: &PtySession, limit: Duration, stage: &str) -> Vec<u8> {
    let mut reader = session.try_clone_reader().expect("clone reader");
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    let pump = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        let _ = sender.send(output);
    });
    if let Ok(output) = receiver.recv_timeout(limit) {
        join_within(pump, Duration::from_secs(10), stage);
        return output;
    }
    session.force_reader_eof();
    let partial = receiver.recv_timeout(Duration::from_secs(10)).ok();
    panic!(
        "stalled at stage `{stage}`: no EOF within {limit:?}; output so far: {:?}",
        partial.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn panic_text(result: std::thread::Result<()>) -> String {
        let payload = result.expect_err("expected a panic");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_stalled_join_fails_naming_its_stage_and_a_finished_one_returns() {
        let (release, blocked) = mpsc::channel::<()>();
        let stuck = std::thread::spawn(move || {
            let _ = blocked.recv();
        });
        let text = panic_text(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || {
                join_within(stuck, Duration::from_millis(100), "pump join");
            },
        )));
        assert!(text.contains("`pump join`"), "{text}");
        drop(release);
        assert_eq!(
            join_within(std::thread::spawn(|| 7), Duration::from_secs(5), "done"),
            7
        );
    }

    #[test]
    fn a_stalled_body_fails_naming_the_last_stage_entered() {
        let (release, blocked) = mpsc::channel::<()>();
        let text = panic_text(std::panic::catch_unwind(|| {
            run_staged(Duration::from_millis(150), move |stage| {
                stage.enter("spawn");
                stage.enter("write probe");
                let _ = blocked.recv();
                stage.enter("never reached");
            });
        }));
        assert!(text.contains("`write probe`"), "{text}");
        drop(release);
    }

    #[test]
    fn a_body_panic_is_raised_unchanged() {
        let text = panic_text(std::panic::catch_unwind(|| {
            run_staged(Duration::from_secs(5), |_| panic!("assertion text"));
        }));
        assert_eq!(text, "assertion text");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_exits_is_read_to_its_end() {
        let session = PtySession::spawn_shell_command(
            crate::core::Dimensions::new(40, 4),
            "printf 'stall-guard'",
        )
        .expect("spawn");
        let output = read_to_end_within(&session, Duration::from_secs(30), "read child");
        assert!(String::from_utf8_lossy(&output).contains("stall-guard"));
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_keeps_the_slave_open_fails_naming_the_stage() {
        let session =
            PtySession::spawn_shell_command(crate::core::Dimensions::new(40, 4), "exec sleep 30")
                .expect("spawn");
        let text = panic_text(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || {
                read_to_end_within(&session, Duration::from_millis(300), "read quiet child");
            },
        )));
        assert!(text.contains("`read quiet child`"), "{text}");
    }
}
