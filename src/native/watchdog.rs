// SPDX-License-Identifier: GPL-3.0-only
//! Freeze watchdog (FREEZE-HARDEN item b).
//!
//! The v0.7.0 freeze presented as a live event loop that serviced compositor
//! events mechanically while the render/input path was dead: pending input
//! and redraws, but no frame ever presented, at 0% CPU. This module detects
//! exactly that signature and logs the app's state machine so the next
//! freeze names its latch instead of requiring a live debugger session.
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
//! text, no window titles. The seam test below pins the record's charset so
//! a future edit cannot quietly interpolate free-form strings.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use super::app::App;
use super::pty::UserEvent;

/// How long input/redraw work may stay pending with no presented frame
/// before the watchdog logs a stall. Conservative: normal frames land in
/// milliseconds; ten seconds of pending-but-unpresented work is a freeze.
const STALL_AFTER: Duration = Duration::from_secs(10);
/// While a stall persists, re-log at most this often.
const RELOG_EVERY: Duration = Duration::from_secs(60);
/// Monitor thread poll cadence. Coarse on purpose — the watchdog trades
/// detection latency for near-zero idle cost.
const POLL_EVERY: Duration = Duration::from_secs(2);

/// State snapshot the probe (`App::watchdog_state`, see
/// `app/watchdog_probe.rs`) hands the wrapper after every delegated event.
/// Everything is a bool / counter / C-like enum by construction — the type
/// itself enforces that no terminal content can flow into the stall record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WatchdogAppState {
    pub(super) focused: bool,
    pub(super) window_minimized: bool,
    pub(super) window_present: bool,
    pub(super) gpu_present: bool,
    pub(super) overlay_open: bool,
    pub(super) context_menu_open: bool,
    /// Discriminant of `ActiveModal` (0 = None, 1 = CopyMode,
    /// 2 = HintsSelect, 3 = RenameTab).
    pub(super) modal: u8,
    pub(super) needs_rebuild: bool,
    /// Frames that reached `present()` since GPU init.
    pub(super) frames_presented: u64,
    pub(super) consecutive_skipped_frames: u32,
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
    // --- mirrored state (last snapshot after a delegated event) ---
    focused: AtomicBool,
    window_minimized: AtomicBool,
    window_present: AtomicBool,
    gpu_present: AtomicBool,
    overlay_open: AtomicBool,
    context_menu_open: AtomicBool,
    modal: AtomicU8,
    needs_rebuild: AtomicBool,
    frames_presented: AtomicU64,
    consecutive_skipped_frames: AtomicU64,
    /// Whether a frame is genuinely owed (gates the stall log; not logged).
    render_owed: AtomicBool,
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
            focused: AtomicBool::new(true),
            window_minimized: AtomicBool::new(false),
            window_present: AtomicBool::new(false),
            gpu_present: AtomicBool::new(false),
            overlay_open: AtomicBool::new(false),
            context_menu_open: AtomicBool::new(false),
            modal: AtomicU8::new(0),
            needs_rebuild: AtomicBool::new(false),
            frames_presented: AtomicU64::new(0),
            consecutive_skipped_frames: AtomicU64::new(0),
            render_owed: AtomicBool::new(false),
            redraws_delivered: AtomicU64::new(0),
            redraws_at_pending_start: AtomicU64::new(0),
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn note_activity(&self) {
        if !self.pending.swap(true, Ordering::Relaxed) {
            self.pending_since_ms
                .store(self.now_ms(), Ordering::Relaxed);
            self.logged.store(false, Ordering::Relaxed);
            // Baseline the delivered-redraw counter for this episode. The
            // wrapper calls this BEFORE delegating the event, so a
            // `RedrawRequested` that opens an episode still counts inside it.
            self.redraws_at_pending_start.store(
                self.redraws_delivered.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
    }

    fn note_present(&self) {
        self.pending.store(false, Ordering::Relaxed);
        self.logged.store(false, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn set_render_owed(&self, owed: bool) {
        self.render_owed.store(owed, Ordering::Relaxed);
    }

    fn store_state(&self, state: &WatchdogAppState) {
        self.focused.store(state.focused, Ordering::Relaxed);
        self.window_minimized
            .store(state.window_minimized, Ordering::Relaxed);
        self.window_present
            .store(state.window_present, Ordering::Relaxed);
        self.gpu_present.store(state.gpu_present, Ordering::Relaxed);
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
        self.render_owed.store(state.render_owed, Ordering::Relaxed);
        self.redraws_delivered
            .store(state.redraws_delivered, Ordering::Relaxed);
    }

    fn snapshot(&self) -> WatchdogAppState {
        WatchdogAppState {
            focused: self.focused.load(Ordering::Relaxed),
            window_minimized: self.window_minimized.load(Ordering::Relaxed),
            window_present: self.window_present.load(Ordering::Relaxed),
            gpu_present: self.gpu_present.load(Ordering::Relaxed),
            overlay_open: self.overlay_open.load(Ordering::Relaxed),
            context_menu_open: self.context_menu_open.load(Ordering::Relaxed),
            modal: self.modal.load(Ordering::Relaxed),
            needs_rebuild: self.needs_rebuild.load(Ordering::Relaxed),
            frames_presented: self.frames_presented.load(Ordering::Relaxed),
            consecutive_skipped_frames: u32::try_from(
                self.consecutive_skipped_frames.load(Ordering::Relaxed),
            )
            .unwrap_or(u32::MAX),
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
    fn note_redraw_delivered(&self) {
        self.redraws_delivered.fetch_add(1, Ordering::Relaxed);
    }

    /// One monitor-thread evaluation step at `now_ms`. Returns the stall
    /// record to log, if the stall condition holds and rate limits allow.
    /// Pure decision logic, factored for the tests below.
    fn evaluate(&self, now_ms: u64) -> Option<String> {
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
        if pending_for < u64::try_from(STALL_AFTER.as_millis()).unwrap_or(u64::MAX) {
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
                tracing::warn!("{record}");
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

fn modal_name(discriminant: u8) -> &'static str {
    match discriminant {
        0 => "none",
        1 => "copy_mode",
        2 => "hints_select",
        3 => "rename_tab",
        _ => "unknown",
    }
}

/// The winit handler odytty actually runs: the real [`App`] plus watchdog
/// bookkeeping around every delegated event. All state and behavior stay in
/// `App`; this wrapper only observes.
pub(super) struct WatchdogApp {
    app: App,
    shared: Arc<WatchdogShared>,
    last_seen_frames: u64,
}

impl WatchdogApp {
    pub(super) fn new(app: App, shared: Arc<WatchdogShared>) -> Self {
        Self {
            app,
            shared,
            last_seen_frames: 0,
        }
    }

    pub(super) fn into_inner(self) -> App {
        self.app
    }

    /// Mirror the app state after a delegated event; a grown frame counter
    /// means a frame presented since last time, which clears the pending
    /// latch.
    fn refresh(&mut self) {
        let state = self.app.watchdog_state();
        if state.frames_presented != self.last_seen_frames {
            self.last_seen_frames = state.frames_presented;
            self.shared.note_present();
        }
        self.shared.store_state(&state);
    }
}

/// Whether a window event implies work the user can observe not happening:
/// input that should reach the PTY/UI, or a redraw the compositor asked for.
fn implies_pending_work(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::RedrawRequested
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::Touch(_)
    )
}

impl ApplicationHandler<UserEvent> for WatchdogApp {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.app.new_events(event_loop, cause);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
        self.refresh();
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.app.suspended(event_loop);
        self.refresh();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if implies_pending_work(&event) {
            self.shared.note_activity();
        }
        self.app.window_event(event_loop, window_id, event);
        self.refresh();
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: DeviceId,
        event: DeviceEvent,
    ) {
        self.app.device_event(event_loop, device_id, event);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        // PTY pump wakes and session events imply a redraw is wanted.
        self.shared.note_activity();
        self.app.user_event(event_loop, event);
        self.refresh();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.about_to_wait(event_loop);
        self.refresh();
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.app.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.app.memory_warning(event_loop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> WatchdogAppState {
        WatchdogAppState {
            focused: true,
            window_minimized: false,
            window_present: true,
            gpu_present: true,
            overlay_open: false,
            context_menu_open: false,
            modal: 0,
            needs_rebuild: true,
            frames_presented: 1234,
            consecutive_skipped_frames: 0,
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
            window_present: true,
            gpu_present: false,
            overlay_open: true,
            context_menu_open: true,
            modal: 2,
            needs_rebuild: true,
            frames_presented: 987,
            consecutive_skipped_frames: 3,
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
}
