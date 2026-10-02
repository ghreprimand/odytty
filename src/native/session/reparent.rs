// SPDX-License-Identifier: GPL-3.0-only
//! Moving one tab, or one pane, from a window to another window of the same
//! process (the keyboard and menu "Move Tab/Pane to Window" actions).
//!
//! Like the window merge ([`super::window_merge`]) this moves owned values
//! between two [`WorkspaceSet`] arenas: each moved [`Session`] keeps its token,
//! PTY or attach source, writer, pump, terminal model, scrollback, images,
//! selection, search, recorder, and profile binding. Nothing is respawned,
//! detached, or replayed, and the pump keeps routing by token to whichever
//! window owns the session now. Unlike a merge, the source window usually
//! survives, so the transaction removes exactly one tab or one pane leaf and
//! leaves the rest of the source intact.
//!
//! ## Shape of the transaction
//!
//! 1. [`WorkspaceSet::preflight_accept`] on the destination checks token
//!    collisions and the destination's capacity without mutating either set.
//! 2. [`WorkspaceSet::detach_for_move`] removes the tab (or the pane leaf,
//!    collapsing its split parent) from the source and returns a
//!    [`MovedContent`] holding the values plus an exact restore point.
//! 3. [`WorkspaceSet::attach_moved`] inserts it into the destination as the
//!    destination's active tab, or hands the content back on failure. A moved
//!    pane instead joins the destination's active tab when that tab is already
//!    floating, and arrives as a floating pane in front.
//! 4. On any failure after step 2, [`WorkspaceSet::restore_moved`] puts the
//!    content back exactly where it was (tab index, pane tree, focus, zoom),
//!    so a refused move changes nothing.
//!
//! A new window is built with [`WorkspaceSet::adopting`] around the moved
//! content, with no shell spawn; [`WorkspaceSet::release_adopted`] takes it
//! back out when the new window's surface cannot be created.
//!
//! Identities: a moved tab keeps its tab identity. A moved pane becomes the
//! only pane of a new tab, whose identity is minted from the destination's own
//! token range (never seeded from the pane token, which may equal the source
//! tab's identity). A new window's workspace identity is minted the same way.

use std::collections::HashMap;

use super::model::{Session, SessionToken, Tab, Workspace, WorkspaceSet, default_workspace_name};
use crate::native::layout::{EVEN_RATIO, PaneNode, SplitAxis};
use crate::native::pty::UserEvent;
use winit::event_loop::EventLoopProxy;

/// What a move takes from the source window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum MoveScope {
    /// The active tab of the active workspace, with every pane it holds.
    ActiveTab,
    /// The focused pane of the active tab. It becomes the only pane of a new
    /// tab in the destination. A pane that is its tab's only pane moves as that
    /// tab (same result, and the tab keeps its identity).
    ActivePane,
}

/// Why a move was refused. Every refusal leaves both windows unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum MoveError {
    /// The source holds nothing to move (a window mid-teardown).
    SourceEmpty,
    /// A moved session token already lives in the destination arena. Tokens
    /// are unique across windows by construction, so this only trips on a
    /// violated invariant; it fails closed.
    TokenCollision(SessionToken),
    /// The moved tree names a token with no backing session.
    MissingSession(SessionToken),
    /// The destination would exceed its tab or token capacity.
    CapacityExceeded,
    /// The destination could not reserve memory for the incoming values.
    AllocationFailed,
}

/// Where moved content came from, for an exact rollback.
#[derive(Debug, Clone)]
enum RestorePoint {
    /// A whole tab left its workspace. `workspace` is set when that emptied
    /// the workspace, which was removed (it is reinserted with the tab).
    Tab {
        ws_idx: usize,
        tab_idx: usize,
        active_tab: usize,
        active_ws: usize,
        workspace: Option<WorkspaceShell>,
    },
    /// One pane left a multi-pane tab. The tab's tree, focus, and zoom are
    /// kept so a rollback restores them exactly.
    Pane {
        ws_idx: usize,
        tab_idx: usize,
        layout: PaneNode,
        focused: SessionToken,
        zoomed: bool,
    },
}

/// A removed workspace without its tabs.
#[derive(Debug, Clone)]
struct WorkspaceShell {
    identity: SessionToken,
    name: String,
    default_profile: Option<String>,
    launch_profile: Option<String>,
}

/// Moved values and their restore point. Holding one means the source has
/// already given the content up: it must reach a destination or go back
/// through [`WorkspaceSet::restore_moved`].
pub(in crate::native) struct MovedContent {
    tab: Tab,
    sessions: Vec<Session>,
    /// The tab is new (a pane move) and needs an identity minted by its
    /// destination.
    fresh_tab: bool,
    restore: RestorePoint,
}

impl std::fmt::Debug for MovedContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MovedContent")
            .field("tokens", &self.tokens())
            .field("fresh_tab", &self.fresh_tab)
            .finish_non_exhaustive()
    }
}

impl MovedContent {
    /// Every moved session token, in pane-tree order.
    pub(in crate::native) fn tokens(&self) -> Vec<SessionToken> {
        self.tab.layout.leaves()
    }
}

impl WorkspaceSet {
    /// The tokens `scope` would move out of this set, or why it cannot.
    fn tokens_for_move(&self, scope: MoveScope) -> Result<Vec<SessionToken>, MoveError> {
        if self.workspaces.is_empty() {
            return Err(MoveError::SourceEmpty);
        }
        let ws = self.active_workspace();
        let Some(tab) = ws.tabs.get(ws.active_tab) else {
            return Err(MoveError::SourceEmpty);
        };
        let tokens = match scope {
            MoveScope::ActiveTab => tab.layout.leaves(),
            MoveScope::ActivePane => vec![tab.focused],
        };
        for token in &tokens {
            if !self.sessions.contains_key(token) {
                return Err(MoveError::MissingSession(*token));
            }
        }
        Ok(tokens)
    }

    /// Whether `scope` would take this set's last tab of its last workspace,
    /// leaving the window empty.
    pub(in crate::native) fn move_empties_window(&self, scope: MoveScope) -> bool {
        let single_tab = self.workspaces.len() == 1 && self.active_workspace().tabs.len() == 1;
        match scope {
            MoveScope::ActiveTab => single_tab,
            MoveScope::ActivePane => {
                single_tab
                    && self
                        .active_workspace()
                        .tabs
                        .first()
                        .is_some_and(|tab| tab.layout.is_single_pane())
            }
        }
    }

    /// Check, without mutating either set, that this set can take what
    /// `scope` moves out of `source`.
    pub(in crate::native) fn preflight_accept(
        &self,
        source: &WorkspaceSet,
        scope: MoveScope,
    ) -> Result<(), MoveError> {
        let tokens = source.tokens_for_move(scope)?;
        if let Some(token) = tokens
            .iter()
            .find(|token| self.sessions.contains_key(token))
        {
            return Err(MoveError::TokenCollision(*token));
        }
        if self.workspaces.is_empty() {
            return Err(MoveError::SourceEmpty);
        }
        // A pane move into an existing window needs one fresh tab identity.
        if self.next_token >= self.token_ceiling {
            return Err(MoveError::CapacityExceeded);
        }
        Ok(())
    }

    /// Remove what `scope` names from this set and return it with an exact
    /// restore point. The source keeps every other tab, pane, and workspace;
    /// a pane's split parent collapses into its sibling and the tab un-zooms,
    /// exactly as when a pane closes. When the last tab of the last workspace
    /// leaves, the set is left with no workspaces and its window is retired
    /// by the caller.
    pub(in crate::native) fn detach_for_move(
        &mut self,
        scope: MoveScope,
    ) -> Result<MovedContent, MoveError> {
        let tokens = self.tokens_for_move(scope)?;
        let ws_idx = self.active_ws;
        let tab_idx = self.workspaces[ws_idx].active_tab;
        let single_pane = self.workspaces[ws_idx].tabs[tab_idx]
            .layout
            .is_single_pane();
        if scope == MoveScope::ActivePane && !single_pane {
            let token = tokens[0];
            let tab = &mut self.workspaces[ws_idx].tabs[tab_idx];
            let restore = RestorePoint::Pane {
                ws_idx,
                tab_idx,
                layout: tab.layout.clone(),
                focused: tab.focused,
                zoomed: tab.zoomed,
            };
            let Some(layout) = tab.layout.clone().close_leaf(token) else {
                return Err(MoveError::SourceEmpty);
            };
            if let Some(first) = layout.leaves().first().copied() {
                tab.focused = first;
            }
            tab.layout = layout;
            tab.zoomed = false;
            let session = self
                .sessions
                .remove(&token)
                .expect("move token checked present");
            return Ok(MovedContent {
                tab: Tab::single(token),
                sessions: vec![session],
                fresh_tab: true,
                restore,
            });
        }

        // A whole tab leaves (the active tab, or the tab of a lone pane).
        let active_ws = self.active_ws;
        let ws = &mut self.workspaces[ws_idx];
        let active_tab = ws.active_tab;
        let tab = ws.tabs.remove(tab_idx);
        let workspace = if ws.tabs.is_empty() {
            let removed = self.workspaces.remove(ws_idx);
            if self.active_ws >= self.workspaces.len() {
                self.active_ws = self.workspaces.len().saturating_sub(1);
            }
            Some(WorkspaceShell {
                identity: removed.identity,
                name: removed.name,
                default_profile: removed.default_profile,
                launch_profile: removed.launch_profile,
            })
        } else {
            ws.active_tab = tab_idx.min(ws.tabs.len() - 1);
            None
        };
        let sessions = tab
            .layout
            .leaves()
            .into_iter()
            .map(|token| {
                self.sessions
                    .remove(&token)
                    .expect("move token checked present")
            })
            .collect();
        Ok(MovedContent {
            tab,
            sessions,
            fresh_tab: false,
            restore: RestorePoint::Tab {
                ws_idx,
                tab_idx,
                active_tab,
                active_ws,
                workspace,
            },
        })
    }

    /// Put moved content back exactly where [`Self::detach_for_move`] took it
    /// from. Used when the destination refuses the content or a new window
    /// cannot be created.
    pub(in crate::native) fn restore_moved(&mut self, content: MovedContent) {
        let MovedContent {
            tab,
            sessions,
            restore,
            ..
        } = content;
        for session in sessions {
            self.sessions.insert(session.id, session);
        }
        match restore {
            RestorePoint::Pane {
                ws_idx,
                tab_idx,
                layout,
                focused,
                zoomed,
            } => {
                let tab = &mut self.workspaces[ws_idx].tabs[tab_idx];
                tab.layout = layout;
                tab.focused = focused;
                tab.zoomed = zoomed;
            }
            RestorePoint::Tab {
                ws_idx,
                tab_idx,
                active_tab,
                active_ws,
                workspace,
            } => {
                match workspace {
                    Some(shell) => {
                        let at = ws_idx.min(self.workspaces.len());
                        self.workspaces.insert(
                            at,
                            Workspace {
                                identity: shell.identity,
                                name: shell.name,
                                tabs: vec![tab],
                                active_tab: 0,
                                default_profile: shell.default_profile,
                                launch_profile: shell.launch_profile,
                            },
                        );
                    }
                    None => {
                        let ws = &mut self.workspaces[ws_idx];
                        let at = tab_idx.min(ws.tabs.len());
                        ws.tabs.insert(at, tab);
                        ws.active_tab = active_tab.min(ws.tabs.len() - 1);
                    }
                }
                self.active_ws = active_ws.min(self.workspaces.len().saturating_sub(1));
            }
        }
    }

    /// Insert moved content as a new tab at the end of this set's active
    /// workspace and make it the active tab. On failure the content comes back
    /// untouched for [`Self::restore_moved`] on the source.
    pub(in crate::native) fn attach_moved(
        &mut self,
        mut content: MovedContent,
    ) -> Result<(), Box<(MoveError, MovedContent)>> {
        if let Some(token) = content
            .sessions
            .iter()
            .map(|session| session.id)
            .find(|token| self.sessions.contains_key(token))
        {
            return Err(Box::new((MoveError::TokenCollision(token), content)));
        }
        if self.workspaces.is_empty() {
            return Err(Box::new((MoveError::SourceEmpty, content)));
        }
        if self.sessions.try_reserve(content.sessions.len()).is_err() {
            return Err(Box::new((MoveError::AllocationFailed, content)));
        }
        let ws_idx = self.active_ws;
        if self.workspaces[ws_idx].tabs.try_reserve(1).is_err() {
            return Err(Box::new((MoveError::AllocationFailed, content)));
        }
        // A moved pane joins the destination's active tab, as a floating pane,
        // only when that tab is already floating; otherwise it becomes a new
        // tab. A whole moved tab always arrives as a tab.
        let joins_floating = content.fresh_tab
            && self.workspaces[ws_idx]
                .tabs
                .get(self.workspaces[ws_idx].active_tab)
                .is_some_and(Tab::is_floating);
        if content.fresh_tab && !joins_floating {
            let Some(identity) = self.mint_session_token() else {
                return Err(Box::new((MoveError::CapacityExceeded, content)));
            };
            content.tab.identity = identity;
        }
        for session in std::mem::take(&mut content.sessions) {
            self.sessions.insert(session.id, session);
        }
        let ws = &mut self.workspaces[ws_idx];
        if joins_floating {
            let moved = content.tab.focused;
            let active = ws.active_tab;
            let dest = &mut ws.tabs[active];
            let anchor = dest.focused;
            let layout = std::mem::replace(&mut dest.layout, PaneNode::leaf(moved));
            dest.layout = layout.split_leaf(anchor, SplitAxis::Columns, EVEN_RATIO, moved);
            dest.focused = moved;
            dest.zoomed = false;
            dest.raise_focused();
            return Ok(());
        }
        ws.tabs.push(content.tab);
        ws.active_tab = ws.tabs.len() - 1;
        Ok(())
    }

    /// A new window's set built around moved content, with no shell spawn.
    /// `base` is the new window's first token (its disjoint range starts
    /// there); the workspace identity, and a moved pane's tab identity, are
    /// minted from that range. Recording, hostname, and shell-integration
    /// state follow the source set.
    pub(in crate::native) fn adopting(
        base: SessionToken,
        mut content: MovedContent,
        source: &WorkspaceSet,
        proxy: Option<EventLoopProxy<UserEvent>>,
    ) -> Self {
        let stride = crate::native::window_owner::WINDOW_TOKEN_STRIDE;
        let token_ceiling = (base.0 / stride).saturating_add(1).saturating_mul(stride);
        let mut next_token = base.0;
        let mut mint = || {
            let token = SessionToken(next_token);
            next_token = next_token.saturating_add(1);
            token
        };
        let workspace_identity = mint();
        if content.fresh_tab {
            content.tab.identity = mint();
        }
        let mut sessions = HashMap::new();
        for session in std::mem::take(&mut content.sessions) {
            sessions.insert(session.id, session);
        }
        let mut workspace = Workspace::single(default_workspace_name(0), workspace_identity);
        workspace.tabs = vec![content.tab];
        Self {
            sessions,
            workspaces: vec![workspace],
            active_ws: 0,
            next_token,
            token_ceiling,
            proxy,
            recording_enabled: source.recording_enabled,
            local_hostname: source.local_hostname.clone(),
            shell_integration_enabled: source.shell_integration_enabled,
            launch_geometry_settled: false,
            held_launches_pending: true,
            held_fallback_armed: false,
        }
    }

    /// Take the content back out of a set built by [`Self::adopting`], so it
    /// can be restored to its source when the new window cannot be created.
    /// `restore` is the restore point the content carried before adoption.
    pub(in crate::native) fn release_adopted(&mut self, template: MovedRestore) -> MovedContent {
        let mut workspace = self.workspaces.remove(0);
        let tab = workspace.tabs.remove(0);
        let sessions = tab
            .layout
            .leaves()
            .into_iter()
            .filter_map(|token| self.sessions.remove(&token))
            .collect();
        MovedContent {
            tab,
            sessions,
            fresh_tab: template.fresh_tab,
            restore: template.restore,
        }
    }
}

/// The parts of a [`MovedContent`] a caller keeps while the content itself is
/// inside a new window that may still fail to open.
pub(in crate::native) struct MovedRestore {
    fresh_tab: bool,
    restore: RestorePoint,
}

impl MovedContent {
    /// Split off the restore point before the content is adopted by a new
    /// window, so [`WorkspaceSet::release_adopted`] can rebuild it.
    pub(in crate::native) fn restore_template(&self) -> MovedRestore {
        MovedRestore {
            fresh_tab: self.fresh_tab,
            restore: self.restore.clone(),
        }
    }
}
