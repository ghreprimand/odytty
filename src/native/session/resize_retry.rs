// SPDX-License-Identifier: GPL-3.0-only
//! Idle retry of a backend resize that failed.
//!
//! A pane's terminal model is resized first and the backend (local PTY or
//! attached session host) second. When the backend resize fails, the session
//! stays `pty_resize_dirty`, and before this retry nothing resent it until the
//! next structural or window geometry event: an attached host whose stream
//! recovered while the window stayed still kept the old size indefinitely.
//!
//! Each failed attempt schedules a retry with a bounded backoff (250 ms,
//! doubling to at most 5 s). The event loop wakes at the earliest scheduled
//! retry, and the retry resends the model's current dimensions. An attached
//! retry never blocks the main thread: it skips a busy client lock and sends
//! without waiting (`MSG_DONTWAIT` on Linux, a zero-timeout `POLLOUT` check
//! first on macOS/BSD), so a still-stalled host costs one or two syscalls. A
//! live divider drag suppresses backend resizes on purpose and cancels any
//! pending retry; the drag release flushes the final size as before.
//! A launch held for its first real resize (`crate::pty::spawn_held`) whose
//! resize failed starts only when a retry here succeeds.

use std::time::{Duration, Instant};

use super::model::WorkspaceSet;
use super::transport::SessionSource;

const FIRST_RETRY: Duration = Duration::from_millis(250);
const MAX_RETRY: Duration = Duration::from_secs(5);

/// Retry schedule for one session's backend resize.
#[derive(Debug, Default)]
pub(in crate::native) struct ResizeRetry {
    next_at: Option<Instant>,
    backoff: Duration,
}

impl ResizeRetry {
    /// Record a failed attempt at `now` and schedule the next one.
    pub(super) fn failed(&mut self, now: Instant) {
        self.backoff = if self.backoff.is_zero() {
            FIRST_RETRY
        } else {
            (self.backoff * 2).min(MAX_RETRY)
        };
        self.next_at = Some(now + self.backoff);
    }

    /// The backend accepted the dimensions, or a retry is no longer wanted.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.next_at
    }

    fn is_due(&self, now: Instant) -> bool {
        self.next_at.is_some_and(|at| now >= at)
    }
}

impl WorkspaceSet {
    /// Earliest scheduled backend-resize retry, for the event loop's wake
    /// set. `None` unless a backend resize has failed and is still pending.
    pub(in crate::native) fn next_backend_resize_retry(&self) -> Option<Instant> {
        self.sessions
            .values()
            .filter_map(|session| session.resize_retry.deadline())
            .min()
    }

    /// Resend the current model dimensions to every backend whose resize
    /// failed and whose retry is due.
    pub(in crate::native) fn retry_backend_resizes(&mut self, now: Instant) {
        for session in self.sessions.values_mut() {
            if !session.resize_retry.is_due(now) {
                continue;
            }
            if !session.pty_resize_dirty {
                session.resize_retry.clear();
                continue;
            }
            let (dimensions, metrics) = {
                let terminal = crate::native::lock_recover(&session.terminal);
                (terminal.screen().dimensions(), terminal.cell_metrics())
            };
            let accepted = match &session.source {
                SessionSource::Local { pty } => pty.try_lock().is_ok_and(|pty| {
                    pty.set_cell_metrics(metrics);
                    pty.resize(dimensions).is_ok()
                }),
                #[cfg(unix)]
                SessionSource::Attached { client } => client.try_lock().is_ok_and(|mut client| {
                    client
                        .try_resize_now(dimensions.columns as u32, dimensions.rows as u32)
                        .is_ok()
                }),
                #[cfg(test)]
                SessionSource::Headless { session } => {
                    session.record_cell_metrics(metrics);
                    session.try_record_resize(dimensions)
                }
            };
            if accepted {
                session.pty_resize_dirty = false;
                session.resize_retry.clear();
            } else {
                session.resize_retry.failed(now);
            }
        }
        // A held launch whose first resize failed starts only now that its
        // backend has the model's size.
        self.release_held_launches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_a_bounded_cadence() {
        let start = Instant::now();
        let mut retry = ResizeRetry::default();
        assert_eq!(retry.deadline(), None);
        let mut gaps = Vec::new();
        for _ in 0..8 {
            retry.failed(start);
            gaps.push(retry.deadline().expect("scheduled") - start);
        }
        assert_eq!(gaps[0], FIRST_RETRY);
        assert_eq!(gaps[1], FIRST_RETRY * 2);
        assert!(gaps.iter().all(|gap| *gap <= MAX_RETRY));
        assert_eq!(*gaps.last().expect("last"), MAX_RETRY);
        retry.clear();
        assert_eq!(retry.deadline(), None);
        retry.failed(start);
        assert_eq!(
            retry.deadline(),
            Some(start + FIRST_RETRY),
            "success resets"
        );
    }
}
