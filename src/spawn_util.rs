// SPDX-License-Identifier: GPL-3.0-only
//! Shared thread-spawn ownership helpers.
//!
//! Background threads are created in many places (PTY writer + stall monitor,
//! freeze watchdog, image-upload and connection-probe workers, opener and
//! upload-cleanup child reapers, the ConPTY child waiter). Thread creation is
//! fallible — it only fails under resource exhaustion (the thread ceiling or
//! address-space limits) — and each site historically decided ad hoc whether
//! that was fatal or degradable, and whether to log. These two helpers unify
//! the pattern into one diagnostic convention:
//!
//! * [`spawn_named`] — "spawn a task or return the error". The caller decides
//!   whether a failure is fatal (propagate the `io::Result`) or degradable.
//! * [`spawn_child_reaper`] — "spawn a detached reaper for a child process".
//!   Fire-and-forget with a fixed degrade-to-drop policy on reaper-spawn
//!   failure, so a fire-and-forget child is not left a zombie on Unix.
//!
//! Std-only and layer-neutral, so both `src/native/` and `src/pty/` sites use
//! the same seam.

use std::io;
use std::thread::{Builder, JoinHandle};

/// Spawn a named worker thread, returning its join handle or the spawn error.
///
/// This is the single "spawn a task or surface the error" seam. Thread creation
/// only fails under resource exhaustion; the caller chooses whether that is
/// fatal (propagate) or degradable (log and continue) — this helper never
/// swallows the error.
pub fn spawn_named<F, T>(name: impl Into<String>, f: F) -> io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().name(name.into()).spawn(f)
}

/// Spawn a detached reaper that blocks in [`std::process::Child::wait`] until
/// `child` exits, so a fire-and-forget child is not left a zombie on Unix.
///
/// The reaper is never joined and never delays process exit (teardown does not
/// join detached threads). If the reaper thread itself cannot be created
/// (resource exhaustion) the child is dropped un-reaped — the pre-reaper
/// behavior — and a warning naming the reaper is logged; the caller's own path
/// is never failed. On Windows there is no zombie concept (dropping a `Child`
/// closes the handle), but the reaper is harmless and keeps one behavior on both
/// platforms.
pub fn spawn_child_reaper(name: impl Into<String>, mut child: std::process::Child) {
    let name = name.into();
    if spawn_named(name.clone(), move || {
        let _ = child.wait();
    })
    .is_err()
    {
        tracing::warn!("reaper thread '{name}' unavailable; child not reaped");
    }
}
