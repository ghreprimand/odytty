// SPDX-License-Identifier: GPL-3.0-only
//! Shape capture, restore, append, validation, fingerprinting, and rollback.
//!
//! A restore builds the replacement tree off-side, spends one aggregate attach
//! budget across the whole batch, and swaps or appends only after the complete
//! build validates. Any failure rolls back every token spawned during the
//! attempt, so a failed restore never leaves a partial or empty window.

use super::SNAPSHOT_DEADLINE;
#[cfg(test)]
use super::model::Session;
use super::model::{SessionToken, Tab, Workspace, WorkspaceSet};
#[cfg(test)]
use super::transport::HeadlessSession;
#[cfg(test)]
use crate::core::Terminal;
use crate::native::layout::{PaneNode, SplitAxis};
#[cfg(test)]
use crate::native::pty::PtyWriter;
use std::path::Path;
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Per-connection snapshot budget for one pane in a restore batch: whatever of
/// the shared `batch_deadline` remains at `now`, capped at `cap`. `None` once the
/// batch budget is spent, so the caller skips the handshake and falls through to
/// a fresh shell instead of blocking the UI for the full per-connection deadline.
///
/// The sole call site is the Unix-only reattach path, so the helper is gated to
/// Unix; the Windows lib target compiles that path out.
#[cfg(unix)]
pub(super) fn per_connection_attach_budget(
    batch_deadline: Instant,
    now: Instant,
    cap: std::time::Duration,
) -> Option<std::time::Duration> {
    let remaining = batch_deadline.saturating_duration_since(now);
    if remaining.is_zero() {
        None
    } else {
        Some(remaining.min(cap))
    }
}

/// Workspace SHAPE capture (persistence WP1, design §10). Walks the workspace /
/// tab / pane hierarchy into a serializable [`ShapeSnapshot`] that records
/// structure only — names, tab titles/order, the pane split tree + ratios, and
/// per-pane cwd — and NEVER grid content, scrollback, env, or command lines
/// (the FREEZE-HARDEN privacy invariant; command re-execution is an explicit
/// non-goal, sub-ODP 8i). `allow(dead_code)` mirrors the `layout.rs` /
/// pane-ops scaffold: WP2 wires the autosave/restore call sites that consume
/// The outcome of a launch-time workspace restore (WP2). Advisory only: the
/// caller turns a stale-cwd count into a single compact notice (sub-ODP 8f) and
/// otherwise proceeds silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum RestoreReport {
    /// The saved shape was rebuilt. `stale_cwd` is how many panes fell back to
    /// home because their captured directory no longer exists.
    Restored {
        workspaces: usize,
        panes: usize,
        stale_cwd: usize,
        /// How many panes reattached to a still-alive detached session-host
        /// (WP3 / 8h). Drives the "N of M sessions reattached" notice.
        reattached: usize,
        /// How many panes CARRIED a session-host id to try (the "M"); a dead id
        /// spawned a fresh shell and is counted here but not in `reattached`.
        reattach_attempted: usize,
        /// How many panes recorded a remote host that could not be resolved on
        /// restore (RESTORE-REMOTE) — neither a currently-saved profile nor a
        /// parseable `[user@]host[:port]` destination — and so opened as a local
        /// shell instead. Drives the "N opened locally" line in the notice.
        remote_fallback: usize,
    },
    /// Nothing restorable (empty snapshot) or a spawn failed mid-rebuild; the
    /// launch layout was left untouched.
    Skipped,
}

/// Scratch accumulator for a snapshot rebuild (WP3): the assembled workspaces,
/// the sessions spawned/reattached (so a failed build can reap them), and the
/// running counts the caller reports. Shared by replace-mode restore and
/// append-mode layout instantiation.
#[derive(Default)]
struct SnapshotBuild {
    workspaces: Vec<Workspace>,
    spawned: Vec<SessionToken>,
    stale_cwd: usize,
    reattached: usize,
    reattach_attempted: usize,
    remote_fallback: usize,
    aborted: bool,
    /// Wall-clock deadline for the ENTIRE reattach batch. The first slow host may
    /// consume it; remaining panes then fast-fail to fresh shells rather than
    /// each blocking the UI for the full per-connection snapshot deadline.
    attach_deadline: Option<Instant>,
}

/// Hash the STRUCTURE of a pane subtree (split axis + ratio bits + shape), with
/// no session/cwd identity, for [`WorkspaceSet::structural_fingerprint`].
fn hash_pane_shape(node: &PaneNode, hasher: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match node {
        PaneNode::Leaf(_) => 0u8.hash(hasher),
        PaneNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            1u8.hash(hasher);
            (matches!(axis, SplitAxis::Rows) as u8).hash(hasher);
            ratio.to_bits().hash(hasher);
            hash_pane_shape(first, hasher);
            hash_pane_shape(second, hasher);
        }
    }
}

/// this. WP2 has since wired the autosave / restore call sites, so these are
/// live.
impl WorkspaceSet {
    /// Capture the current window shape as a serializable snapshot.
    pub(in crate::native) fn capture_shape(&self) -> crate::native::persistence::ShapeSnapshot {
        use crate::native::persistence::{
            SNAPSHOT_VERSION, ShapeSnapshot, TabShape, WorkspaceShape,
        };
        let workspaces = self
            .workspaces
            .iter()
            .map(|workspace| WorkspaceShape {
                name: workspace.name.clone(),
                default_profile: workspace.default_profile.clone(),
                active_tab: workspace.active_tab,
                tabs: workspace
                    .tabs
                    .iter()
                    .map(|tab| {
                        let leaves = tab.layout.leaves();
                        let focused_leaf = leaves
                            .iter()
                            .position(|token| *token == tab.focused)
                            .unwrap_or(0);
                        TabShape {
                            title: tab.title_override.clone(),
                            focused_leaf,
                            layout: self.capture_pane(&tab.layout),
                        }
                    })
                    .collect(),
            })
            .collect();
        ShapeSnapshot {
            version: SNAPSHOT_VERSION,
            active_workspace: self.active_ws,
            workspaces,
        }
    }

    /// Recursively mirror a live pane tree into a [`PaneShape`], capturing each
    /// leaf's cwd in place of its (ephemeral) session token.
    fn capture_pane(&self, node: &PaneNode) -> crate::native::persistence::PaneShape {
        use crate::native::persistence::PaneShape;
        match node {
            PaneNode::Leaf(token) => PaneShape::Leaf {
                cwd: self.pane_cwd(*token),
                session_host_id: self.pane_session_host_id(*token),
                remote_host: self.pane_remote_destination(*token),
            },
            PaneNode::Split {
                axis,
                ratio,
                first,
                second,
            } => PaneShape::Split {
                axis: (*axis).into(),
                ratio: *ratio,
                first: Box::new(self.capture_pane(first)),
                second: Box::new(self.capture_pane(second)),
            },
        }
    }

    /// The advisory cwd of the pane backed by `token` (OSC 7, or the spawn
    /// seed), or `None` when unknown — restore lands that pane at the home
    /// directory (design §10.5 degrade path). Never touches the filesystem.
    fn pane_cwd(&self, token: SessionToken) -> Option<String> {
        let session = self.sessions.get(&token)?;
        let terminal = session.terminal.lock().ok()?;
        terminal.current_working_directory().map(str::to_owned)
    }

    /// The detached session-host id the pane backed by `token` is attached to
    /// (WP3 / 8h), or `None` for a locally-spawned pane. On Windows this is
    /// always `None` — the detached-session transport is Unix-only — so no ids
    /// are ever captured there (the design's Windows all-fresh guarantee holds
    /// by construction).
    fn pane_session_host_id(&self, token: SessionToken) -> Option<String> {
        self.sessions
            .get(&token)
            .and_then(|session| session.attached_session_id.clone())
    }

    /// The remote destination the pane backed by `token` is connected to
    /// (RESTORE-REMOTE), or `None` for a local pane. Captured into the shape so
    /// restore respawns the pane through the `ssh` connect path rather than a
    /// local shell. Local panes leave it `None`, so their capture is unchanged.
    fn pane_remote_destination(&self, token: SessionToken) -> Option<String> {
        self.sessions
            .get(&token)
            .and_then(|session| session.remote_destination.clone())
    }

    /// Rebuild the ENTIRE workspace list from a saved shape (design §10.6, WP2).
    /// Every local pane spawns a fresh interactive shell at its captured cwd;
    /// every remote pane reconnects through `spawn_remote` (RESTORE-REMOTE),
    /// supplied by the App, which owns settings and the saved-host list — or
    /// falls back to a local shell when the host is unresolvable. The
    /// pre-existing launch session(s) are reaped once the restored tree is in
    /// place, so the window shows exactly the saved shape. A local pane that
    /// cannot spawn even at home aborts the whole restore
    /// ([`RestoreReport::Skipped`], sub-ODP 8f: never a broken/empty window).
    pub(in crate::native) fn restore_from_snapshot_remote(
        &mut self,
        snapshot: &crate::native::persistence::ShapeSnapshot,
        grid: crate::core::Dimensions,
        home: Option<&Path>,
        spawn_remote: impl FnMut(&mut Self, &str) -> Option<SessionToken>,
    ) -> RestoreReport {
        self.restore_from_snapshot_with(
            snapshot,
            home,
            |set, cwd| set.insert_restored_session(grid, cwd).ok(),
            spawn_remote,
        )
    }

    /// Shape-rebuild core, generic over how a leaf is spawned so tests can drive
    /// the full capture -> serialize -> load -> restore round trip headlessly
    /// (the production spawner needs a live event-loop proxy). `spawn_leaf`
    /// spawns a session at the resolved cwd and returns its token, or `None` on
    /// failure (which aborts the whole restore).
    pub(in crate::native) fn restore_from_snapshot_with(
        &mut self,
        snapshot: &crate::native::persistence::ShapeSnapshot,
        home: Option<&Path>,
        spawn_leaf: impl FnMut(&mut Self, Option<std::path::PathBuf>) -> Option<SessionToken>,
        spawn_remote: impl FnMut(&mut Self, &str) -> Option<SessionToken>,
    ) -> RestoreReport {
        let build = self.build_from_snapshot(snapshot, home, spawn_leaf, spawn_remote);
        if build.aborted || build.workspaces.is_empty() {
            for token in build.spawned {
                self.discard_session(token);
            }
            return RestoreReport::Skipped;
        }

        // Everything spawned; swap the restored tree in and reap the launch
        // session(s) that are not part of it (typically just the initial pane).
        let discard: Vec<SessionToken> = self
            .sessions
            .keys()
            .copied()
            .filter(|token| !build.spawned.contains(token))
            .collect();
        let active_ws = snapshot.active_workspace.min(build.workspaces.len() - 1);
        let panes = build.spawned.len();
        let workspaces = build.workspaces.len();
        self.workspaces = build.workspaces;
        self.active_ws = active_ws;
        for token in discard {
            self.discard_session(token);
        }

        RestoreReport::Restored {
            workspaces,
            panes,
            stale_cwd: build.stale_cwd,
            reattached: build.reattached,
            reattach_attempted: build.reattach_attempted,
            remote_fallback: build.remote_fallback,
        }
    }

    /// WP3 / 8e: instantiate a saved layout by APPENDING its workspace(s) after
    /// the current list and switching to the first one — never clobbering the
    /// live layout (PRISTINE-CONSUME placement). Remote panes reconnect through
    /// `spawn_remote` (RESTORE-REMOTE); the append-mode counterpart to
    /// [`Self::restore_from_snapshot_remote`]. On a spawn failure mid-build
    /// everything spawned so far is reaped and the current workspaces are
    /// untouched ([`RestoreReport::Skipped`]).
    pub(in crate::native) fn append_from_snapshot_remote(
        &mut self,
        snapshot: &crate::native::persistence::ShapeSnapshot,
        grid: crate::core::Dimensions,
        home: Option<&Path>,
        spawn_remote: impl FnMut(&mut Self, &str) -> Option<SessionToken>,
    ) -> RestoreReport {
        self.append_from_snapshot_with(
            snapshot,
            home,
            |set, cwd| set.insert_restored_session(grid, cwd).ok(),
            spawn_remote,
        )
    }

    /// Append-mode rebuild core, generic over the leaf spawner (headless tests).
    pub(in crate::native) fn append_from_snapshot_with(
        &mut self,
        snapshot: &crate::native::persistence::ShapeSnapshot,
        home: Option<&Path>,
        spawn_leaf: impl FnMut(&mut Self, Option<std::path::PathBuf>) -> Option<SessionToken>,
        spawn_remote: impl FnMut(&mut Self, &str) -> Option<SessionToken>,
    ) -> RestoreReport {
        let build = self.build_from_snapshot(snapshot, home, spawn_leaf, spawn_remote);
        if build.aborted || build.workspaces.is_empty() {
            for token in build.spawned {
                self.discard_session(token);
            }
            return RestoreReport::Skipped;
        }
        let panes = build.spawned.len();
        let workspaces = build.workspaces.len();
        // PRISTINE-CONSUME: opening a layout onto a bare launch (exactly one
        // untouched default workspace) should yield precisely the saved set, so
        // the built workspaces REPLACE the pristine one instead of appending
        // beside it. Its lone session is reaped first so the arena never leaks.
        // Any real state appends as before, never clobbering (8e).
        if self.is_single_pristine_workspace() {
            let stale = std::mem::replace(&mut self.workspaces, build.workspaces);
            self.discard_session(stale[0].tabs[0].focused);
            self.active_ws = 0;
        } else {
            let first_appended = self.workspaces.len();
            self.workspaces.extend(build.workspaces);
            self.active_ws = first_appended;
        }
        RestoreReport::Restored {
            workspaces,
            panes,
            stale_cwd: build.stale_cwd,
            reattached: build.reattached,
            reattach_attempted: build.reattach_attempted,
            remote_fallback: build.remote_fallback,
        }
    }

    /// Test seam (RESTORE-THEME): append a snapshot through the production
    /// append core ([`Self::append_from_snapshot_with`]) with a HEADLESS leaf
    /// spawner. The real leaf spawner ([`Self::insert_restored_session`])
    /// requires an event-loop proxy to wire each session's reader thread, so it
    /// cannot run without a real winit `EventLoop`; this seam inserts a
    /// proxy-less test session per leaf (exactly as the module's `fake_spawner`
    /// does) so a headless test EXERCISES the append-and-seed path instead of
    /// skipping the proxy-backed variant. Returns the same [`RestoreReport`] the
    /// production path does, so replace-vs-append and pristine-consume behave
    /// identically.
    #[cfg(test)]
    pub(in crate::native) fn append_from_snapshot_headless_for_test(
        &mut self,
        snapshot: &crate::native::persistence::ShapeSnapshot,
        home: Option<&Path>,
    ) -> RestoreReport {
        self.append_from_snapshot_with(
            snapshot,
            home,
            |set, _cwd| {
                let dims = crate::core::Dimensions::new(20, 8);
                let writer: PtyWriter = crate::native::test_support::headless_writer();
                let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
                let headless = Arc::new(HeadlessSession::new(dims));
                let token = SessionToken(set.next_token);
                set.next_token = set.next_token.saturating_add(1);
                set.sessions.insert(
                    token,
                    Session::new_headless(token, terminal, writer, headless),
                );
                Some(token)
            },
            |_, _| None,
        )
    }

    /// Build the workspaces from a snapshot without deciding replace-vs-append:
    /// spawns (or 8h-reattaches) a session per leaf and assembles the tab trees,
    /// tracking the sessions spawned so a failed build can be reaped cleanly.
    /// The caller places `workspaces` (replace or append) and reaps `spawned` on
    /// `aborted`.
    fn build_from_snapshot(
        &mut self,
        snapshot: &crate::native::persistence::ShapeSnapshot,
        home: Option<&Path>,
        mut spawn_leaf: impl FnMut(&mut Self, Option<std::path::PathBuf>) -> Option<SessionToken>,
        mut spawn_remote: impl FnMut(&mut Self, &str) -> Option<SessionToken>,
    ) -> SnapshotBuild {
        // Aggregate budget for the entire reattach batch: the first slow host can
        // consume it, after which remaining panes fast-fail to fresh shells rather
        // than each blocking startup for the full per-connection deadline.
        let mut build = SnapshotBuild {
            attach_deadline: Some(Instant::now() + SNAPSHOT_DEADLINE),
            ..SnapshotBuild::default()
        };

        'workspaces: for ws in &snapshot.workspaces {
            if ws.tabs.is_empty() {
                continue;
            }
            let mut tabs: Vec<Tab> = Vec::new();
            for tab_shape in &ws.tabs {
                let mut leaves: Vec<SessionToken> = Vec::new();
                let Some(layout) = self.rebuild_pane(
                    &tab_shape.layout,
                    home,
                    &mut spawn_leaf,
                    &mut spawn_remote,
                    &mut build,
                    &mut leaves,
                ) else {
                    build.aborted = true;
                    break 'workspaces;
                };
                let focused = leaves
                    .get(tab_shape.focused_leaf)
                    .copied()
                    .or_else(|| leaves.first().copied())
                    .expect("a rebuilt pane tree always has at least one leaf");
                tabs.push(Tab {
                    layout,
                    focused,
                    title_override: tab_shape.title.clone(),
                    zoomed: false,
                    activity: false,
                });
            }
            if tabs.is_empty() {
                continue;
            }
            let active_tab = ws.active_tab.min(tabs.len() - 1);
            build.workspaces.push(Workspace {
                name: ws.name.clone(),
                tabs,
                active_tab,
                default_profile: ws.default_profile.clone(),
            });
        }
        build
    }

    /// Rebuild one pane subtree, spawning a leaf session per [`PaneShape::Leaf`]
    /// at its resolved cwd and recording each token in `leaves` (tree order, so
    /// the caller can map `focused_leaf`). Returns `None` if a leaf spawn fails.
    fn rebuild_pane(
        &mut self,
        shape: &crate::native::persistence::PaneShape,
        home: Option<&Path>,
        spawn_leaf: &mut impl FnMut(&mut Self, Option<std::path::PathBuf>) -> Option<SessionToken>,
        spawn_remote: &mut impl FnMut(&mut Self, &str) -> Option<SessionToken>,
        build: &mut SnapshotBuild,
        leaves: &mut Vec<SessionToken>,
    ) -> Option<PaneNode> {
        use crate::native::persistence::{PaneShape, resolve_cwd};
        match shape {
            PaneShape::Leaf {
                cwd,
                session_host_id,
                remote_host,
            } => {
                // 8h: a pane that was attached to a detached session-host tries to
                // reattach first. A live host reattaches (full scrollback); a dead
                // id, an already-reattached id, or any non-Unix build falls through
                // to a fresh shell at the captured cwd — silently, per the design.
                if let Some(id) = session_host_id.as_deref() {
                    build.reattach_attempted += 1;
                    let attach_batch_deadline = build.attach_deadline.unwrap_or_else(Instant::now);
                    if let Some(token) = self.reattach_restored_session(id, attach_batch_deadline) {
                        build.reattached += 1;
                        build.spawned.push(token);
                        leaves.push(token);
                        return Some(PaneNode::leaf(token));
                    }
                }
                // RESTORE-REMOTE: a pane captured from an `ssh` connection
                // respawns through the connect path — a fresh remote login shell,
                // never a re-run of any captured command (8i). An unresolvable
                // host (no saved profile and not a parseable destination) yields
                // `None` and falls through to a local shell, counted for the
                // notice. The remote shell lands at its own default directory; the
                // captured (remote) cwd is not chdir'd locally in v1.
                if let Some(host) = remote_host.as_deref() {
                    if let Some(token) = spawn_remote(self, host) {
                        build.spawned.push(token);
                        leaves.push(token);
                        return Some(PaneNode::leaf(token));
                    }
                    build.remote_fallback += 1;
                }
                let resolved = resolve_cwd(cwd.as_deref(), home);
                if resolved.stale {
                    build.stale_cwd += 1;
                }
                // A captured directory that still exists but denies the spawn
                // (EACCES on a mode-000 dir, or a remote cwd like `/root` that
                // exists locally but refuses `chdir`) must not abort the whole
                // restore. Retry once at home before giving up (counted stale);
                // abort only if home also fails or there is no home to try.
                let token = match spawn_leaf(self, resolved.path.clone()) {
                    Some(token) => token,
                    None => {
                        let home_path = home.map(Path::to_path_buf);
                        if resolved.path == home_path {
                            return None;
                        }
                        let token = spawn_leaf(self, home_path)?;
                        if !resolved.stale {
                            build.stale_cwd += 1;
                        }
                        token
                    }
                };
                build.spawned.push(token);
                leaves.push(token);
                Some(PaneNode::leaf(token))
            }
            PaneShape::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let first =
                    self.rebuild_pane(first, home, spawn_leaf, spawn_remote, build, leaves)?;
                let second =
                    self.rebuild_pane(second, home, spawn_leaf, spawn_remote, build, leaves)?;
                Some(PaneNode::Split {
                    axis: axis.to_split_axis(),
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                })
            }
        }
    }

    /// Remove a session from the arena and reap its shell + pump thread. Used by
    /// restore to drop the launch session(s) once the saved shape is in place.
    fn discard_session(&mut self, token: SessionToken) {
        if let Some(session) = self.sessions.remove(&token) {
            session.close();
        }
    }

    /// A cheap, lock-free hash of the workspace/tab/pane STRUCTURE — names, tab
    /// titles/order/count, split axes + ratios, focused-pane position, and the
    /// active workspace/tab indices. Deliberately excludes per-pane cwd so it
    /// never locks a terminal and never churns on an OSC 7 cwd update; the
    /// debounced autosave uses it to detect shape mutations without capturing
    /// the full snapshot every maintenance pass (WP2 sub-ODP 8c).
    pub(in crate::native) fn structural_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.active_ws.hash(&mut hasher);
        self.workspaces.len().hash(&mut hasher);
        for ws in &self.workspaces {
            ws.name.hash(&mut hasher);
            ws.active_tab.hash(&mut hasher);
            ws.tabs.len().hash(&mut hasher);
            for tab in &ws.tabs {
                tab.title_override.hash(&mut hasher);
                let leaves = tab.layout.leaves();
                leaves
                    .iter()
                    .position(|token| *token == tab.focused)
                    .unwrap_or(0)
                    .hash(&mut hasher);
                hash_pane_shape(&tab.layout, &mut hasher);
            }
        }
        hasher.finish()
    }
}
