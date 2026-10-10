// SPDX-License-Identifier: GPL-3.0-only
//! Watchdog state probe (FREEZE-HARDEN item b): the one place the freeze
//! watchdog reads `App` state. Lives as an `App` child module (like
//! `pointer`/`ime`) so it reaches private fields without widening any field
//! visibility; the wrapper in `native::watchdog` calls this after every
//! delegated event.
//!
//! PRIVACY: this probe may only ever export booleans, counters, and C-like
//! enum discriminants (see [`WatchdogAppState`]) — never strings, titles, or
//! buffer contents. That is what makes the watchdog's stall record safe to
//! ship to a log file by construction.

use super::*;
use crate::native::watchdog::{WatchdogAppState, WatchdogShared};

impl App {
    /// The count of GPU frames this window has actually presented (v0.15.0 A
    /// readiness probe). Zero until the first usable terminal frame lands.
    /// Cheap: a single atomic load through the GPU state, no snapshot build.
    /// Used as the first-usable-frame predicate that gates deferred
    /// global-shortcut registration, so the OS grab never runs before a real
    /// terminal surface exists.
    pub(in crate::native) fn frames_presented(&self) -> u64 {
        self.gpu
            .as_ref()
            .map(GpuState::frames_presented)
            .unwrap_or(0)
    }

    /// Snapshot the freeze-relevant state machine: the postmortem's requested
    /// fields (focused flag, occluded/minimized latch, active overlay/modal,
    /// `self.window` presence, frame counters), all as plain state.
    pub(in crate::native) fn watchdog_state(&self) -> WatchdogAppState {
        WatchdogAppState {
            focused: self.focused,
            window_minimized: self.window_minimized,
            window_occluded: self.window_occluded,
            window_present: self.window.is_some(),
            gpu_present: self.gpu.is_some(),
            wayland_surface: self.is_wayland_client(),
            overlay_open: self.overlay.is_open(),
            context_menu_open: self.overlay.is_context_menu(),
            modal: match self.active_modal() {
                ActiveModal::None => 0,
                ActiveModal::CopyMode => 1,
                ActiveModal::HintsSelect => 2,
                ActiveModal::RenameTab => 3,
                ActiveModal::FloatArrange => 4,
            },
            needs_rebuild: self.needs_rebuild,
            frames_presented: self
                .gpu
                .as_ref()
                .map(GpuState::frames_presented)
                .unwrap_or(0),
            consecutive_skipped_frames: self.consecutive_skipped_frames,
            // The fast retry budget is spent: retries now run on the slow
            // keep-alive cadence (see `next_skipped_retry_delay`).
            skip_slow_retry: self.consecutive_skipped_frames >= frame::MAX_SKIPPED_RETRIES,
            redraws_delivered: self.redraws_delivered,
            // Gating discriminator for the stall log: is a frame genuinely
            // owed right now? Use the multipane-aware `should_rebuild_frame()`
            // (NOT the bare single-pane `needs_rebuild`, which is still
            // exported above for the postmortem record), a pending
            // skipped-frame retry, or the owed-frame latch that remains armed
            // until a present. When this is false the watchdog treats
            // latched-but-unpresented work as idle/background, not a freeze.
            // A lost device owes no frame: rendering is paused (and logged
            // once), so latched work is not a stall.
            render_owed: self.watchdog_render_owed(),
        }
    }

    /// Whether this window owes a frame it can be expected to present: it
    /// owes one (see [`Self::watchdog_render_owed`]) and it is shown, not
    /// minimized or occluded, since a hidden window is legitimately not asked
    /// to draw. The process host asks every window, so one window's presented
    /// frames cannot vouch for a visible sibling that owes one.
    pub(in crate::native) fn watchdog_owes_visible_frame(&self) -> bool {
        self.window.is_some()
            && !self.window_minimized
            && !self.window_occluded
            && self.watchdog_render_owed()
    }

    /// Whether this window genuinely owes a frame right now (see
    /// `render_owed` above).
    fn watchdog_render_owed(&self) -> bool {
        (!self.live_drag_destination || self.window.is_some())
            && !self.live_drag_source
            && !self.gpu_device_lost
            && (self.frame_owed_since.is_some()
                || self.should_rebuild_frame()
                || self.skipped_frame_retry_deadline.is_some())
    }
}

/// One window's presentation progress, as the process host's freeze
/// watchdog sees it: a stable window id, its presented-frame and
/// delivered-redraw counts, and whether it owes a frame while shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct WindowProgress {
    pub(in crate::native) id: u64,
    pub(in crate::native) frames: u64,
    pub(in crate::native) redraws: u64,
    pub(in crate::native) owes_frame: bool,
}

/// The progress of every window of the process host, in window order.
pub(in crate::native) fn window_progress(windows: &[App]) -> Vec<WindowProgress> {
    windows
        .iter()
        .map(|app| WindowProgress {
            id: app.process_window_id().0,
            frames: app.frames_presented(),
            redraws: app.redraws_delivered,
            owes_frame: app.watchdog_owes_visible_frame(),
        })
        .collect()
}

/// What one host observation decided: whether to report a presented frame to
/// the shared watchdog, and the window whose state it should mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct Observation {
    pub(in crate::native) report: bool,
    pub(in crate::native) stalled: Option<usize>,
}

/// The process host's per-window bookkeeping for the shared freeze watchdog.
/// Every counter is kept per stable window id, because each window counts its
/// own frames and redraws: one window's counter never measures another's.
#[derive(Debug, Default)]
pub(in crate::native) struct WatchdogLedger {
    /// Presented frames per window at the last report.
    reported: Vec<(u64, u64)>,
    /// Presented frames per window at the previous observation.
    seen: Vec<(u64, u64)>,
    /// For each window that owes a frame: its presented frames when it began
    /// owing (the previous observation's count, so a present observed in the
    /// same step still counts).
    owed_from: Vec<(u64, u64)>,
    /// Delivered redraws per window when the watchdog's current pending
    /// episode opened.
    episode_redraws: Vec<(u64, u64)>,
}

fn count_for(counts: &[(u64, u64)], id: u64) -> u64 {
    counts
        .iter()
        .find(|(window, _)| *window == id)
        .map_or(0, |(_, count)| *count)
}

impl WatchdogLedger {
    /// Record every window's delivered-redraw count as the baseline of a
    /// pending episode the watchdog just opened. A window created later starts
    /// from zero, which is its own count at creation.
    pub(in crate::native) fn open_episode(&mut self, windows: &[WindowProgress]) {
        self.episode_redraws.clear();
        self.episode_redraws
            .extend(windows.iter().map(|window| (window.id, window.redraws)));
    }

    /// The baseline for window `id`'s own redraw counter in the current
    /// episode, which the host mirrors with that window's state.
    pub(in crate::native) fn episode_redraws(&self, id: u64) -> u64 {
        count_for(&self.episode_redraws, id)
    }

    /// Observe every window after an event. A presented frame is reported
    /// when some window presented since the last report and every window that
    /// owes a frame presented after it began owing and after the last report,
    /// so a window that keeps animating cannot vouch for a stalled sibling,
    /// and a present made before a window's newly owed frame cannot pay for
    /// it. On a report the baseline advances to the current counts. With one
    /// window this reports whenever its frame count changed. `stalled` is the
    /// first window holding the report back: it owes a frame and has not
    /// presented since both the last report and the moment it began owing.
    pub(in crate::native) fn observe(&mut self, windows: &[WindowProgress]) -> Observation {
        let seen = std::mem::take(&mut self.seen);
        self.owed_from.retain(|(id, _)| {
            windows
                .iter()
                .any(|window| window.id == *id && window.owes_frame)
        });
        for window in windows.iter().filter(|window| window.owes_frame) {
            if !self.owed_from.iter().any(|(id, _)| *id == window.id) {
                self.owed_from
                    .push((window.id, count_for(&seen, window.id)));
            }
        }
        self.seen
            .extend(windows.iter().map(|window| (window.id, window.frames)));
        let since_report =
            |window: &WindowProgress| window.frames != count_for(&self.reported, window.id);
        // One predicate decides both whether the report is held back and
        // which window is stalled: a window that owes a frame is unpaid
        // until it presents after the last report and after it began owing.
        let unpaid = |window: &WindowProgress| {
            window.owes_frame
                && !(since_report(window) && window.frames != count_for(&self.owed_from, window.id))
        };
        let any = windows.iter().any(since_report);
        let stalled = windows.iter().position(unpaid);
        let report = any && stalled.is_none();
        if report {
            self.reported.clear();
            self.reported
                .extend(windows.iter().map(|window| (window.id, window.frames)));
        }
        Observation { report, stalled }
    }
}

/// Report the host's progress to the shared watchdog and mirror one window:
/// the stalled window when one is stalled, else the first window that is not
/// a live-drag source. `state_of(i)` is window `i`'s state, `None` for a
/// live-drag source, which is never mirrored. The episode's redraw baseline
/// is rebased onto the mirrored window's own counter, so the stall gates
/// count that window's deliveries and never another window's.
pub(in crate::native) fn publish_progress(
    shared: &WatchdogShared,
    ledger: &mut WatchdogLedger,
    progress: &[WindowProgress],
    state_of: impl Fn(usize) -> Option<WatchdogAppState>,
) {
    let observed = ledger.observe(progress);
    if observed.report {
        shared.note_present();
    }
    let subject = observed
        .stalled
        .and_then(|index| state_of(index).map(|state| (index, state)))
        .or_else(|| {
            (0..progress.len()).find_map(|index| state_of(index).map(|state| (index, state)))
        });
    if let Some((index, state)) = subject {
        let id = progress[index].id;
        shared.mirror_window(id, &state, ledger.episode_redraws(id));
    }
}

#[cfg(test)]
mod progress_tests {
    use super::{WatchdogLedger, WindowProgress, publish_progress};
    use crate::native::watchdog::{WatchdogAppState, WatchdogShared};

    fn window(id: u64, frames: u64, owes_frame: bool) -> WindowProgress {
        WindowProgress {
            id,
            frames,
            redraws: 0,
            owes_frame,
        }
    }

    fn report(ledger: &mut WatchdogLedger, windows: &[WindowProgress]) -> bool {
        ledger.observe(windows).report
    }

    #[test]
    fn one_window_reports_whenever_its_frames_change() {
        let mut ledger = WatchdogLedger::default();
        assert!(!report(&mut ledger, &[window(1, 0, true)]));
        assert!(report(&mut ledger, &[window(1, 1, true)]));
        assert!(!report(&mut ledger, &[window(1, 1, true)]));
        assert!(report(&mut ledger, &[window(1, 2, false)]));
    }

    /// An animating window cannot vouch for a sibling that owes a frame and
    /// presents nothing; once the sibling presents, the report goes through.
    #[test]
    fn an_animating_window_does_not_hide_a_stalled_sibling() {
        let mut ledger = WatchdogLedger::default();
        assert!(report(
            &mut ledger,
            &[window(1, 10, false), window(2, 5, false)]
        ));
        for frames in 11..20 {
            let observed = ledger.observe(&[window(1, frames, true), window(2, 5, true)]);
            assert!(!observed.report, "window 2 still owes its frame");
            assert_eq!(observed.stalled, Some(1), "the stuck window is mirrored");
        }
        assert!(report(
            &mut ledger,
            &[window(1, 20, true), window(2, 6, false)]
        ));
        let observed = ledger.observe(&[window(1, 20, true), window(2, 6, true)]);
        assert_eq!(observed.stalled, Some(0), "window 1 now owes and is stuck");
        // A sibling that owes nothing does not hold the report back.
        assert!(report(
            &mut ledger,
            &[window(1, 21, true), window(2, 6, false)]
        ));
    }

    /// A present a window made while a sibling held the report back cannot
    /// pay for a frame the window begins to owe afterwards.
    #[test]
    fn an_earlier_unreported_present_does_not_pay_for_a_newly_owed_frame() {
        let mut ledger = WatchdogLedger::default();
        assert!(report(
            &mut ledger,
            &[window(1, 10, false), window(2, 5, false)]
        ));
        // Window 1 presents while window 2 owes: no report.
        assert!(!report(
            &mut ledger,
            &[window(1, 11, false), window(2, 5, true)]
        ));
        // Window 2 presents, but window 1 now owes a new frame it has not
        // presented: still no report.
        let observed = ledger.observe(&[window(1, 11, true), window(2, 6, false)]);
        assert!(!observed.report, "the old present does not count");
        // Window 1 presents its owed frame: the report goes through.
        assert!(report(
            &mut ledger,
            &[window(1, 12, true), window(2, 6, false)]
        ));
        // A present observed in the same step as the owing starts counts.
        assert!(report(
            &mut ledger,
            &[window(1, 13, true), window(2, 7, true)]
        ));
    }

    fn owed_state(redraws_delivered: u64, frames_presented: u64) -> WatchdogAppState {
        WatchdogAppState {
            focused: true,
            window_minimized: false,
            window_occluded: false,
            window_present: true,
            gpu_present: true,
            wayland_surface: false,
            overlay_open: false,
            context_menu_open: false,
            modal: 0,
            needs_rebuild: true,
            frames_presented,
            consecutive_skipped_frames: 0,
            skip_slow_retry: false,
            redraws_delivered,
            render_owed: true,
        }
    }

    /// Two windows with unequal redraw counters share one watchdog. The
    /// stalled subject alternates; each time the stall gate counts the
    /// mirrored window's own deliveries since the episode opened, so a
    /// sibling that keeps being asked to draw and presents nothing is
    /// reported, and one that is not asked to draw is not a classic stall.
    #[test]
    fn the_shared_watchdog_counts_the_mirrored_windows_own_redraws() {
        const LATE: u64 = 1_000_000_000;
        let shared = WatchdogShared::new();
        let mut ledger = WatchdogLedger::default();
        let progress = |a: (u64, u64, bool), b: (u64, u64, bool)| {
            [
                WindowProgress {
                    id: 1,
                    frames: a.0,
                    redraws: a.1,
                    owes_frame: a.2,
                },
                WindowProgress {
                    id: 2,
                    frames: b.0,
                    redraws: b.1,
                    owes_frame: b.2,
                },
            ]
        };
        let publish =
            |shared: &WatchdogShared, ledger: &mut WatchdogLedger, p: &[WindowProgress; 2]| {
                publish_progress(shared, ledger, p, |i| {
                    Some(owed_state(p[i].redraws, p[i].frames))
                });
            };
        // Both windows idle and presented; the primary's counter is far ahead.
        let idle = progress((50, 100, false), (7, 20, false));
        publish(&shared, &mut ledger, &idle);
        // Work arrives: the episode opens with each window's own count.
        assert!(shared.note_activity());
        ledger.open_episode(&idle);
        // The sibling owes a frame and is not asked to draw: no classic
        // stall (and no callback record off Wayland).
        let stuck = progress((50, 100, false), (7, 20, true));
        publish(&shared, &mut ledger, &stuck);
        assert_eq!(
            shared.evaluate(LATE),
            None,
            "the sibling was never asked to draw"
        );
        // The sibling is asked to draw ten times and presents nothing.
        let asked = progress((50, 100, false), (7, 30, true));
        publish(&shared, &mut ledger, &asked);
        let record = shared
            .evaluate(LATE)
            .expect("the sibling's stall is reported");
        assert!(
            record.ends_with(" redraws_delivered=10"),
            "ten since the episode opened: {record}"
        );
        // The sibling recovers and the primary is now the one that is stuck,
        // asked to draw once since the episode opened.
        shared.note_present();
        assert!(shared.note_activity());
        ledger.open_episode(&asked);
        let swapped = progress((50, 101, true), (8, 30, false));
        publish(&shared, &mut ledger, &swapped);
        let record = shared
            .evaluate(LATE)
            .expect("the primary's stall is reported");
        assert!(
            record.ends_with(" redraws_delivered=1"),
            "one since the episode opened: {record}"
        );
    }

    /// The window holding the report back is the one mirrored. A, B, C
    /// start presented; B presents while C owes; then C recovers and B begins
    /// to owe at the count it just reached. The report stays held for B, so B
    /// is the stalled subject, and the shared watchdog reports B's stall with
    /// B's own delivered redraws even though the primary owes nothing.
    #[test]
    fn the_window_holding_the_report_back_is_the_stalled_subject() {
        const LATE: u64 = 1_000_000_000;
        let shared = WatchdogShared::new();
        let mut ledger = WatchdogLedger::default();
        let progress = |a: (u64, u64, bool), b: (u64, u64, bool), c: (u64, u64, bool)| {
            [(1, a), (2, b), (3, c)].map(|(id, (frames, redraws, owes_frame))| WindowProgress {
                id,
                frames,
                redraws,
                owes_frame,
            })
        };
        let publish =
            |shared: &WatchdogShared, ledger: &mut WatchdogLedger, p: &[WindowProgress; 3]| {
                publish_progress(shared, ledger, p, |i| {
                    let mut state = owed_state(p[i].redraws, p[i].frames);
                    state.render_owed = p[i].owes_frame;
                    Some(state)
                });
            };
        let idle = progress((10, 0, false), (5, 0, false), (2, 0, false));
        assert!(ledger.observe(&idle).report);
        assert!(shared.note_activity());
        ledger.open_episode(&idle);
        let c_owes = progress((10, 0, false), (6, 0, false), (2, 0, true));
        assert!(
            !ledger.observe(&c_owes).report,
            "C owes and has not presented"
        );
        let b_owes = progress((10, 0, false), (6, 4, true), (3, 0, false));
        let observed = ledger.observe(&b_owes);
        assert!(!observed.report, "B's earlier present cannot pay");
        assert_eq!(observed.stalled, Some(1), "B holds the report back");
        let b_asked = progress((10, 0, false), (6, 7, true), (3, 0, false));
        publish(&shared, &mut ledger, &b_asked);
        let record = shared.evaluate(LATE).expect("B's stall is reported");
        assert!(
            record.ends_with(" redraws_delivered=7"),
            "B's own deliveries: {record}"
        );
    }

    /// A diagnostic class earned by one window does not pass to the next
    /// subject. Two windows owe a frame on Wayland; window 1 is never asked
    /// to draw and is classified callback-outstanding. Window 1 then presents
    /// while window 2 still owes (no report, so no global present), and
    /// window 2 keeps being asked to draw: its classic stall is reported.
    #[test]
    fn a_callback_classification_does_not_pass_to_the_next_subject() {
        const LATE: u64 = 1_000_000_000;
        let shared = WatchdogShared::new();
        let mut ledger = WatchdogLedger::default();
        let progress = |a: (u64, u64, bool), b: (u64, u64, bool)| {
            [(1, a), (2, b)].map(|(id, (frames, redraws, owes_frame))| WindowProgress {
                id,
                frames,
                redraws,
                owes_frame,
            })
        };
        let publish =
            |shared: &WatchdogShared, ledger: &mut WatchdogLedger, p: &[WindowProgress; 2]| {
                publish_progress(shared, ledger, p, |i| {
                    let mut state = owed_state(p[i].redraws, p[i].frames);
                    state.wayland_surface = true;
                    state.render_owed = p[i].owes_frame;
                    Some(state)
                });
            };
        let idle = progress((10, 0, false), (5, 0, false));
        publish(&shared, &mut ledger, &idle);
        assert!(shared.note_activity());
        ledger.open_episode(&idle);
        let both_owe = progress((10, 0, true), (5, 0, true));
        publish(&shared, &mut ledger, &both_owe);
        let record = shared
            .evaluate(LATE)
            .expect("window 1 is classified callback-outstanding");
        assert!(
            !record.starts_with("freeze_watchdog: work pending"),
            "{record}"
        );
        let second = progress((11, 0, false), (5, 3, true));
        let observed = ledger.observe(&second);
        assert!(!observed.report, "window 2 still owes");
        publish(&shared, &mut ledger, &second);
        let record = shared
            .evaluate(LATE)
            .expect("window 2's classic stall is reported");
        assert!(
            record.starts_with("freeze_watchdog: work pending"),
            "{record}"
        );
        assert!(record.ends_with(" redraws_delivered=3"), "{record}");
    }
}
