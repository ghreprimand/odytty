// SPDX-License-Identifier: GPL-3.0-only
//! Same-process window merge transaction tests (v0.15.0 D).
//!
//! These exercise the arena-level transfer primitive
//! (`WorkspaceSet::preflight_merge_from` / `commit_merge_from` / `merge_from`)
//! headlessly: two sibling `WorkspaceSet`s with disjoint session tokens, no
//! event loop, no PTY. They pin the atomic-transfer contract - whole workspaces
//! and their owned sessions move intact, the source empties only on a validated
//! commit, a failed preflight OR a stale/over-capacity commit leaves both sets
//! byte-for-byte unchanged, and the destination keeps its own token-allocator
//! range (imported tokens never advance it).

use super::*;
use crate::native::layout::{PaneNode, SplitAxis};
use crate::native::session::window_merge::{MAX_MERGED_WORKSPACES, MergeError};

/// Base of the source window's disjoint token range. Sibling windows own
/// non-overlapping strided ranges (`window_owner::WINDOW_TOKEN_STRIDE`); the
/// tests use a realistic disjoint base so the "destination allocator range is
/// preserved" invariant is meaningful - the destination minting from 2 can
/// never reach the source's range.
const SRC_BASE: u64 = 1 << 40;

/// A destination set with two single-pane workspaces holding tokens 0 and 1
/// (the primary window's range starts at 0). Its allocator sits at 2.
fn dest_set() -> WorkspaceSet {
    let mut set = WorkspaceSet::new(build_session_with_id(SessionToken(0)), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    set
}

/// A source set with two single-pane workspaces holding tokens in the disjoint
/// `[SRC_BASE, ...)` range, so same-process tokens are unique across windows.
fn source_set() -> WorkspaceSet {
    let mut set = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);
    set.push_workspace(build_session_with_id(SessionToken(SRC_BASE + 1)));
    set
}

#[test]
fn merge_appends_source_workspaces_and_moves_every_session() {
    let mut dest = dest_set();
    let mut source = source_set();
    let dest_active_before = dest.active_workspace_index();

    let plan = dest
        .merge_from(&mut source)
        .expect("disjoint sets merge cleanly");

    // Plan reports the transfer size.
    assert_eq!(plan.workspace_count(), 2);
    assert_eq!(plan.session_count(), 2);

    // Destination absorbed both workspaces, appended after its own.
    assert_eq!(dest.workspace_count(), 4);
    // Every token - the destination's originals and the moved ones - resolves
    // in the destination arena now.
    for token in [0, 1, SRC_BASE, SRC_BASE + 1] {
        assert!(
            dest.get(SessionToken(token)).is_some(),
            "token {token} present in destination arena after merge"
        );
    }

    // A merge appends; it never steals focus.
    assert_eq!(dest.active_workspace_index(), dest_active_before);

    // The source is fully drained; its window is now safe to tear down.
    assert!(source.workspaces.is_empty(), "source workspaces drained");
    assert!(source.sessions.is_empty(), "source sessions drained");

    // The destination keeps its OWN allocator range: imported tokens (from the
    // source's disjoint range) do not advance it. It still mints 2 next, so it
    // can never re-mint a moved session's token and never crosses into the
    // source's range.
    assert_eq!(dest.next_token, 2);
    assert!(dest.get(SessionToken(SRC_BASE)).is_some());
}

#[test]
fn merge_preserves_multi_pane_tabs_and_workspace_bindings() {
    // Source workspace 0: a single tab holding a two-pane split
    // (SRC_BASE | SRC_BASE+2), a title override, and a host binding.
    // Workspace 1: plain single pane SRC_BASE+1.
    let mut source = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);
    source.push_arena_only(build_session_with_id(SessionToken(SRC_BASE + 2)));
    {
        let ws = source.active_workspace_mut();
        ws.default_profile = Some("prod-host".to_owned());
        ws.launch_profile = Some("prod-profile".to_owned());
        let tab = &mut ws.tabs[0];
        tab.layout = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(SessionToken(SRC_BASE))),
            second: Box::new(PaneNode::Leaf(SessionToken(SRC_BASE + 2))),
        };
        tab.focused = SessionToken(SRC_BASE + 2);
        tab.title_override = Some("build".to_owned());
    }
    source.push_workspace(build_session_with_id(SessionToken(SRC_BASE + 1)));

    let mut dest = dest_set();
    dest.merge_from(&mut source).expect("merge succeeds");

    // Four workspaces: dest's 2, then the two moved ones in order.
    assert_eq!(dest.workspace_count(), 4);
    let moved = &dest.workspaces[2];
    assert_eq!(moved.default_profile.as_deref(), Some("prod-host"));
    assert_eq!(moved.launch_profile.as_deref(), Some("prod-profile"));
    let tab = &moved.tabs[0];
    assert_eq!(tab.title_override.as_deref(), Some("build"));
    assert_eq!(tab.focused, SessionToken(SRC_BASE + 2));
    assert_eq!(
        tab.layout.leaves(),
        vec![SessionToken(SRC_BASE), SessionToken(SRC_BASE + 2)]
    );

    // Both panes of the split, and the second workspace's pane, are all in the
    // destination arena.
    for token in [SRC_BASE, SRC_BASE + 1, SRC_BASE + 2] {
        assert!(dest.get(SessionToken(token)).is_some());
    }
    assert!(source.sessions.is_empty());
    // Destination allocator range preserved (still 2), unaffected by the higher
    // imported tokens.
    assert_eq!(dest.next_token, 2);
}

#[test]
fn preflight_rejects_a_token_collision_and_touches_nothing() {
    let mut dest = dest_set();
    // Source shares token 1 with the destination - a violated uniqueness
    // invariant. Merge must fail closed rather than clobber dest's session 1.
    let mut source = WorkspaceSet::new(build_session_with_id(SessionToken(1)), None);
    source.push_workspace(build_session_with_id(SessionToken(SRC_BASE + 200)));

    let err = dest.merge_from(&mut source).expect_err("collision refused");
    assert_eq!(err, MergeError::TokenCollision(SessionToken(1)));

    // Both sets are exactly as they were.
    assert_eq!(dest.workspace_count(), 2);
    assert_eq!(source.workspace_count(), 2);
    assert!(source.get(SessionToken(1)).is_some());
    assert!(source.get(SessionToken(SRC_BASE + 200)).is_some());
    assert!(dest.get(SessionToken(SRC_BASE + 200)).is_none());
}

#[test]
fn preflight_rejects_a_self_merge() {
    let set = dest_set();
    // Preflight takes two shared borrows, so a self-merge is a legal call that
    // the identity guard rejects before any mutation could alias the set.
    let err = set
        .preflight_merge_from(&set)
        .expect_err("self-merge refused");
    assert_eq!(err, MergeError::SelfMerge);
}

#[test]
fn preflight_rejects_an_empty_source() {
    let dest = dest_set();
    let mut source = source_set();
    // Drain the source to the mid-teardown shape a closing window leaves.
    source.workspaces.clear();
    source.sessions.clear();

    let err = dest
        .preflight_merge_from(&source)
        .expect_err("empty source refused");
    assert_eq!(err, MergeError::SourceEmpty);
    assert_eq!(dest.workspace_count(), 2);
}

#[test]
fn preflight_rejects_a_dangling_pane_reference() {
    let dest = dest_set();
    let mut source = source_set();
    // Corrupt the source: a tab still references token SRC_BASE+1, but its arena
    // session is gone. Refuse rather than move a dangling reference.
    source.sessions.remove(&SessionToken(SRC_BASE + 1));

    let err = dest
        .preflight_merge_from(&source)
        .expect_err("dangling reference refused");
    assert_eq!(err, MergeError::MissingSession(SessionToken(SRC_BASE + 1)));
    // Destination untouched.
    assert_eq!(dest.workspace_count(), 2);
    assert!(dest.get(SessionToken(SRC_BASE)).is_none());
}

#[test]
fn preflight_rejects_a_duplicate_source_leaf() {
    let dest = dest_set();
    // Corrupt the source tree: a single tab's split names the SAME session in
    // both panes. Moving it would insert one session and reference it twice /
    // move it twice. Refuse.
    let mut source = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);
    {
        let tab = &mut source.active_workspace_mut().tabs[0];
        tab.layout = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(SessionToken(SRC_BASE))),
            second: Box::new(PaneNode::Leaf(SessionToken(SRC_BASE))),
        };
    }

    let err = dest
        .preflight_merge_from(&source)
        .expect_err("duplicate leaf refused");
    assert_eq!(err, MergeError::DuplicateLeaf(SessionToken(SRC_BASE)));
    assert_eq!(dest.workspace_count(), 2);
}

#[test]
fn preflight_rejects_an_orphan_arena_session() {
    let dest = dest_set();
    // The source arena holds a session (SRC_BASE+9) that no pane leaf
    // references. Moving the referenced workspaces would strand it and it would
    // be lost when the source window retires. Refuse rather than drop it.
    let mut source = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);
    source.push_arena_only(build_session_with_id(SessionToken(SRC_BASE + 9)));

    let err = dest
        .preflight_merge_from(&source)
        .expect_err("orphan arena session refused");
    assert_eq!(err, MergeError::OrphanSession(SessionToken(SRC_BASE + 9)));
    assert_eq!(dest.workspace_count(), 2);
}

#[test]
fn preflight_rejects_a_merge_that_would_exceed_capacity() {
    // Destination already at the ceiling.
    let mut dest = WorkspaceSet::new(build_session_with_id(SessionToken(0)), None);
    for i in 1..MAX_MERGED_WORKSPACES {
        dest.push_workspace(build_session_with_id(SessionToken(i as u64)));
    }
    assert_eq!(dest.workspace_count(), MAX_MERGED_WORKSPACES);

    // Any non-empty source now overflows.
    let mut source = WorkspaceSet::new(build_session_with_id(SessionToken(SRC_BASE)), None);

    let err = dest
        .preflight_merge_from(&source)
        .expect_err("over-capacity refused");
    assert_eq!(
        err,
        MergeError::CapacityExceeded {
            current: MAX_MERGED_WORKSPACES,
            incoming: 1,
        }
    );
    // Nothing moved; the source keeps its workspace.
    assert!(dest.merge_from(&mut source).is_err());
    assert_eq!(source.workspace_count(), 1);
}

#[test]
fn two_phase_preflight_then_commit_matches_one_shot() {
    let mut dest = dest_set();
    let mut source = source_set();

    let plan = dest
        .preflight_merge_from(&source)
        .expect("preflight validates");
    assert_eq!(plan.workspace_count(), 2);
    // Preflight alone mutates nothing.
    assert_eq!(dest.workspace_count(), 2);
    assert_eq!(source.workspace_count(), 2);

    // Commit consumes the plan by value, revalidates, and returns the executed
    // plan.
    let committed = dest
        .commit_merge_from(&mut source, plan)
        .expect("commit succeeds");
    assert_eq!(committed.workspace_count(), 2);
    assert_eq!(dest.workspace_count(), 4);
    assert!(source.workspaces.is_empty());
    assert!(source.sessions.is_empty());
}

#[test]
fn commit_revalidates_a_stale_plan_and_fails_closed() {
    let mut dest = dest_set();
    let mut source = source_set();

    // Validate while both arenas are healthy.
    let plan = dest
        .preflight_merge_from(&source)
        .expect("preflight validates");

    // Between preflight and commit the source window concurrently closes: its
    // arena empties. The stale plan must NOT be applied blindly.
    source.workspaces.clear();
    source.sessions.clear();

    let err = dest
        .commit_merge_from(&mut source, plan)
        .expect_err("stale plan refused at commit");
    assert_eq!(err, MergeError::SourceEmpty);

    // The destination is exactly as it was - no workspaces appended, allocator
    // range untouched.
    assert_eq!(dest.workspace_count(), 2);
    assert_eq!(dest.next_token, 2);
    assert!(dest.get(SessionToken(SRC_BASE)).is_none());
}

#[test]
fn a_set_mints_a_deterministic_sequential_sequence() {
    // Within one window token allocation is deterministic and unchanged: the
    // launch session is 0 and mints continue 1, 2, 3, .... Cross-window
    // uniqueness comes from disjoint bases, not a shared per-session counter, so
    // this stays reproducible regardless of what other windows/tests did.
    let mut set = WorkspaceSet::new(build_session_with_id(SessionToken(0)), None);
    assert_eq!(set.mint_session_token(), Some(SessionToken(1)));
    assert_eq!(set.mint_session_token(), Some(SessionToken(2)));
    assert_eq!(set.mint_session_token(), Some(SessionToken(3)));
}

#[test]
fn windows_seeded_with_disjoint_bases_never_collide() {
    // The primary window allocates from 0; a sibling window is seeded at a large
    // base (its launch session token). Each mints sequentially within its own
    // range, so no interleaving of mints can produce the same token - the
    // invariant that lets a merge move a session between arenas without re-keying
    // its live pump.
    const STRIDE: u64 = 1 << 40;
    let mut primary = WorkspaceSet::new(build_session_with_id(SessionToken(0)), None);
    let mut sibling = WorkspaceSet::new(build_session_with_id(SessionToken(STRIDE)), None);
    let mut seen = std::collections::HashSet::new();
    assert!(seen.insert(0));
    assert!(seen.insert(STRIDE));
    for _ in 0..50 {
        assert!(
            seen.insert(primary.mint_session_token().expect("primary range open").0),
            "no cross-window collision"
        );
        assert!(
            seen.insert(sibling.mint_session_token().expect("sibling range open").0),
            "no cross-window collision"
        );
    }
}

#[test]
fn a_set_refuses_to_mint_past_its_disjoint_range() {
    // A set seeded near the top of its stride refuses the mint that would cross
    // into the next window's range, so it can never alias a sibling's token.
    const STRIDE: u64 = 1 << 40;
    // Seed the launch session one below the ceiling of the first sibling range.
    let mut set = WorkspaceSet::new(build_session_with_id(SessionToken(2 * STRIDE - 2)), None);
    // next_token is 2*STRIDE-1, still inside [STRIDE, 2*STRIDE): one mint left.
    assert_eq!(set.mint_session_token(), Some(SessionToken(2 * STRIDE - 1)));
    // next_token is now 2*STRIDE == the ceiling: minting would cross into the
    // next window's range, so it refuses.
    assert_eq!(set.mint_session_token(), None);
    assert_eq!(set.mint_session_token(), None);
}
