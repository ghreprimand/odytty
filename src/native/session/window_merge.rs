// SPDX-License-Identifier: GPL-3.0-only
//! Same-process window merge: the atomic transfer transaction that moves whole
//! workspaces (and the sessions they own) from one [`WorkspaceSet`] into
//! another (v0.15.0 D, "Merge this window into..." / "Pull window ... into this
//! one").
//!
//! ## Ownership model
//!
//! A same-process merge moves *owned* Rust values between two live
//! [`WorkspaceSet`] arenas that belong to two sibling windows in ONE process
//! (per AD-13-08 and `docs/v0.15.0-foundation.md`). It does NOT respawn a
//! shell, detach and reattach a session, replay terminal bytes, or reparent a
//! native/GPU surface. Each moved [`Session`](super::model::Session) is a plain
//! value move out of the source arena and into the destination arena, so its
//! terminal model, PTY/attach source, writer, pump-thread handle, recorder,
//! scrollback, selection, and profile/theme state travel intact and its
//! `SessionToken` never changes. Because same-process tokens are unique across
//! sibling windows, the destination arena can take them without re-keying, so
//! the live pump thread - which addresses its session purely by token through
//! the shared event loop - keeps resolving to the same session after the move.
//!
//! ## Two-phase, atomic
//!
//! The transfer is split so the destination reserves capacity and validates
//! identity BEFORE either owner is mutated:
//!
//! - [`WorkspaceSet::preflight_merge_from`] validates the source non-empty,
//!   every referenced token backed by a real session, every arena session
//!   referenced by exactly one leaf (no duplicates, no orphans), no token
//!   colliding with the destination arena, and the merged workspace count
//!   within [`MAX_MERGED_WORKSPACES`]. It mutates nothing and returns a
//!   [`MergePlan`].
//! - [`WorkspaceSet::commit_merge_from`] consumes the plan and performs the
//!   move. It REVALIDATES both arenas one more time and reserves destination
//!   capacity BEFORE it mutates anything, so a stale plan (either arena changed
//!   since preflight) or an allocation failure is refused with both arenas
//!   untouched rather than stranding sessions or appending a tab tree that names
//!   a session no longer in the arena. The consumed plan cannot be replayed.
//!
//! The caller (the window router) resolves stable window identities, rejects
//! stale/closing targets and conflicting modal operations, runs preflight, and
//! only tears the SOURCE window down after commit returns. If preflight fails,
//! both windows are left untouched. Window-level concerns (which window is
//! "stale", event-loop serialization of a concurrent close) live in the router;
//! this module owns only the arena-level transaction.

// The model-layer transaction for the v0.15.0 keyboard window merge. Its
// within-`native` surface is exercised by the headless regression suite
// (`session/tests/window_merge.rs`) and is consumed by the same-process window
// router / merge UI, which lands as a separate reviewable step. Allow the
// transaction API to exist ahead of that wiring without tripping the
// deny-warnings gate on the non-test build; the tests keep it honest meanwhile.
#![allow(dead_code)]

use std::collections::HashSet;

use super::model::{SessionToken, WorkspaceSet};

/// Upper bound on the destination workspace count AFTER a merge. A merge that
/// would push the destination past this is refused in preflight rather than
/// building an unusably large rail. Generous: real use merges a handful of
/// windows, never hundreds.
pub(in crate::native) const MAX_MERGED_WORKSPACES: usize = 256;

/// Why a merge could not be validated. Every variant is a preflight refusal, so
/// a merge that returns one of these has changed NOTHING in either set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum MergeError {
    /// The source set holds no workspaces to move. A `WorkspaceSet` normally
    /// never empties (its last workspace closing exits its window), so this
    /// guards a source window already mid-teardown.
    SourceEmpty,
    /// A source session token already exists in the destination arena. Same-
    /// process tokens are unique across sibling windows by construction, so
    /// this only trips on a violated invariant or a self-merge; it fails closed
    /// rather than overwrite a live destination session.
    TokenCollision(SessionToken),
    /// A source tab references a token with no backing session in the source
    /// arena - a corrupt source tree. Refused rather than move a dangling pane.
    MissingSession(SessionToken),
    /// The same session token appears as a leaf in more than one pane/tab of the
    /// source - a corrupt tree that would move (or alias) one session twice.
    /// Refused rather than double-move a pane.
    DuplicateLeaf(SessionToken),
    /// A session exists in the source arena but no pane leaf references it. It
    /// would be silently lost when the source window is retired after the move.
    /// Refused rather than drop a live session on the floor.
    OrphanSession(SessionToken),
    /// The merge would exceed [`MAX_MERGED_WORKSPACES`].
    CapacityExceeded { current: usize, incoming: usize },
    /// The destination could not reserve capacity for the incoming sessions or
    /// workspaces (allocation failure). Refused before any mutation, so both
    /// arenas are untouched.
    AllocationFailed,
    /// Source and destination are the same set. A self-merge is meaningless and
    /// would alias `&self`/`&mut self`; refused explicitly.
    SelfMerge,
}

/// A validated merge plan: the ordered source tokens to move and the count of
/// workspaces incoming. Produced by [`WorkspaceSet::preflight_merge_from`] and
/// consumed by [`WorkspaceSet::commit_merge_from`]; holding one is evidence the
/// transfer was validated against a specific pair of sets.
#[derive(Debug, Clone)]
pub(in crate::native) struct MergePlan {
    /// Every source session token, in workspace -> tab -> pane-tree order.
    tokens: Vec<SessionToken>,
    /// Number of whole workspaces the commit will append.
    workspace_count: usize,
}

impl MergePlan {
    /// Number of whole workspaces this plan moves.
    pub(in crate::native) fn workspace_count(&self) -> usize {
        self.workspace_count
    }

    /// Number of sessions (panes) this plan moves across every workspace.
    pub(in crate::native) fn session_count(&self) -> usize {
        self.tokens.len()
    }
}

impl WorkspaceSet {
    /// Validate moving every workspace of `source` into `self` WITHOUT mutating
    /// either set, returning the ordered tokens and workspace count that a
    /// commit would move. Shared by [`Self::preflight_merge_from`] and by
    /// [`Self::commit_merge_from`]'s pre-mutation revalidation, so both phases
    /// enforce the exact same invariants.
    fn validate_merge(&self, source: &WorkspaceSet) -> Result<MergePlan, MergeError> {
        if std::ptr::eq(self, source) {
            return Err(MergeError::SelfMerge);
        }
        if source.workspaces.is_empty() {
            return Err(MergeError::SourceEmpty);
        }

        // Walk the source tree in a stable order and validate each pane token:
        // it must appear as a leaf exactly once (no pane claims a session twice),
        // it must back a real session in the source arena, and it must not
        // already live in the destination arena. Collect the tokens once so the
        // commit never re-walks the tree.
        let mut tokens = Vec::new();
        let mut seen = HashSet::new();
        for ws in &source.workspaces {
            for tab in &ws.tabs {
                for token in tab.layout.leaves() {
                    if !seen.insert(token) {
                        return Err(MergeError::DuplicateLeaf(token));
                    }
                    if !source.sessions.contains_key(&token) {
                        return Err(MergeError::MissingSession(token));
                    }
                    if self.sessions.contains_key(&token) {
                        return Err(MergeError::TokenCollision(token));
                    }
                    tokens.push(token);
                }
            }
        }

        // Every session in the source arena must be referenced by exactly one
        // moved leaf. A session that no leaf names ("orphan") would be dropped
        // when the source window is retired after the transfer, silently losing
        // a live session; refuse rather than lose it.
        if source.sessions.len() != seen.len() {
            let orphan = source.sessions.keys().find(|token| !seen.contains(token));
            if let Some(orphan) = orphan {
                return Err(MergeError::OrphanSession(*orphan));
            }
        }

        let incoming = source.workspaces.len();
        if self.workspaces.len().saturating_add(incoming) > MAX_MERGED_WORKSPACES {
            return Err(MergeError::CapacityExceeded {
                current: self.workspaces.len(),
                incoming,
            });
        }

        Ok(MergePlan {
            tokens,
            workspace_count: incoming,
        })
    }

    /// Validate moving every workspace of `source` into `self` WITHOUT mutating
    /// either set. Returns a [`MergePlan`] on success; see [`MergeError`] for
    /// the refusal cases. It is safe to call speculatively (an interactive
    /// picker can probe feasibility), and a failure leaves both windows exactly
    /// as they were. The commit revalidates, so a plan produced here is a
    /// feasibility snapshot, not a license to mutate blindly.
    pub(in crate::native) fn preflight_merge_from(
        &self,
        source: &WorkspaceSet,
    ) -> Result<MergePlan, MergeError> {
        self.validate_merge(source)
    }

    /// Perform the validated transfer: move every session from `source`'s arena
    /// into `self`'s, then append `source`'s whole workspaces (tabs, pane trees,
    /// focus, names, and host/profile bindings intact) to `self`'s workspace
    /// list in order.
    ///
    /// The `plan` from preflight is CONSUMED, not trusted: this revalidates both
    /// arenas one final time and reserves destination capacity BEFORE it mutates
    /// anything. If either arena changed since preflight (a stale plan, a
    /// concurrent close) or the reservation fails, it returns the refusal with
    /// BOTH arenas untouched - a partial transfer that strands sessions in
    /// neither arena, or a tab tree that names a session no longer present, is
    /// impossible. On success it returns the revalidated plan. Consuming the
    /// plan by value means it cannot be replayed against a now-empty source.
    ///
    /// The destination's focused workspace is UNCHANGED: a merge appends, it
    /// does not steal focus. The destination's own token-allocator range is
    /// PRESERVED - imported tokens do not advance `next_token`. After this
    /// returns `Ok`, `source` is empty (no workspaces, none of the moved
    /// sessions); the caller tears the source window down and must not
    /// dereference `source` in the interim (an empty set violates the "always
    /// one workspace" invariant of `active()`).
    pub(in crate::native) fn commit_merge_from(
        &mut self,
        source: &mut WorkspaceSet,
        _plan: MergePlan,
    ) -> Result<MergePlan, MergeError> {
        // One-shot revalidation against the CURRENT state of both arenas. The
        // preflight plan is taken by value (so it cannot be replayed) but its
        // contents are re-derived here; if anything changed since preflight this
        // refuses before touching either arena.
        let plan = self.validate_merge(source)?;

        // Reserve destination capacity for every incoming session and workspace
        // BEFORE any mutation, so an allocation failure fails closed with both
        // arenas untouched rather than stranding sessions mid-move.
        self.sessions
            .try_reserve(plan.tokens.len())
            .map_err(|_| MergeError::AllocationFailed)?;
        self.workspaces
            .try_reserve(plan.workspace_count)
            .map_err(|_| MergeError::AllocationFailed)?;

        // Move sessions first so the destination arena holds every token before
        // its owning tab tree is appended: no interim observer can see a tab
        // whose pane session is missing from the arena. Revalidation just
        // confirmed each token is present and unique and this runs with no
        // intervening yield, so a missing token here is a broken invariant, not
        // a race - it panics loudly rather than silently dropping a pane.
        for token in &plan.tokens {
            let session = source
                .sessions
                .remove(token)
                .expect("revalidated source session present at commit");
            self.sessions.insert(*token, session);
        }

        // Append whole workspaces by moving them directly into the destination -
        // no intermediate `Vec`. `Vec::append` empties `source.workspaces` into
        // `self.workspaces` in order; each `Workspace` carries its entire state
        // (name, tabs, pane trees, active indices, host/profile bindings).
        self.workspaces.append(&mut source.workspaces);

        // NOTE: `next_token` is deliberately NOT advanced past the imported
        // tokens. Each window mints session tokens within its own disjoint
        // stride range (`window_owner::next_window_token_base`), so this window
        // can never re-mint a moved session's token, and advancing past an
        // imported (higher-range) token would push this window's allocator into
        // a sibling's range and risk colliding with that sibling's later mints.

        // `source` is now empty; its active index no longer names a workspace.
        // Reset it to a defined value so any defensive lookup before teardown
        // sees index 0 rather than a stale out-of-range index.
        source.active_ws = 0;

        Ok(plan)
    }

    /// Convenience one-shot: preflight then commit. Returns the executed
    /// [`MergePlan`] on success, or the [`MergeError`] with both sets untouched.
    /// Prefer the explicit two-phase form when the router needs to probe
    /// feasibility, cancel pending input on both endpoints, or confirm across an
    /// interactive picker before committing.
    pub(in crate::native) fn merge_from(
        &mut self,
        source: &mut WorkspaceSet,
    ) -> Result<MergePlan, MergeError> {
        let plan = self.preflight_merge_from(source)?;
        self.commit_merge_from(source, plan)
    }
}
