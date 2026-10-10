// SPDX-License-Identifier: GPL-3.0-only
//! Freeze watchdog (FREEZE-HARDEN item b).
//!
//! The v0.7.0 freeze presented as a live event loop that serviced compositor
//! events mechanically while the render/input path was dead: pending input
//! and redraws, but no frame ever presented, at 0% CPU. This module detects
//! that signature and the distinct focused-Wayland callback-outstanding shape,
//! logging the app's state machine so the next freeze names its latch instead
//! of requiring a live debugger session.
//!
//! Design: [`WatchdogApp`] wraps the real [`App`] as the winit
//! [`ApplicationHandler`], noting "work-implying" events (input, IME, redraw
//! requests, PTY pump wakes) before delegating and mirroring a small state
//! snapshot into shared atomics after delegating. A detached monitor thread
//! wakes every couple of seconds and, when work has been pending for
//! [`STALL_AFTER`] with no frame presented since, emits ONE `warn!` record
//! with the mirrored state (re-emitted at most every [`RELOG_EVERY`] while
//! the stall persists; re-armed by the next presented frame). On the healthy
//! path the per-event cost is a handful of relaxed atomic stores and the
//! monitor thread sleeps — no locks, no allocation.
//!
//! PRIVACY (hard release rule): the stall record is STATE ONLY — booleans,
//! counters, and enum names baked into this file. No PTY bytes, no grid
//! text, no window titles. The seam tests below pin the charset of all three
//! records (stall, slow retry, callback outstanding) so a future edit cannot
//! quietly interpolate free-form strings.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long input/redraw work may stay pending with no presented frame
/// before the watchdog logs a stall. Conservative: normal frames land in
/// milliseconds; ten seconds of pending-but-unpresented work is a freeze.
const STALL_AFTER: Duration = Duration::from_secs(10);
/// While a stall persists, re-log at most this often.
const RELOG_EVERY: Duration = Duration::from_secs(60);
/// Monitor thread poll cadence. Coarse on purpose — the watchdog trades
/// detection latency for near-zero idle cost.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// A window in the renderer's skipped-frame slow retry is only classified as
/// healthy-but-hidden while redraws keep being delivered. The slow retry fires
/// about once a second ([`POLL_EVERY`] is two), so six seconds without a single
/// delivered redraw means the retry timer itself stopped: that is the loop not
/// delivering redraws, and the episode goes back to the classic stall record.
const SLOW_RETRY_LIVENESS: Duration = Duration::from_secs(6);
/// Fixed prefix of the slow-retry classification record (not a stall).
pub(super) const SLOW_RETRY_RECORD_PREFIX: &str =
    "freeze_watchdog: window in skipped-frame slow retry";

/// State snapshot the probe (`App::watchdog_state`, see
/// `app/watchdog_probe.rs`) hands the wrapper after every delegated event.
/// Everything is a bool / counter / C-like enum by construction — the type
/// itself enforces that no terminal content can flow into the stall record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WatchdogAppState {
    pub(super) focused: bool,
    pub(super) window_minimized: bool,
    pub(super) window_occluded: bool,
    pub(super) window_present: bool,
    pub(super) gpu_present: bool,
    /// Whether the live presentation uses the Wayland backend.
    pub(super) wayland_surface: bool,
    pub(super) overlay_open: bool,
    pub(super) context_menu_open: bool,
    /// Discriminant of `ActiveModal` (0 = None, 1 = CopyMode,
    /// 2 = HintsSelect, 3 = RenameTab, 4 = FloatArrange).
    pub(super) modal: u8,
    pub(super) needs_rebuild: bool,
    /// Frames that reached `present()` since GPU init.
    pub(super) frames_presented: u64,
    pub(super) consecutive_skipped_frames: u32,
    /// The renderer spent its fast skipped-frame retry budget and is on the
    /// slow keep-alive retry (about one attempt per second). Together with an
    /// unfocused or occluded window this is a hidden surface the compositor
    /// is not presenting, not a render-path freeze, as long as the retries
    /// keep delivering redraws (see [`SLOW_RETRY_LIVENESS`]).
    pub(super) skip_slow_retry: bool,
    /// `RedrawRequested` events DELIVERED to the app since launch. Compared
    /// against the episode-start snapshot in [`WatchdogShared::evaluate`]: a
    /// flat counter means the windowing system never asked for the frame the
    /// app is waiting to draw, which is a hidden/asleep surface rather than a
    /// stall. See the `App::redraws_delivered` field docs.
    pub(super) redraws_delivered: u64,
    /// Whether the render path genuinely OWES a frame right now: a rebuild is
    /// due (multipane-aware, not the bare `needs_rebuild` flag) or a skipped
    /// frame is scheduled to retry. This is the gating discriminator for the
    /// stall log (see `evaluate`) and is intentionally NOT part of the logged
    /// postmortem record; it only decides whether a stall is real. An idle or
    /// background window latches pending work without owing a frame, so gating
    /// on this silences that false positive while the genuine
    /// redraws-owed-but-not-presented freeze still trips.
    pub(super) render_owed: bool,
}

/// Atomics shared between the wrapper (writer) and the monitor thread
/// (reader). Millisecond timestamps are offsets from `epoch`.
pub(super) struct WatchdogShared {
    epoch: Instant,
    /// Work-implying event seen and no frame presented since.
    pending: AtomicBool,
    pending_since_ms: AtomicU64,
    /// Stall already logged for the current pending episode.
    logged: AtomicBool,
    last_log_ms: AtomicU64,
    /// Callback-outstanding record already logged for this pending episode.
    callback_logged: AtomicBool,
    callback_last_log_ms: AtomicU64,
    // --- mirrored state (last snapshot after a delegated event) ---
    focused: AtomicBool,
    window_minimized: AtomicBool,
    window_occluded: AtomicBool,
    window_present: AtomicBool,
    gpu_present: AtomicBool,
    wayland_surface: AtomicBool,
    overlay_open: AtomicBool,
    context_menu_open: AtomicBool,
    modal: AtomicU8,
    needs_rebuild: AtomicBool,
    frames_presented: AtomicU64,
    consecutive_skipped_frames: AtomicU64,
    skip_slow_retry: AtomicBool,
    /// Slow-retry classification record already logged this episode.
    slow_retry_logged: AtomicBool,
    slow_retry_last_log_ms: AtomicU64,
    /// Liveness tracking for the slow-retry class: whether it is armed, the
    /// delivered-redraw count last seen, and when that count last advanced.
    slow_retry_tracking: AtomicBool,
    slow_retry_last_redraws: AtomicU64,
    slow_retry_progress_ms: AtomicU64,
    /// Whether a frame is genuinely owed (gates the stall log; not logged).
    render_owed: AtomicBool,
    /// Monitor-clock instant when `render_owed` most recently became true.
    render_owed_since_ms: AtomicU64,
    /// Delivered-`RedrawRequested` counter, mirrored from the app.
    redraws_delivered: AtomicU64,
    /// Value of `redraws_delivered` when the current pending episode opened.
    /// The DIFFERENCE is the gate: zero deliveries during the episode means
    /// the windowing system never asked for a frame.
    redraws_at_pending_start: AtomicU64,
}

impl WatchdogShared {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: Instant::now(),
            pending: AtomicBool::new(false),
            pending_since_ms: AtomicU64::new(0),
            logged: AtomicBool::new(false),
            last_log_ms: AtomicU64::new(0),
            callback_logged: AtomicBool::new(false),
            callback_last_log_ms: AtomicU64::new(0),
            focused: AtomicBool::new(true),
            window_minimized: AtomicBool::new(false),
            window_occluded: AtomicBool::new(false),
            window_present: AtomicBool::new(false),
            gpu_present: AtomicBool::new(false),
            wayland_surface: AtomicBool::new(false),
            overlay_open: AtomicBool::new(false),
            context_menu_open: AtomicBool::new(false),
            modal: AtomicU8::new(0),
            needs_rebuild: AtomicBool::new(false),
            frames_presented: AtomicU64::new(0),
            consecutive_skipped_frames: AtomicU64::new(0),
            skip_slow_retry: AtomicBool::new(false),
            slow_retry_logged: AtomicBool::new(false),
            slow_retry_last_log_ms: AtomicU64::new(0),
            slow_retry_tracking: AtomicBool::new(false),
            slow_retry_last_redraws: AtomicU64::new(0),
            slow_retry_progress_ms: AtomicU64::new(0),
            render_owed: AtomicBool::new(false),
            render_owed_since_ms: AtomicU64::new(0),
            redraws_delivered: AtomicU64::new(0),
            redraws_at_pending_start: AtomicU64::new(0),
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Latch pending work. Returns whether this opened a new episode.
    pub(in crate::native) fn note_activity(&self) -> bool {
        let opened = !self.pending.swap(true, Ordering::Relaxed);
        if opened {
            self.pending_since_ms
                .store(self.now_ms(), Ordering::Relaxed);
            self.logged.store(false, Ordering::Relaxed);
            self.callback_logged.store(false, Ordering::Relaxed);
            self.reset_slow_retry_episode();
            // Baseline the delivered-redraw counter for this episode. The
            // wrapper calls this BEFORE delegating the event, so a
            // `RedrawRequested` that opens an episode still counts inside it.
            self.redraws_at_pending_start.store(
                self.redraws_delivered.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
        opened
    }

    /// Set the episode's redraw baseline to `at_start`, the mirrored window's
    /// own delivered-redraw count when the episode opened. The process host
    /// mirrors one window at a time and each window counts its own redraws,
    /// so the baseline must follow the mirrored window: another window's
    /// counter would hide (or invent) this window's deliveries.
    pub(in crate::native) fn rebase_episode_redraws(&self, at_start: u64) {
        self.redraws_at_pending_start
            .store(at_start, Ordering::Relaxed);
    }

    pub(in crate::native) fn note_present(&self) {
        self.pending.store(false, Ordering::Relaxed);
        self.logged.store(false, Ordering::Relaxed);
        self.callback_logged.store(false, Ordering::Relaxed);
        self.reset_slow_retry_episode();
    }

    fn reset_slow_retry_episode(&self) {
        self.slow_retry_logged.store(false, Ordering::Relaxed);
        self.slow_retry_tracking.store(false, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::native) fn set_render_owed(&self, owed: bool) {
        self.store_render_owed(owed);
    }

    fn store_render_owed(&self, owed: bool) {
        let was_owed = self.render_owed.swap(owed, Ordering::Relaxed);
        if owed && !was_owed {
            self.render_owed_since_ms
                .store(self.now_ms(), Ordering::Relaxed);
        } else if !owed {
            self.render_owed_since_ms.store(0, Ordering::Relaxed);
        }
    }

    pub(in crate::native) fn store_state(&self, state: &WatchdogAppState) {
        self.focused.store(state.focused, Ordering::Relaxed);
        self.window_minimized
            .store(state.window_minimized, Ordering::Relaxed);
        self.window_occluded
            .store(state.window_occluded, Ordering::Relaxed);
        self.window_present
            .store(state.window_present, Ordering::Relaxed);
        self.gpu_present.store(state.gpu_present, Ordering::Relaxed);
        self.wayland_surface
            .store(state.wayland_surface, Ordering::Relaxed);
        self.overlay_open
            .store(state.overlay_open, Ordering::Relaxed);
        self.context_menu_open
            .store(state.context_menu_open, Ordering::Relaxed);
        self.modal.store(state.modal, Ordering::Relaxed);
        self.needs_rebuild
            .store(state.needs_rebuild, Ordering::Relaxed);
        self.frames_presented
            .store(state.frames_presented, Ordering::Relaxed);
        self.consecutive_skipped_frames.store(
            u64::from(state.consecutive_skipped_frames),
            Ordering::Relaxed,
        );
        self.skip_slow_retry
            .store(state.skip_slow_retry, Ordering::Relaxed);
        self.store_render_owed(state.render_owed);
        self.redraws_delivered
            .store(state.redraws_delivered, Ordering::Relaxed);
    }

    fn snapshot(&self) -> WatchdogAppState {
        WatchdogAppState {
            focused: self.focused.load(Ordering::Relaxed),
            window_minimized: self.window_minimized.load(Ordering::Relaxed),
            window_occluded: self.window_occluded.load(Ordering::Relaxed),
            window_present: self.window_present.load(Ordering::Relaxed),
            gpu_present: self.gpu_present.load(Ordering::Relaxed),
            wayland_surface: self.wayland_surface.load(Ordering::Relaxed),
            overlay_open: self.overlay_open.load(Ordering::Relaxed),
            context_menu_open: self.context_menu_open.load(Ordering::Relaxed),
            modal: self.modal.load(Ordering::Relaxed),
            needs_rebuild: self.needs_rebuild.load(Ordering::Relaxed),
            frames_presented: self.frames_presented.load(Ordering::Relaxed),
            consecutive_skipped_frames: u32::try_from(
                self.consecutive_skipped_frames.load(Ordering::Relaxed),
            )
            .unwrap_or(u32::MAX),
            skip_slow_retry: self.skip_slow_retry.load(Ordering::Relaxed),
            render_owed: self.render_owed.load(Ordering::Relaxed),
            redraws_delivered: self.redraws_delivered.load(Ordering::Relaxed),
        }
    }

    /// `RedrawRequested` deliveries observed since the current pending episode
    /// opened. Zero means the windowing system has not asked this app to draw
    /// for the whole episode.
    fn redraws_this_episode(&self) -> u64 {
        self.redraws_delivered
            .load(Ordering::Relaxed)
            .saturating_sub(self.redraws_at_pending_start.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(in crate::native) fn note_redraw_delivered(&self) {
        self.redraws_delivered.fetch_add(1, Ordering::Relaxed);
    }

    /// One monitor-thread evaluation step at `now_ms`. Returns the stall
    /// record to log, if the stall condition holds and rate limits allow.
    /// Pure decision logic, factored for the tests below.
    pub(in crate::native) fn evaluate(&self, now_ms: u64) -> Option<String> {
        if !self.pending.load(Ordering::Relaxed) {
            return None;
        }
        // Gate: pending work alone is not a stall. An idle or background
        // window (unfocused, or a redraw requested for a non-visible pane)
        // latches `pending` but owes no present, so it would otherwise cry
        // wolf at STALL_AFTER and re-log every RELOG_EVERY. Only a genuinely
        // owed-but-unpresented frame is the v0.7.0 freeze signature this
        // module exists to catch, so require `render_owed` here.
        if !self.render_owed.load(Ordering::Relaxed) {
            return None;
        }
        // A focused, visible window with an owed frame and no delivered redraw
        // is the distinct outstanding-Wayland-callback signature. This branch
        // deliberately precedes and does not alter the classic v0.7.0 gate
        // below. A parallel rate-limit latch lets a later delivered redraw use
        // the original record immediately if the render path is also stalled.
        if self.redraws_this_episode() == 0 {
            let owed_since = self.render_owed_since_ms.load(Ordering::Relaxed);
            let owed_for = now_ms.saturating_sub(owed_since);
            let state = self.snapshot();
            let stale_after = u64::try_from(
                crate::native::app::frame_callback_hatch::FRAME_CALLBACK_STALE_AFTER.as_millis(),
            )
            .unwrap_or(u64::MAX);
            let already_logged = self.callback_logged.load(Ordering::Relaxed);
            let last_log = self.callback_last_log_ms.load(Ordering::Relaxed);
            if state.focused
                && !state.window_minimized
                && !state.window_occluded
                && state.window_present
                && state.gpu_present
                && state.wayland_surface
                && owed_for >= stale_after
                && (!already_logged
                    || now_ms.saturating_sub(last_log)
                        >= u64::try_from(RELOG_EVERY.as_millis()).unwrap_or(u64::MAX))
            {
                self.callback_logged.store(true, Ordering::Relaxed);
                self.callback_last_log_ms.store(now_ms, Ordering::Relaxed);
                return Some(format_callback_outstanding_record(owed_for / 1000, &state));
            }
        }
        // Once this episode has been identified as callback-outstanding, keep
        // its diagnostic class stable even if a later hatch paint increments
        // the redraw counter. A successful present opens a fresh episode; a
        // genuine v0.7.0 episode that begins with delivered redraws never sets
        // this latch and continues through the byte-identical classic path.
        if self.callback_logged.load(Ordering::Relaxed) {
            return None;
        }
        // Gate: an owed frame the windowing system never ASKED for is not a
        // stall either. When an output sleeps (DPMS-off), a surface is
        // occluded, or redraws are throttled to a compositor frame callback
        // that legitimately stops arriving, zero presented frames is the
        // correct steady state — the app is simply not being asked to draw.
        // The freeze this module exists to catch has the opposite shape:
        // `RedrawRequested` keeps being delivered and no frame comes out.
        // Requiring at least one delivery inside the episode separates them
        // without weakening that catch.
        if self.redraws_this_episode() == 0 {
            return None;
        }
        let pending_since = self.pending_since_ms.load(Ordering::Relaxed);
        let pending_for = now_ms.saturating_sub(pending_since);
        let stall_after = u64::try_from(STALL_AFTER.as_millis()).unwrap_or(u64::MAX);
        // An unfocused or occluded window on the renderer's slow skipped-frame
        // retry whose retries keep delivering redraws is a surface the
        // windowing system is not presenting, not a render-path freeze. Classify
        // it distinctly. When the redraws stop advancing the retry timer itself
        // is dead (the loop is not delivering), and the episode falls through
        // to the classic stall record below, so real stalls stay detectable.
        match self.classify_slow_retry(now_ms, pending_for) {
            SlowRetry::Record(record) => return Some(record),
            SlowRetry::Quiet => return None,
            SlowRetry::NotApplicable => {}
        }
        if pending_for < stall_after {
            return None;
        }
        let already_logged = self.logged.load(Ordering::Relaxed);
        let last_log = self.last_log_ms.load(Ordering::Relaxed);
        if already_logged
            && now_ms.saturating_sub(last_log)
                < u64::try_from(RELOG_EVERY.as_millis()).unwrap_or(u64::MAX)
        {
            return None;
        }
        self.logged.store(true, Ordering::Relaxed);
        self.last_log_ms.store(now_ms, Ordering::Relaxed);
        Some(format_stall_record(
            pending_for / 1000,
            self.redraws_this_episode(),
            &self.snapshot(),
        ))
    }
}

/// Outcome of the slow-retry classification step in [`WatchdogShared::evaluate`].
enum SlowRetry {
    /// The episode is not in the slow-retry class (or its retries stopped
    /// delivering redraws): continue with the classic stall decision.
    NotApplicable,
    /// In the class, nothing to log now (inside the stall window or rate limit).
    Quiet,
    /// In the class: log this distinct, non-stall record.
    Record(String),
}

impl WatchdogShared {
    fn classify_slow_retry(&self, now_ms: u64, pending_for: u64) -> SlowRetry {
        let state = self.snapshot();
        let in_class = state.skip_slow_retry
            && (!state.focused || state.window_occluded)
            && !state.window_minimized
            && state.window_present
            && state.gpu_present;
        if !in_class {
            self.slow_retry_tracking.store(false, Ordering::Relaxed);
            return SlowRetry::NotApplicable;
        }
        let was_tracking = self.slow_retry_tracking.swap(true, Ordering::Relaxed);
        let last = self
            .slow_retry_last_redraws
            .swap(state.redraws_delivered, Ordering::Relaxed);
        if !was_tracking || state.redraws_delivered != last {
            self.slow_retry_progress_ms.store(now_ms, Ordering::Relaxed);
        }
        let idle_for = now_ms.saturating_sub(self.slow_retry_progress_ms.load(Ordering::Relaxed));
        if idle_for >= u64::try_from(SLOW_RETRY_LIVENESS.as_millis()).unwrap_or(u64::MAX) {
            return SlowRetry::NotApplicable;
        }
        if pending_for < u64::try_from(STALL_AFTER.as_millis()).unwrap_or(u64::MAX) {
            return SlowRetry::Quiet;
        }
        let already_logged = self.slow_retry_logged.load(Ordering::Relaxed);
        let last_log = self.slow_retry_last_log_ms.load(Ordering::Relaxed);
        if already_logged
            && now_ms.saturating_sub(last_log)
                < u64::try_from(RELOG_EVERY.as_millis()).unwrap_or(u64::MAX)
        {
            return SlowRetry::Quiet;
        }
        self.slow_retry_logged.store(true, Ordering::Relaxed);
        self.slow_retry_last_log_ms.store(now_ms, Ordering::Relaxed);
        SlowRetry::Record(format_slow_retry_record(
            pending_for / 1000,
            self.redraws_this_episode(),
            &state,
        ))
    }
}

/// Spawn the detached monitor thread. It holds only a weak reference so it
/// unwinds naturally when the event loop (and its `Arc`) is gone.
pub(super) fn spawn_monitor(shared: &Arc<WatchdogShared>) {
    let weak = Arc::downgrade(shared);
    // Fire-and-forget: a failed spawn here just means no freeze diagnostics, not
    // a broken session, so the error is intentionally dropped.
    let _ = crate::spawn_util::spawn_named("odytty-freeze-watchdog", move || {
        loop {
            std::thread::sleep(POLL_EVERY);
            let Some(shared) = weak.upgrade() else {
                return;
            };
            if let Some(record) = shared.evaluate(shared.now_ms()) {
                if record.starts_with(SLOW_RETRY_RECORD_PREFIX) {
                    // A classification, not a freeze: the surface is hidden
                    // or unfocused and the retry loop is alive.
                    tracing::info!("{record}");
                } else {
                    tracing::warn!("{record}");
                }
            }
        }
    });
}

/// The stall record: STATE ONLY, single line, fixed key set. See the module
/// privacy note and the charset seam test.
fn format_stall_record(
    pending_secs: u64,
    redraws_this_episode: u64,
    state: &WatchdogAppState,
) -> String {
    format!(
        "freeze_watchdog: work pending {pending_secs}s with no presented frame; \
         focused={} minimized={} window_present={} gpu_present={} overlay_open={} \
         context_menu={} modal={} needs_rebuild={} frames_presented={} skipped_frames={} \
         redraws_delivered={redraws_this_episode}",
        state.focused,
        state.window_minimized,
        state.window_present,
        state.gpu_present,
        state.overlay_open,
        state.context_menu_open,
        modal_name(state.modal),
        state.needs_rebuild,
        state.frames_presented,
        state.consecutive_skipped_frames,
    )
}

/// State-only record for the slow-retry class: an unfocused or occluded
/// window whose skipped-frame retries still deliver redraws. Not a stall.
fn format_slow_retry_record(
    pending_secs: u64,
    redraws_this_episode: u64,
    state: &WatchdogAppState,
) -> String {
    format!(
        "{SLOW_RETRY_RECORD_PREFIX} for {pending_secs}s (retries delivering redraws, not a stall); \
         focused={} minimized={} occluded={} window_present={} gpu_present={} \
         frames_presented={} skipped_frames={} redraws_delivered={redraws_this_episode}",
        state.focused,
        state.window_minimized,
        state.window_occluded,
        state.window_present,
        state.gpu_present,
        state.frames_presented,
        state.consecutive_skipped_frames,
    )
}

/// Distinct state-only record for the no-redraw callback-outstanding class.
fn format_callback_outstanding_record(owed_secs: u64, state: &WatchdogAppState) -> String {
    format!(
        "freeze_watchdog: frame owed with compositor callback outstanding for {owed_secs}s; \
         focused={} minimized={} window_present={} gpu_present={} frames_presented={} \
         redraws_delivered=0",
        state.focused,
        state.window_minimized,
        state.window_present,
        state.gpu_present,
        state.frames_presented,
    )
}

fn modal_name(discriminant: u8) -> &'static str {
    match discriminant {
        0 => "none",
        1 => "copy_mode",
        2 => "hints_select",
        3 => "rename_tab",
        4 => "float_arrange",
        _ => "unknown",
    }
}

// The winit handler odytty actually runs is
// `crate::native::app::MultiWindowHost` (v0.15.0 D): it owns every live `App`,
// routes events through current ownership, aggregates control flow and freeze
// watchdog across windows, and services New Window / keyboard-merge requests.
// This module now owns only the shared freeze-detector record
// ([`WatchdogShared`]) and its monitor thread; the host calls `note_activity`,
// `note_present`, and `store_state` directly. The former single-window
// `WatchdogApp` wrapper was folded into the host so watchdog bookkeeping and
// multi-window routing live in one place.

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> WatchdogAppState {
        WatchdogAppState {
            focused: true,
            window_minimized: false,
            window_occluded: false,
            window_present: true,
            gpu_present: true,
            wayland_surface: true,
            overlay_open: false,
            context_menu_open: false,
            modal: 0,
            needs_rebuild: true,
            frames_presented: 1234,
            consecutive_skipped_frames: 0,
            skip_slow_retry: false,
            render_owed: true,
            redraws_delivered: 77,
        }
    }

    /// PRIVACY SEAM (hard release rule): the stall record must be state-only.
    /// Pin the full charset — lowercase key names, digits, `=`/`_`/spaces and
    /// the fixed prefix — so no future edit can interpolate terminal content
    /// (PTY bytes, grid text, window titles) without failing this test.
    #[test]
    fn stall_record_is_state_only() {
        let record = format_stall_record(17, 4, &state());
        assert!(
            record.starts_with("freeze_watchdog: work pending 17s with no presented frame; "),
            "got: {record}"
        );
        let body = &record["freeze_watchdog: work pending 17s with no presented frame; ".len()..];
        for token in body.split_whitespace() {
            let (key, value) = token.split_once('=').expect("key=value tokens only");
            assert!(
                key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "unexpected key charset: {key}"
            );
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "unexpected value charset: {value} (free-form strings are banned here)"
            );
        }
    }

    /// The shared state-only charset rule for every watchdog record: a fixed
    /// prefix, then `key=value` tokens of lowercase names, digits and `_`.
    fn assert_state_only(record: &str, prefix: &str) {
        assert!(record.starts_with(prefix), "got: {record}");
        for token in record[prefix.len()..].split_whitespace() {
            let (key, value) = token.split_once('=').expect("key=value tokens only");
            assert!(
                key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "unexpected key charset: {key}"
            );
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "unexpected value charset: {value}"
            );
        }
    }

    const CALLBACK_PREFIX: &str =
        "freeze_watchdog: frame owed with compositor callback outstanding for ";

    fn callback_stale_after_ms() -> u64 {
        u64::try_from(
            crate::native::app::frame_callback_hatch::FRAME_CALLBACK_STALE_AFTER.as_millis(),
        )
        .unwrap()
    }

    /// A focused, visible Wayland window owes a frame and is never asked to
    /// draw: one state-only callback record once the stale window passes,
    /// silence until the re-log interval, then one more.
    #[test]
    fn callback_outstanding_record_fires_after_the_stale_window_and_relogs() {
        let shared = WatchdogShared::new();
        shared.store_state(&state());
        shared.note_activity();
        shared.set_render_owed(true);
        let since = shared.render_owed_since_ms.load(Ordering::Relaxed);
        let stale = callback_stale_after_ms();
        assert_eq!(shared.evaluate(since + stale - 1), None);
        let record = shared.evaluate(since + stale).expect("callback record");
        let secs = (stale / 1000).to_string();
        assert_state_only(&record, &format!("{CALLBACK_PREFIX}{secs}s; "));
        assert!(record.contains("redraws_delivered=0"));
        assert_eq!(
            shared.evaluate(since + stale + 2_000),
            None,
            "no immediate relog"
        );
        let relog = u64::try_from(RELOG_EVERY.as_millis()).unwrap();
        assert!(
            shared
                .evaluate(since + stale + relog)
                .is_some_and(|record| record.starts_with(CALLBACK_PREFIX)),
            "relogs after the interval"
        );
    }

    /// Once the callback class fired, a later delivered redraw does not
    /// switch the episode to the classic stall record; a present opens a fresh
    /// episode where the classic path works again.
    #[test]
    fn callback_record_suppresses_the_classic_record_until_a_present() {
        let shared = WatchdogShared::new();
        shared.store_state(&state());
        shared.note_activity();
        shared.set_render_owed(true);
        let since = shared.render_owed_since_ms.load(Ordering::Relaxed);
        let stale = callback_stale_after_ms();
        assert!(shared.evaluate(since + stale).is_some());
        shared.note_redraw_delivered();
        let stall = u64::try_from(STALL_AFTER.as_millis()).unwrap();
        assert_eq!(
            shared.evaluate(since + stale + stall + 1_000),
            None,
            "the classic record stays suppressed in this episode"
        );

        shared.note_present();
        shared.note_activity();
        shared.note_redraw_delivered();
        let pending = shared.pending_since_ms.load(Ordering::Relaxed);
        let classic = shared.evaluate(pending + stall).expect("classic record");
        assert!(
            classic.starts_with("freeze_watchdog: work pending "),
            "got: {classic}"
        );
    }

    #[test]
    fn stall_record_names_every_postmortem_field() {
        let record = format_stall_record(10, 4, &state());
        for key in [
            "focused=",
            "minimized=",
            "window_present=",
            "gpu_present=",
            "overlay_open=",
            "context_menu=",
            "modal=",
            "needs_rebuild=",
            "frames_presented=",
            "skipped_frames=",
            "redraws_delivered=",
        ] {
            assert!(record.contains(key), "missing {key} in: {record}");
        }
    }

    #[test]
    fn evaluate_triggers_only_after_the_stall_window() {
        let shared = WatchdogShared::new();
        // No pending work: never triggers.
        assert_eq!(shared.evaluate(1_000_000), None);

        shared.note_activity();
        shared.set_render_owed(true);
        shared.note_redraw_delivered();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        // Inside the window: silent.
        assert_eq!(shared.evaluate(since + 9_999), None);
        // Past the window: logs once…
        assert!(shared.evaluate(since + 10_000).is_some());
        // …and not again immediately…
        assert_eq!(shared.evaluate(since + 12_000), None);
        // …until the re-log interval elapses.
        assert!(shared.evaluate(since + 10_000 + 60_000).is_some());
    }

    #[test]
    fn presented_frame_rearms_the_watchdog() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        shared.set_render_owed(true);
        shared.note_redraw_delivered();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert!(shared.evaluate(since + 10_000).is_some());

        shared.note_present();
        assert_eq!(
            shared.evaluate(since + 20_000),
            None,
            "present clears the pending latch"
        );

        shared.note_activity();
        shared.note_redraw_delivered();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert!(
            shared.evaluate(since + 10_000).is_some(),
            "a fresh episode logs again"
        );
    }

    #[test]
    fn mirrored_state_round_trips_through_the_atomics() {
        let shared = WatchdogShared::new();
        let state = WatchdogAppState {
            focused: false,
            window_minimized: true,
            window_occluded: false,
            window_present: true,
            gpu_present: false,
            wayland_surface: true,
            overlay_open: true,
            context_menu_open: true,
            modal: 2,
            needs_rebuild: true,
            frames_presented: 987,
            consecutive_skipped_frames: 3,
            skip_slow_retry: true,
            render_owed: true,
            redraws_delivered: 4_242,
        };
        shared.store_state(&state);
        assert_eq!(shared.snapshot(), state);
    }

    /// REGRESSION GUARD for the observed false positive: an idle or background
    /// window latches pending work (a redraw request, a PTY wake) but owes no
    /// frame. Even well past STALL_AFTER, `evaluate` must stay silent when
    /// `render_owed` is false — this is the ~33-minute unfocused/no-owed noise
    /// series from the real log that the gate removes.
    #[test]
    fn idle_window_with_no_owed_frame_never_stalls() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        // render_owed defaults to false; leave it so (nothing is owed).
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert_eq!(
            shared.evaluate(since + 10_000),
            None,
            "pending without an owed frame is not a stall"
        );
        // …and it stays silent no matter how long it idles.
        assert_eq!(shared.evaluate(since + 33 * 60_000), None);
    }

    /// The v0.7.0 freeze this module exists to catch MUST still fire: the event
    /// loop is alive but the render path is dead, so redraws are genuinely owed
    /// (`render_owed` true) and no frame presents. Gating on `render_owed` must
    /// not weaken that catch.
    #[test]
    fn v070_freeze_signature_still_stalls() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        shared.set_render_owed(true);
        shared.note_redraw_delivered();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert!(
            shared.evaluate(since + 10_000).is_some(),
            "redraws owed but never presented is the freeze the watchdog must log"
        );
    }

    /// Timing is unchanged by the gate: an owed frame within the stall window
    /// is still not yet a stall.
    #[test]
    fn owed_frame_within_the_window_is_not_yet_a_stall() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        shared.set_render_owed(true);
        shared.note_redraw_delivered();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert_eq!(shared.evaluate(since + 9_999), None);
    }

    /// REGRESSION GUARD — asleep/hidden output. The observed false positive:
    /// terminal output keeps arriving (pending work, a rebuild genuinely owed)
    /// while the display is DPMS-off, so the compositor stops asking for
    /// frames and nothing presents. `render_owed` is TRUE here, so the older
    /// gate does not catch this; the delivered-redraw gate must. Zero frames
    /// is the correct steady state for a surface nobody is painting.
    #[test]
    fn owed_frame_with_no_delivered_redraw_is_not_a_stall() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        shared.set_render_owed(true);
        // No `note_redraw_delivered()`: the windowing system never asked.
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert_eq!(
            shared.evaluate(since + 10_000),
            None,
            "an owed frame nobody asked for is a hidden surface, not a freeze"
        );
        assert_eq!(
            shared.evaluate(since + 33 * 60_000),
            None,
            "and it stays silent for the whole sleep, however long"
        );
    }

    /// The wake-up side of the same episode: once the output comes back and
    /// redraws are delivered again, a genuinely stalled render path is still
    /// reported. The gate suppresses the sleep, not the freeze after it.
    #[test]
    fn stall_is_reported_once_redraws_resume() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        shared.set_render_owed(true);
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert_eq!(shared.evaluate(since + 20_000), None);
        shared.note_redraw_delivered();
        assert!(
            shared.evaluate(since + 20_002).is_some(),
            "a delivered redraw with no present is the freeze signature"
        );
    }

    /// The episode baseline is per-episode, not lifetime: redraws delivered
    /// during an EARLIER episode must not license a stall log for a later one
    /// that never got asked to draw.
    #[test]
    fn redraw_credit_does_not_carry_across_episodes() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        shared.set_render_owed(true);
        shared.note_redraw_delivered();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert!(shared.evaluate(since + 10_000).is_some());

        // A present closes the episode; the next one opens with a fresh
        // baseline and no deliveries of its own (the display went to sleep).
        shared.note_present();
        shared.note_activity();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        assert_eq!(
            shared.evaluate(since + 60_000),
            None,
            "the previous episode's redraws must not carry over"
        );
    }

    /// The record carries the discriminator itself, so a future report names
    /// which of the two shapes it was without needing a live debugger.
    #[test]
    fn stall_record_reports_episode_redraw_count() {
        let record = format_stall_record(11, 0, &state());
        assert!(
            record.contains("redraws_delivered=0"),
            "record must carry the episode's delivered-redraw count: {record}"
        );
    }

    /// An unfocused window on the slow skipped-frame retry whose retries keep
    /// delivering redraws (the macOS log shape: `focused=false`, skipped
    /// frames rising about once a second, `redraws_delivered` equal to the
    /// skip count, no frame presented) is classified as slow-retry, never as
    /// the freeze record.
    fn slow_retry_state(redraws: u64) -> WatchdogAppState {
        WatchdogAppState {
            focused: false,
            consecutive_skipped_frames: 12,
            skip_slow_retry: true,
            redraws_delivered: redraws,
            ..state()
        }
    }

    const CLASSIC_PREFIX: &str = "freeze_watchdog: work pending";

    #[test]
    fn unfocused_slow_retry_with_live_redraws_is_classified_not_a_stall() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        let mut records = Vec::new();
        for t in 0..=30u64 {
            shared.store_state(&slow_retry_state(t + 1));
            if let Some(record) = shared.evaluate(since + t * 1_000) {
                records.push((t, record));
            }
        }
        assert_eq!(
            records.len(),
            1,
            "one classification, rate limited: {records:?}"
        );
        let (t, record) = &records[0];
        assert_eq!(*t, 10, "logged at the stall window, not before");
        assert!(
            record.starts_with(SLOW_RETRY_RECORD_PREFIX),
            "got: {record}"
        );
        assert!(
            !record.starts_with(CLASSIC_PREFIX),
            "must not read as a freeze: {record}"
        );
        assert!(record.contains("focused=false"), "{record}");
    }

    #[test]
    fn occluded_focused_slow_retry_is_classified_too() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        let mut last = None;
        for t in 0..=10u64 {
            let mut s = slow_retry_state(t + 1);
            s.focused = true;
            s.window_occluded = true;
            shared.store_state(&s);
            last = shared.evaluate(since + t * 1_000);
        }
        let record = last.expect("classification at the stall window");
        assert!(
            record.starts_with(SLOW_RETRY_RECORD_PREFIX),
            "got: {record}"
        );
        assert!(record.contains("occluded=true"), "{record}");
    }

    #[test]
    fn slow_retry_classification_relogs_only_after_the_rate_limit() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        let mut count = 0;
        for t in 0..=80u64 {
            shared.store_state(&slow_retry_state(t + 1));
            if shared.evaluate(since + t * 1_000).is_some() {
                count += 1;
            }
        }
        assert_eq!(count, 2, "first at 10s, again after RELOG_EVERY");
    }

    /// Real stall preserved: the retry timer stopped delivering redraws, so
    /// the slow-retry class lapses after SLOW_RETRY_LIVENESS and the classic
    /// freeze record fires at the stall window.
    #[test]
    fn slow_retry_with_stopped_redraws_still_reports_the_classic_stall() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        let mut records = Vec::new();
        for t in (0..=12u64).step_by(2) {
            // Redraw count frozen at 1: the loop stopped delivering.
            shared.store_state(&slow_retry_state(1));
            if let Some(record) = shared.evaluate(since + t * 1_000) {
                records.push((t, record));
            }
        }
        assert_eq!(records.len(), 1, "{records:?}");
        let (t, record) = &records[0];
        assert_eq!(*t, 10);
        assert!(record.starts_with(CLASSIC_PREFIX), "got: {record}");
    }

    /// A focused, visible window stuck on the slow retry is NOT excused: the
    /// surface should present, so it keeps the classic record.
    #[test]
    fn focused_visible_slow_retry_stays_a_classic_stall() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        let mut last = None;
        for t in 0..=10u64 {
            let mut s = slow_retry_state(t + 1);
            s.focused = true;
            shared.store_state(&s);
            last = shared.evaluate(since + t * 1_000);
        }
        let record = last.expect("classic stall");
        assert!(record.starts_with(CLASSIC_PREFIX), "got: {record}");
    }

    /// Before the fast retry budget is spent the class does not apply: an
    /// unfocused window with a few skips keeps the previous classic behavior.
    #[test]
    fn unfocused_below_the_slow_retry_budget_stays_a_classic_stall() {
        let shared = WatchdogShared::new();
        shared.note_activity();
        let since = shared.pending_since_ms.load(Ordering::Relaxed);
        let mut s = slow_retry_state(5);
        s.skip_slow_retry = false;
        s.consecutive_skipped_frames = 3;
        shared.store_state(&s);
        let record = shared.evaluate(since + 10_000).expect("classic stall");
        assert!(record.starts_with(CLASSIC_PREFIX), "got: {record}");
    }

    /// A present clears the slow-retry episode so the next one classifies and
    /// logs afresh.
    #[test]
    fn present_rearms_the_slow_retry_classification() {
        let shared = WatchdogShared::new();
        for round in 0..2u64 {
            shared.note_activity();
            let since = shared.pending_since_ms.load(Ordering::Relaxed);
            let mut got = None;
            for t in 0..=10u64 {
                shared.store_state(&slow_retry_state(round * 100 + t + 1));
                got = shared.evaluate(since + t * 1_000);
            }
            assert!(got.expect("record").starts_with(SLOW_RETRY_RECORD_PREFIX));
            shared.note_present();
        }
    }

    #[test]
    fn slow_retry_record_is_state_only() {
        let record = format_slow_retry_record(11, 11, &slow_retry_state(11));
        let body = record.split_once("; ").expect("prefix; body").1;
        for token in body.split_whitespace() {
            let (key, value) = token.split_once('=').expect("key=value tokens only");
            assert!(
                key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{key}"
            );
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{value}"
            );
        }
        for key in [
            "focused=",
            "occluded=",
            "skipped_frames=",
            "redraws_delivered=",
        ] {
            assert!(record.contains(key), "missing {key} in: {record}");
        }
    }

    #[test]
    fn float_arrange_modal_has_a_name() {
        assert_eq!(modal_name(4), "float_arrange");
        assert_eq!(modal_name(200), "unknown");
    }
}
