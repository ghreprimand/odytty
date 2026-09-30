// SPDX-License-Identifier: GPL-3.0-only
//! Held ConPTY children (see `crate::pty::spawn_held`): the suspended
//! primary thread with its release gate and fallback event, and the
//! startup-failure report that measures a held child from its resume.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{CreateEventW, ResumeThread, SetEvent};
use windows::core::PCWSTR;

use super::{describe_immediate_exit, should_report_startup_failure};
use crate::pty::held_gate::{HeldGate, StartOutcome};

/// The pane diagnostic for a child that has exited, if its exit was a
/// startup failure worth surfacing. The startup-failure window counts from
/// when the child began running: its resume for a held child, otherwise its
/// creation (`spawned`). A held child whose resume failed reports that
/// failure instead of an exit code; its exit is the termination that
/// followed, not the shell's.
pub(super) fn startup_report(
    outcome: &StartOutcome,
    spawned: Instant,
    exit_code: Option<u32>,
    teardown_requested: bool,
) -> Option<String> {
    let started = match outcome {
        StartOutcome::ResumeFailed(error) => {
            return (!teardown_requested).then(|| describe_failed_resume(error));
        }
        StartOutcome::Started(at) => *at,
        StartOutcome::NotStarted => spawned,
    };
    let code = exit_code?;
    should_report_startup_failure(code, started.elapsed(), teardown_requested)
        .then(|| describe_immediate_exit(code))
}

fn describe_failed_resume(error: &str) -> String {
    let error = error.replace(['\r', '\n'], " ");
    format!(
        "\r\n  OdyTTY: the shell could not be started ({}).\r\n  \
         The pseudoconsole could not start a usable shell.\r\n",
        error.trim()
    )
}

/// The suspended primary thread of a held spawn, shared by the session (which
/// releases it after the first successful surface-derived resize) and the
/// child-waiter thread (which releases it as a fallback once the window arms
/// it; see [`super::PtySession::arm_held_start_fallback`]).
pub(super) struct HeldStart {
    /// The primary thread while held, then the resume outcome and start
    /// time, published together (see `crate::pty::held_gate`).
    gate: HeldGate<OwnedHandle>,
    /// The armed fallback limit; `None` until the window arms it.
    fallback: Mutex<Option<Duration>>,
    /// Manual-reset event the waiter waits on alongside the process. Set when
    /// the fallback is armed or the child is released.
    event: OwnedHandle,
}

impl HeldStart {
    pub(super) fn new(thread: OwnedHandle, event: OwnedHandle) -> Self {
        Self {
            gate: HeldGate::new(thread),
            fallback: Mutex::new(None),
            event,
        }
    }

    pub(super) fn event(&self) -> HANDLE {
        HANDLE(self.event.as_raw_handle())
    }

    pub(super) fn signal(&self) {
        // SAFETY: `self.event` is a live, owned event handle.
        let _ = unsafe { SetEvent(self.event()) };
    }

    /// Arm the fallback once; a later arm keeps the first limit.
    pub(super) fn arm(&self, limit: Duration) {
        self.fallback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(limit);
        self.signal();
    }

    pub(super) fn fallback(&self) -> Option<Duration> {
        *self
            .fallback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Resume the child once. `Ok(false)` when it was already released. The
    /// outcome (start time or resume error) is published before the waiter
    /// can sample it; after a failed resume the caller terminates the child.
    pub(super) fn release(&self) -> io::Result<bool> {
        self.gate.release(|thread| {
            // Let the waiter leave its first wait; it finds the child released
            // once this resume completes.
            self.signal();
            // SAFETY: `thread` is the live, owned primary-thread handle
            // returned by `CreateProcessW` for this child; it is closed when
            // dropped at the end of this closure.
            if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })
    }

    pub(super) fn outcome(&self) -> StartOutcome {
        self.gate.outcome()
    }

    pub(super) fn is_held(&self) -> bool {
        self.gate.is_held()
    }
}

/// A manual-reset, initially unsignaled, unnamed event.
pub(super) fn create_manual_reset_event() -> io::Result<OwnedHandle> {
    // SAFETY: no security attributes and no name; on success the returned
    // handle is owned here and closed when the `OwnedHandle` drops.
    let event =
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(io::Error::other)?;
    // SAFETY: `event` is a fresh handle owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(event.0 as RawHandle) })
}
