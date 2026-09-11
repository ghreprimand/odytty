// SPDX-License-Identifier: GPL-3.0-only
//! Process window owner: identity, event routing, and the keyboard window
//! merge orchestration across sibling `App` windows (v0.15.0 D).
//!
//! The v0.14.0 baseline runs one `App` (one window, one `WorkspaceSet`) as the
//! `winit` `ApplicationHandler`. v0.15.0 introduces same-process sibling windows
//! and a keyboard merge that moves whole workspaces (and their owned sessions)
//! between two windows. This module owns the process-level concerns that are
//! mandatory before a second window can exist:
//!
//! - **Stable, non-reused window identities** ([`ProcessWindowId`]) alongside the
//!   process-wide session-token allocator in `session::model`.
//! - **Event routing through current ownership** ([`owner_index_for_user_event`],
//!   [`window_index_for`]): a PTY `UserEvent` reaches whichever window owns the
//!   session NOW, and an event for a closed/moved session is a no-op rather than
//!   applied to the wrong window.
//! - **The merge transaction across two windows** ([`execute_window_merge`]):
//!   preflight, cancel pending input on BOTH endpoints, commit the model-layer
//!   transfer, and leave the source arena empty so the owner can retire the
//!   source window WITHOUT its normal session shutdown (which would kill the
//!   PTYs that just moved).
//! - **The last-window close decision** ([`resolve_window_close`]): a window
//!   close removes only that window while siblings remain, and exits the process
//!   only when the last window closes.
//!
//! The routing/decision helpers are pure functions over the window slice so they
//! are unit-tested headlessly; the live `ApplicationHandler`
//! (`app::multi_window_host`) that drives real window creation and the event
//! loop consumes them.

use std::sync::atomic::{AtomicU64, Ordering};

use winit::window::WindowId;

use super::app::{App, NewWindowRequest};
use super::pty::UserEvent;
use super::session::{MergeError, MergePlan};

/// A process-stable window identity. Allocated once per window from a monotonic
/// counter and never reused for the process lifetime, so a merge target picker
/// and future automation can name a window unambiguously even as windows open
/// and close. Distinct from `winit::window::WindowId`, which is a backend handle
/// that may not be stable across a surface teardown/recreate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in crate::native) struct ProcessWindowId(pub(in crate::native) u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(0);

/// Mint the next process-unique [`ProcessWindowId`], or `None` once the id space
/// is exhausted. The allocator advances by one and REFUSES (returns `None`)
/// rather than wrapping past `u64::MAX`, so it can never hand out a repeated id
/// even in principle: a wrapping `fetch_add` would silently reissue `0` after
/// `u64::MAX` allocations and alias a live window's identity. The ceiling is
/// physically unreachable (it needs 2^64 window constructions), so callers at an
/// infallible construction boundary treat `None` as an unreachable fail-closed
/// error rather than a case to recover from.
pub(in crate::native) fn next_window_id() -> Option<ProcessWindowId> {
    NEXT_WINDOW_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            cur.checked_add(1)
        })
        .ok()
        .map(ProcessWindowId)
}

/// Process-monotonic surface-incarnation counter. Bumped every time a window
/// actually creates its native surface (see `App::try_resume_presentation`), so
/// each surface incarnation carries a value that is NEVER reused for the process
/// lifetime, even when the platform hands back a recycled `wl_surface` address.
/// v0.15.0 C uses it to make native Wayland file-drop routing ABA-safe: a drop
/// captured against generation N is refused once the window's live surface has
/// advanced to a later generation. Starts at 0 so 0 can mean "no surface yet";
/// the first minted generation is 1.
#[cfg(target_os = "linux")]
static NEXT_SURFACE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Mint the next process-monotonic surface generation (>= 1), or `None` once the
/// generation space is exhausted. Like [`next_window_id`], this advances with a
/// checked `fetch_update` and REFUSES rather than wrapping: a wrapping
/// `fetch_add` would reissue `0` after `u64::MAX` bumps and alias a live
/// surface's generation, and saturating the RETURNED value does not stop that
/// because the stored atomic still wraps. The ceiling needs 2^64 surface
/// creations and is physically unreachable, so the caller at the surface-creation
/// boundary treats `None` as an unreachable fail-closed error rather than a case
/// to recover from.
#[cfg(target_os = "linux")]
pub(in crate::native) fn next_surface_generation() -> Option<u64> {
    // `fetch_update` stores `cur + 1` and returns the PREVIOUS `cur`; adding 1 to
    // that yields the minted generation (first mint: cur 0 -> stored 1 -> return
    // Some(1)). `checked_add` on the stored value fails closed at the ceiling so
    // the atomic never wraps and no generation is ever reissued.
    NEXT_SURFACE_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            cur.checked_add(1)
        })
        .ok()
        .map(|prev| prev + 1)
}

/// The session-token range each sibling window owns. A window allocates session
/// tokens sequentially from its base ([`WorkspaceSet`] `next_token`); the next
/// window's base is one stride higher, so two windows' token ranges never
/// overlap unless a single window outlives 2^40 sessions, which cannot happen in
/// practice. This is what lets the keyboard merge move sessions between arenas
/// without re-keying a live pump.
pub(in crate::native) const WINDOW_TOKEN_STRIDE: u64 = 1 << 40;

/// Sibling window token bases start one stride above the primary window's range.
/// The primary window (constructed by `run_native`) owns `[0, STRIDE)` with its
/// launch session at token 0; the first sibling owns `[STRIDE, 2*STRIDE)`, and
/// so on.
static NEXT_WINDOW_TOKEN_BASE: AtomicU64 = AtomicU64::new(WINDOW_TOKEN_STRIDE);

/// The highest base that still has a full disjoint stride below `u64::MAX`. A
/// base above this cannot own a complete `[base, base + STRIDE)` range without
/// overflowing, so the allocator refuses rather than hand it out.
const MAX_WINDOW_TOKEN_BASE: u64 = u64::MAX - WINDOW_TOKEN_STRIDE + 1;

/// The base session token for a newly created sibling window, so its token range
/// is disjoint from every other window's, or `None` when the process has
/// exhausted the base space. The owner spawns the sibling's launch session at
/// this token; its `WorkspaceSet` then allocates upward from `base + 1`. Tests
/// and the primary window never call this, so their token sequence stays
/// `0, 1, 2, ...`.
pub(in crate::native) fn next_window_token_base() -> Option<u64> {
    // Hand out bases a full stride apart, and REFUSE (return None) once the
    // space is exhausted rather than saturate. Saturating would repeat the final
    // base for every further window, aliasing one window's range onto another
    // and breaking cross-window token uniqueness - the very thing the disjoint
    // ranges exist to guarantee. A process never opens ~2^24 windows, so this
    // ceiling is unreachable in practice; refusing fails the window creation
    // closed instead of silently producing a colliding allocator.
    NEXT_WINDOW_TOKEN_BASE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            if cur > MAX_WINDOW_TOKEN_BASE {
                // Exhausted: leave the counter pinned and refuse.
                None
            } else {
                // Hand out `cur`; advance by a stride, pinning at the sentinel
                // `u64::MAX` (which is `> MAX_WINDOW_TOKEN_BASE`) when the last
                // valid base is handed out so the next call refuses.
                Some(cur.saturating_add(WINDOW_TOKEN_STRIDE))
            }
        })
        .ok()
}

/// What a window-close request resolves to under the process owner. A close is
/// only the process exit when it is the last window; otherwise the owner removes
/// just that window and the loop keeps running for the survivors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum WindowCloseAction {
    /// Remove the window at this index and keep the loop running.
    RemoveWindow(usize),
    /// The last window closed; exit the process event loop.
    ExitProcess,
}

/// Resolve a close of the window at `idx` given how many windows are live. With
/// siblings present the window is removed; the last window's close exits the
/// process, preserving the single-window meaning of a window close exactly.
pub(in crate::native) fn resolve_window_close(
    window_count: usize,
    idx: usize,
) -> WindowCloseAction {
    if window_count <= 1 {
        WindowCloseAction::ExitProcess
    } else {
        WindowCloseAction::RemoveWindow(idx)
    }
}

/// Index of the window whose live surface is `id`, or `None` when no window owns
/// it (a stale event after the surface was torn down). The owner routes a
/// `WindowEvent` through this.
pub(in crate::native) fn window_index_for(windows: &[App], id: WindowId) -> Option<usize> {
    windows
        .iter()
        .position(|app| app.window_winit_id() == Some(id))
}

/// Index of the window that should receive `event`, resolved through CURRENT
/// ownership, or `None` when no live window owns its target (the event is stale
/// and must be dropped rather than misapplied). Session-scoped events route by
/// the owning arena; the save-dialog result routes by the window holding that
/// pending command export.
pub(in crate::native) fn owner_index_for_user_event(
    windows: &[App],
    event: &UserEvent,
) -> Option<usize> {
    if let Some(token) = event.routed_session() {
        return windows.iter().position(|app| app.owns_session(token));
    }
    if let Some(request_id) = event.command_export_request() {
        return windows
            .iter()
            .position(|app| app.has_pending_command_export(request_id));
    }
    None
}

/// Move every workspace of `source` into `target` as one atomic transaction
/// (v0.15.0 D "Merge this window into..." / "Pull window ... into this one").
///
/// Order of operations:
/// 1. **Preflight** the model-layer transfer against both arenas. On any refusal
///    both windows are left completely untouched and the error is returned.
/// 2. **Cancel pending input on BOTH endpoints** so no staged risky-paste,
///    file-drop, or image-upload confirmation can flush into a session while it
///    changes owners.
/// 3. **Commit**: move whole workspaces and their sessions into `target`; the
///    source arena is left empty.
///
/// It never respawns, detaches, replays bytes, or reparents a surface, and it
/// never shuts a session down: the moved sessions are live in `target` when this
/// returns. The owner retires the now-empty `source` window AFTER a successful
/// return, and must do so WITHOUT the normal session shutdown (there is nothing
/// left to shut down, and calling it would be a contract violation). `target`'s
/// focused workspace is unchanged (a merge appends).
pub(in crate::native) fn execute_window_merge(
    target: &mut App,
    source: &mut App,
) -> Result<MergePlan, MergeError> {
    // Phase 1: validate without mutating either side.
    let plan = target
        .workspace_set()
        .preflight_merge_from(source.workspace_set())?;

    // Phase 2: retire in-flight, unconfirmed input on both windows before either
    // arena changes, so nothing writes to a transferring session.
    target.cancel_pending_input_for_merge();
    source.cancel_pending_input_for_merge();

    // Phase 3: the move. `target` and `source` are distinct `App`s, so the two
    // arena borrows are disjoint. Commit revalidates both arenas and reserves
    // capacity before mutating, so a stale plan or allocation failure is refused
    // with both windows still untouched (input was already cancelled, which is
    // idempotent and safe). It returns the revalidated plan on success.
    let committed = target
        .workspace_set_mut()
        .commit_merge_from(source.workspace_set_mut(), plan)?;

    Ok(committed)
}

/// Drain each window's pending New Window request and spawn the sibling window
/// through `factory`, appending each successfully created window. `factory`
/// returns `None` when the sibling could not be spawned (e.g. a failed shell
/// spawn), in which case the request is dropped rather than crashing the
/// requesting window, matching the pre-v0.15.0 log-and-drop New Window policy.
/// Returns the number of windows created.
///
/// Requests are collected before any window is created so a factory that
/// appends to `windows` cannot be observed mid-iteration.
pub(in crate::native) fn service_new_window_requests<F>(
    windows: &mut Vec<App>,
    mut factory: F,
) -> usize
where
    F: FnMut(NewWindowRequest) -> Option<App>,
{
    let requests: Vec<NewWindowRequest> = windows
        .iter_mut()
        .filter_map(App::take_new_window_request)
        .collect();
    let mut created = 0;
    for request in requests {
        if let Some(app) = factory(request) {
            windows.push(app);
            created += 1;
        }
    }
    created
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::session::SessionToken;
    use crate::native::test_support::headless_app_for_test;

    #[test]
    fn window_ids_are_unique_and_monotonic() {
        let a = next_window_id().expect("id space available");
        let b = next_window_id().expect("id space available");
        let c = next_window_id().expect("id space available");
        assert!(b.0 > a.0 && c.0 > b.0);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..500 {
            assert!(seen.insert(next_window_id().expect("id space available").0));
        }
    }

    #[test]
    fn window_id_refuses_at_the_ceiling_rather_than_wrapping() {
        // A private counter driven to the top confirms the allocator returns
        // `None` at exhaustion instead of wrapping to a repeated id. The
        // production static shares this `checked_add` logic; a local copy keeps
        // the test hermetic (it must not perturb the process-wide counter).
        let counter = AtomicU64::new(u64::MAX);
        let alloc = |c: &AtomicU64| -> Option<u64> {
            c.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                cur.checked_add(1)
            })
            .ok()
        };
        assert_eq!(alloc(&counter), None, "refuses at the ceiling");
        assert_eq!(alloc(&counter), None, "stays refused (no wrap to 0)");
    }

    #[test]
    fn window_token_bases_are_disjoint_and_strided() {
        let a = next_window_token_base().expect("base available");
        let b = next_window_token_base().expect("base available");
        assert!(
            b >= a + WINDOW_TOKEN_STRIDE,
            "bases are at least a stride apart"
        );
        // A window allocating from base `a` cannot reach `b` without minting a
        // full stride of sessions.
        assert!(b - a >= WINDOW_TOKEN_STRIDE);
    }

    #[test]
    fn window_token_base_refuses_when_exhausted_rather_than_aliasing() {
        // Drive a private allocator to the ceiling and confirm it REFUSES the
        // exhausted allocation instead of saturating (which would repeat the
        // final base and alias an existing window's range). The production
        // static shares this logic; exercising a local copy keeps the test
        // hermetic and fast.
        let counter = AtomicU64::new(MAX_WINDOW_TOKEN_BASE);
        let alloc = |c: &AtomicU64| -> Option<u64> {
            c.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                if cur > MAX_WINDOW_TOKEN_BASE {
                    None
                } else {
                    Some(cur.saturating_add(WINDOW_TOKEN_STRIDE))
                }
            })
            .ok()
        };
        // The last valid base is handed out exactly once...
        assert_eq!(alloc(&counter), Some(MAX_WINDOW_TOKEN_BASE));
        // ...and every subsequent request refuses rather than repeating it.
        assert_eq!(alloc(&counter), None);
        assert_eq!(alloc(&counter), None);
    }

    #[test]
    fn resolve_window_close_removes_siblings_but_exits_last() {
        assert_eq!(resolve_window_close(1, 0), WindowCloseAction::ExitProcess);
        assert_eq!(
            resolve_window_close(3, 1),
            WindowCloseAction::RemoveWindow(1)
        );
        assert_eq!(
            resolve_window_close(2, 0),
            WindowCloseAction::RemoveWindow(0)
        );
    }

    #[test]
    fn user_event_routes_to_the_owning_window_and_stale_tokens_are_dropped() {
        // Two headless windows. Give the second a disjoint token so ownership is
        // unambiguous (production tokens are process-unique; the default
        // headless launch session is token 0 in both).
        let (win_a, _ta) = headless_app_for_test();
        let (mut win_b, _tb) = headless_app_for_test();
        win_b
            .workspace_set_mut()
            .rekey_sole_session_for_test(SessionToken(500));
        let windows = vec![win_a, win_b];

        let to_a = UserEvent::Redraw {
            session: SessionToken(0),
        };
        let to_b = UserEvent::ShellExited {
            session: SessionToken(500),
        };
        let stale = UserEvent::Redraw {
            session: SessionToken(999),
        };

        assert_eq!(owner_index_for_user_event(&windows, &to_a), Some(0));
        assert_eq!(owner_index_for_user_event(&windows, &to_b), Some(1));
        assert_eq!(owner_index_for_user_event(&windows, &stale), None);
    }

    #[test]
    fn merge_moves_the_source_workspace_and_cancels_pending_input_on_both() {
        let (mut target, _tt) = headless_app_for_test();
        let (mut source, _ts) = headless_app_for_test();
        // Disjoint tokens so preflight does not (correctly) refuse a collision.
        source
            .workspace_set_mut()
            .rekey_sole_session_for_test(SessionToken(500));

        // Arm a pending, unconfirmed input on BOTH endpoints; the merge must
        // clear both so nothing flushes into a transferring session.
        target.arm_pending_image_paste_for_test();
        source.arm_pending_image_paste_for_test();
        assert!(target.has_pending_merge_input_for_test());
        assert!(source.has_pending_merge_input_for_test());

        let plan = execute_window_merge(&mut target, &mut source).expect("merge succeeds");
        assert_eq!(plan.workspace_count(), 1);

        // Source workspace + session moved into target; both tokens live there.
        assert!(target.workspace_set().owns_session(SessionToken(0)));
        assert!(target.workspace_set().owns_session(SessionToken(500)));
        assert_eq!(target.workspace_set().workspace_count(), 2);

        // Source arena is empty: the owner can retire it without any shutdown.
        assert!(source.workspace_set().is_empty());

        // Pending input cleared on both windows.
        assert!(!target.has_pending_merge_input_for_test());
        assert!(!source.has_pending_merge_input_for_test());
    }

    #[test]
    fn new_window_request_is_captured_and_serviced_into_a_sibling() {
        let (mut win, _t) = headless_app_for_test();
        assert!(win.take_new_window_request().is_none(), "none at rest");

        win.request_new_window();
        // Idempotent: a second chord while one is pending does not stack.
        win.request_new_window();

        let mut windows = vec![win];
        let mut factory_calls = 0;
        let created = service_new_window_requests(&mut windows, |_req| {
            factory_calls += 1;
            Some(headless_app_for_test().0)
        });
        assert_eq!(created, 1, "exactly one sibling spawned");
        assert_eq!(factory_calls, 1, "request captured once");
        assert_eq!(windows.len(), 2, "sibling appended");
        // Request consumed: a follow-up service pass spawns nothing.
        assert_eq!(
            service_new_window_requests(&mut windows, |_| Some(headless_app_for_test().0)),
            0
        );
    }

    #[test]
    fn merge_picker_request_is_captured_and_drained_once() {
        use crate::native::merge_picker::MergeDirection;

        let (mut win, _t) = headless_app_for_test();
        assert!(win.take_merge_picker_request().is_none(), "none at rest");

        // A single-window session offers no merge target.
        assert!(!win.merge_targets_available());
        win.set_sibling_window_count(2);
        assert!(win.merge_targets_available());

        // Selecting a palette merge row captures a directional request; the last
        // direction wins and the request does not stack.
        win.request_merge_picker(MergeDirection::MergeThisInto);
        win.request_merge_picker(MergeDirection::PullIntoThis);
        assert_eq!(
            win.take_merge_picker_request(),
            Some(MergeDirection::PullIntoThis)
        );
        // Drained: a second take is empty until the user re-invokes.
        assert!(win.take_merge_picker_request().is_none());
    }

    #[test]
    fn new_window_spawn_failure_is_dropped_not_fatal() {
        let (mut win, _t) = headless_app_for_test();
        win.request_new_window();
        let mut windows = vec![win];
        // A factory that cannot spawn returns None; the request is dropped.
        let created = service_new_window_requests(&mut windows, |_| None);
        assert_eq!(created, 0);
        assert_eq!(
            windows.len(),
            1,
            "requesting window survives a failed spawn"
        );
    }

    #[test]
    fn merge_refuses_a_token_collision_without_touching_either_window() {
        // Both windows own token 0: a merge must fail closed and change nothing.
        let (mut target, _tt) = headless_app_for_test();
        let (mut source, _ts) = headless_app_for_test();

        let err = execute_window_merge(&mut target, &mut source).expect_err("collision refused");
        assert_eq!(err, MergeError::TokenCollision(SessionToken(0)));
        assert_eq!(target.workspace_set().workspace_count(), 1);
        assert_eq!(source.workspace_set().workspace_count(), 1);
        assert!(source.workspace_set().owns_session(SessionToken(0)));
    }
}
