// SPDX-License-Identifier: GPL-3.0-only
//! Creation identities survive changes to the panes that originally seeded them.

use super::*;
use crate::native::session_navigator::{NavigatorTarget, live_entries, live_token};

#[test]
fn navigator_tab_identity_survives_focus_and_original_pane_closure() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let tab = NavigatorTarget::Tab(SessionToken(0));
    let workspace = NavigatorTarget::Workspace(SessionToken(0));
    set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(1)));
    set.switch(SessionToken(1));
    assert!(
        live_entries(&set, false)
            .iter()
            .any(|entry| entry.target == tab)
    );
    assert_eq!(live_token(&set, &tab), Some(SessionToken(1)));
    assert!(!set.close(SessionToken(0)));
    assert_eq!(live_token(&set, &tab), Some(SessionToken(1)));
    assert_eq!(live_token(&set, &workspace), Some(SessionToken(1)));
    assert_eq!(
        live_token(&set, &NavigatorTarget::Live(SessionToken(0))),
        None
    );
}

#[test]
fn navigator_workspace_identity_does_not_follow_its_first_tab_when_moved() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push(build_session_with_id(SessionToken(1)));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    let original = NavigatorTarget::Workspace(SessionToken(0));
    assert_eq!(set.move_tab_to_workspace(SessionToken(0), 1), (true, false));
    assert_eq!(live_token(&set, &original), Some(SessionToken(1)));
    assert_eq!(
        live_token(&set, &NavigatorTarget::Tab(SessionToken(0))),
        Some(SessionToken(0))
    );
    set.move_workspace(0, false);
    assert_eq!(live_token(&set, &original), Some(SessionToken(1)));
}

#[test]
fn navigator_merge_preserves_ids_after_the_origin_pane_has_closed() {
    let mut dest = WorkspaceSet::new(build_session(), None);
    let base = 1 << 40;
    let mut source = WorkspaceSet::new(build_session_with_id(SessionToken(base)), None);
    let tab = NavigatorTarget::Tab(SessionToken(base));
    let workspace = NavigatorTarget::Workspace(SessionToken(base));
    source.split_active_for_test(
        SplitAxis::Rows,
        build_session_with_id(SessionToken(base + 1)),
    );
    assert!(!source.close(SessionToken(base)));
    dest.merge_from(&mut source).expect("disjoint owners merge");
    assert_eq!(live_token(&source, &tab), None);
    assert_eq!(live_token(&dest, &tab), Some(SessionToken(base + 1)));
    assert_eq!(live_token(&dest, &workspace), Some(SessionToken(base + 1)));
    assert!(!dest.close(SessionToken(base + 1)));
    assert_eq!(live_token(&dest, &tab), None);
    assert_eq!(live_token(&dest, &workspace), None);
}

#[test]
fn navigator_restore_replaces_creation_ids_without_persisting_them() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let snapshot = set.capture_shape();
    let tab = NavigatorTarget::Tab(set.workspaces[0].tabs[0].identity);
    let workspace = NavigatorTarget::Workspace(set.workspaces[0].identity);
    let mut handed = Vec::new();
    let report = set.restore_from_snapshot_with(
        &snapshot,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(matches!(report, RestoreReport::Restored { .. }));
    assert_eq!(set.capture_shape(), snapshot);
    assert_eq!(live_token(&set, &tab), None);
    assert_eq!(live_token(&set, &workspace), None);
    assert_ne!(set.workspaces[0].identity, SessionToken(0));
}
