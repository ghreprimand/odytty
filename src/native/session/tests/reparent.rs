// SPDX-License-Identifier: GPL-3.0-only
//! Moving one tab or one pane between two windows' session sets.
//!
//! Headless: two `WorkspaceSet`s with disjoint token ranges, no event loop, no
//! PTY. They pin the move contract: the moved session keeps its token and its
//! terminal model; the source keeps everything else; a moved pane gets a fresh
//! tab identity from the destination range; any refusal restores the source
//! exactly; a new window's set is built around the content and can hand it
//! back.

use super::*;
use crate::native::session::reparent::{MoveError, MoveScope};

const SRC_BASE: u64 = 1 << 40;
const NEW_BASE: u64 = 2 << 40;

fn dest_set() -> WorkspaceSet {
    WorkspaceSet::new(build_session_with_id(SessionToken(0)), None)
}

/// A source window: workspace 1 has a split tab (SRC_BASE, SRC_BASE + 1) and
/// a second single-pane tab (SRC_BASE + 2); the split tab is active.
fn source_set() -> WorkspaceSet {
    let mut set = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);
    set.split_active_for_test(
        SplitAxis::Columns,
        build_session_with_id(SessionToken(SRC_BASE + 1)),
    );
    let second = build_session_with_id(SessionToken(SRC_BASE + 2));
    let token = second.id;
    set.sessions.insert(token, second);
    set.workspaces[0].tabs.push(Tab::single(token));
    set.workspaces[0].active_tab = 0;
    set.workspaces[0].tabs[0].focused = SessionToken(SRC_BASE);
    set
}

/// A comparable picture of a set's shape: per workspace, its identity and
/// active tab, and per tab its identity, leaves, focus, and zoom.
/// One tab: identity, leaves, focus, zoom.
type TabShape = (u64, Vec<u64>, u64, bool);
/// One workspace: identity, active tab, tabs.
type WorkspaceShape = (u64, usize, Vec<TabShape>);

fn shape(set: &WorkspaceSet) -> Vec<WorkspaceShape> {
    set.workspaces
        .iter()
        .map(|ws| {
            (
                ws.identity.0,
                ws.active_tab,
                ws.tabs
                    .iter()
                    .map(|tab| {
                        (
                            tab.identity.0,
                            tab.layout.leaves().iter().map(|token| token.0).collect(),
                            tab.focused.0,
                            tab.zoomed,
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn a_moved_pane_keeps_its_session_and_becomes_a_new_tab_with_a_fresh_identity() {
    let mut source = source_set();
    let mut dest = dest_set();
    // Focus the right-hand pane, which is not the pane the tab's identity was
    // seeded from.
    source.workspaces[0].tabs[0].focused = SessionToken(SRC_BASE + 1);
    let model = Arc::clone(
        &source
            .get(SessionToken(SRC_BASE + 1))
            .expect("pane")
            .terminal,
    );
    let source_tab_identity = source.workspaces[0].tabs[0].identity;

    dest.preflight_accept(&source, MoveScope::ActivePane)
        .expect("preflight");
    let content = source
        .detach_for_move(MoveScope::ActivePane)
        .expect("detach");
    dest.attach_moved(content).expect("attach");

    // The source tab keeps its other pane, un-split, with focus on it.
    let tab = &source.workspaces[0].tabs[0];
    assert_eq!(tab.layout.leaves(), vec![SessionToken(SRC_BASE)]);
    assert_eq!(tab.focused, SessionToken(SRC_BASE));
    assert!(source.get(SessionToken(SRC_BASE + 1)).is_none());
    assert_eq!(source.workspaces[0].tabs.len(), 2, "other tabs untouched");

    // The destination's new active tab holds the same session and model.
    let ws = dest.active_workspace();
    let moved_tab = &ws.tabs[ws.active_tab];
    assert_eq!(moved_tab.layout.leaves(), vec![SessionToken(SRC_BASE + 1)]);
    let moved = dest.get(SessionToken(SRC_BASE + 1)).expect("moved session");
    assert!(Arc::ptr_eq(&moved.terminal, &model), "same terminal model");
    assert_eq!(moved.id, SessionToken(SRC_BASE + 1), "token unchanged");
    assert_ne!(moved_tab.identity, source_tab_identity);
    assert_ne!(moved_tab.identity, SessionToken(SRC_BASE + 1));
    assert!(
        moved_tab.identity.0 < SRC_BASE,
        "a fresh tab identity is minted from the destination range"
    );
}

#[test]
fn a_moved_tab_keeps_its_identity_and_the_source_keeps_its_other_tabs() {
    let mut source = source_set();
    let mut dest = dest_set();
    let tab_identity = source.workspaces[0].tabs[0].identity;

    let content = source
        .detach_for_move(MoveScope::ActiveTab)
        .expect("detach");
    assert_eq!(
        content.tokens(),
        vec![SessionToken(SRC_BASE), SessionToken(SRC_BASE + 1)]
    );
    dest.attach_moved(content).expect("attach");

    assert_eq!(source.workspaces[0].tabs.len(), 1);
    assert_eq!(
        source.workspaces[0].tabs[0].layout.leaves(),
        vec![SessionToken(SRC_BASE + 2)]
    );
    let ws = dest.active_workspace();
    assert_eq!(ws.tabs.len(), 2);
    assert_eq!(ws.active_tab, 1, "the moved tab becomes active");
    assert_eq!(ws.tabs[1].identity, tab_identity);
    assert!(
        source.move_empties_window(MoveScope::ActiveTab),
        "one tab is left, so moving it would empty the window"
    );
}

#[test]
fn a_refused_attach_restores_the_source_exactly() {
    for scope in [MoveScope::ActivePane, MoveScope::ActiveTab] {
        let mut source = source_set();
        source.workspaces[0].tabs[0].zoomed = true;
        let before = shape(&source);
        // A destination that already holds a moved token refuses.
        let mut dest = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);

        assert_eq!(
            dest.preflight_accept(&source, scope),
            Err(MoveError::TokenCollision(SessionToken(SRC_BASE)))
        );
        let content = source.detach_for_move(scope).expect("detach");
        let refused = dest.attach_moved(content).expect_err("collision");
        let (err, content) = *refused;
        assert_eq!(err, MoveError::TokenCollision(SessionToken(SRC_BASE)));
        source.restore_moved(content);

        assert_eq!(shape(&source), before, "{scope:?} restored exactly");
        for token in [SRC_BASE, SRC_BASE + 1, SRC_BASE + 2] {
            assert!(source.get(SessionToken(token)).is_some());
        }
    }
}

#[test]
fn moving_the_last_tab_empties_the_source_and_a_rollback_rebuilds_its_workspace() {
    let mut source = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);
    source.workspaces[0].name = "Build".to_owned();
    let before = shape(&source);
    assert!(source.move_empties_window(MoveScope::ActiveTab));
    assert!(source.move_empties_window(MoveScope::ActivePane));

    let content = source
        .detach_for_move(MoveScope::ActivePane)
        .expect("a lone pane moves as its tab");
    assert!(source.workspaces.is_empty(), "the window is left empty");
    assert!(source.sessions.is_empty());

    source.restore_moved(content);
    assert_eq!(shape(&source), before);
    assert_eq!(source.workspaces[0].name, "Build");
}

#[test]
fn a_new_window_set_adopts_the_content_and_can_hand_it_back() {
    let mut source = source_set();
    let before = shape(&source);
    let content = source
        .detach_for_move(MoveScope::ActivePane)
        .expect("detach");
    let template = content.restore_template();

    let mut adopted = WorkspaceSet::adopting(SessionToken(NEW_BASE), content, &source, None);
    assert_eq!(adopted.workspaces.len(), 1);
    let tab = &adopted.workspaces[0].tabs[0];
    assert_eq!(tab.layout.leaves(), vec![SessionToken(SRC_BASE)]);
    let tab_identity = tab.identity;
    let workspace_identity = adopted.workspaces[0].identity;
    assert!(
        workspace_identity.0 >= NEW_BASE && tab_identity.0 >= NEW_BASE,
        "identities come from the new window's own range"
    );
    assert_ne!(workspace_identity, tab_identity);
    let minted = adopted.mint_session_token().expect("range has room");
    assert!(minted.0 >= NEW_BASE && minted.0 < NEW_BASE + (1 << 40));
    assert_ne!(minted, tab_identity);

    // The new window could not open: everything goes back.
    let content = adopted.release_adopted(template);
    source.restore_moved(content);
    assert_eq!(shape(&source), before);
}
