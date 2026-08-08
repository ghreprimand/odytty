// SPDX-License-Identifier: GPL-3.0-only
//! Session persistence tests: capture, restore, append, rollback, attach
//! budget, and structural fingerprinting.

use super::*;

#[cfg(unix)]
#[test]
fn per_connection_attach_budget_bounds_the_whole_restore_batch() {
    use std::time::Duration;
    let cap = Duration::from_secs(5);
    let now = Instant::now();

    // Budget remaining and under the cap: the pane gets what is left, so a
    // batch of K slow panes shares one 5s budget instead of K * 5s.
    let mid_batch = now + Duration::from_millis(1500);
    let budget =
        per_connection_attach_budget(mid_batch, now, cap).expect("budget available mid-batch");
    assert!(budget <= cap && budget > Duration::from_millis(1400));

    // Plenty of budget: capped at the per-connection maximum.
    let fresh = now + Duration::from_secs(30);
    assert_eq!(per_connection_attach_budget(fresh, now, cap), Some(cap));

    // Batch budget spent: no handshake attempted (fast-fail to a fresh shell).
    let spent = now - Duration::from_millis(1);
    assert_eq!(per_connection_attach_budget(spent, now, cap), None);
    assert_eq!(per_connection_attach_budget(now, now, cap), None);
}

#[test]
fn move_workspace_order_round_trips_through_the_shape_snapshot() {
    // Reorder, then confirm the captured shape preserves the new rail order
    // (the autosave/restore path serializes `workspaces` in this order).
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    set.rename_workspace(0, "alpha".to_owned());
    set.rename_workspace(1, "beta".to_owned());
    set.rename_workspace(2, "gamma".to_owned());
    assert!(set.switch_workspace(2)); // gamma active
    // Move gamma (idx 2) up to the front.
    assert!(set.move_workspace(2, true));
    assert!(set.move_workspace(1, true));
    assert_eq!(set.workspace_names(), vec!["gamma", "alpha", "beta"]);
    assert_eq!(
        set.active_workspace_index(),
        0,
        "gamma still active after the move"
    );

    let shape = set.capture_shape();
    let order: Vec<&str> = shape.workspaces.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(
        order,
        vec!["gamma", "alpha", "beta"],
        "snapshot preserves the reordered rail"
    );
    assert_eq!(
        shape.active_workspace, 0,
        "active index captured after the reorder"
    );
}

/// F6-W5: a workspace host binding survives the capture -> restore round
/// trip so a restored remote workspace routes New Tab through the host again.
#[test]
fn workspace_host_binding_survives_restore() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.set_active_workspace_default_profile(Some("edge-1".to_owned()));
    let snapshot = set.capture_shape();
    assert_eq!(
        snapshot.workspaces[0].default_profile.as_deref(),
        Some("edge-1"),
        "capture carries the binding into the snapshot"
    );

    let mut restored = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    restored.restore_from_snapshot_with(
        &snapshot,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert_eq!(
        restored.active_workspace_default_profile(),
        Some("edge-1"),
        "restore re-applies the binding"
    );
}

/// WP3 / 8e: instantiating a layout APPENDS its workspace(s) after the
/// current list and focuses the first appended one — the live workspaces are
/// untouched (never clobbered).
#[test]
fn append_from_snapshot_appends_without_clobbering() {
    // Start with two live workspaces.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    assert_eq!(set.workspace_count(), 2);
    set.rename_workspace(0, "live-a".to_owned());
    set.rename_workspace(1, "live-b".to_owned());

    // A one-workspace layout snapshot.
    let layout = crate::native::persistence::ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![crate::native::persistence::WorkspaceShape {
            name: "from-layout".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![crate::native::persistence::TabShape {
                title: None,
                focused_leaf: 0,
                layout: crate::native::persistence::PaneShape::Leaf {
                    cwd: None,
                    session_host_id: None,
                    remote_host: None,
                },
            }],
        }],
    };

    let mut handed = Vec::new();
    let report = set.append_from_snapshot_with(
        &layout,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(matches!(
        report,
        RestoreReport::Restored { workspaces: 1, .. }
    ));
    // The two live workspaces survive; the layout is appended as a third and
    // becomes active.
    assert_eq!(set.workspace_count(), 3);
    assert_eq!(set.workspace_name(0), Some("live-a"));
    assert_eq!(set.workspace_name(1), Some("live-b"));
    assert_eq!(set.workspace_name(2), Some("from-layout"));
    assert_eq!(set.active_workspace_index(), 2);
}

/// SAVE-ALL-LAYOUT: `capture_shape` records EVERY workspace (not just the
/// active one), preserving rail order and the active-workspace index. This is
/// the whole-app save side — the same snapshot the single-workspace save then
/// slices down to one.
#[test]
fn capture_shape_records_every_workspace() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    set.rename_workspace(0, "one".to_owned());
    set.rename_workspace(1, "two".to_owned());
    set.rename_workspace(2, "three".to_owned());
    // Focus the middle workspace so the captured active index is non-zero.
    set.switch_workspace(1);

    let snapshot = set.capture_shape();
    assert_eq!(
        snapshot.workspaces.len(),
        3,
        "captures all three workspaces"
    );
    assert_eq!(snapshot.workspaces[0].name, "one");
    assert_eq!(snapshot.workspaces[1].name, "two");
    assert_eq!(snapshot.workspaces[2].name, "three");
    assert_eq!(
        snapshot.active_workspace, 1,
        "the active-workspace index is preserved in the whole-app capture"
    );
}

/// SAVE-ALL-LAYOUT: opening a whole-app layout (a multi-workspace snapshot)
/// APPENDS every one of its workspaces after the live list, never just the
/// first — the open side of the whole-app save.
#[test]
fn append_from_snapshot_appends_all_workspaces_of_a_multi_workspace_layout() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set.rename_workspace(0, "live-a".to_owned());
    set.rename_workspace(1, "live-b".to_owned());
    assert_eq!(set.workspace_count(), 2);

    // A three-workspace layout snapshot (the whole-app save output shape).
    let leaf = || crate::native::persistence::TabShape {
        title: None,
        focused_leaf: 0,
        layout: crate::native::persistence::PaneShape::Leaf {
            cwd: None,
            session_host_id: None,
            remote_host: None,
        },
    };
    let ws = |name: &str| crate::native::persistence::WorkspaceShape {
        name: name.to_owned(),
        default_profile: None,
        active_tab: 0,
        tabs: vec![leaf()],
    };
    let layout = crate::native::persistence::ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![ws("lay-1"), ws("lay-2"), ws("lay-3")],
    };

    let mut handed = Vec::new();
    let report = set.append_from_snapshot_with(
        &layout,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(
        matches!(report, RestoreReport::Restored { workspaces: 3, .. }),
        "all three layout workspaces are appended"
    );
    // The two live workspaces survive; the three layout workspaces follow.
    assert_eq!(set.workspace_count(), 5);
    assert_eq!(set.workspace_name(0), Some("live-a"));
    assert_eq!(set.workspace_name(1), Some("live-b"));
    assert_eq!(set.workspace_name(2), Some("lay-1"));
    assert_eq!(set.workspace_name(3), Some("lay-2"));
    assert_eq!(set.workspace_name(4), Some("lay-3"));
    // The first appended workspace becomes active (8e focus rule).
    assert_eq!(set.active_workspace_index(), 2);
}

/// PRISTINE-CONSUME: a bare launch is exactly one untouched default
/// workspace, so the predicate reads `true`.
#[test]
fn is_single_pristine_workspace_true_for_a_fresh_launch() {
    let set = WorkspaceSet::new(build_session(), None);
    assert!(set.is_single_pristine_workspace());
}

/// PRISTINE-CONSUME: every kind of real state defeats the pristine check —
/// a second workspace, a rename, a host binding, a split, or an extra tab.
#[test]
fn is_single_pristine_workspace_false_for_any_real_state() {
    // A second workspace.
    let mut two = WorkspaceSet::new(build_session(), None);
    two.push_workspace(build_session_with_id(SessionToken(1)));
    assert!(!two.is_single_pristine_workspace(), "two workspaces");

    // A renamed sole workspace.
    let mut renamed = WorkspaceSet::new(build_session(), None);
    renamed.rename_workspace(0, "prod".to_owned());
    assert!(!renamed.is_single_pristine_workspace(), "renamed");

    // A host-bound sole workspace.
    let mut bound = WorkspaceSet::new(build_session(), None);
    bound.set_active_workspace_default_profile(Some("edge".to_owned()));
    assert!(!bound.is_single_pristine_workspace(), "host-bound");

    // A split sole workspace (two panes in the one tab).
    let mut split = WorkspaceSet::new(build_session(), None);
    split.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert!(!split.is_single_pristine_workspace(), "split");

    // A sole workspace with a second tab.
    let mut two_tabs = WorkspaceSet::new(build_session(), None);
    let extra = two_tabs.push_arena_only(build_session_with_id(SessionToken(1)));
    two_tabs.workspaces[0].tabs.push(Tab::single(extra));
    assert!(!two_tabs.is_single_pristine_workspace(), "two tabs");

    // A renamed tab title on the sole tab.
    let mut titled = WorkspaceSet::new(build_session(), None);
    let token = titled.workspaces[0].tabs[0].focused;
    titled.set_title_override(token, Some("build".to_owned()));
    assert!(!titled.is_single_pristine_workspace(), "tab titled");
}

/// PRISTINE-CONSUME: opening a layout onto a pristine launch replaces the
/// default workspace with the saved set — no stray "Workspace 1" left over,
/// and the pristine session is reaped from the arena.
#[test]
fn append_consumes_a_pristine_workspace_on_open() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let pristine_token = set.workspaces[0].tabs[0].focused;
    assert!(set.sessions.contains_key(&pristine_token));

    let leaf = || crate::native::persistence::TabShape {
        title: None,
        focused_leaf: 0,
        layout: crate::native::persistence::PaneShape::Leaf {
            cwd: None,
            session_host_id: None,
            remote_host: None,
        },
    };
    let ws = |name: &str| crate::native::persistence::WorkspaceShape {
        name: name.to_owned(),
        default_profile: None,
        active_tab: 0,
        tabs: vec![leaf()],
    };
    let layout = crate::native::persistence::ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![ws("saved-a"), ws("saved-b")],
    };

    let mut handed = Vec::new();
    let report = set.append_from_snapshot_with(
        &layout,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(matches!(
        report,
        RestoreReport::Restored { workspaces: 2, .. }
    ));
    // Exactly the saved set — the pristine workspace is gone, not appended.
    assert_eq!(set.workspace_count(), 2);
    assert_eq!(set.workspace_name(0), Some("saved-a"));
    assert_eq!(set.workspace_name(1), Some("saved-b"));
    assert_eq!(set.active_workspace_index(), 0);
    // The consumed workspace's session was reaped.
    assert!(!set.sessions.contains_key(&pristine_token));
}

/// PRISTINE-CONSUME: a single but NON-pristine workspace (here, renamed) is
/// NOT consumed — the layout appends beside it, never clobbering real state.
#[test]
fn append_does_not_consume_a_single_but_renamed_workspace() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.rename_workspace(0, "live".to_owned());

    let layout = crate::native::persistence::ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![crate::native::persistence::WorkspaceShape {
            name: "saved".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![crate::native::persistence::TabShape {
                title: None,
                focused_leaf: 0,
                layout: crate::native::persistence::PaneShape::Leaf {
                    cwd: None,
                    session_host_id: None,
                    remote_host: None,
                },
            }],
        }],
    };

    let mut handed = Vec::new();
    set.append_from_snapshot_with(
        &layout,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    // The renamed workspace survives; the layout is appended as a second.
    assert_eq!(set.workspace_count(), 2);
    assert_eq!(set.workspace_name(0), Some("live"));
    assert_eq!(set.workspace_name(1), Some("saved"));
    assert_eq!(set.active_workspace_index(), 1);
}

/// LAYOUT-OPEN-MODE (Replace): instantiating a layout via the restore path
/// onto a populated multi-workspace window installs EXACTLY the saved set —
/// every prior workspace and its sessions are reaped (no survivors in the
/// arena), and the saved active-workspace index is honored.
#[test]
fn replace_via_restore_leaves_no_survivors() {
    // A populated window: three workspaces, one session each.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    set.rename_workspace(0, "old-a".to_owned());
    set.rename_workspace(1, "old-b".to_owned());
    set.rename_workspace(2, "old-c".to_owned());
    assert_eq!(set.workspace_count(), 3);
    assert_eq!(set.len(), 3, "three live sessions before replace");

    // A two-workspace layout with a non-zero active index.
    let leaf = || crate::native::persistence::TabShape {
        title: None,
        focused_leaf: 0,
        layout: crate::native::persistence::PaneShape::Leaf {
            cwd: None,
            session_host_id: None,
            remote_host: None,
        },
    };
    let ws = |name: &str| crate::native::persistence::WorkspaceShape {
        name: name.to_owned(),
        default_profile: None,
        active_tab: 0,
        tabs: vec![leaf()],
    };
    let layout = crate::native::persistence::ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 1,
        workspaces: vec![ws("saved-a"), ws("saved-b")],
    };

    let mut handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &layout,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(matches!(
        report,
        RestoreReport::Restored { workspaces: 2, .. }
    ));
    // Exactly the saved set — no old workspaces survive.
    assert_eq!(set.workspace_count(), 2);
    assert_eq!(set.workspace_name(0), Some("saved-a"));
    assert_eq!(set.workspace_name(1), Some("saved-b"));
    // Every prior session was reaped: the arena holds only the two new panes.
    assert_eq!(set.len(), 2, "no survivor sessions from the old set");
    // The saved active-workspace index is honored.
    assert_eq!(set.active_workspace_index(), 1);
}

/// WP3 / 8h: a pane carrying a session-host id whose host is not alive (no
/// runtime dir in the test) is counted as a reattach attempt but falls back
/// to a fresh shell — never a dead pane. Verifies the "N of M" accounting.
#[test]
fn reattach_counts_attempt_and_falls_back_to_fresh_when_host_is_dead() {
    let snapshot = crate::native::persistence::ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![crate::native::persistence::WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![crate::native::persistence::TabShape {
                title: None,
                focused_leaf: 0,
                layout: crate::native::persistence::PaneShape::Leaf {
                    cwd: None,
                    session_host_id: Some("odytty-nonexistent-host".to_owned()),
                    remote_host: None,
                },
            }],
        }],
    };
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(
        matches!(
            report,
            RestoreReport::Restored {
                panes: 1,
                reattached: 0,
                reattach_attempted: 1,
                ..
            }
        ),
        "report was {report:?}"
    );
    // A fresh shell was spawned for the pane despite the dead host id.
    assert_eq!(handed.len(), 1);
}

/// Capture a rich shape, restore it into a fresh set, and assert the rebuilt
/// shape equals the captured one (structural equality; the fake sessions
/// carry no cwd, so every captured cwd here is `None` too). Exercises the
/// end-to-end capture -> restore round trip headlessly.
#[test]
fn restore_rebuilds_the_captured_shape() {
    // ws0: tab0 split (Rows) into two panes; tab1 a titled single pane.
    // ws1: one single-pane tab, renamed. Active stays ws0 / tab0.
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.set_title_override(SessionToken(1), Some("build".to_owned()));
    set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(2)));
    set.push_workspace(build_session_with_id(SessionToken(3)));
    set.rename_workspace(1, "logs".to_owned());

    let snapshot = set.capture_shape();
    assert_eq!(snapshot.workspaces.len(), 2);

    let mut restored = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    let report = restored.restore_from_snapshot_with(
        &snapshot,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );

    assert!(
        matches!(
            report,
            RestoreReport::Restored {
                workspaces: 2,
                panes: 4,
                stale_cwd: 0,
                ..
            }
        ),
        "report was {report:?}"
    );
    // The launch session was reaped; only the 4 restored leaves remain.
    assert_eq!(restored.len(), 4);
    // The rebuilt shape mirrors the captured one exactly.
    assert_eq!(restored.capture_shape(), snapshot);
}

/// A captured directory that no longer exists lands the pane at home and is
/// counted stale; an unknown (`None`) cwd also lands at home but is NOT
/// counted (a quiet fallback). Both drive the resolved cwd handed to spawn.
#[test]
fn restore_lands_stale_and_unknown_cwds_at_home() {
    use crate::native::persistence::{
        PaneShape, ShapeSnapshot, SplitAxisShape, TabShape, WorkspaceShape,
    };
    let snapshot = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "W".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Split {
                    axis: SplitAxisShape::Columns,
                    ratio: 0.5,
                    first: Box::new(PaneShape::Leaf {
                        cwd: Some("/definitely/not/a/real/dir/odytty-wp2".to_owned()),
                        session_host_id: None,
                        remote_host: None,
                    }),
                    second: Box::new(PaneShape::Leaf {
                        cwd: None,
                        session_host_id: None,
                        remote_host: None,
                    }),
                },
            }],
        }],
    };
    let home = std::env::temp_dir();
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        Some(&home),
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );

    assert!(
        matches!(
            report,
            RestoreReport::Restored {
                panes: 2,
                stale_cwd: 1,
                ..
            }
        ),
        "report was {report:?}"
    );
    // Both leaves (stale and unknown) were handed the home directory.
    assert_eq!(handed, vec![Some(home.clone()), Some(home)]);
}

/// A spawn failure mid-rebuild aborts the whole restore, reaping anything
/// already spawned and leaving the launch layout untouched (sub-ODP 8f:
/// never a broken/empty window).
#[test]
fn restore_aborts_cleanly_when_a_leaf_fails_to_spawn() {
    use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
    let leaf = |cwd: Option<&str>| PaneShape::Leaf {
        cwd: cwd.map(str::to_owned),
        session_host_id: None,
        remote_host: None,
    };
    let snapshot = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "W".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![
                TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: leaf(None),
                },
                TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: leaf(None),
                },
            ],
        }],
    };
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut spawned = 0u32;
    let report = set.restore_from_snapshot_with(
        &snapshot,
        None,
        |inner, _cwd| {
            spawned += 1;
            if spawned >= 2 {
                return None; // second leaf fails
            }
            let token = SessionToken(inner.next_token);
            inner.next_token = inner.next_token.saturating_add(1);
            inner.sessions.insert(token, build_session_with_id(token));
            Some(token)
        },
        no_remote_spawner(),
    );

    assert_eq!(report, RestoreReport::Skipped);
    // Launch layout intact: one workspace, one (launch) session; the partial
    // spawn was reaped.
    assert_eq!(set.workspace_count(), 1);
    assert_eq!(set.len(), 1);
}

/// H4: a snapshot that PARSES cleanly but carries semantically hostile
/// values — out-of-range active/focused indices reachable by hand-editing
/// `workspaces.json` — must never panic the launch-time rebuild. Every index
/// is clamped or falls back into range, so restore lands one valid, focused
/// workspace. Belt-and-suspenders over the audited guards
/// (`active_workspace.min(len-1)`, `active_tab.min(len-1)`, and the
/// `focused_leaf` first-leaf fallback). Platform-neutral: the rebuild is
/// index math with no OS surface.
#[test]
fn out_of_range_indices_in_a_snapshot_clamp_never_panic() {
    use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
    let leaf = || PaneShape::Leaf {
        cwd: None,
        session_host_id: None,
        remote_host: None,
    };
    let snapshot = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        // Far past the (one) workspace, the (one) tab, and the (one) leaf.
        active_workspace: 999,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            active_tab: 999,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 999,
                layout: leaf(),
            }],
        }],
    };
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    // One valid workspace restored; the runaway active index clamps to the
    // last real workspace rather than panicking a bounds check.
    assert!(matches!(
        report,
        RestoreReport::Restored {
            workspaces: 1,
            panes: 1,
            ..
        }
    ));
    assert_eq!(
        set.active_workspace_index(),
        0,
        "active_workspace clamps to the last real index"
    );
}

/// H4: a snapshot with nothing restorable — zero workspaces, or workspaces
/// that are all tab-less — must degrade to `Skipped` (the launch layout is
/// left untouched, so the caller keeps its fresh session) and never panic on
/// the empty vectors. Both are reachable by hand-editing the file.
#[test]
fn empty_and_tabless_snapshots_are_skipped_never_panic() {
    use crate::native::persistence::{ShapeSnapshot, WorkspaceShape};

    // (i) zero workspaces: nothing to build -> Skipped, layout intact.
    let empty = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![],
    };
    let mut set = WorkspaceSet::new(build_session(), None);
    let before = set.workspace_count();
    let mut handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &empty,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert_eq!(report, RestoreReport::Skipped);
    assert_eq!(
        set.workspace_count(),
        before,
        "a no-op restore leaves the live layout intact"
    );

    // (ii) every workspace has empty tabs: each is `continue`d, nothing is
    // built -> Skipped, and no `active_tab.min(len-1)` underflow on len 0.
    let tabless = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![
            WorkspaceShape {
                name: "a".to_owned(),
                default_profile: None,
                active_tab: 3,
                tabs: vec![],
            },
            WorkspaceShape {
                name: "b".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![],
            },
        ],
    };
    let mut set2 = WorkspaceSet::new(build_session(), None);
    let mut handed2 = Vec::new();
    let report2 = set2.restore_from_snapshot_with(
        &tabless,
        None,
        fake_spawner(&mut handed2),
        no_remote_spawner(),
    );
    assert_eq!(report2, RestoreReport::Skipped);
}

/// H4: absurd split ratios and a deep split spine must not panic the
/// recursive rebuild. Out-of-[0,1] ratios (negative, > 1, huge) are
/// reachable by hand-editing the file; NaN / infinity are not parser-
/// reachable (JSON has no such literals) but are constructed here to prove
/// the rebuild is total over any `f32` it is handed. The ratios are stored
/// verbatim into the layout tree; geometry that consumes them is a separate,
/// later concern — restore itself only builds the tree.
#[test]
fn absurd_ratios_and_deep_nesting_restore_without_panicking() {
    use crate::native::persistence::{
        PaneShape, ShapeSnapshot, SplitAxisShape, TabShape, WorkspaceShape,
    };
    let leaf = || PaneShape::Leaf {
        cwd: None,
        session_host_id: None,
        remote_host: None,
    };
    // A right-leaning split spine 40 deep, each level carrying a
    // pathological ratio. 40 added leaves + the original = 41 leaves.
    let mut node = leaf();
    for i in 0..40 {
        let ratio = match i % 4 {
            0 => f32::NAN,
            1 => -3.0,
            2 => 17.0,
            _ => f32::INFINITY,
        };
        node = PaneShape::Split {
            axis: SplitAxisShape::Columns,
            ratio,
            first: Box::new(leaf()),
            second: Box::new(node),
        };
    }
    let snapshot = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "deep".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: node,
            }],
        }],
    };
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    // The whole spine spawned; the rebuild survived the ratios and depth.
    assert!(matches!(report, RestoreReport::Restored { panes: 41, .. }));
}

/// RESTORE-REMOTE: a leaf carrying a `remote_host` reconnects through the
/// remote spawner (with the exact stored identity), while a local leaf beside
/// it still routes to the local spawner. No pane falls back.
#[test]
fn restore_reconnects_remote_leaves_and_keeps_local_leaves_local() {
    use crate::native::persistence::{
        PaneShape, ShapeSnapshot, SplitAxisShape, TabShape, WorkspaceShape,
    };
    let snapshot = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "W".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Split {
                    axis: SplitAxisShape::Columns,
                    ratio: 0.5,
                    first: Box::new(PaneShape::Leaf {
                        cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                        session_host_id: None,
                        remote_host: None,
                    }),
                    second: Box::new(PaneShape::Leaf {
                        // A remote pane captured the REMOTE cwd; it must not be
                        // used to chdir a local shell — this leaf reconnects.
                        cwd: Some("/root".to_owned()),
                        session_host_id: None,
                        remote_host: Some("prod".to_owned()),
                    }),
                },
            }],
        }],
    };
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut local_handed = Vec::new();
    let mut remote_seen = Vec::new();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        None,
        fake_spawner(&mut local_handed),
        fake_remote_spawner(&mut remote_seen),
    );
    // The remote leaf reached the connect spawner with its stored identity;
    // the local leaf reached the local spawner with its own cwd — and the
    // remote leaf never touched the local spawner (no /root local shell).
    assert_eq!(remote_seen, vec!["prod".to_owned()]);
    assert_eq!(local_handed, vec![Some(std::env::temp_dir())]);
    assert!(
        matches!(
            report,
            RestoreReport::Restored {
                panes: 2,
                remote_fallback: 0,
                ..
            }
        ),
        "{report:?}"
    );
}

/// RESTORE-REMOTE: a remote leaf whose host cannot be resolved (the spawner
/// returns `None`) falls back to a local shell, counted in `remote_fallback`,
/// and the restore still succeeds — never a wholesale abort.
#[test]
fn restore_falls_back_to_local_when_remote_host_unresolvable() {
    use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
    let snapshot = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "W".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Leaf {
                    // cwd None so the local fallback lands at home cleanly
                    // (no stale-cwd noise) — this asserts the remote_fallback
                    // count in isolation.
                    cwd: None,
                    session_host_id: None,
                    remote_host: Some("gone.example.invalid".to_owned()),
                },
            }],
        }],
    };
    let home = std::env::temp_dir();
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut local_handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        Some(&home),
        fake_spawner(&mut local_handed),
        no_remote_spawner(),
    );
    assert_eq!(local_handed, vec![Some(home)]);
    assert!(
        matches!(
            report,
            RestoreReport::Restored {
                panes: 1,
                remote_fallback: 1,
                stale_cwd: 0,
                ..
            }
        ),
        "{report:?}"
    );
}

/// RESTORE-REMOTE / sub-ODP 8f: a local leaf whose captured directory exists
/// but denies the spawn (the EACCES a real `chdir` would hit, e.g. a remote
/// `/root` that exists locally at mode 700) retries once at home — counted as
/// stale_cwd — instead of aborting the whole restore.
#[test]
fn restore_retries_at_home_when_spawn_fails_at_an_existing_cwd() {
    use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
    // A real directory that EXISTS (so resolve_cwd does not pre-fall-back to
    // home) but that the spawner will refuse, standing in for a live chdir
    // EACCES on a mode-000/700 directory.
    let bad = std::env::temp_dir().join(format!("odytty-eacces-{}", std::process::id()));
    std::fs::create_dir_all(&bad).unwrap();
    let bad_str = bad.to_string_lossy().into_owned();
    let home = std::env::temp_dir();
    let snapshot = ShapeSnapshot {
        version: crate::native::persistence::SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "W".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Leaf {
                    cwd: Some(bad_str.clone()),
                    session_host_id: None,
                    remote_host: None,
                },
            }],
        }],
    };
    let mut set = WorkspaceSet::new(build_session(), None);
    let mut handed: Vec<Option<std::path::PathBuf>> = Vec::new();
    let bad_path = bad.clone();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        Some(&home),
        |inner: &mut WorkspaceSet, cwd: Option<std::path::PathBuf>| {
            handed.push(cwd.clone());
            // Refuse the captured directory (the simulated EACCES); accept the
            // home retry.
            if cwd.as_deref() == Some(bad_path.as_path()) {
                return None;
            }
            let token = SessionToken(inner.next_token);
            inner.next_token = inner.next_token.saturating_add(1);
            inner.sessions.insert(token, build_session_with_id(token));
            Some(token)
        },
        no_remote_spawner(),
    );
    let _ = std::fs::remove_dir_all(&bad);
    // First tried the captured dir, then retried at home; counted stale, and
    // the restore succeeded rather than aborting.
    assert_eq!(handed, vec![Some(bad), Some(home)]);
    assert!(
        matches!(
            report,
            RestoreReport::Restored {
                panes: 1,
                stale_cwd: 1,
                ..
            }
        ),
        "{report:?}"
    );
}

/// RESTORE-REMOTE: a session's remote destination is captured into the shape
/// as the leaf's `remote_host`; a local session leaves it `None`.
#[test]
fn capture_records_remote_destination_as_remote_host() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let token = set.active_id();
    set.sessions.get_mut(&token).unwrap().remote_destination = Some("prod".to_owned());
    let snapshot = set.capture_shape();
    match &snapshot.workspaces[0].tabs[0].layout {
        crate::native::persistence::PaneShape::Leaf { remote_host, .. } => {
            assert_eq!(remote_host.as_deref(), Some("prod"));
        }
        other => panic!("expected a leaf, got {other:?}"),
    }
}

/// The structural fingerprint changes when the shape changes and is stable
/// otherwise (the debounce trigger, sub-ODP 8c).
#[test]
fn structural_fingerprint_tracks_shape_changes() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let base = set.structural_fingerprint();
    assert_eq!(base, set.structural_fingerprint(), "stable when unchanged");

    set.push(build_session_with_id(SessionToken(1)));
    let after_tab = set.structural_fingerprint();
    assert_ne!(after_tab, base, "adding a tab changes the fingerprint");

    set.rename_workspace(0, "renamed".to_owned());
    assert_ne!(
        set.structural_fingerprint(),
        after_tab,
        "renaming a workspace changes the fingerprint"
    );
}
