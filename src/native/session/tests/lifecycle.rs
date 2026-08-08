// SPDX-License-Identifier: GPL-3.0-only
//! Session lifecycle tests: bounded close and shutdown, pane, tab, and
//! workspace creation, movement, and removal.

use super::*;

#[cfg(test)]
mod held_exit_tests {
    use super::held_exit_banner;

    #[test]
    fn held_exit_banner_never_invents_a_numeric_status() {
        assert!(held_exit_banner(Some(7)).contains("status 7"));
        let unknown = held_exit_banner(None);
        assert!(unknown.contains("unknown status"));
        assert!(unknown.contains("may have exited from a signal"));
        assert!(unknown.contains("Press any key to close."));
    }
}

/// CLOSE-HANG regression: whole-app shutdown must stay bounded even when a
/// session's reader thread is wedged (no EOF), the shape that hung Super+Q
/// with remote workspaces. The old serial path joined the pump thread on the
/// caller, so a parked reader would block shutdown for the full park time;
/// `shutdown_all` offloads the reap + join to a detached thread and returns
/// within the bounded deadline. Fail-before: the caller would take ~the park
/// duration (5s) and blow the assertion; pass-after: it returns in ~the
/// deadline.
#[test]
fn shutdown_all_is_bounded_when_a_reader_is_wedged() {
    let park = std::time::Duration::from_secs(5);
    let session = build_session_with_parked_reader(SessionToken(1), park);
    let mut set = WorkspaceSet::new(session, None);

    let deadline = std::time::Duration::from_millis(200);
    let start = Instant::now();
    set.shutdown_all(deadline);
    let elapsed = start.elapsed();

    assert!(set.is_empty(), "shutdown drains the session set");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "shutdown must be bounded, not block on the wedged reader join (took {elapsed:?})"
    );
}

/// CLOSE-HANG-2 regression: an interactive tab/workspace close must not
/// block the caller on a wedged reader join. A ControlPersist mux master can
/// hold the PTY slave open after the client is killed, so the reader never
/// sees EOF and its join would otherwise block for the full park. The
/// parked-reader session reproduces that shape; `close` kills synchronously
/// and defers wait + join to a detached reaper, returning promptly while the
/// reaper finishes later. Fail-before: the old inline `pty.wait()` + join
/// blocked the caller ~the park (3s) and blew the assertion; pass-after: it
/// returns well under a second.
#[test]
fn interactive_close_is_bounded_when_a_reader_is_wedged() {
    let park = std::time::Duration::from_secs(3);
    let mut set = WorkspaceSet::new(build_session(), None);
    let wedged = SessionToken(1);
    set.push(build_session_with_parked_reader(wedged, park));

    let start = Instant::now();
    let last = set.close(wedged);
    let elapsed = start.elapsed();

    assert!(
        !last,
        "a live tab remains, so close does not signal app exit"
    );
    assert_eq!(set.len(), 1, "the wedged session leaves the arena");
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "interactive close must not block on the wedged reader join (took {elapsed:?})"
    );
}

/// CLOSE-HANG-2 regression: closing several wedged sessions in quick
/// succession (the "closed several remote workspaces fast" freeze) must not
/// compound serially. Each close defers its reap, so N closes return in
/// aggregate well under a second even though every reader is parked.
/// Fail-before: the closes summed to N x park (~15s) on the caller.
#[test]
fn rapid_successive_closes_do_not_compound() {
    let park = std::time::Duration::from_secs(3);
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut wedged = Vec::new();
    for i in 1..=5u64 {
        let token = SessionToken(i);
        set.push(build_session_with_parked_reader(token, park));
        wedged.push(token);
    }

    let start = Instant::now();
    for token in wedged {
        let _ = set.close(token);
    }
    let elapsed = start.elapsed();

    assert_eq!(set.len(), 1, "every wedged session left the arena");
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "rapid successive closes must not compound serially (took {elapsed:?})"
    );
}

#[test]
fn session_set_switches_wraps_and_closes() {
    let mut sessions = WorkspaceSet::new(build_session(), None);
    let second = SessionToken(1);
    let third = SessionToken(2);
    sessions.push(build_session_with_id(second));
    sessions.push(build_session_with_id(third));

    assert_eq!(sessions.active_id(), SessionToken(0));
    assert!(sessions.next());
    assert_eq!(sessions.active_id(), second);
    assert!(sessions.prev());
    assert_eq!(sessions.active_id(), SessionToken(0));
    assert!(sessions.switch(third));
    assert_eq!(sessions.active_id(), third);

    let last = sessions.close(third);
    assert!(!last);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.active_id(), second);

    assert!(!sessions.close(second));
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions.active_id(), SessionToken(0));

    assert!(sessions.close(SessionToken(0)));
    assert!(sessions.is_empty());
}

#[test]
fn split_active_grows_a_pane_within_the_same_tab() {
    let mut set = WorkspaceSet::new(build_session(), None);
    // Single pane → byte-identical fast path.
    assert!(set.active_is_single_pane());
    assert_eq!(set.active_pane_count(), 1);
    assert_eq!(set.tab_count(), 1);

    let pane =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));

    // Same tab, now two panes; the new pane is focused (tmux semantics).
    assert_eq!(set.tab_count(), 1, "split adds a pane, not a tab");
    assert_eq!(set.active_pane_count(), 2);
    assert!(!set.active_is_single_pane());
    assert_eq!(set.active_id(), pane);
    // Both panes are visited by iter() (resize/scrollback-cap reach them).
    assert_eq!(set.iter().count(), 2);
}

#[test]
fn focus_next_pane_cycles_in_tree_order() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let p1 = set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let p2 = set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(2)));
    // Tree leaves in order: 0, 1, 2 (focus currently p2).
    assert_eq!(set.active_id(), p2);
    assert!(set.focus_next_pane());
    assert_eq!(set.active_id(), SessionToken(0)); // wraps to first
    assert!(set.focus_next_pane());
    assert_eq!(set.active_id(), p1);
    // Single-pane tab: no-op.
    let mut single = WorkspaceSet::new(build_session(), None);
    assert!(!single.focus_next_pane());
}

#[test]
fn set_active_focus_accepts_panes_and_rejects_strangers() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let p1 = set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert_eq!(set.active_id(), p1);
    assert!(set.set_active_focus(SessionToken(0)));
    assert_eq!(set.active_id(), SessionToken(0));
    // Same focus → no change.
    assert!(!set.set_active_focus(SessionToken(0)));
    // Unknown token → rejected.
    assert!(!set.set_active_focus(SessionToken(99)));
    assert_eq!(set.active_id(), SessionToken(0));
}

#[test]
fn closing_a_pane_keeps_the_multi_pane_tab() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let p1 = set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert_eq!(set.active_pane_count(), 2);
    // Closing one pane collapses the split; the tab survives (not last).
    assert!(!set.close(p1));
    assert_eq!(set.tab_count(), 1);
    assert_eq!(set.active_pane_count(), 1);
    assert!(set.active_is_single_pane());
    assert_eq!(set.active_id(), SessionToken(0));
}

#[test]
fn close_active_tab_reaps_the_whole_multi_pane_tab() {
    // tab0 = two panes (sessions 0 + 1); tab1 = single pane (session 2).
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    set.push(build_session_with_id(SessionToken(2)));
    assert_eq!(set.tab_count(), 2);
    assert_eq!(set.len(), 3, "three sessions across two tabs");
    // The active tab (tab0) is multi-pane.
    assert!(!set.active_is_single_pane());

    // "Close Tab" removes the ENTIRE active tab — both leaf sessions reaped,
    // the tab gone — not just the focused pane.
    let last = set.close_active_tab();
    assert!(!last, "another tab remains, so not the last tab");
    assert_eq!(set.tab_count(), 1, "the whole multi-pane tab was removed");
    assert_eq!(set.len(), 1, "both panes of the closed tab were reaped");
    // The survivor is tab1's session, now the active single-pane tab.
    assert_eq!(set.active_id(), SessionToken(2));
    assert!(set.active_is_single_pane());
}

#[test]
fn close_active_tab_differs_from_close_pane_on_a_multi_pane_tab() {
    // Two structurally identical multi-pane sets; one gets Close Tab, the
    // other Close Pane. Prove the outcomes differ (the core bug:
    // Close Tab must not behave like Close Pane).
    let build = || {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        set.push(build_session_with_id(SessionToken(2)));
        set
    };

    // Close Tab: the multi-pane tab is gone entirely.
    let mut close_tab = build();
    close_tab.close_active_tab();
    assert_eq!(close_tab.tab_count(), 1);
    assert_eq!(close_tab.active_pane_count(), 1);

    // Close Pane: collapses one leaf, the multi-pane tab SURVIVES as single.
    let mut close_pane = build();
    close_pane.close(close_pane.active_id());
    assert_eq!(close_pane.tab_count(), 2, "Close Pane keeps the tab");
    // The formerly multi-pane tab is now single-pane but still present.
    assert!(close_pane.active_is_single_pane());

    // The defining contrast: same starting state, different tab counts.
    assert_ne!(close_tab.tab_count(), close_pane.tab_count());
}

#[test]
fn close_active_tab_on_the_last_tab_signals_exit_even_when_multi_pane() {
    // A single tab holding multiple panes: Close Tab on it is the last tab,
    // so it empties the set (the App maps this to app exit). Exit keys on
    // the last TAB, never on the last pane.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert_eq!(set.tab_count(), 1);
    assert!(!set.active_is_single_pane());

    let last = set.close_active_tab();
    assert!(last, "closing the sole tab empties the set");
    assert!(set.is_empty());
}

#[test]
fn close_active_tab_on_a_single_pane_tab_matches_close_active_id() {
    // Single-pane byte-identical proof: Close Tab on a single-pane tab does
    // exactly what the old `close(active_id())` path did — same surviving
    // session, same active token, same tab count.
    let mut via_close_tab = WorkspaceSet::new(build_session(), None);
    via_close_tab.push(build_session_with_id(SessionToken(1)));
    let mut via_close_id = WorkspaceSet::new(build_session(), None);
    via_close_id.push(build_session_with_id(SessionToken(1)));

    let last_a = via_close_tab.close_active_tab();
    let last_b = via_close_id.close(via_close_id.active_id());
    assert_eq!(last_a, last_b);
    assert_eq!(via_close_tab.tab_count(), via_close_id.tab_count());
    assert_eq!(via_close_tab.active_id(), via_close_id.active_id());
    assert_eq!(via_close_tab.len(), via_close_id.len());
}

#[test]
fn close_tab_at_reaps_a_non_active_multi_pane_tab_and_leaves_active_untouched() {
    // tab0 = single pane (session 0, active); tab1 = two panes (sessions 2
    // + 1), NON-active. The tab-strip `×` can target tab1 while tab0 is
    // active — it must reap the WHOLE tab1 and leave tab0 (and the active
    // index) untouched.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(2))); // tab1, single
    assert!(set.switch(SessionToken(2))); // activate tab1
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert!(set.switch(SessionToken(0))); // back to tab0 (active)
    assert_eq!(set.tab_count(), 2);
    assert_eq!(set.len(), 3);
    assert_eq!(set.active_id(), SessionToken(0));
    assert!(set.active_is_single_pane());

    // Close the NON-active multi-pane tab1 by index.
    let last = set.close_tab_at(1);
    assert!(!last, "tab0 remains");
    assert_eq!(set.tab_count(), 1, "the whole non-active tab was removed");
    assert_eq!(set.len(), 1, "both panes of tab1 were reaped");
    // The active tab0 is unchanged: same session, still active, still single.
    assert_eq!(set.active_id(), SessionToken(0));
    assert!(set.active_is_single_pane());
}

#[test]
fn close_tab_at_a_later_index_keeps_the_active_index_stable() {
    // active = tab0; closing tab2 (to the right) must not shift the active
    // index, and closing tab0 (the active one) clamps the active index.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1))); // tab1
    set.push(build_session_with_id(SessionToken(2))); // tab2
    assert_eq!(set.active_id(), SessionToken(0)); // tab0 active
    // Close the rightmost tab: active stays on tab0.
    assert!(!set.close_tab_at(2));
    assert_eq!(set.active_id(), SessionToken(0));
    assert_eq!(set.tab_count(), 2);
    // Close the active tab0: active clamps onto the survivor (old tab1).
    assert!(!set.close_tab_at(0));
    assert_eq!(set.active_id(), SessionToken(1));
    assert_eq!(set.tab_count(), 1);
}

#[test]
fn equalize_active_is_a_noop_on_single_pane() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.equalize_active();
    assert!(set.active_is_single_pane());
    // With a split present, layout tree stays valid (ratios reset).
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    set.equalize_active();
    assert_eq!(set.active_pane_count(), 2);
}

#[test]
fn toggle_zoom_flips_and_is_a_noop_on_single_pane() {
    let mut set = WorkspaceSet::new(build_session(), None);
    // Single pane: zoom is meaningless, toggle is a no-op.
    assert!(!set.toggle_active_zoom());
    assert!(!set.active_is_zoomed());

    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // Multi-pane: toggle on, then off.
    assert!(set.toggle_active_zoom());
    assert!(set.active_is_zoomed());
    assert!(set.toggle_active_zoom());
    assert!(!set.active_is_zoomed());
}

#[test]
fn closing_a_pane_unzooms_the_tab() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert!(set.toggle_active_zoom());
    assert!(set.active_is_zoomed());
    // Close the zoomed (focused) pane: the tab survives and is no longer
    // zoomed.
    assert!(!set.close(right));
    assert!(!set.active_is_zoomed());
    assert!(set.active_is_single_pane());
}

#[test]
fn splitting_a_zoomed_tab_unzooms_it() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert!(set.toggle_active_zoom());
    assert!(set.active_is_zoomed());
    set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(2)));
    assert!(!set.active_is_zoomed(), "split clears zoom");
}

#[test]
fn active_layout_exposes_the_tree() {
    let mut set = WorkspaceSet::new(build_session(), None);
    assert!(set.active_layout().is_some_and(PaneNode::is_single_pane));
    set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(1)));
    assert_eq!(set.active_layout().map(PaneNode::pane_count), Some(2));
}

#[test]
fn a_fresh_set_holds_one_named_workspace() {
    let set = WorkspaceSet::new(build_session(), None);
    assert_eq!(set.workspace_count(), 1);
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.workspace_name(0), Some("Workspace 1"));
    assert_eq!(set.workspace_name(1), None);
}

#[test]
fn switching_workspaces_isolates_each_workspaces_tab_list() {
    // ws0: two tabs (sessions 0, 1). ws1: one tab (session 2).
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    assert_eq!(set.tab_count(), 2, "ws0 has two tabs");
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert_eq!(set.workspace_count(), 2);
    // push_workspace never switches: ws0 stays active with its two tabs.
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.tab_count(), 2);
    assert_eq!(set.active_id(), SessionToken(0));

    // Switch to ws1: its own single-tab list, its own active session.
    assert!(set.switch_workspace(1));
    assert_eq!(set.active_workspace_index(), 1);
    assert_eq!(set.tab_count(), 1);
    assert_eq!(set.active_id(), SessionToken(2));

    // A same-index / out-of-range switch is a no-op.
    assert!(!set.switch_workspace(1));
    assert!(!set.switch_workspace(9));

    // Switch back: ws0's two-tab list and prior active session are intact.
    assert!(set.switch_workspace(0));
    assert_eq!(set.tab_count(), 2);
    assert_eq!(set.active_id(), SessionToken(0));
}

#[test]
fn move_workspace_reorders_and_follows_the_active_by_identity() {
    // Three workspaces (tokens 0/1/2), ws1 active.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert!(set.switch_workspace(1));
    assert_eq!(
        set.workspace_names(),
        vec!["Workspace 1", "Workspace 2", "Workspace 3"]
    );
    assert_eq!(set.active_workspace_index(), 1);
    assert_eq!(set.active_id(), SessionToken(1));

    // Move the active workspace (idx 1) up: it swaps with idx 0, and the
    // active index follows it to 0 -- same workspace stays focused.
    assert!(set.move_workspace(1, true));
    assert_eq!(
        set.workspace_names(),
        vec!["Workspace 2", "Workspace 1", "Workspace 3"]
    );
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(
        set.active_id(),
        SessionToken(1),
        "active workspace unchanged by the move"
    );

    // Move a NON-active workspace (idx 2 = "Workspace 3") up: it swaps into
    // idx 1, which does NOT touch the active slot (idx 0), so the active
    // index is unchanged and still points at the same workspace.
    assert!(set.move_workspace(2, true));
    assert_eq!(
        set.workspace_names(),
        vec!["Workspace 2", "Workspace 3", "Workspace 1"]
    );
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.active_id(), SessionToken(1));

    // The last slot (idx 2) cannot move down: a no-op past the end.
    assert!(
        !set.move_workspace(2, false),
        "cannot move the last slot down"
    );
    assert_eq!(set.active_workspace_index(), 0);

    // Reorder back to the original rail order via the down direction: move
    // "Workspace 2" (the active, at idx 0) down twice.
    assert!(set.move_workspace(0, false));
    assert_eq!(
        set.active_workspace_index(),
        1,
        "active follows its slot down"
    );
    assert!(set.move_workspace(1, false));
    assert_eq!(set.active_workspace_index(), 2);
    assert_eq!(
        set.workspace_names(),
        vec!["Workspace 3", "Workspace 1", "Workspace 2"]
    );
    assert_eq!(
        set.active_id(),
        SessionToken(1),
        "same workspace focused throughout"
    );
}

#[test]
fn move_workspace_guards_the_ends_and_bad_indices() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    // Top guard: idx 0 cannot move up. Bottom guard: last idx cannot move down.
    assert!(!set.move_workspace(0, true));
    assert!(!set.move_workspace(1, false));
    // Out-of-range index is a no-op.
    assert!(!set.move_workspace(9, true));
    assert!(!set.move_workspace(9, false));
    // Order untouched by every rejected move.
    assert_eq!(set.workspace_names(), vec!["Workspace 1", "Workspace 2"]);
}

#[test]
fn shell_exit_closes_workspace_only_for_a_sole_pane_sole_tab() {
    // SHELL-EXIT-CLOSES: the predicate is true only when the exiting session
    // is the sole pane of the sole tab of its workspace (reaping it empties
    // the workspace). Sibling panes or tabs make it false -- those exits
    // close only a pane or tab, never the workspace.
    let mut set = WorkspaceSet::new(build_session(), None);
    // ws0: one single-pane tab (token 0). The predicate holds.
    assert!(set.shell_exit_closes_workspace(SessionToken(0)));

    // Give ws0 a second tab (token 1): now token 0 has a sibling tab.
    set.push(build_session_with_id(SessionToken(1)));
    assert!(
        !set.shell_exit_closes_workspace(SessionToken(0)),
        "a sibling tab means the exit closes only the tab"
    );
    assert!(
        !set.shell_exit_closes_workspace(SessionToken(1)),
        "the sibling tab itself closes only the tab"
    );

    // A second workspace with a SPLIT tab (tokens 2 + 3 in one tab): a
    // sibling pane means the exit closes only the pane.
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert!(set.switch_workspace(1));
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(3)));
    assert!(
        !set.shell_exit_closes_workspace(SessionToken(2)),
        "a sibling pane means the exit closes only the pane"
    );
    assert!(!set.shell_exit_closes_workspace(SessionToken(3)));

    // An unknown token is never a workspace-closing exit.
    assert!(!set.shell_exit_closes_workspace(SessionToken(999)));
}

#[test]
fn any_foreground_job_running_except_excludes_the_named_session() {
    // SHELL-EXIT-CLOSES: the "except" scan skips the exiting session. With a
    // lone workspace, excluding its only session leaves nothing to scan, so
    // the result is false regardless of that session's (already-ended) job.
    let set = WorkspaceSet::new(build_session(), None);
    assert!(!set.any_foreground_job_running_except(SessionToken(0)));
    // An unknown exclusion token scans every real session; the test shells
    // are idle (`ForegroundJob::None`), so still false -- and never panics.
    assert!(!set.any_foreground_job_running_except(SessionToken(999)));
}

#[test]
fn closing_a_workspaces_last_tab_closes_the_workspace_and_switches_out() {
    // ws0 (token 0), ws1 (token 1); ws1 active.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    assert!(set.switch_workspace(1));
    assert_eq!(set.workspace_count(), 2);

    // Closing ws1's only tab removes ws1 entirely — not app exit, because
    // ws0 survives — and clamps the active workspace back onto ws0.
    let exit = set.close_active_tab();
    assert!(!exit, "another workspace survives, so not app exit");
    assert_eq!(set.workspace_count(), 1);
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.active_id(), SessionToken(0));
    assert_eq!(set.len(), 1, "ws1's session was reaped");
}

#[test]
fn closing_the_last_workspaces_last_tab_signals_app_exit() {
    let mut set = WorkspaceSet::new(build_session(), None);
    assert_eq!(set.workspace_count(), 1);
    let exit = set.close_active_tab();
    assert!(exit, "the last tab of the last workspace exits the app");
    assert!(set.is_empty());
    assert_eq!(set.workspace_count(), 0);
}

#[test]
fn a_background_workspaces_shell_exit_reaps_it_without_disturbing_the_active_one() {
    // ws0 active (token 0); ws1 background (token 1, its sole tab).
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.workspace_count(), 2);

    // The background workspace's shell exits: its tab (and thus the now-empty
    // workspace) is reaped without app exit, and the active workspace is
    // untouched. This is the NF21 §5 background-workspace polarity: a
    // producer in a non-active workspace still serviced correctly.
    let exit = set.close_shell_exited(SessionToken(1));
    assert!(!exit);
    assert_eq!(set.workspace_count(), 1);
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.active_id(), SessionToken(0));
    assert_eq!(set.len(), 1);
}

#[test]
fn switch_deep_focuses_across_workspaces_for_attach_dedup() {
    // ws0: tabs for tokens 0 and 1. ws1: token 2, active.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert!(set.switch_workspace(1));
    assert_eq!(set.active_workspace_index(), 1);

    // Selecting a token that lives in ws0 (the attach-dedup deep-switch,
    // ODP-10) moves the active workspace + tab + focused pane in one step.
    assert!(set.switch(SessionToken(1)));
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.active_id(), SessionToken(1));

    // Re-selecting the already-focused token is a no-op; an unknown token
    // never switches.
    assert!(!set.switch(SessionToken(1)));
    assert!(!set.switch(SessionToken(99)));
}

#[test]
fn close_active_workspace_reaps_every_tab_and_pane() {
    // ws0: two tabs, one multi-pane (tokens 0+3 split, token 1). ws1: token 2.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(3)));
    set.push(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.len(), 4);

    let exit = set.close_active_workspace();
    assert!(!exit, "ws1 survives");
    assert_eq!(set.workspace_count(), 1);
    assert_eq!(set.active_id(), SessionToken(2), "ws1 is now active");
    assert_eq!(set.len(), 1, "all of ws0's sessions were reaped");
}

#[test]
fn close_active_workspace_on_the_last_workspace_signals_exit() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let exit = set.close_active_workspace();
    assert!(exit);
    assert!(set.is_empty());
    assert_eq!(set.workspace_count(), 0);
}

#[test]
fn renaming_a_workspace_updates_only_that_rail_name() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.rename_workspace(0, "infra".to_owned());
    assert_eq!(set.workspace_name(0), Some("infra"));
    set.push_workspace(build_session_with_id(SessionToken(1)));
    assert!(set.switch_workspace(1));
    set.rename_workspace(set.active_workspace_index(), "app".to_owned());
    assert_eq!(set.workspace_name(0), Some("infra"));
    assert_eq!(set.workspace_name(1), Some("app"));
}

#[test]
fn next_and_prev_workspace_wrap_in_rail_order() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert_eq!(set.active_workspace_index(), 0);
    assert!(set.next_workspace());
    assert_eq!(set.active_workspace_index(), 1);
    assert!(set.prev_workspace());
    assert_eq!(set.active_workspace_index(), 0);
    // Wrap backward from the first to the last, then forward wraps to the first.
    assert!(set.prev_workspace());
    assert_eq!(set.active_workspace_index(), 2);
    assert!(set.next_workspace());
    assert_eq!(set.active_workspace_index(), 0);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[test]
fn a_new_workspace_appends_switches_and_holds_one_tab() {
    // new_workspace needs a real event-loop proxy for the PTY spawn.
    let Some((mut set, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    assert_eq!(set.workspace_count(), 1);
    let grid = Dimensions::new(20, 8);
    let token = set.new_workspace(grid).expect("spawn new workspace");
    assert_eq!(set.workspace_count(), 2);
    // The new workspace is active and holds exactly one single-pane tab.
    assert_eq!(set.active_workspace_index(), 1);
    assert_eq!(set.tab_count(), 1);
    assert!(set.active_is_single_pane());
    assert_eq!(set.active_id(), token);
    assert_eq!(set.workspace_name(1), Some("Workspace 2"));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[test]
fn new_workspace_in_threads_cwd_and_appends_like_new_workspace() {
    // Duplicate Workspace threads the active pane's cwd through the cwd-aware
    // `new_workspace_in`, which spawns via the SAME `insert_spawned_session_in`
    // path New Tab's cwd inheritance uses. A `Some(cwd)` still appends,
    // switches to, and holds exactly one single-pane tab -- identical shape to
    // the cwd-less `new_workspace`. (The spawn honors the directory the same
    // way the tab path does; the pty's cwd is not observable here without
    // shell integration, so this pins the workspace-level behavior.)
    let Some((mut set, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    assert_eq!(set.workspace_count(), 1);
    let grid = Dimensions::new(20, 8);
    let cwd = Some(std::env::temp_dir());
    let token = set
        .new_workspace_in(grid, cwd)
        .expect("spawn new workspace in cwd");
    assert_eq!(set.workspace_count(), 2);
    assert_eq!(set.active_workspace_index(), 1);
    assert_eq!(set.tab_count(), 1);
    assert!(set.active_is_single_pane());
    assert_eq!(set.active_id(), token);
    assert_eq!(set.workspace_name(1), Some("Workspace 2"));
}

#[test]
fn move_tab_to_workspace_splices_the_tab_without_touching_the_active() {
    // ws0 holds tokens [0, 1]; ws1 holds [2]. Active stays ws0.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.tab_count(), 2);

    // Move the background tab (token 1) into ws1.
    let (moved, source_closed) = set.move_tab_to_workspace(SessionToken(1), 1);
    assert!(moved);
    assert!(!source_closed, "ws0 still has token 0");
    // Active workspace unchanged (v1: move without following) and now holds
    // only token 0.
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.tab_count(), 1);
    assert_eq!(set.active_id(), SessionToken(0));
    // The moved tab landed at the END of ws1.
    assert!(set.switch_workspace(1));
    assert_eq!(set.tab_count(), 2);
    assert_eq!(set.token_at_position(1), Some(SessionToken(1)));
    // No session left the arena.
    assert_eq!(set.len(), 3);
}

#[test]
fn moving_the_last_tab_out_closes_the_source_workspace() {
    // ws0 holds [0]; ws1 holds [1]. Moving token 0 out empties and closes ws0.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    assert_eq!(set.workspace_count(), 2);
    assert_eq!(set.active_workspace_index(), 0);

    let (moved, source_closed) = set.move_tab_to_workspace(SessionToken(0), 1);
    assert!(moved);
    assert!(source_closed, "the emptied source workspace closes (ODP-3)");
    assert_eq!(set.workspace_count(), 1);
    // The surviving workspace (old ws1, now index 0) holds both tabs: its
    // own token 1 then the moved token 0.
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.tab_count(), 2);
    assert_eq!(set.token_at_position(0), Some(SessionToken(1)));
    assert_eq!(set.token_at_position(1), Some(SessionToken(0)));
    assert_eq!(set.len(), 2);
}

#[test]
fn move_tab_destinations_excludes_the_source_and_is_empty_alone() {
    // Single workspace: no destinations, so the picker never opens (W4-v2).
    let mut set = WorkspaceSet::new(build_session(), None);
    assert!(
        set.move_tab_destinations(SessionToken(0)).is_empty(),
        "one workspace = nowhere to move"
    );

    // Three workspaces named in order; from ws2 the destinations are ws0 and
    // ws1 (the source ws2 is excluded), carrying their ORIGINAL indices.
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    set.rename_workspace(0, "alpha".to_owned());
    set.rename_workspace(1, "beta".to_owned());
    set.rename_workspace(2, "gamma".to_owned());
    assert!(set.switch_workspace(2));
    let token = set.active_id();
    let dests = set.move_tab_destinations(token);
    assert_eq!(
        dests,
        vec![(0, "alpha".to_owned()), (1, "beta".to_owned())],
        "source workspace excluded; original indices + names preserved"
    );
}

#[test]
fn reposition_active_tab_after_slides_new_tab_next_to_a_non_active_anchor() {
    // ODP-5D: the connect flow appends the remote tab last + switches to it;
    // reposition then slides it to sit right after the CLICKED (anchor) tab,
    // even when the anchor is neither the active nor the last tab.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.push(build_session_with_id(SessionToken(2)));
    // The freshly-appended remote tab, made active (mirrors connect + switch).
    set.push(build_session_with_id(SessionToken(3)));
    assert!(set.switch(SessionToken(3)));
    // Strip is [0, 1, 2, 3]; anchor on the clicked tab 1 (≠ active, ≠ last).
    set.reposition_active_tab_after(SessionToken(1));
    assert_eq!(set.token_at_position(0), Some(SessionToken(0)));
    assert_eq!(set.token_at_position(1), Some(SessionToken(1)));
    assert_eq!(set.token_at_position(2), Some(SessionToken(3)));
    assert_eq!(set.token_at_position(3), Some(SessionToken(2)));
    // The moved tab stays active/focused at its new index.
    assert_eq!(set.active_id(), SessionToken(3));
}

#[test]
fn reorder_tab_splices_with_insertion_semantics_and_preserves_active_identity() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.push(build_session_with_id(SessionToken(2)));
    assert!(set.switch(SessionToken(1)));

    assert!(set.reorder_tab(0, 3));
    assert_eq!(set.token_at_position(0), Some(SessionToken(1)));
    assert_eq!(set.token_at_position(1), Some(SessionToken(2)));
    assert_eq!(set.token_at_position(2), Some(SessionToken(0)));
    assert_eq!(set.active_id(), SessionToken(1));

    assert!(!set.reorder_tab(2, 3), "drop after itself is a no-op");
    assert!(!set.reorder_tab(9, 0), "invalid source is a no-op");
    assert!(!set.reorder_tab(0, 9), "invalid insertion is a no-op");
}

#[test]
fn reposition_active_tab_after_is_a_noop_when_already_adjacent_or_anchor_missing() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.push(build_session_with_id(SessionToken(2)));
    assert!(set.switch(SessionToken(2)));
    // Anchor is the tab right before the moved (last) tab: already in place.
    set.reposition_active_tab_after(SessionToken(1));
    assert_eq!(set.token_at_position(2), Some(SessionToken(2)));
    // Unknown anchor: nothing moves.
    set.reposition_active_tab_after(SessionToken(99));
    assert_eq!(set.token_at_position(2), Some(SessionToken(2)));
}

#[test]
fn tab_foreground_job_running_resolves_tokens_and_defaults_false_when_idle() {
    // ODP-5D replace gating: a resolvable idle/headless tab reports
    // not-running (→ replace-direct path), and an unknown token is false.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    assert!(!set.tab_foreground_job_running(SessionToken(1)));
    assert!(!set.tab_foreground_job_running(SessionToken(99)));
}

#[test]
fn move_tab_rejects_unknown_token_out_of_range_and_same_workspace() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    // Unknown token.
    assert_eq!(
        set.move_tab_to_workspace(SessionToken(99), 1),
        (false, false)
    );
    // Out-of-range destination.
    assert_eq!(
        set.move_tab_to_workspace(SessionToken(0), 9),
        (false, false)
    );
    // Same workspace (token 0 already in ws0 → dest 0).
    assert_eq!(
        set.move_tab_to_workspace(SessionToken(0), 0),
        (false, false)
    );
    // Nothing changed.
    assert_eq!(set.workspace_count(), 2);
    assert_eq!(set.tab_count(), 2);
}

/// F6-W5: binding the active workspace to a host alias is observable through
/// the accessor, idempotent, and unbinding returns the previous alias.
#[test]
fn workspace_host_binding_set_and_clear() {
    let mut set = WorkspaceSet::new(build_session(), None);
    assert_eq!(set.active_workspace_default_profile(), None);
    assert_eq!(
        set.set_active_workspace_default_profile(Some("prod".to_owned())),
        None,
        "first bind returns the previous (empty) binding"
    );
    assert_eq!(set.active_workspace_default_profile(), Some("prod"));
    assert_eq!(
        set.set_active_workspace_default_profile(None),
        Some("prod".to_owned()),
        "unbind returns the alias that was bound"
    );
    assert_eq!(set.active_workspace_default_profile(), None);
}

/// RAIL-BIND: the by-index bind/query pair targets a specific slot and is a
/// safe no-op out of range.
#[test]
fn workspace_host_binding_by_index() {
    let mut set = WorkspaceSet::new(build_session(), None);
    assert_eq!(set.workspace_default_profile_at(0), None);
    assert_eq!(
        set.set_workspace_default_profile_at(0, Some("edge".to_owned())),
        None
    );
    assert_eq!(set.workspace_default_profile_at(0), Some("edge"));
    // The active accessor sees the same binding (slot 0 is active here).
    assert_eq!(set.active_workspace_default_profile(), Some("edge"));
    // Out-of-range index is a no-op returning None.
    assert_eq!(
        set.set_workspace_default_profile_at(9, Some("x".to_owned())),
        None
    );
    assert_eq!(set.workspace_default_profile_at(9), None);
}
