// SPDX-License-Identifier: GPL-3.0-only
//! Release state of a held child (see [`crate::pty::spawn_held`]).
//!
//! The child-waiter decides whether an exit was an immediate startup failure
//! by measuring from the moment the child began running. For a held child
//! that moment is its resume, so the resume and the publication of its
//! outcome happen under one lock. A sampler that has seen the child exit (a
//! shell that fails at once after a long hold) therefore cannot observe the
//! state between a successful resume and its start time and fall back to
//! the creation time, a losing release waits for the winner's outcome and
//! never overwrites it, and a failed resume is recorded as such rather than
//! looking like a child that was never resumed. Only the Windows backend
//! holds children; the state machine is portable so its ordering is tested
//! on every platform.

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

enum State<H> {
    /// Suspended; holds whatever the resume needs (the primary thread handle).
    Held(H),
    /// Running since the recorded instant, taken after the resume succeeded.
    Started(Instant),
    /// The resume failed with this error; the caller terminates the child.
    ResumeFailed(String),
}

/// A published release outcome, as sampled by [`HeldGate::outcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    /// Still held: never resumed.
    NotStarted,
    /// Resumed successfully; running since this instant.
    Started(Instant),
    /// The resume failed with this error.
    ResumeFailed(String),
}

pub(crate) struct HeldGate<H> {
    state: Mutex<State<H>>,
}

impl<H> HeldGate<H> {
    pub(crate) fn new(handle: H) -> Self {
        Self {
            state: Mutex::new(State::Held(handle)),
        }
    }

    fn state(&self) -> MutexGuard<'_, State<H>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Resume the child once through `resume`. `Ok(false)` when another
    /// release already ran; it returns only after that release published its
    /// outcome, and leaves the outcome unchanged. The state lock is held
    /// across `resume` and the publication, so `resume` must not call back
    /// into this gate. On success the start time (taken after `resume`
    /// returned) is published; on failure the error is published.
    pub(crate) fn release(&self, resume: impl FnOnce(H) -> io::Result<()>) -> io::Result<bool> {
        let mut state = self.state();
        let State::Held(_) = &*state else {
            return Ok(false);
        };
        // Placeholder while the handle is moved out; replaced below before
        // the lock is released on every path.
        let State::Held(handle) =
            std::mem::replace(&mut *state, State::ResumeFailed(String::new()))
        else {
            return Ok(false);
        };
        match resume(handle) {
            Ok(()) => {
                *state = State::Started(Instant::now());
                Ok(true)
            }
            Err(error) => {
                *state = State::ResumeFailed(error.to_string());
                Err(error)
            }
        }
    }

    /// The published release outcome. Waits for a release in progress, so a
    /// running child is never sampled without its start time.
    pub(crate) fn outcome(&self) -> StartOutcome {
        Self::sample(&self.state())
    }

    fn sample(state: &State<H>) -> StartOutcome {
        match state {
            State::Held(_) => StartOutcome::NotStarted,
            State::Started(at) => StartOutcome::Started(*at),
            State::ResumeFailed(error) => StartOutcome::ResumeFailed(error.clone()),
        }
    }

    /// Whether the child is still held (never resumed, and no resume failed).
    pub(crate) fn is_held(&self) -> bool {
        matches!(*self.state(), State::Held(_))
    }

    /// Test-only: sample without waiting; `None` while a release holds the
    /// state (between the resume and its publication).
    #[cfg(test)]
    fn try_outcome(&self) -> Option<StartOutcome> {
        match self.state.try_lock() {
            Ok(state) => Some(Self::sample(&state)),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                Some(Self::sample(&poisoned.into_inner()))
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Once the resume has succeeded the child can run and exit at once. No
    /// sample may be taken from that point until the start time is
    /// published; with a separate publication step the sample here would
    /// read `NotStarted` (or a transient state) and the waiter would fall
    /// back to the creation time.
    #[test]
    fn the_outcome_cannot_be_sampled_between_the_resume_and_its_publication() {
        let gate = HeldGate::new(());
        assert_eq!(gate.try_outcome(), Some(StartOutcome::NotStarted));
        let resumed_at = std::cell::Cell::new(None);
        let released = gate
            .release(|()| {
                resumed_at.set(Some(Instant::now()));
                // The child is runnable from here on.
                assert_eq!(
                    gate.try_outcome(),
                    None,
                    "the outcome was observable before it was published"
                );
                Ok(())
            })
            .expect("resume");
        assert!(released);
        let resumed_at = resumed_at.get().expect("resume ran");
        match gate.try_outcome() {
            Some(StartOutcome::Started(at)) => {
                assert!(at >= resumed_at, "start time precedes the resume");
            }
            other => panic!("expected a published start, got {other:?}"),
        }
        assert!(!gate.is_held());
    }

    /// A sampler on another thread that starts while the resume is in
    /// progress (the waiter woken by the child's exit) gets the published
    /// start time, never `NotStarted`.
    #[test]
    fn a_sampler_on_another_thread_gets_the_published_start_time() {
        let gate = Arc::new(HeldGate::new(()));
        let mut sampler = None;
        gate.release(|()| {
            let gate = Arc::clone(&gate);
            sampler = Some(std::thread::spawn(move || gate.outcome()));
            Ok(())
        })
        .expect("resume");
        let sampled = sampler.expect("sampler").join().expect("sampler thread");
        assert!(matches!(sampled, StartOutcome::Started(_)), "{sampled:?}");
        assert_eq!(sampled, gate.outcome());
    }

    /// A losing release that arrives during the winner's resume waits for
    /// the winner's outcome, reports `false`, and leaves the outcome as the
    /// winner published it.
    #[test]
    fn a_losing_release_sees_the_winner_and_does_not_overwrite_it() {
        let gate = Arc::new(HeldGate::new(()));
        let mut loser = None;
        gate.release(|()| {
            let gate = Arc::clone(&gate);
            loser = Some(std::thread::spawn(move || {
                let released = gate
                    .release(|()| panic!("a released child is resumed again"))
                    .expect("losing release");
                (released, gate.outcome())
            }));
            Ok(())
        })
        .expect("winning resume");
        let winner = gate.outcome();
        let (released, seen) = loser.expect("loser").join().expect("loser thread");
        assert!(!released);
        assert!(matches!(winner, StartOutcome::Started(_)));
        assert_eq!(seen, winner, "the loser saw the winner's outcome");
        assert_eq!(gate.outcome(), winner, "the winner's outcome is unchanged");
    }

    /// A failed resume is published as its own outcome, distinct from a
    /// child that was never resumed, and a later release does not retry it.
    #[test]
    fn a_failed_resume_is_published_explicitly_and_not_retried() {
        let gate = HeldGate::new(());
        let error = gate
            .release(|()| Err(io::Error::other("resume failed")))
            .expect_err("resume failure");
        assert_eq!(error.to_string(), "resume failed");
        assert_eq!(
            gate.outcome(),
            StartOutcome::ResumeFailed("resume failed".to_owned())
        );
        assert!(!gate.is_held());
        assert!(
            !gate
                .release(|()| panic!("a failed resume is retried"))
                .expect("second release")
        );
        assert_eq!(
            gate.outcome(),
            StartOutcome::ResumeFailed("resume failed".to_owned())
        );
    }
}
