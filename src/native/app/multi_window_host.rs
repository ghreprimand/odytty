// SPDX-License-Identifier: GPL-3.0-only
//! The same-process multi-window `winit` `ApplicationHandler` (v0.15.0 D).
//!
//! [`MultiWindowHost`] is the process event handler odytty actually runs. It
//! owns every live [`App`] (each an independent window + `WorkspaceSet`) and is
//! the single place cross-window concerns are resolved:
//!
//! - **Event routing through current ownership.** A `WindowEvent` reaches the
//!   window whose surface it names ([`window_owner::window_index_for`]); a PTY
//!   `UserEvent` reaches whichever window owns the session NOW
//!   ([`window_owner::owner_index_for_user_event`]), and an event whose target
//!   has closed or moved is dropped rather than misapplied.
//! - **Per-window exit consumption.** A window that requests close removes only
//!   that window while siblings remain; the last window's close exits the
//!   process ([`window_owner::resolve_window_close`]). This preserves the
//!   single-window meaning of a window close exactly: with one window every
//!   close is the process exit.
//! - **Control-flow aggregation.** Each window computes its own next wake; the
//!   host sets the loop's control flow to the SOONEST across all windows, so no
//!   window's animation/timer starves because another set a later deadline.
//! - **Watchdog aggregation.** Input/redraw activity and presented frames are
//!   noted across all windows into the one freeze-detector, so the shipping
//!   watchdog still fires on a wedged window. With a single window this is
//!   byte-identical to the previous per-`App` watchdog wrapper.
//! - **New Window + keyboard merge servicing.** Pending New Window requests spawn
//!   sibling windows in-process; pending merge-picker requests open the keyboard
//!   target picker, paint numerals inside candidate windows, and resolve a
//!   selection into the atomic same-process merge, retiring the emptied source
//!   window WITHOUT a session shutdown (the moved PTYs are live in the target).
//!
//! With exactly one window the host reproduces the previous single-window run
//! path (route to window 0, close = exit, one control-flow deadline, one
//! watchdog), so the default behavior is unchanged.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
#[cfg(test)]
use crate::automation::dispatch::MAX_PER_DISPATCH;
#[cfg(test)]
use crate::automation::protocol::{
    Action as AutomationAction, ErrorCode as AutomationError, ObjectId, ObjectKind,
    Reply as AutomationReply,
};
use crate::native::automation::AutomationRuntime;
use crate::native::merge_picker::{MergeDirection, MergePicker};
use crate::native::quick_terminal::{
    Accelerator, GlobalShortcutAdapter, MonitorRect, QuickTerminalAction, QuickTerminalAnimation,
    QuickTerminalController, QuickTerminalIdentity, QuickTerminalSettings, RevealTimeline,
    ShortcutRegistration, SummonSink, platform_shortcut_adapter, resolve_monitor_rect,
};
use crate::native::watchdog::WatchdogShared;
use crate::native::window_owner::{
    ProcessWindowId, WindowCloseAction, execute_window_merge, owner_index_for_user_event,
    resolve_window_close, window_index_for,
};

/// A factory that builds an unresumed sibling [`App`] for a New Window request,
/// or `None` when the spawn fails or the process token-base space is exhausted.
/// Built in `run_native`, which owns the launch primitives (shell spawn, PTY
/// pump, token-base allocator).
pub(in crate::native) type SiblingFactory = Box<dyn FnMut(NewWindowRequest) -> Option<App>>;

/// An open keyboard merge target picker and the window that opened it.
struct ActiveMergePicker {
    /// The window that invoked the picker; the merge direction is relative to
    /// it. Held by stable identity so a concurrent close of the origin resolves
    /// to a no-op rather than a wrong window.
    origin: ProcessWindowId,
    picker: MergePicker,
}

/// A decoded merge-picker keypress. Only digit selection and cancel are
/// intercepted while a picker is open; every other key falls through to the
/// focused window unchanged.
enum PickerKey {
    Select(u8),
    Cancel,
}

/// An in-progress quick-terminal reveal slide (v0.15.0 A). The host advances it
/// each `about_to_wait` tick, applying the interpolated geometry to the quick
/// window until the timeline finishes. Never built under `Instant`/reduced-
/// motion (the host applies the final geometry directly instead).
struct QuickReveal {
    window: ProcessWindowId,
    timeline: RevealTimeline,
    start: Instant,
}

/// The process multi-window event handler. Owns every live window.
pub(in crate::native) struct MultiWindowHost {
    pub(super) windows: Vec<App>,
    shared: Arc<WatchdogShared>,
    last_seen_frames: u64,
    factory: SiblingFactory,
    picker: Option<ActiveMergePicker>,
    /// The single quick-terminal lifecycle (v0.15.0 A). Disabled until
    /// `configure_quick_terminal` is called with an enabled setting, so the
    /// ordinary window path is unaffected by default.
    pub(super) quick: QuickTerminalController,
    /// The live, registered global-shortcut backend, produced AFTER readiness
    /// (v0.15.0 A). Empty until a successful registration lands; kept alive here
    /// so its `Drop` ungrabs the key at teardown. Written by the registration
    /// worker (Linux/Windows) or inline on the main thread (macOS), and taken
    /// out to drop on reconfigure/teardown. It is never populated before the
    /// first usable terminal exists, so startup readiness is unaffected.
    quick_live: Arc<Mutex<Option<Box<dyn GlobalShortcutAdapter + Send>>>>,
    /// The enabled quick-terminal config awaiting deferred registration. Set by
    /// [`Self::stage_quick_terminal`] before the loop runs and consumed once by
    /// [`Self::service_quick_registration`] after the first window is ready, so
    /// the blocking OS grab never runs on the startup path. `None` when disabled
    /// or already dispatched.
    quick_pending_config: Option<Accelerator>,
    /// One-shot guard: the deferred registration is dispatched at most once.
    quick_registration_started: bool,
    /// Generation/cancellation token for deferred registration workers
    /// (v0.15.0 A). Bumped whenever a prior registration is invalidated
    /// ([`Self::take_live_adapter`], reached on re-stage / reconfigure /
    /// unregister). A Linux/Windows worker captures the generation live at
    /// dispatch; when it resolves it stores its confirmed adapter and has its
    /// outcome recorded ONLY if the generation still matches, so an outcome from
    /// a superseded or torn-down registration can never store a stale grab,
    /// register, or log after disable. Shared with the worker thread by `Arc`.
    quick_registration_generation: Arc<AtomicU64>,
    /// An in-progress reveal slide, if any (v0.15.0 A). `None` at rest and under
    /// `Instant`/reduced-motion, so the default path schedules no extra wakes.
    quick_reveal: Option<QuickReveal>,
    /// The last registration outcome, recorded for status reporting. `None`
    /// until the deferred registration resolves (asynchronously on
    /// Linux/Windows).
    quick_registration_status: Option<ShortcutRegistration>,
    /// Event-loop proxy the global-shortcut backend uses to deliver a summon
    /// into the loop from its own thread (v0.15.0 A). `None` until
    /// [`Self::set_quick_summon_proxy`] is called during run setup; without it
    /// the quick terminal is still summonable from the command palette.
    quick_summon_proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    /// One opt-in owner-private endpoint for every live window in this process.
    /// Default construction is inert: no entropy, queue, socket, or thread.
    pub(super) automation: AutomationRuntime,
    /// Wakes the event loop after a transport worker enqueues a request. Stored
    /// without binding; ordinary startup still creates no endpoint or thread.
    pub(super) automation_proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
}

impl MultiWindowHost {
    /// Create the host around the primary window (window 0). `factory` builds
    /// siblings for New Window requests; `shared` is the freeze watchdog.
    pub(in crate::native) fn new(
        primary: App,
        shared: Arc<WatchdogShared>,
        factory: SiblingFactory,
    ) -> Self {
        Self {
            windows: vec![primary],
            shared,
            last_seen_frames: 0,
            factory,
            picker: None,
            quick: QuickTerminalController::new(QuickTerminalSettings::default()),
            quick_live: Arc::new(Mutex::new(None)),
            quick_pending_config: None,
            quick_registration_started: false,
            quick_registration_generation: Arc::new(AtomicU64::new(0)),
            quick_registration_status: None,
            quick_reveal: None,
            quick_summon_proxy: None,
            automation: AutomationRuntime::default(),
            automation_proxy: None,
        }
    }

    /// Install the event-loop proxy the global-shortcut backend uses to deliver
    /// a summon from its own thread (v0.15.0 A). Call before
    /// [`Self::configure_quick_terminal`] so the registered shortcut can wake an
    /// idle loop; a summon posted this way is handled on the main thread.
    pub(in crate::native) fn set_quick_summon_proxy(
        &mut self,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        self.quick_summon_proxy = Some(proxy);
    }

    /// Consume the host, returning every live window for deterministic teardown
    /// (reap shells, save shape) in `run_native`. Window 0 is the primary.
    pub(in crate::native) fn into_windows(mut self) -> Vec<App> {
        self.automation.shutdown();
        std::mem::take(&mut self.windows)
    }

    /// Persist only an ordinary, restorable primary window on clean shutdown.
    /// The App's primary-instance guard remains authoritative, while the quick
    /// identity is excluded explicitly so restore safety does not depend on the
    /// quick App merely having been constructed as a secondary.
    pub(in crate::native) fn save_restorable_shape_on_exit(&mut self) {
        let quick = &self.quick;
        if let Some(primary) = self.windows.iter_mut().find(|app| {
            app.startup_error.is_none() && quick.window_is_restorable(app.process_window_id())
        }) {
            primary.save_shape_on_exit();
        }
    }

    pub(in crate::native) fn set_automation_proxy(
        &mut self,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        self.automation_proxy = Some(proxy);
    }

    /// Mirror aggregate app state into the freeze watchdog after a delegated
    /// event. A grown TOTAL frame counter across all windows means a frame
    /// presented since last time, which clears the pending latch. The stored
    /// state snapshot is the primary window's (a representative surface for the
    /// human-readable log); the frame-progress signal is the aggregate.
    fn refresh(&mut self) {
        let total_frames: u64 = self
            .windows
            .iter()
            .map(|app| app.watchdog_state().frames_presented)
            .sum();
        if total_frames != self.last_seen_frames {
            self.last_seen_frames = total_frames;
            self.shared.note_present();
        }
        if let Some(primary) = self.windows.first() {
            self.shared.store_state(&primary.watchdog_state());
        }
    }

    /// Keep every window's sibling count current so the command palette offers
    /// the merge/pull rows only when a real target exists.
    fn sync_sibling_counts(&mut self) {
        let total = self.windows.len();
        for app in &mut self.windows {
            app.set_sibling_window_count(total.saturating_sub(1));
        }
    }

    /// Resolve a window close: remove only that window while siblings remain,
    /// reaping its sessions on the way out; exit the process on the last window.
    fn close_window(&mut self, idx: usize, event_loop: &ActiveEventLoop) {
        match resolve_window_close(self.windows.len(), idx) {
            WindowCloseAction::ExitProcess => event_loop.exit(),
            WindowCloseAction::RemoveWindow(i) => {
                if i < self.windows.len() {
                    // A genuine window close reaps its own sessions (kills +
                    // joins the PTYs). This is NOT the merge-retirement path,
                    // whose source arena is already empty.
                    let mut app = self.windows.remove(i);
                    let removed_id = app.process_window_id();
                    app.close_all_sessions();
                    self.detach_quick_if_owned(removed_id);
                    self.cancel_picker_if_target_gone();
                    self.sync_sibling_counts();
                }
            }
        }
    }

    /// Drain each window's pending New Window request and spawn the sibling
    /// in-process, creating its surface immediately so it appears without
    /// waiting for another resume. A failed spawn drops the request.
    fn service_new_windows(&mut self, event_loop: &ActiveEventLoop) {
        // Disjoint field borrows: the shared drain helper takes `&mut windows`
        // while the factory closure borrows `&mut factory` and the event loop.
        let windows = &mut self.windows;
        let factory = &mut self.factory;
        crate::native::window_owner::service_new_window_requests(windows, |request| {
            let mut app = (factory)(request)?;
            // Create the sibling's surface now (the event loop is in scope) so it
            // appears without waiting for another resume.
            app.on_resumed(event_loop);
            Some(app)
        });
    }

    /// Drain pending merge-picker requests and open the picker for the first
    /// one. Only one picker is open at a time; further requests are drained (so
    /// they do not stack) but ignored while one is live.
    fn service_merge_requests(&mut self) {
        let mut requested: Option<(usize, MergeDirection)> = None;
        for (i, app) in self.windows.iter_mut().enumerate() {
            if let Some(direction) = app.take_merge_picker_request()
                && requested.is_none()
            {
                requested = Some((i, direction));
            }
        }
        if self.picker.is_some() {
            return;
        }
        if let Some((origin_idx, direction)) = requested {
            self.open_picker(origin_idx, direction);
        }
    }

    /// Open the keyboard merge target picker over the live windows, painting a
    /// numeral inside each candidate. Does nothing when there is no other window
    /// (the picker refuses an empty candidate set).
    fn open_picker(&mut self, origin_idx: usize, direction: MergeDirection) {
        let Some(origin) = self.windows.get(origin_idx).map(App::process_window_id) else {
            return;
        };
        let listing: Vec<(ProcessWindowId, String)> = self
            .windows
            .iter()
            .map(|app| (app.process_window_id(), app.merge_picker_label()))
            .collect();
        let Some(picker) = MergePicker::open(direction, origin, &listing) else {
            return;
        };
        for candidate in picker.candidates() {
            if let Some(app) = self
                .windows
                .iter_mut()
                .find(|app| app.process_window_id() == candidate.id)
            {
                app.set_merge_numeral(Some(candidate.numeral));
            }
        }
        self.picker = Some(ActiveMergePicker { origin, picker });
    }

    /// Clear every candidate numeral and drop the active picker.
    fn close_picker(&mut self) {
        for app in &mut self.windows {
            app.set_merge_numeral(None);
        }
        self.picker = None;
    }

    /// Cancel an open picker if its origin or any candidate window is no longer
    /// live (a concurrent close). Simplest safe policy: any window-set change
    /// re-validates by cancelling; the user re-invokes over the new set.
    fn cancel_picker_if_target_gone(&mut self) {
        let stale = self.picker.as_ref().is_some_and(|active| {
            let origin_live = self
                .windows
                .iter()
                .any(|app| app.process_window_id() == active.origin);
            let candidates_live = active.picker.candidates().iter().all(|candidate| {
                self.windows
                    .iter()
                    .any(|app| app.process_window_id() == candidate.id)
            });
            !origin_live || !candidates_live
        });
        if stale {
            self.close_picker();
        }
    }

    /// Handle a decoded picker keypress: cancel closes the picker; a digit
    /// resolves to a candidate and runs the merge.
    fn handle_picker_key(&mut self, key: PickerKey) {
        let Some(active) = self.picker.take() else {
            return;
        };
        // Numerals are cleared regardless of outcome.
        for app in &mut self.windows {
            app.set_merge_numeral(None);
        }
        match key {
            PickerKey::Cancel => {}
            PickerKey::Select(numeral) => {
                if let Some(target_id) = active.picker.resolve_numeral(numeral) {
                    self.execute_merge(active.origin, target_id, active.picker.direction());
                }
            }
        }
    }

    /// Run the same-process merge for a resolved selection, then retire the
    /// emptied source window. Re-validates both windows are still live (a stale
    /// pick after a concurrent close is a no-op). A refused merge (token
    /// collision, corrupt tree, capacity) leaves both windows untouched.
    fn execute_merge(
        &mut self,
        origin: ProcessWindowId,
        selected: ProcessWindowId,
        direction: MergeDirection,
    ) {
        // "Merge this window into..." moves the origin INTO the selected target;
        // "Pull window ... into this one" moves the selected INTO the origin.
        let (source_id, target_id) = match direction {
            MergeDirection::MergeThisInto => (origin, selected),
            MergeDirection::PullIntoThis => (selected, origin),
        };
        let Some(source_idx) = self.index_of(source_id) else {
            return;
        };
        let Some(target_idx) = self.index_of(target_id) else {
            return;
        };
        if source_idx == target_idx {
            return;
        }
        let (target, source) = match two_mut(&mut self.windows, target_idx, source_idx) {
            Some(pair) => pair,
            None => return,
        };
        match execute_window_merge(target, source) {
            Ok(_plan) => {
                // The source arena is now empty: retire the window WITHOUT a
                // session shutdown (its PTYs moved and are live in the target).
                // Dropping the App tears down only its now-idle surface.
                let retired = self.windows.remove(source_idx);
                // If the quick terminal was merged away, forget it so a later
                // summon recreates it cleanly.
                self.detach_quick_if_owned(retired.process_window_id());
                // Repaint the target so the merged workspaces show immediately.
                if let Some(target) = self.index_of(target_id).and_then(|i| self.windows.get(i)) {
                    target.request_redraw_now();
                }
                self.sync_sibling_counts();
            }
            Err(err) => {
                tracing::warn!(?err, "window merge refused; both windows left untouched");
            }
        }
    }

    fn index_of(&self, id: ProcessWindowId) -> Option<usize> {
        self.windows
            .iter()
            .position(|app| app.process_window_id() == id)
    }

    // -----------------------------------------------------------------------
    // Quick terminal (v0.15.0 A)
    // -----------------------------------------------------------------------

    /// Apply quick-terminal settings and (re)register the global shortcut when
    /// enabled, SYNCHRONOUSLY. Returns the registration outcome so a caller can
    /// surface an actionable limitation. Never reports Registered without the
    /// platform backend confirming the grab.
    ///
    /// This performs the blocking OS grab inline, so production run setup uses
    /// the deferred pair ([`Self::stage_quick_terminal`] +
    /// [`Self::service_quick_registration`]) instead, keeping the grab off the
    /// first-usable-terminal path. This method remains for direct, order-
    /// independent unit verification of the registration contract.
    #[cfg(test)]
    pub(in crate::native) fn configure_quick_terminal(
        &mut self,
        settings: QuickTerminalSettings,
    ) -> ShortcutRegistration {
        self.quick.update_settings(settings.clone());
        // Release any prior grab before re-registering (idempotent): dropping
        // the live adapter ungrabs the key.
        self.take_live_adapter();
        match quick_validate(&settings) {
            Ok(accelerator) => {
                let (outcome, live) = register_adapter(&accelerator, self.quick_sink());
                if let Some(adapter) = live {
                    self.store_live_adapter(adapter);
                }
                self.quick_registration_status = Some(outcome.clone());
                outcome
            }
            Err(outcome) => {
                self.quick_registration_status = Some(outcome.clone());
                outcome
            }
        }
    }

    /// Record the quick-terminal settings and, when enabled with a valid
    /// accelerator, STAGE the registration for after readiness WITHOUT touching
    /// the OS. Returns a static, non-blocking outcome (`Unavailable` for
    /// disabled/malformed; the enabled+valid case defers and reports later via
    /// [`Self::service_quick_registration`]). Called during run setup before the
    /// event loop starts, so no grab work runs on the startup path. `Ok(())`
    /// means a deferred registration was staged.
    pub(in crate::native) fn stage_quick_terminal(
        &mut self,
        settings: QuickTerminalSettings,
    ) -> Result<(), ShortcutRegistration> {
        self.quick.update_settings(settings.clone());
        self.take_live_adapter();
        self.quick_pending_config = None;
        self.quick_registration_started = false;
        match quick_validate(&settings) {
            Ok(accelerator) => {
                self.quick_pending_config = Some(accelerator);
                Ok(())
            }
            Err(outcome) => {
                self.quick_registration_status = Some(outcome.clone());
                Err(outcome)
            }
        }
    }

    /// The first-usable-terminal readiness predicate for deferred
    /// global-shortcut registration (v0.15.0 A). True only once at least one
    /// live window has actually PRESENTED a frame - a real usable terminal
    /// surface, not merely an existing window struct or an entered event loop.
    /// False during teardown when no window remains, so the stopping pass never
    /// dispatches the OS grab. Cheap: a single atomic load per window.
    pub(super) fn first_usable_frame_ready(&self) -> bool {
        quick_registration_ready(
            self.windows
                .iter()
                .filter(|app| self.quick.window_is_restorable(app.process_window_id()))
                .map(App::frames_presented),
        )
    }

    /// Dispatch the deferred registration exactly once, after the first usable
    /// terminal exists. On Linux/Windows the blocking OS grab runs on a worker
    /// thread and the confirmed outcome is delivered back via
    /// `UserEvent::QuickTerminalRegistration`, so the event loop is never
    /// blocked. On macOS the Carbon registration must run on the main thread; it
    /// is a fast local call executed inline here, after readiness.
    fn service_quick_registration(&mut self, _event_loop: &ActiveEventLoop) {
        if self.quick_registration_started {
            return;
        }
        // Nothing staged (disabled, malformed, or already dispatched): leave
        // without touching state.
        if self.quick_pending_config.is_none() {
            return;
        }
        // READINESS GATE: a window merely
        // existing - or the event loop merely having entered - is NOT evidence
        // of a first usable terminal. Require a real presented frame, and do
        // NOT consume the pending config until eligible. This also guards the
        // stopping/teardown pass: when every window is closing there is no
        // usable frame, so registration never fires during shutdown.
        if !self.first_usable_frame_ready() {
            return;
        }
        let Some(accelerator) = self.quick_pending_config.take() else {
            return;
        };
        self.quick_registration_started = true;
        let sink = self.quick_sink();

        #[cfg(target_os = "macos")]
        {
            // Carbon requires the main run-loop thread; this is a fast, local,
            // post-readiness call.
            let (outcome, live) = register_adapter(&accelerator, sink);
            if let Some(adapter) = live {
                self.store_live_adapter(adapter);
            }
            self.record_registration_outcome(outcome);
        }

        #[cfg(not(target_os = "macos"))]
        {
            let slot = Arc::clone(&self.quick_live);
            let generation = Arc::clone(&self.quick_registration_generation);
            // The generation live at dispatch. If a reconfigure/teardown bumps
            // it before this worker resolves, the worker is stale.
            let my_generation = generation.load(Ordering::SeqCst);
            let outcome_proxy = self.quick_summon_proxy.clone();
            let spawned = std::thread::Builder::new()
                .name("odytty-quick-register".to_owned())
                .spawn(move || {
                    let (outcome, live) = register_adapter(&accelerator, sink);
                    if let Some(adapter) = live {
                        // Store the confirmed grab ONLY if this registration is
                        // still current. Take under the slot lock so a
                        // concurrent `take_live_adapter` (which bumps the
                        // generation under the same lock) cannot interleave. A
                        // stale/superseded grab is released here, never leaked.
                        let mut adapter = Some(adapter);
                        if let Ok(mut guard) = slot.lock()
                            && generation.load(Ordering::SeqCst) == my_generation
                        {
                            *guard = adapter.take();
                        }
                        if let Some(mut stale) = adapter.take() {
                            stale.unregister();
                        }
                    }
                    if let Some(proxy) = outcome_proxy {
                        // Carry the generation so the main thread ignores an
                        // outcome from a superseded registration (no stale
                        // record/log after disable/reconfigure/teardown).
                        let _ = proxy.send_event(UserEvent::QuickTerminalRegistration {
                            generation: my_generation,
                            outcome,
                        });
                    }
                });
            if let Err(err) = spawned {
                self.record_registration_outcome(ShortcutRegistration::Unavailable {
                    reason: format!("cannot spawn quick-terminal registration thread: {err}"),
                });
            }
        }
    }

    /// Record a resolved registration outcome and log it honestly: a confirmed
    /// grab at info, a real failure (enabled but Unsupported/Unavailable) at
    /// warn and in the first live window's notice banner so it is never silently
    /// swallowed. Success never raises a notice.
    fn record_registration_outcome(&mut self, outcome: ShortcutRegistration) {
        let failure_notice = match &outcome {
            ShortcutRegistration::Registered { .. } => None,
            ShortcutRegistration::Unsupported { reason, .. }
            | ShortcutRegistration::Unavailable { reason } => Some(reason.clone()),
        };
        match &outcome {
            ShortcutRegistration::Registered { backend } => {
                tracing::info!(backend, "quick terminal global shortcut registered");
            }
            ShortcutRegistration::Unsupported { platform, reason } => {
                tracing::warn!(platform, %reason, "quick terminal global shortcut unsupported");
            }
            ShortcutRegistration::Unavailable { reason } => {
                tracing::warn!(%reason, "quick terminal global shortcut unavailable");
            }
        }
        self.quick_registration_status = Some(outcome);
        if let Some(reason) = failure_notice
            && let Some(app) = self.windows.first_mut()
        {
            app.raise_open_notice(reason);
        }
    }

    /// The summon sink: the backend fires it from its own thread; it posts a
    /// summon into the loop when a proxy is installed (waking an idle loop).
    /// With no proxy the grab still confirms, but delivery relies on the palette
    /// path - the honest fallback, never a false claim.
    fn quick_sink(&self) -> SummonSink {
        match self.quick_summon_proxy.clone() {
            Some(proxy) => Arc::new(move || {
                let _ = proxy.send_event(UserEvent::QuickTerminalSummon);
            }),
            None => Arc::new(|| {}),
        }
    }

    /// Store the live registered adapter, dropping any prior one first. Used by
    /// the synchronous test path and by the macOS inline (main-thread)
    /// registration; the Linux/Windows worker stores directly into the shared
    /// slot it was handed.
    #[cfg(any(test, target_os = "macos"))]
    fn store_live_adapter(&mut self, adapter: Box<dyn GlobalShortcutAdapter + Send>) {
        if let Ok(mut guard) = self.quick_live.lock() {
            *guard = Some(adapter);
        }
    }

    /// Explicitly unregister and drop any live adapter, ungrabbing the key, and
    /// INVALIDATE any in-flight deferred registration. Idempotent: `unregister`
    /// releases the grab and the subsequent drop is a no-op teardown.
    ///
    /// The generation bump happens under the same slot lock a resolving worker
    /// takes to store its adapter, so the two are serialized: a worker that
    /// wins the lock after this point observes the newer generation and drops
    /// its confirmed grab instead of storing a stale one. Reached on re-stage,
    /// reconfigure, and teardown.
    fn take_live_adapter(&mut self) {
        if let Ok(mut guard) = self.quick_live.lock() {
            self.quick_registration_generation
                .fetch_add(1, Ordering::SeqCst);
            if let Some(mut adapter) = guard.take() {
                adapter.unregister();
            }
        }
    }

    /// Drain any pending quick-terminal toggle request across the windows and
    /// drive the lifecycle. Each drained request is one toggle (summon or hide);
    /// the controller guarantees never more than one dedicated window.
    fn service_quick_toggle(&mut self, event_loop: &ActiveEventLoop) {
        let mut toggles = 0usize;
        for app in &mut self.windows {
            toggles = toggles.saturating_add(app.take_quick_toggle_requests());
        }
        for _ in 0..toggles {
            let action = self.quick.toggle();
            self.execute_quick_action(action, event_loop);
        }
    }

    /// Execute a resolved quick-terminal action. Splitting the DECISION (the
    /// controller's [`QuickTerminalAction`], unit-tested headlessly) from this
    /// surface work keeps the state machine verifiable; the surface calls
    /// (create/show/hide/position a real window) are the on-device step, no-ops
    /// on a headless `App` without a surface.
    fn execute_quick_action(&mut self, action: QuickTerminalAction, event_loop: &ActiveEventLoop) {
        match action {
            QuickTerminalAction::Nothing => {}
            QuickTerminalAction::CreateAndShow => {
                // Build the dedicated quick window with the same in-process
                // factory New Window uses, launching the configured quick
                // profile when one is set, then record its identity and reveal
                // it. The quick window is a secondary window, so it never
                // participates in ordinary workspace save/restore (only the
                // primary window persists shape) - it is summoned, never
                // reopened at startup.
                let request = NewWindowRequest {
                    cwd: None,
                    profile: self.quick.settings().profile.clone(),
                };
                if let Some(mut app) = (self.factory)(request) {
                    // Enforce the role at the ownership boundary even though
                    // sibling construction defaults to non-primary. This keeps
                    // the quick App out of debounced autosave as well as the
                    // explicit clean-exit persistence selection below.
                    app.set_primary_instance(false);
                    app.on_resumed(event_loop);
                    let id = app.process_window_id();
                    self.windows.push(app);
                    self.quick.attach_window(QuickTerminalIdentity::new(id));
                    self.sync_sibling_counts();
                    self.position_and_show_quick(event_loop);
                } else {
                    // The spawn failed; forget the (never created) window so a
                    // later summon retries cleanly rather than believing it
                    // exists.
                    self.quick.detach_window();
                    tracing::warn!("quick terminal window spawn failed; summon dropped");
                }
            }
            QuickTerminalAction::Show => self.position_and_show_quick(event_loop),
            QuickTerminalAction::Hide => {
                if let Some(app) = self.quick_window() {
                    app.set_window_visible(false);
                }
            }
        }
    }

    /// Position the quick window to its configured geometry on the target
    /// monitor, reveal it, and focus it. Under `Slide` (and not reduced-motion)
    /// the window is shown at its off-edge start and a reveal timeline is armed
    /// for the tick loop to advance; under `Instant`/reduced-motion the final
    /// geometry is applied at once.
    fn position_and_show_quick(&mut self, event_loop: &ActiveEventLoop) {
        let Some(identity) = self.quick.identity() else {
            return;
        };
        let window_id = identity.window();
        let work_area = self.quick_work_area(event_loop);
        let settings = self.quick.settings();
        let geometry = settings.geometry(work_area);
        let animate = settings.effective_animation() == QuickTerminalAnimation::Slide;
        let edge = settings.edge;

        // A fresh summon supersedes any in-flight reveal.
        self.quick_reveal = None;
        let reveal = animate.then(|| {
            let timeline = RevealTimeline::new(edge, geometry, work_area);
            let (start_geometry, _) = timeline.sample(0);
            (timeline, start_geometry)
        });

        if let Some(app) = self
            .windows
            .iter()
            .find(|app| app.process_window_id() == window_id)
        {
            match &reveal {
                Some((_, start_geometry)) => {
                    // Show at the off-edge start; the tick loop slides it in.
                    app.apply_quick_geometry(*start_geometry);
                    app.set_window_visible(true);
                    app.focus_quick_window();
                }
                None => {
                    app.apply_quick_geometry(geometry);
                    app.set_window_visible(true);
                    app.focus_quick_window();
                }
            }
        }
        if let Some((timeline, _)) = reveal {
            self.quick_reveal = Some(QuickReveal {
                window: window_id,
                timeline,
                start: Instant::now(),
            });
        }
    }

    /// Advance an in-progress reveal slide, applying the interpolated geometry to
    /// the quick window. Clears the reveal (snapping to the final geometry) when
    /// the timeline finishes or its window is gone. No-op when no reveal is
    /// active, so the default path costs one `Option` check.
    fn tick_quick_reveal(&mut self, now: Instant) {
        let Some(reveal) = self.quick_reveal.as_ref() else {
            return;
        };
        let elapsed_ms = u32::try_from(now.saturating_duration_since(reveal.start).as_millis())
            .unwrap_or(u32::MAX);
        let (geometry, done) = reveal.timeline.sample(elapsed_ms);
        let window_id = reveal.window;
        let target = self
            .windows
            .iter()
            .find(|app| app.process_window_id() == window_id);
        match target {
            Some(app) => {
                app.apply_quick_geometry(geometry);
                if done {
                    self.quick_reveal = None;
                }
            }
            // The quick window closed mid-reveal: abandon the animation.
            None => self.quick_reveal = None,
        }
    }

    /// The soonest wake a live reveal needs (roughly one frame out), so the tick
    /// loop keeps advancing the slide even when the windows are otherwise idle.
    fn quick_reveal_wake(&self) -> Option<Instant> {
        self.quick_reveal
            .as_ref()
            .map(|_| Instant::now() + Duration::from_millis(16))
    }

    /// The live quick window, if it exists.
    fn quick_window(&self) -> Option<&App> {
        let id = self.quick.identity()?.window();
        self.windows
            .iter()
            .find(|app| app.process_window_id() == id)
    }

    /// Detach the quick-terminal lifecycle if `id` was its window (a user close
    /// or a merge retired it), so a later summon recreates it cleanly.
    fn detach_quick_if_owned(&mut self, id: ProcessWindowId) {
        if self.quick.owns_window(id) {
            self.quick.detach_window();
        }
    }

    /// Resolve focus-loss hiding without touching a native surface. A process
    /// merge picker or an App-owned overlay/search/modal keeps the quick window
    /// visible until that interaction releases ownership.
    fn quick_focus_loss_action(&mut self, window_index: usize) -> QuickTerminalAction {
        let interaction_owned = self.picker.is_some()
            || self
                .windows
                .get(window_index)
                .is_some_and(App::interaction_busy);
        self.quick.on_focus_lost(interaction_owned)
    }

    /// The target monitor work area for the quick terminal, in physical pixels,
    /// honoring the configured [`MonitorPolicy`] (v0.15.0 A):
    ///
    /// - `Primary` uses the primary monitor.
    /// - `Index(i)` uses the i-th enumerated monitor when present.
    /// - `ActiveMonitor` (the default) uses the monitor of the FOCUSED ordinary
    ///   window - the one the user is actually on. When the backend reports no
    ///   focused window (e.g. focus is on another application), it falls back to
    ///   any live ordinary window, then the primary. Pointer-position selection
    ///   is not used: winit exposes no cross-platform global pointer location,
    ///   and focus is the portable "where the user is" signal.
    ///
    /// Every branch falls back (chosen -> active -> primary -> first available
    /// -> a 1080p default) so a stale index or a headless/monitor-less loop
    /// still yields a valid rect and never a silent no-show.
    fn quick_work_area(&self, event_loop: &ActiveEventLoop) -> MonitorRect {
        let available: Vec<MonitorRect> = event_loop
            .available_monitors()
            .map(|monitor| monitor_rect_of(&monitor))
            .collect();
        let primary = event_loop.primary_monitor().as_ref().map(monitor_rect_of);
        // The monitor a live ordinary window is on, for the ActiveMonitor policy
        // and as the fallback for a gone indexed monitor. The quick window
        // itself is skipped so it does not anchor to its own last position. The
        // FOCUSED ordinary window wins - that is the monitor the user is on -
        // and only when none reports focus does any live ordinary window's
        // monitor stand in.
        let active = {
            let quick_id = self.quick.identity().map(|identity| identity.window());
            let ordinary = || {
                self.windows
                    .iter()
                    .filter(move |app| Some(app.process_window_id()) != quick_id)
            };
            ordinary()
                .filter(|app| app.window_has_focus())
                .find_map(App::current_monitor)
                .or_else(|| ordinary().find_map(App::current_monitor))
                .as_ref()
                .map(monitor_rect_of)
        };
        resolve_monitor_rect(self.quick.settings().monitor, &available, active, primary)
    }
}

/// Registration may begin only after some ordinary startup surface has
/// presented. Kept pure so startup readiness is pinned without constructing an
/// OS event loop or GPU surface.
fn quick_registration_ready(frames_presented: impl IntoIterator<Item = u64>) -> bool {
    frames_presented.into_iter().any(|frames| frames > 0)
}

/// Validate quick-terminal settings for registration. `Ok(accelerator)` when
/// the feature is enabled and the shortcut parses; `Err(outcome)` for the two
/// static, non-blocking cases (disabled, or a malformed accelerator). Shared by
/// the synchronous and deferred registration paths so the validation contract
/// cannot drift between them.
fn quick_validate(settings: &QuickTerminalSettings) -> Result<Accelerator, ShortcutRegistration> {
    if !settings.enabled {
        return Err(ShortcutRegistration::Unavailable {
            reason: "quick terminal disabled".to_owned(),
        });
    }
    Accelerator::parse(&settings.shortcut).map_err(|err| ShortcutRegistration::Unavailable {
        reason: format!(
            "invalid quick-terminal shortcut {:?}: {err:?}",
            settings.shortcut
        ),
    })
}

/// The single registration core: build the platform adapter and attempt the OS
/// grab (the blocking confirmation). Returns the honest outcome and, on success,
/// the live adapter to keep alive (its `Drop` ungrabs). Runs on a worker thread
/// on Linux/Windows and inline on the main thread on macOS; keeping it one
/// function prevents the two dispatch sites from drifting.
fn register_adapter(
    accelerator: &Accelerator,
    sink: SummonSink,
) -> (
    ShortcutRegistration,
    Option<Box<dyn GlobalShortcutAdapter + Send>>,
) {
    let mut adapter = platform_shortcut_adapter();
    let outcome = adapter.register(accelerator, sink);
    let live = outcome.is_registered().then_some(adapter);
    (outcome, live)
}

/// Convert a `winit` monitor handle to a `MonitorRect` in physical pixels.
/// (`winit` reports the full monitor bounds; panel/dock exclusion is not exposed
/// cross-platform, matching the prior behavior.)
fn monitor_rect_of(monitor: &winit::monitor::MonitorHandle) -> MonitorRect {
    let position = monitor.position();
    let size = monitor.size();
    MonitorRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    }
}

/// Two disjoint mutable borrows into `windows` by index, or `None` when the
/// indices are equal or out of range. `execute_window_merge` needs `&mut` to
/// both the target and source `App`.
fn two_mut(windows: &mut [App], a: usize, b: usize) -> Option<(&mut App, &mut App)> {
    if a == b || a >= windows.len() || b >= windows.len() {
        return None;
    }
    if a < b {
        let (left, right) = windows.split_at_mut(b);
        Some((&mut left[a], &mut right[0]))
    } else {
        let (left, right) = windows.split_at_mut(a);
        Some((&mut right[0], &mut left[b]))
    }
}

/// Whether a window event implies work the user can observe not happening:
/// input that should reach the PTY/UI, or a redraw the compositor asked for.
/// Mirrors the watchdog wrapper's classification so freeze detection is
/// unchanged.
fn implies_pending_work(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::RedrawRequested
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::Touch(_)
            | WindowEvent::DroppedFile(_)
    )
}

/// Decode a merge-picker keypress. Only a pressed 1-9 digit or Escape is
/// intercepted; everything else returns `None` and falls through to the window.
fn decode_picker_key(event: &winit::event::KeyEvent) -> Option<PickerKey> {
    if event.state != ElementState::Pressed {
        return None;
    }
    match &event.logical_key {
        WinitKey::Named(NamedKey::Escape) => Some(PickerKey::Cancel),
        WinitKey::Character(s) => {
            let digit = s.chars().next().and_then(|c| c.to_digit(10))?;
            let numeral = u8::try_from(digit).ok()?;
            if (1..=9).contains(&numeral) {
                Some(PickerKey::Select(numeral))
            } else {
                None
            }
        }
        _ => None,
    }
}

impl ApplicationHandler<UserEvent> for MultiWindowHost {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        for app in &mut self.windows {
            app.on_resumed(event_loop);
        }
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

        // While a merge picker is open, digit/Escape drive the picker and never
        // reach the terminal. Every other key falls through to the focused
        // window unchanged.
        if self.picker.is_some()
            && let WindowEvent::KeyboardInput { event: ref key, .. } = event
            && let Some(action) = decode_picker_key(key)
        {
            self.handle_picker_key(action);
            self.refresh();
            return;
        }

        let Some(idx) = window_index_for(&self.windows, window_id) else {
            // Stale event for a torn-down surface: drop it.
            self.refresh();
            return;
        };
        // v0.15.0 A: quick-terminal hide-on-focus-loss. When the dedicated quick
        // window loses focus and the policy is set, hide it (preserving its
        // session) so it never lingers over other work. The App still processes
        // the focus-out below, exactly as it would for any window.
        if matches!(event, WindowEvent::Focused(false))
            && self
                .quick
                .owns_window(self.windows[idx].process_window_id())
        {
            let action = self.quick_focus_loss_action(idx);
            self.execute_quick_action(action, event_loop);
        }
        let redraw_early_exit = self.windows[idx].process_window_event(event_loop, event);
        if !redraw_early_exit && self.windows[idx].wants_exit() {
            self.close_window(idx, event_loop);
        }
        self.refresh();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        // A PTY pump wake or session event implies a redraw is wanted.
        self.shared.note_activity();
        // v0.15.0 A: a global-shortcut summon is not session-scoped. Drive the
        // quick-terminal toggle directly on the main thread (same path the
        // command palette uses through `service_quick_toggle`).
        if matches!(event, UserEvent::QuickTerminalSummon) {
            let action = self.quick.toggle();
            self.execute_quick_action(action, event_loop);
            self.refresh();
            return;
        }
        // v0.15.0 A: the deferred registration finished on its worker thread.
        // The live grab (if any) was already stored in the registration slot
        // under the generation gate; record and log the honest outcome on the
        // main thread (a real failure is warned, never silently swallowed) -
        // but ONLY when the outcome belongs to the current registration. An
        // outcome from a superseded/torn-down registration is dropped so it
        // cannot log or overwrite status after disable/reconfigure/teardown.
        if let UserEvent::QuickTerminalRegistration {
            generation,
            outcome,
        } = event
        {
            if generation == self.quick_registration_generation.load(Ordering::SeqCst) {
                self.record_registration_outcome(outcome);
            }
            self.refresh();
            return;
        }
        if matches!(event, UserEvent::AutomationWake) {
            self.dispatch_automation();
            // Winit calls `about_to_wait` after this event batch; that pass
            // drains accepted quick-terminal requests through
            // `service_quick_toggle`, so no second synthetic wake is needed.
            self.refresh();
            return;
        }
        if let Some(idx) = owner_index_for_user_event(&self.windows, &event)
            && self.windows[idx].apply_user_event(event)
        {
            self.close_window(idx, event_loop);
        }
        // An event whose owning session/window is gone is stale and dropped.
        self.refresh();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();

        // v0.15.0 A: advance an in-progress quick-terminal reveal slide before
        // per-window maintenance so the interpolated geometry is applied this
        // tick. No-op (one Option check) when no reveal is active.
        self.tick_quick_reveal(now);

        // Per-window maintenance, collecting windows that want to close (an
        // autoclose deadline fired or a confirmed exit).
        let mut to_close: Vec<usize> = Vec::new();
        for (i, app) in self.windows.iter_mut().enumerate() {
            app.run_about_to_wait_maintenance(now);
            let autoclose_fired = app.autoclose_deadline_reached(now);
            if autoclose_fired || app.wants_exit() {
                to_close.push(i);
            }
        }
        // Close highest index first so lower indices stay valid.
        for i in to_close.into_iter().rev() {
            self.close_window(i, event_loop);
            if self.windows.is_empty() {
                break;
            }
        }

        // v0.15.0 A: dispatch the deferred global-shortcut registration once,
        // now that the loop is running and the first window exists (readiness is
        // reached). The blocking OS grab runs off the event-loop thread on
        // Linux/Windows and inline (fast) on macOS - never on the startup path.
        self.service_quick_registration(event_loop);
        self.service_automation_endpoint();

        // Service cross-window requests (may add or remove windows).
        self.service_new_windows(event_loop);
        self.service_merge_requests();
        self.service_quick_toggle(event_loop);
        self.sync_sibling_counts();

        if self.windows.is_empty() {
            event_loop.exit();
            self.refresh();
            return;
        }

        // Aggregate control flow: wake at the SOONEST deadline any window wants,
        // including a live reveal slide (v0.15.0 A) so its animation keeps
        // advancing even when the windows are otherwise idle.
        match self
            .windows
            .iter()
            .filter_map(App::next_wake_deadline)
            .chain(self.quick_reveal_wake())
            .min()
        {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
        self.refresh();
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::automation::dispatch;
    use crate::automation::protocol::{Request, VERSION};
    use crate::native::session::SessionToken;
    use crate::native::test_support::headless_app_for_test;

    /// A host over headless windows with a factory that spawns nothing, so the
    /// cross-window orchestration (picker, merge, sibling counts) can be driven
    /// without a real event loop. The event-loop-scoped methods (`resumed`,
    /// `window_event`, `about_to_wait`) are display-coupled and validated
    /// on-device; every helper exercised here holds `&mut self` only.
    pub(in crate::native::app) fn host_of(windows: Vec<App>) -> MultiWindowHost {
        MultiWindowHost {
            windows,
            shared: WatchdogShared::new(),
            last_seen_frames: 0,
            factory: Box::new(|_| None),
            picker: None,
            quick: QuickTerminalController::new(QuickTerminalSettings::default()),
            quick_live: Arc::new(Mutex::new(None)),
            quick_pending_config: None,
            quick_registration_started: false,
            quick_registration_generation: Arc::new(AtomicU64::new(0)),
            quick_registration_status: None,
            quick_reveal: None,
            quick_summon_proxy: None,
            automation: AutomationRuntime::default(),
            automation_proxy: None,
        }
    }

    pub(in crate::native::app) fn headless() -> App {
        headless_app_for_test().0
    }

    #[test]
    fn sibling_counts_reflect_the_other_window_total() {
        let mut host = host_of(vec![headless()]);
        host.sync_sibling_counts();
        assert!(
            !host.windows[0].merge_targets_available(),
            "a lone window offers no merge target"
        );

        host.windows.push(headless());
        host.windows.push(headless());
        host.sync_sibling_counts();
        assert!(host.windows.iter().all(App::merge_targets_available));
    }

    #[test]
    fn open_picker_paints_numerals_on_candidates_not_the_origin() {
        let mut host = host_of(vec![headless(), headless(), headless()]);
        host.open_picker(0, MergeDirection::MergeThisInto);

        assert!(host.picker.is_some(), "picker opened over two candidates");
        assert_eq!(host.windows[0].merge_numeral(), None, "origin unbadged");
        assert_eq!(host.windows[1].merge_numeral(), Some(1));
        assert_eq!(host.windows[2].merge_numeral(), Some(2));

        host.close_picker();
        assert!(host.picker.is_none());
        assert!(
            host.windows.iter().all(|w| w.merge_numeral().is_none()),
            "numerals cleared on close"
        );
    }

    #[test]
    fn a_lone_window_opens_no_picker() {
        let mut host = host_of(vec![headless()]);
        host.open_picker(0, MergeDirection::PullIntoThis);
        assert!(host.picker.is_none(), "no other window to target");
        assert_eq!(host.windows[0].merge_numeral(), None);
    }

    #[test]
    fn selecting_a_candidate_merges_this_into_it_and_retires_the_source() {
        let source = headless();
        let mut target = headless();
        // Disjoint tokens so the merge does not (correctly) refuse a collision.
        target
            .workspace_set_mut()
            .rekey_sole_session_for_test(SessionToken(500));
        let source_id = source.process_window_id();
        let target_id = target.process_window_id();
        let mut host = host_of(vec![source, target]);

        // "Merge this window into..." from the source: candidate 1 is the target.
        host.open_picker(0, MergeDirection::MergeThisInto);
        host.handle_picker_key(PickerKey::Select(1));

        // Source window retired; target absorbed its workspace + session.
        assert_eq!(host.windows.len(), 1, "source window removed after merge");
        let survivor = &host.windows[0];
        assert_eq!(survivor.process_window_id(), target_id);
        assert!(survivor.workspace_set().owns_session(SessionToken(0)));
        assert!(survivor.workspace_set().owns_session(SessionToken(500)));
        assert_eq!(survivor.workspace_set().workspace_count(), 2);
        assert!(host.picker.is_none(), "picker closed after select");
        assert_eq!(survivor.merge_numeral(), None, "numerals cleared");
        assert!(
            host.index_of(source_id).is_none(),
            "source id no longer live"
        );
    }

    #[test]
    fn pull_into_this_moves_the_selected_window_into_the_origin() {
        let origin = headless();
        let mut other = headless();
        other
            .workspace_set_mut()
            .rekey_sole_session_for_test(SessionToken(500));
        let origin_id = origin.process_window_id();
        let mut host = host_of(vec![origin, other]);

        // "Pull window ... into this one" from the origin: candidate 1 is `other`,
        // which becomes the SOURCE and is retired.
        host.open_picker(0, MergeDirection::PullIntoThis);
        host.handle_picker_key(PickerKey::Select(1));

        assert_eq!(host.windows.len(), 1);
        let survivor = &host.windows[0];
        assert_eq!(survivor.process_window_id(), origin_id, "origin survives");
        assert_eq!(survivor.workspace_set().workspace_count(), 2);
    }

    #[test]
    fn a_refused_merge_leaves_both_windows_untouched() {
        // Both windows own token 0: the merge fails closed and changes nothing.
        let a = headless();
        let b = headless();
        let mut host = host_of(vec![a, b]);
        host.open_picker(0, MergeDirection::MergeThisInto);
        host.handle_picker_key(PickerKey::Select(1));

        assert_eq!(host.windows.len(), 2, "both windows remain");
        assert_eq!(host.windows[0].workspace_set().workspace_count(), 1);
        assert_eq!(host.windows[1].workspace_set().workspace_count(), 1);
        assert!(host.picker.is_none());
    }

    #[test]
    fn cancel_closes_the_picker_without_merging() {
        let mut host = host_of(vec![headless(), headless()]);
        host.open_picker(0, MergeDirection::MergeThisInto);
        assert!(host.picker.is_some());
        host.handle_picker_key(PickerKey::Cancel);
        assert!(host.picker.is_none());
        assert_eq!(host.windows.len(), 2, "cancel merges nothing");
        assert!(host.windows.iter().all(|w| w.merge_numeral().is_none()));
    }

    #[test]
    fn a_closed_candidate_cancels_an_open_picker() {
        let mut host = host_of(vec![headless(), headless()]);
        host.open_picker(0, MergeDirection::MergeThisInto);
        assert!(host.picker.is_some());
        // Simulate the candidate window closing out from under the open picker.
        host.windows.pop();
        host.cancel_picker_if_target_gone();
        assert!(host.picker.is_none(), "stale picker cancelled");
    }

    #[test]
    fn configure_quick_terminal_reports_disabled_when_off() {
        let mut host = host_of(vec![headless()]);
        let status = host.configure_quick_terminal(QuickTerminalSettings::default());
        assert!(
            !status.is_registered(),
            "a disabled feature registers nothing"
        );
        assert!(matches!(status, ShortcutRegistration::Unavailable { .. }));
    }

    #[test]
    fn configure_quick_terminal_rejects_a_malformed_shortcut() {
        let mut host = host_of(vec![headless()]);
        let status = host.configure_quick_terminal(QuickTerminalSettings {
            enabled: true,
            shortcut: "ctrl+shift".to_owned(), // no key
            ..QuickTerminalSettings::default()
        });
        match status {
            ShortcutRegistration::Unavailable { reason } => {
                assert!(reason.contains("invalid"), "reason names the parse failure");
            }
            other => panic!("expected Unavailable for a malformed shortcut, got {other:?}"),
        }
    }

    #[test]
    fn registration_failure_is_visible_but_confirmed_success_is_not() {
        let mut failed = host_of(vec![headless()]);
        let reason = "Choose a different quick_terminal_shortcut, then restart OdyTTY.";
        failed.record_registration_outcome(ShortcutRegistration::Unavailable {
            reason: reason.to_owned(),
        });
        assert_eq!(
            failed.windows[0].open_notice_message_for_test().as_deref(),
            Some(reason)
        );

        let mut registered = host_of(vec![headless()]);
        registered.record_registration_outcome(ShortcutRegistration::Registered {
            backend: "confirmed-test-backend",
        });
        assert!(
            registered.windows[0]
                .open_notice_message_for_test()
                .is_none()
        );
    }

    #[test]
    fn registration_readiness_requires_a_presented_frame() {
        assert!(!quick_registration_ready([]));
        assert!(!quick_registration_ready([0, 0, 0]));
        assert!(quick_registration_ready([0, 1, 0]));
    }

    #[test]
    fn focus_loss_waits_for_quick_overlay_and_merge_picker_interactions() {
        let mut quick_window = headless();
        quick_window.open_settings_overlay_for_test();
        let quick_id = quick_window.process_window_id();
        let mut host = host_of(vec![headless(), quick_window]);
        host.quick.update_settings(QuickTerminalSettings {
            enabled: true,
            hide_on_focus_loss: true,
            ..QuickTerminalSettings::default()
        });
        assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
        host.quick
            .attach_window(QuickTerminalIdentity::new(quick_id));

        assert_eq!(
            host.quick_focus_loss_action(1),
            QuickTerminalAction::Nothing,
            "the quick window's overlay owns interaction"
        );

        // A process-wide merge picker also owns interaction, regardless of
        // which candidate received the native focus transition.
        let mut host = host_of(vec![headless(), headless()]);
        let quick_id = host.windows[1].process_window_id();
        host.quick.update_settings(QuickTerminalSettings {
            enabled: true,
            hide_on_focus_loss: true,
            ..QuickTerminalSettings::default()
        });
        assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
        host.quick
            .attach_window(QuickTerminalIdentity::new(quick_id));
        host.open_picker(0, MergeDirection::MergeThisInto);
        assert!(host.picker.is_some());
        assert_eq!(
            host.quick_focus_loss_action(1),
            QuickTerminalAction::Nothing,
            "the merge picker keeps its quick candidate visible"
        );
    }

    #[test]
    fn hiding_preserves_the_same_quick_app_and_session() {
        let quick_window = headless();
        let quick_id = quick_window.process_window_id();
        let quick_session = quick_window.active_session_token_for_test();
        let mut host = host_of(vec![headless(), quick_window]);
        host.quick.update_settings(QuickTerminalSettings {
            enabled: true,
            ..QuickTerminalSettings::default()
        });
        assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
        host.quick
            .attach_window(QuickTerminalIdentity::new(quick_id));

        assert_eq!(host.quick.hide(), QuickTerminalAction::Hide);
        assert_eq!(host.windows.len(), 2, "hide removes no App");
        assert!(
            host.windows
                .iter()
                .any(|app| app.process_window_id() == quick_id && app.owns_session(quick_session)),
            "the same quick App still owns the same session"
        );
        assert_eq!(host.quick.summon(), QuickTerminalAction::Show);
        assert_eq!(host.quick.identity().map(|id| id.window()), Some(quick_id));
    }

    #[test]
    fn clean_exit_persistence_explicitly_excludes_the_quick_window() {
        let mut ordinary = headless();
        let mut quick_window = headless();
        let quick_id = quick_window.process_window_id();

        // Deliberately give the quick App the primary bit: the explicit role
        // check must still exclude it rather than relying on that incidental
        // construction default.
        ordinary.set_primary_instance(false);
        quick_window.set_primary_instance(true);
        let mut host = host_of(vec![ordinary, quick_window]);
        host.quick.update_settings(QuickTerminalSettings {
            enabled: true,
            ..QuickTerminalSettings::default()
        });
        assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
        host.quick
            .attach_window(QuickTerminalIdentity::new(quick_id));

        host.save_restorable_shape_on_exit();
        assert_eq!(host.windows[0].autosave_saves_for_test(), 0);
        assert_eq!(
            host.windows[1].autosave_saves_for_test(),
            0,
            "the quick identity never reaches the restoration writer"
        );

        host.windows[0].set_primary_instance(true);
        host.save_restorable_shape_on_exit();
        assert_eq!(host.windows[0].autosave_saves_for_test(), 1);
        assert_eq!(host.windows[1].autosave_saves_for_test(), 0);
    }

    #[test]
    fn closing_or_retiring_the_quick_window_allows_one_clean_recreation() {
        let quick_window = headless();
        let quick_id = quick_window.process_window_id();
        let mut host = host_of(vec![headless(), quick_window]);
        host.quick.update_settings(QuickTerminalSettings {
            enabled: true,
            ..QuickTerminalSettings::default()
        });
        assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
        host.quick
            .attach_window(QuickTerminalIdentity::new(quick_id));

        host.detach_quick_if_owned(quick_id);
        assert!(host.quick.identity().is_none());
        assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
        assert_eq!(host.quick.summon(), QuickTerminalAction::Nothing);
    }

    #[test]
    fn restaging_invalidates_an_earlier_registration_generation() {
        // The deferred worker and the outcome handler both gate on the
        // generation token: a worker captures it at dispatch and only stores
        // its grab / records its outcome while it still matches the host's
        // current generation. Restaging (or any reconfigure/teardown, which all
        // route through `take_live_adapter`) must bump the token so an in-flight
        // worker's captured value no longer matches - the exact condition that
        // makes a superseded outcome be dropped rather than stored/logged.
        let mut host = host_of(vec![headless()]);
        let enabled = QuickTerminalSettings {
            enabled: true,
            shortcut: "ctrl+shift+grave".to_owned(),
            ..QuickTerminalSettings::default()
        };

        let _ = host.stage_quick_terminal(enabled.clone());
        let captured_at_dispatch = host.quick_registration_generation.load(Ordering::SeqCst);

        // A second stage (a reconfigure) supersedes the first.
        let _ = host.stage_quick_terminal(enabled);
        let current = host.quick_registration_generation.load(Ordering::SeqCst);

        assert!(
            current > captured_at_dispatch,
            "restaging must bump the generation so a stale worker is rejected"
        );
        // This is exactly the comparison the worker/handler make.
        assert_ne!(
            captured_at_dispatch, current,
            "a worker holding the earlier generation must not match the current one"
        );
    }

    #[test]
    fn quick_toggle_request_is_captured_and_drained() {
        let mut app = headless();
        assert_eq!(app.take_quick_toggle_requests(), 0, "none at rest");
        app.request_quick_toggle();
        app.request_quick_toggle();
        assert_eq!(app.take_quick_toggle_requests(), 2, "both captured");
        assert_eq!(
            app.take_quick_toggle_requests(),
            0,
            "drained: a second take is empty"
        );
    }

    #[test]
    fn two_mut_rejects_equal_or_out_of_range_indices() {
        let mut windows = vec![headless(), headless()];
        assert!(two_mut(&mut windows, 0, 0).is_none(), "equal indices");
        assert!(two_mut(&mut windows, 0, 5).is_none(), "out of range");
        let pair = two_mut(&mut windows, 1, 0);
        assert!(pair.is_some(), "distinct in-range indices split cleanly");
    }

    #[test]
    fn default_automation_is_inert_and_enabled_state_waits_for_a_presented_frame() {
        let mut host = host_of(vec![headless()]);
        host.service_automation_endpoint();
        assert!(!host.automation.is_running());
        assert!(host.automation.instance().is_none());

        host.windows[0].settings.automation_endpoint = true;
        host.service_automation_endpoint();
        assert!(
            !host.automation.is_running(),
            "an enabled endpoint must not bind before the first presented frame"
        );
        assert!(
            host.automation.instance().is_none(),
            "pre-readiness service allocates no automation state"
        );
    }

    #[test]
    fn owner_bridge_lists_statuses_focuses_renames_and_rejects_stale_instances() {
        let mut host = host_of(vec![headless()]);
        host.windows[0].settings.automation_endpoint = true;
        let instance = [0x5a; 16];
        let (submission, queue) = dispatch::channel(true);
        host.automation.install_queue_for_test(instance, queue);

        let list = submission
            .submit(Request {
                version: VERSION,
                request_id: 1,
                action: AutomationAction::List,
            })
            .expect("queue list");
        host.dispatch_automation();
        let AutomationReply::Objects(objects) = list.wait().reply else {
            panic!("list reply")
        };
        assert_eq!(objects.len(), 4, "window + workspace + tab + pane");
        let workspace = objects
            .iter()
            .find(|object| object.id.kind == ObjectKind::Workspace)
            .expect("workspace")
            .id;

        let status = submission
            .submit(Request {
                version: VERSION,
                request_id: 2,
                action: AutomationAction::Status { target: workspace },
            })
            .expect("queue status");
        host.dispatch_automation();
        assert!(
            matches!(status.wait().reply, AutomationReply::Objects(rows) if rows.len() == 1 && rows[0].id == workspace)
        );

        let rename = submission
            .submit(Request {
                version: VERSION,
                request_id: 3,
                action: AutomationAction::Rename {
                    target: workspace,
                    name: "ops".to_owned(),
                },
            })
            .expect("queue rename");
        host.dispatch_automation();
        assert_eq!(rename.wait().reply, AutomationReply::Applied(workspace));
        assert_eq!(
            host.windows[0].workspace_set().workspace_name(0),
            Some("ops")
        );

        let stale = ObjectId {
            instance: [0x33; 16],
            ..workspace
        };
        let focus = submission
            .submit(Request {
                version: VERSION,
                request_id: 4,
                action: AutomationAction::Focus { target: stale },
            })
            .expect("queue stale focus");
        host.dispatch_automation();
        assert_eq!(
            focus.wait().reply,
            AutomationReply::Error(AutomationError::StaleIdentity)
        );

        let detached_namespace_id = ObjectId {
            instance,
            kind: ObjectKind::Pane,
            serial: u64::MAX,
        };
        assert!(
            !objects
                .iter()
                .any(|object| object.id == detached_namespace_id),
            "detached-host identities are not projected as live objects"
        );
        let focus = submission
            .submit(Request {
                version: VERSION,
                request_id: 5,
                action: AutomationAction::Focus {
                    target: detached_namespace_id,
                },
            })
            .expect("queue detached namespace target");
        host.dispatch_automation();
        assert_eq!(
            focus.wait().reply,
            AutomationReply::Error(AutomationError::StaleIdentity)
        );
    }

    #[test]
    fn owner_bridge_rechecks_structural_permission_when_dispatching() {
        let mut host = host_of(vec![headless()]);
        let instance = [0x34; 16];
        let (submission, queue) = dispatch::channel(true);
        host.automation.install_queue_for_test(instance, queue);
        let workspace = host
            .automation_objects(instance)
            .into_iter()
            .find(|object| object.id.kind == ObjectKind::Workspace)
            .expect("workspace identity")
            .id;

        let rename = submission
            .submit(Request {
                version: VERSION,
                request_id: 5,
                action: AutomationAction::Rename {
                    target: workspace,
                    name: "must-not-apply".to_owned(),
                },
            })
            .expect("the queue still reflects the formerly enabled setting");
        host.dispatch_automation();

        assert_eq!(
            rename.wait().reply,
            AutomationReply::Error(AutomationError::PermissionDenied)
        );
        assert_ne!(
            host.windows[0].workspace_set().workspace_name(0),
            Some("must-not-apply")
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn owner_bridge_round_trips_a_live_unix_request() {
        use std::sync::mpsc;
        use std::time::Duration;

        let mut host = host_of(vec![headless()]);
        host.windows[0].settings.automation_endpoint = true;
        let dir = std::env::temp_dir().join(format!(
            "odytty-owner-bridge-{:x}",
            host.windows[0].process_window_id().0
        ));
        crate::state_dir::prepare_private_dir(&dir).expect("owner-private fixture dir");
        let endpoint = dir.join(format!("control-{}.sock", std::process::id()));
        let (wake_tx, wake_rx) = mpsc::channel();
        host.automation
            .start_unix_at(endpoint.clone(), move || wake_tx.send(()).is_ok())
            .expect("bind endpoint");
        let instance = host.automation.instance().expect("instance identity");

        let client = std::thread::spawn({
            let endpoint = endpoint.clone();
            move || {
                crate::automation::unix::request(
                    &endpoint,
                    &Request {
                        version: VERSION,
                        request_id: 6,
                        action: AutomationAction::List,
                    },
                )
            }
        });
        wake_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("transport wake");
        host.dispatch_automation();
        let response = client.join().expect("client thread").expect("response");

        assert_eq!(response.request_id, 6);
        assert!(
            matches!(response.reply, AutomationReply::Objects(objects) if objects.len() == 4 && objects.iter().all(|object| object.id.instance == instance))
        );
        host.automation.shutdown();
        assert!(!endpoint.exists());
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn owner_bridge_dispatches_at_most_eight_requests_per_turn() {
        let mut host = host_of(vec![headless()]);
        host.windows[0].settings.automation_endpoint = true;
        let (submission, queue) = dispatch::channel(true);
        host.automation.install_queue_for_test([9; 16], queue);
        let receipts: Vec<_> = (0..=MAX_PER_DISPATCH)
            .map(|request_id| {
                submission
                    .submit(Request {
                        version: VERSION,
                        request_id: request_id as u64,
                        action: AutomationAction::Capabilities,
                    })
                    .expect("queue capabilities")
            })
            .collect();

        host.dispatch_automation();
        assert!(
            receipts[..MAX_PER_DISPATCH]
                .iter()
                .all(|receipt| receipt.try_response().is_some())
        );
        assert!(
            receipts[MAX_PER_DISPATCH].try_response().is_none(),
            "ninth request waits for the next event-loop turn"
        );
        host.dispatch_automation();
        assert!(receipts[MAX_PER_DISPATCH].try_response().is_some());
    }

    #[test]
    fn owner_bridge_maps_creation_actions_to_structured_spawn_and_profile_results() {
        let mut host = host_of(vec![headless()]);
        host.windows[0].settings.automation_endpoint = true;
        let instance = [0x71; 16];
        let (submission, queue) = dispatch::channel(true);
        host.automation.install_queue_for_test(instance, queue);
        let window = host
            .automation_objects(instance)
            .into_iter()
            .find(|object| object.id.kind == ObjectKind::Window)
            .expect("window identity")
            .id;

        let create_tab = submission
            .submit(Request {
                version: VERSION,
                request_id: 10,
                action: AutomationAction::CreateTab { window },
            })
            .expect("queue create tab");
        host.dispatch_automation();
        assert_eq!(
            create_tab.wait().reply,
            AutomationReply::Error(AutomationError::Unavailable),
            "the headless App has no event-loop proxy, so the existing spawn route refuses"
        );

        let create_workspace = submission
            .submit(Request {
                version: VERSION,
                request_id: 11,
                action: AutomationAction::CreateWorkspace {
                    window,
                    name: "deploy".to_owned(),
                },
            })
            .expect("queue create workspace");
        host.dispatch_automation();
        assert_eq!(
            create_workspace.wait().reply,
            AutomationReply::Error(AutomationError::Unavailable)
        );

        let pane = host
            .automation_objects(instance)
            .into_iter()
            .find(|object| object.id.kind == ObjectKind::Pane && object.parent.is_some())
            .expect("pane identity")
            .id;
        let split = submission
            .submit(Request {
                version: VERSION,
                request_id: 12,
                action: AutomationAction::Split {
                    pane,
                    direction: crate::automation::protocol::SplitDirection::Columns,
                },
            })
            .expect("queue split");
        host.dispatch_automation();
        assert_eq!(
            split.wait().reply,
            AutomationReply::Error(AutomationError::Unavailable)
        );

        let missing_profile = submission
            .submit(Request {
                version: VERSION,
                request_id: 13,
                action: AutomationAction::OpenProfile {
                    window,
                    name: "profile-that-does-not-exist".to_owned(),
                },
            })
            .expect("queue missing profile");
        host.dispatch_automation();
        assert_eq!(
            missing_profile.wait().reply,
            AutomationReply::Error(AutomationError::InvalidRequest)
        );
    }
}
