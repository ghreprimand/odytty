// SPDX-License-Identifier: GPL-3.0-only
//! Per-session output recording for the scrubbable replay overlay (Phase 2).
//!
//! A [`RecorderHandle`] is a cheap, clonable handle to one session's bounded
//! ring of recorded screen [`Snapshot`]s. The PTY pump thread writes frames
//! into it as output lands; the App reads a decoupled clone of the ring when the
//! user opens the replay overlay. Recording is **opt-in** (`session_replay`,
//! default off) and **local-only**: frames never leave memory — they are not
//! written to disk, logged, or sent anywhere, and the ring is dropped when the
//! session closes or recording is turned off.
//!
//! ## Presentation-only / isolation
//! Recording lives entirely off the render path: the pump thread clones the live
//! screen snapshot it just produced and pushes it here. The replay overlay only
//! ever reads a *clone* of the ring ([`RecorderHandle::frames_clone`]) and draws
//! into a throwaway snapshot copy, so it never mutates live core terminal state
//! and the live render frame is byte-identical whether or not the overlay is
//! open.
//!
//! ## Bounded memory (documented cap)
//! The ring is bounded by **both** a frame count ([`MAX_FRAMES`]) and a total
//! estimated byte budget ([`MAX_BYTES`]); whichever binds first evicts the
//! oldest frames. At least one frame is always retained once recorded, even if a
//! single frame alone exceeds the byte budget.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::{Cell, Snapshot};

/// Maximum number of recorded frames retained in the ring. Old frames are
/// evicted from the front once this is exceeded. Documented cap.
pub(super) const MAX_FRAMES: usize = 600;

/// Maximum total estimated bytes of recorded frame cell data retained in the
/// ring (24 MiB). Whichever of this and [`MAX_FRAMES`] binds first drives
/// eviction. Documented cap; keeps a runaway producer from growing memory
/// without bound.
pub(super) const MAX_BYTES: usize = 24 * 1024 * 1024;

/// The bounded ring of recorded frames. Not used directly outside this module;
/// callers hold a [`RecorderHandle`].
#[derive(Debug, Default)]
pub(super) struct OutputRecorder {
    frames: VecDeque<Snapshot>,
    /// Running estimate of the buffered cell bytes, kept in sync with `frames`
    /// so eviction is O(evicted) rather than O(n) per push.
    bytes: usize,
}

impl OutputRecorder {
    /// Estimated heap cost of one frame's cell grid. Cheap proxy for the real
    /// allocation; the absolute value only needs to be proportional for the
    /// byte-budget eviction to bound memory.
    fn frame_bytes(snapshot: &Snapshot) -> usize {
        snapshot.cells.len() * std::mem::size_of::<Cell>()
    }

    /// Push a recorded frame, then evict the oldest frames until both caps hold.
    fn record(&mut self, snapshot: Snapshot) {
        self.bytes += Self::frame_bytes(&snapshot);
        self.frames.push_back(snapshot);
        self.evict();
    }

    /// Evict oldest frames until both the frame-count and byte caps are
    /// satisfied. Always keeps at least one frame, so a lone oversized frame is
    /// still scrubbable rather than evicting itself to nothing.
    fn evict(&mut self) {
        while self.frames.len() > MAX_FRAMES {
            self.pop_front();
        }
        while self.bytes > MAX_BYTES && self.frames.len() > 1 {
            self.pop_front();
        }
    }

    fn pop_front(&mut self) {
        if let Some(front) = self.frames.pop_front() {
            self.bytes = self.bytes.saturating_sub(Self::frame_bytes(&front));
        }
    }

    fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.frames.len()
    }

    fn frames_clone(&self) -> Vec<Snapshot> {
        self.frames.iter().cloned().collect()
    }
}

/// A cheap, clonable handle to one session's recorder. The `enabled` flag lives
/// outside the mutex as an atomic so the hot pump path can skip locking (and
/// skip building a snapshot) entirely when recording is off — making the
/// default-off path zero-overhead and byte-identical.
#[derive(Debug, Clone)]
pub(super) struct RecorderHandle {
    enabled: Arc<AtomicBool>,
    inner: Arc<Mutex<OutputRecorder>>,
}

impl Default for RecorderHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl RecorderHandle {
    /// A fresh, disabled, empty recorder handle.
    pub(super) fn new() -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            inner: Arc::new(Mutex::new(OutputRecorder::default())),
        }
    }

    /// Whether recording is currently on. Single relaxed atomic load — the
    /// pump's gate before it does any snapshot work.
    pub(super) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Turn recording on or off. Turning it off clears the ring so memory is
    /// released promptly and a later re-enable starts fresh. Idempotent.
    pub(super) fn set_enabled(&self, on: bool) {
        let was = self.enabled.swap(on, Ordering::Relaxed);
        if was
            && !on
            && let Ok(mut inner) = self.inner.lock()
        {
            inner.clear();
        }
    }

    /// Record a frame. The caller has already checked [`Self::is_enabled`] and
    /// built `snapshot` (typically under the terminal lock it already holds), so
    /// this only locks the ring briefly to push + evict. It also re-checks
    /// [`Self::is_enabled`] as defense-in-depth so a disabled handle never
    /// buffers. A poisoned lock drops the frame silently — recording is
    /// best-effort and never load-bearing.
    pub(super) fn record(&self, snapshot: Snapshot) {
        if !self.is_enabled() {
            return;
        }
        if let Ok(mut inner) = self.inner.lock() {
            // Re-check UNDER the lock. `set_enabled(false)` swaps the atomic
            // false BEFORE it locks + clears, so a disable that races between the
            // pre-lock check above and this acquisition would otherwise leave one
            // stale frame in a just-cleared ring (it would survive into the next
            // session, whose re-enable skips the clear). Re-reading the flag here
            // means whichever of record/clear wins the lock, the ring ends empty.
            if !self.is_enabled() {
                return;
            }
            inner.record(snapshot);
        }
    }

    /// A fully decoupled clone of the recorded frames, oldest first, for the
    /// replay overlay to scrub. The overlay owns this copy, so the live session
    /// keeps recording underneath without any shared mutable state.
    pub(super) fn frames_clone(&self) -> Vec<Snapshot> {
        self.inner
            .lock()
            .map(|inner| inner.frames_clone())
            .unwrap_or_default()
    }

    /// Number of frames currently buffered (test/diagnostic seam).
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.inner.lock().map(|inner| inner.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Attrs, Cell, Dimensions, DynamicColors, Position};

    fn frame(columns: usize, rows: usize, fill: char) -> Snapshot {
        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position::default(),
            cursor_visible: true,
            colors: DynamicColors::default(),
            cells: vec![Cell::new(fill, Attrs::default()); columns * rows],
        }
    }

    #[test]
    fn disabled_handle_records_nothing() {
        // RECORDING-OFF-IS-BYTE-IDENTICAL: a disabled recorder never buffers a
        // frame even if `record` is called, so the default-off path holds no
        // state. (The pump additionally never calls `record` while disabled.)
        let handle = RecorderHandle::new();
        assert!(!handle.is_enabled());
        handle.record(frame(10, 4, 'x'));
        assert_eq!(handle.len(), 0);
        assert!(handle.frames_clone().is_empty());
    }

    #[test]
    fn enabled_handle_buffers_and_clones_frames() {
        let handle = RecorderHandle::new();
        handle.set_enabled(true);
        handle.record(frame(10, 4, 'a'));
        handle.record(frame(10, 4, 'b'));
        assert_eq!(handle.len(), 2);
        let frames = handle.frames_clone();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].cells[0].ch, 'a');
        assert_eq!(frames[1].cells[0].ch, 'b');
    }

    #[test]
    fn disabling_clears_the_ring() {
        let handle = RecorderHandle::new();
        handle.set_enabled(true);
        handle.record(frame(10, 4, 'a'));
        assert_eq!(handle.len(), 1);
        handle.set_enabled(false);
        assert_eq!(handle.len(), 0, "turning recording off frees the ring");
    }

    #[test]
    fn record_racing_a_disable_does_not_repopulate_the_cleared_ring() {
        // TOCTOU regression: `record` checks `is_enabled` once before locking,
        // and `set_enabled(false)` swaps the flag false BEFORE it locks + clears.
        // A `record` that passed its pre-lock gate but is still blocked on the
        // ring lock when the disable+clear runs must NOT push its frame — a
        // survivor would leak into the next session (whose re-enable skips the
        // clear). The under-lock re-check guarantees the ring ends empty.
        let handle = RecorderHandle::new();
        handle.set_enabled(true);

        // Hold the ring lock so a concurrent `record` blocks AFTER its pre-lock
        // `is_enabled()` gate (still true) and BEFORE it can push.
        let guard = handle.inner.lock().expect("lock ring");
        let racer = handle.clone();
        let joiner = std::thread::spawn(move || {
            racer.record(frame(8, 2, 'z'));
        });
        // Let the racer pass the gate and park on the lock.
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Emulate `set_enabled(false)`: swap the flag false (the ring is already
        // empty, so there is nothing to clear) while still holding the lock.
        handle.enabled.store(false, Ordering::Relaxed);
        drop(guard);

        joiner.join().expect("racer thread");
        assert_eq!(
            handle.len(),
            0,
            "a frame recorded across a disable must not survive"
        );
    }

    #[test]
    fn frame_count_cap_evicts_oldest() {
        // RING-BUFFER-BOUND-ENFORCED (frame count): pushing past MAX_FRAMES
        // evicts from the front, keeping exactly MAX_FRAMES newest frames.
        let mut rec = OutputRecorder::default();
        for i in 0..(MAX_FRAMES + 50) {
            // A 1x1 frame tags the index in the cursor row so we can identify
            // the surviving window.
            let mut f = frame(1, 1, 'z');
            f.cursor = Position { row: i, column: 0 };
            rec.record(f);
        }
        assert_eq!(rec.len(), MAX_FRAMES);
        // The oldest surviving frame is index 50 (the first 50 were evicted).
        assert_eq!(rec.frames.front().unwrap().cursor.row, 50);
        assert_eq!(rec.frames.back().unwrap().cursor.row, MAX_FRAMES + 49);
    }

    #[test]
    fn byte_budget_cap_evicts_oldest() {
        // RING-BUFFER-BOUND-ENFORCED (byte budget): large frames evict on the
        // byte cap well before the frame-count cap is reached.
        let mut rec = OutputRecorder::default();
        // Each frame ~ (MAX_BYTES / 4) bytes, so only a few fit under the cap.
        let cells_per_frame = (MAX_BYTES / 4) / std::mem::size_of::<Cell>().max(1);
        let columns = cells_per_frame.max(1);
        for _ in 0..20 {
            rec.record(frame(columns, 1, 'q'));
        }
        assert!(rec.len() < 20, "byte budget evicted frames");
        assert!(rec.bytes <= MAX_BYTES, "byte budget is respected");
        assert!(rec.len() >= 1, "at least one frame is always retained");
    }

    #[test]
    fn lone_oversized_frame_is_retained() {
        // A single frame larger than the whole byte budget is still kept (so it
        // remains scrubbable) rather than evicting itself to an empty ring.
        let mut rec = OutputRecorder::default();
        let cells = (MAX_BYTES * 2) / std::mem::size_of::<Cell>().max(1);
        rec.record(frame(cells.max(1), 1, 'Z'));
        assert_eq!(rec.len(), 1);
    }
}
