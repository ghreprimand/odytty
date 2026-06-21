// SPDX-License-Identifier: GPL-3.0-only
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::core::{LinkId, Snapshot, Terminal};
use crate::pty::PtySession;
use crate::selection::{
    AbsoluteSelectionRange, AbsoluteSelectionState, CellPoint, ClickTracker, PointerDrag,
};
#[cfg(test)]
use crate::text::CellSize;

use winit::event_loop::EventLoopProxy;

use super::app::{
    CursorBlinkState, HintsUi, SessionScrollAnimState, SynchronizedOutputHold, TabBarSource,
};
use super::copy_mode::CopyModeState;
use super::layout::{EVEN_RATIO, PaneNode, SplitAxis};
use super::pty::{PtyWriter, UserEvent, spawn_pty_pump};
use super::render_helpers::RenderSignature;
use super::search_ui::SearchUi;
use super::viewport::Viewport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct SessionToken(pub(super) u64);

pub(super) struct Session {
    pub(super) id: SessionToken,
    pub(super) terminal: Arc<Mutex<Terminal>>,
    pub(super) writer: PtyWriter,
    pub(super) pty: Arc<Mutex<PtySession>>,
    pub(super) pump_thread: Option<JoinHandle<()>>,
    pub(super) tab_title: String,
    pub(super) needs_rebuild: bool,
    pub(super) last_render_signature: Option<RenderSignature>,
    pub(super) synchronized_output_hold: SynchronizedOutputHold,
    pub(super) last_presented_snapshot: Option<Snapshot>,
    pub(super) last_presented_cursor_style: crate::core::CursorStyle,
    pub(super) last_presented_cursor_blinking: bool,
    pub(super) selection: AbsoluteSelectionState,
    pub(super) pointer_cell: Option<CellPoint>,
    pub(super) pointer_px: Option<(f64, f64)>,
    #[cfg(test)]
    pub(super) test_cell: Option<CellSize>,
    pub(super) hovered_hyperlink: Option<LinkId>,
    pub(super) pointer_drag: PointerDrag,
    pub(super) selection_block: bool,
    pub(super) drag_anchor_unit: Option<AbsoluteSelectionRange>,
    pub(super) clicks: ClickTracker,
    pub(super) last_selection_autoscroll: Option<Instant>,
    pub(super) report_button: Option<crate::core::MouseButton>,
    pub(super) viewport: Viewport,
    pub(super) search: SearchUi,
    pub(super) hints: Option<HintsUi>,
    pub(super) copy_mode: Option<CopyModeState>,
    pub(super) search_restore_viewport: Option<usize>,
    pub(super) last_scrollback_len: usize,
    pub(super) cursor_blink: CursorBlinkState,
    pub(super) cursor_anim_alpha: f32,
    pub(super) cursor_ease_deadline: Option<Instant>,
    pub(super) cursor_ease_phase_on: bool,
    pub(super) cursor_ease_toggle_at: Option<Instant>,
    pub(super) cursor_anim_offset: [f32; 2],
    pub(super) cursor_slide_deadline: Option<Instant>,
    pub(super) cursor_slide_start: Option<Instant>,
    pub(super) cursor_slide_from_px: [f32; 2],
    pub(super) row_fade_starts: Vec<Option<Instant>>,
    pub(super) last_scrollback_len_for_fade: usize,
    pub(super) row_fade_epoch: u64,
    pub(super) scroll_anim: Option<SessionScrollAnimState>,
    pub(super) scroll_frac_offset: f32,
}

impl Session {
    pub(super) fn new(
        id: SessionToken,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
        pump_thread: Option<JoinHandle<()>>,
    ) -> Self {
        let tab_title = terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.title().map(ToOwned::to_owned))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "odytty".to_owned());
        Self {
            id,
            terminal,
            writer,
            pty,
            pump_thread,
            tab_title,
            needs_rebuild: true,
            last_render_signature: None,
            synchronized_output_hold: SynchronizedOutputHold::default(),
            last_presented_snapshot: None,
            last_presented_cursor_style: crate::core::CursorStyle::default(),
            last_presented_cursor_blinking: true,
            selection: AbsoluteSelectionState::default(),
            pointer_cell: None,
            pointer_px: None,
            #[cfg(test)]
            test_cell: None,
            hovered_hyperlink: None,
            pointer_drag: PointerDrag::None,
            selection_block: false,
            drag_anchor_unit: None,
            clicks: ClickTracker::default(),
            last_selection_autoscroll: None,
            report_button: None,
            viewport: Viewport::default(),
            search: SearchUi::default(),
            hints: None,
            copy_mode: None,
            search_restore_viewport: None,
            last_scrollback_len: 0,
            cursor_blink: CursorBlinkState::new(super::app::CURSOR_BLINK_INTERVAL),
            cursor_anim_alpha: 1.0,
            cursor_ease_deadline: None,
            cursor_ease_phase_on: true,
            cursor_ease_toggle_at: None,
            cursor_anim_offset: [0.0, 0.0],
            cursor_slide_deadline: None,
            cursor_slide_start: None,
            cursor_slide_from_px: [0.0, 0.0],
            row_fade_starts: Vec::new(),
            last_scrollback_len_for_fade: 0,
            row_fade_epoch: 0,
            scroll_anim: None,
            scroll_frac_offset: 0.0,
        }
    }

    pub(super) fn refresh_tab_title(&mut self) {
        self.tab_title = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.title().map(ToOwned::to_owned))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "odytty".to_owned());
    }

    fn close(mut self) -> bool {
        if let Ok(mut pty) = self.pty.lock() {
            let _ = pty.kill();
            let _ = pty.wait();
        }
        if let Some(thread) = self.pump_thread.take() {
            let _ = thread.join();
        }
        true
    }

    fn close_after_shell_exit(mut self) -> bool {
        let pty = self.pty.clone();
        let pump_thread = self.pump_thread.take();
        let _ = std::thread::Builder::new()
            .name("odytty-session-close".to_owned())
            .spawn(move || {
                if let Ok(mut pty) = pty.lock() {
                    let _ = pty.try_wait();
                }
                if let Some(thread) = pump_thread {
                    let _ = thread.join();
                }
            });
        true
    }
}

/// One tab in the strip. It owns a layout tree of panes (a binary
/// [`PaneNode`]) and tracks which pane within the tab is focused. A fresh tab
/// is a single [`PaneNode::Leaf`], which the render/resize paths treat
/// byte-identically to today's single-session window (design doc §2.3). Pane
/// splitting is wired in a later Phase-1 packet; for now every tab is a single
/// leaf, so `tabs.len()` equals the session count and behaviour is unchanged.
pub(super) struct Tab {
    pub(super) layout: PaneNode,
    pub(super) focused: SessionToken,
    /// Optional user-assigned tab name (the Phase-0 rename feature). When set it
    /// overrides the focused pane's shell-derived title in the tab strip. Once a
    /// tab can hold several panes the name is no longer 1:1 with a session, so
    /// the override lives on the tab, not the session (design doc §2.4/§9.5).
    pub(super) title_override: Option<String>,
}

impl Tab {
    /// A single-pane tab wrapping one session.
    fn single(token: SessionToken) -> Self {
        Self {
            layout: PaneNode::leaf(token),
            focused: token,
            title_override: None,
        }
    }
}

/// The tab strip and the session arena that backs it (design doc §2.1/§2.2).
///
/// Sessions live in an arena keyed by [`SessionToken`] so pump-thread lookup by
/// token stays O(1) and ordering lives entirely in `tabs`. Each tab owns a
/// [`PaneNode`] layout tree whose leaves reference sessions by token. While
/// every tab is still a single leaf this is behaviourally identical to the old
/// `Vec<Session>` model; the two-level structure is what later packets build
/// pane splitting on. `Deref`/`DerefMut` resolve to the focused pane of the
/// active tab — the correct target for all keyboard/cursor/selection sites.
pub(super) struct TabSet {
    sessions: HashMap<SessionToken, Session>,
    tabs: Vec<Tab>,
    active_tab: usize,
    next_token: u64,
    proxy: Option<EventLoopProxy<UserEvent>>,
}

impl TabSet {
    pub(super) fn new(initial: Session, proxy: Option<EventLoopProxy<UserEvent>>) -> Self {
        let token = initial.id;
        let next_token = token.0.saturating_add(1);
        let mut sessions = HashMap::new();
        sessions.insert(token, initial);
        Self {
            sessions,
            tabs: vec![Tab::single(token)],
            active_tab: 0,
            next_token,
            proxy,
        }
    }

    /// The token of the focused pane of the active tab — the `Deref` target.
    fn active_focused_token(&self) -> SessionToken {
        self.tabs
            .get(self.active_tab)
            .or_else(|| self.tabs.first())
            .map(|tab| tab.focused)
            .unwrap_or(SessionToken(0))
    }

    pub(super) fn active(&self) -> &Session {
        let token = self.active_focused_token();
        self.sessions
            .get(&token)
            .or_else(|| self.sessions.values().next())
            .expect("TabSet always holds at least one session while active() is called")
    }

    pub(super) fn active_mut(&mut self) -> &mut Session {
        let token = self.active_focused_token();
        if self.sessions.contains_key(&token) {
            return self
                .sessions
                .get_mut(&token)
                .expect("token presence just checked");
        }
        self.sessions
            .values_mut()
            .next()
            .expect("TabSet always holds at least one session while active_mut() is called")
    }

    pub(super) fn active_id(&self) -> SessionToken {
        self.active_focused_token()
    }

    #[cfg(test)]
    pub(super) fn active_position(&self) -> usize {
        self.active_tab
    }

    pub(super) fn get_mut(&mut self, token: SessionToken) -> Option<&mut Session> {
        self.sessions.get_mut(&token)
    }

    /// The effective display title of the tab that contains `token`: the tab's
    /// user override if set, otherwise the focused pane's shell-derived title
    /// (design doc §2.4). Returns an owned string for the rename UI / test
    /// seams; the tab bar reads the borrowed form via `TabBarSource`.
    pub(super) fn effective_tab_title(&self, token: SessionToken) -> String {
        let Some(tab) = self.tabs.iter().find(|tab| tab.layout.contains(token)) else {
            return "odytty".to_owned();
        };
        if let Some(name) = &tab.title_override {
            return name.clone();
        }
        self.sessions
            .get(&tab.focused)
            .map(|session| session.tab_title.clone())
            .unwrap_or_else(|| "odytty".to_owned())
    }

    /// Set or clear the user title override for the tab that contains `token`,
    /// marking the focused pane for rebuild so the tab strip repaints.
    pub(super) fn set_title_override(&mut self, token: SessionToken, name: Option<String>) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.layout.contains(token)) else {
            return;
        };
        tab.title_override = name;
        let focused = tab.focused;
        if let Some(session) = self.sessions.get_mut(&focused) {
            session.needs_rebuild = true;
        }
    }

    /// Every session, in tab order (and, within a tab, tree order). For
    /// single-pane tabs this is exactly the old `Vec<Session>` order, so
    /// position-indexed callers (resize, scrollback cap, test seams) are
    /// unchanged; it still visits every pane once.
    pub(super) fn iter(&self) -> impl Iterator<Item = &Session> {
        self.tabs.iter().flat_map(move |tab| {
            tab.layout
                .leaves()
                .into_iter()
                .filter_map(move |token| self.sessions.get(&token))
        })
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.sessions.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Spawn a shell + terminal at `grid` and insert it into the arena, **without**
    /// attaching it to any tab. Shared by [`Self::spawn`] (which then opens a new
    /// tab) and [`Self::split_active`] (which then grafts the session into the
    /// active tab's layout tree as a new pane). The caller owns tab/pane wiring.
    fn insert_spawned_session(
        &mut self,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        let Some(proxy) = self.proxy.clone() else {
            return Err(std::io::Error::other(
                "session spawn unavailable without event loop proxy",
            ));
        };
        let session_id = SessionToken(self.next_token);
        self.next_token = self.next_token.saturating_add(1);
        let session = PtySession::spawn_default_shell(grid).map_err(std::io::Error::other)?;
        let reader = session.try_clone_reader().map_err(std::io::Error::other)?;
        let writer: PtyWriter = Arc::new(Mutex::new(
            session.take_writer().map_err(std::io::Error::other)?,
        ));
        let terminal = Arc::new(Mutex::new(Terminal::new(grid.columns, grid.rows)));
        let pump_thread =
            spawn_pty_pump(reader, writer.clone(), terminal.clone(), proxy, session_id);
        let pty = Arc::new(Mutex::new(session));
        self.sessions.insert(
            session_id,
            Session::new(session_id, terminal, writer, pty, Some(pump_thread)),
        );
        Ok(session_id)
    }

    /// Spawn a new session in a brand-new single-pane tab (the existing
    /// new-tab behaviour). Tab order is append-to-end, unchanged.
    pub(super) fn spawn(
        &mut self,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        let session_id = self.insert_spawned_session(grid)?;
        self.tabs.push(Tab::single(session_id));
        Ok(session_id)
    }

    /// The focused-pane token of the tab at `position` in the strip.
    pub(super) fn token_at_position(&self, position: usize) -> Option<SessionToken> {
        self.tabs.get(position).map(|tab| tab.focused)
    }

    /// The strip index of the tab that contains `token` as one of its panes.
    pub(super) fn position_of_token(&self, token: SessionToken) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.layout.contains(token))
    }

    pub(super) fn switch(&mut self, token: SessionToken) -> bool {
        let Some(tab_idx) = self.position_of_token(token) else {
            return false;
        };
        if tab_idx == self.active_tab && self.tabs[tab_idx].focused == token {
            return false;
        }
        self.active_tab = tab_idx;
        self.tabs[tab_idx].focused = token;
        true
    }

    pub(super) fn next(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return false;
        }
        self.active_tab = (self.active_tab + 1) % self.tabs.len();
        true
    }

    pub(super) fn prev(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return false;
        }
        self.active_tab = if self.active_tab == 0 {
            self.tabs.len() - 1
        } else {
            self.active_tab - 1
        };
        true
    }

    pub(super) fn close(&mut self, token: SessionToken) -> bool {
        self.close_with(token, Session::close)
    }

    pub(super) fn close_shell_exited(&mut self, token: SessionToken) -> bool {
        self.close_with(token, Session::close_after_shell_exit)
    }

    fn close_with(
        &mut self,
        token: SessionToken,
        close_session: impl FnOnce(Session) -> bool,
    ) -> bool {
        let Some(tab_idx) = self.position_of_token(token) else {
            return self.sessions.is_empty();
        };
        // Reap the session itself.
        if let Some(session) = self.sessions.remove(&token) {
            let _ = close_session(session);
        }
        // Remove the pane leaf, collapsing its split parent into the sibling.
        // For a single-pane tab this yields `None`, i.e. the tab closes — the
        // byte-identical analogue of removing a session from the old Vec.
        match self.tabs[tab_idx].layout.clone().close_leaf(token) {
            None => {
                let was_active = self.active_tab == tab_idx;
                self.tabs.remove(tab_idx);
                if self.tabs.is_empty() {
                    self.active_tab = 0;
                    return true;
                }
                if was_active {
                    self.active_tab = tab_idx.min(self.tabs.len() - 1);
                } else if self.active_tab > tab_idx {
                    self.active_tab -= 1;
                }
                false
            }
            Some(layout) => {
                // The tab survives (a multi-pane tab lost one pane). Refocus a
                // surviving leaf if the closed pane held focus.
                if self.tabs[tab_idx].focused == token
                    && let Some(first) = layout.leaves().first().copied()
                {
                    self.tabs[tab_idx].focused = first;
                }
                self.tabs[tab_idx].layout = layout;
                false
            }
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn push(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.insert(id, session);
        self.tabs.push(Tab::single(id));
        id
    }

    /// Insert a session into the arena **without** a tab (test-only), so headless
    /// tests can drive [`Self::split_active_with`] — the pure tree-mutation half
    /// of a split — without spawning a real PTY for the new pane.
    #[cfg(test)]
    fn push_arena_only(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.insert(id, session);
        id
    }

    /// Test-only driver for a split: arena-insert `session` then graft it into
    /// the active tab by splitting the focused leaf along `axis`. Mirrors the
    /// production [`Self::split_active`] minus the PTY spawn.
    #[cfg(test)]
    fn split_active_for_test(&mut self, axis: SplitAxis, session: Session) -> SessionToken {
        let token = self.push_arena_only(session);
        self.split_active_with(axis, token);
        token
    }
}

/// Pane-management operations for the active tab (design doc §4–§5). These are
/// the geometry-free half of splits/panes: tree mutation and tree-order focus.
/// They are driven by the keybinding layer (a later packet) and, in this
/// packet, by `#[cfg(test)]` seams + the multi-pane render dispatch (1c). The
/// `allow(dead_code)` is scaffolding parity with `layout.rs`: it comes off as
/// the render path (`active_layout`/`active_pane_count`/`active_is_single_pane`)
/// and the keybinding ops wire these in. Single-pane tabs never reach the
/// mutating ops, so the byte-identical path is untouched.
#[allow(dead_code)]
impl TabSet {
    /// Split the **focused pane of the active tab** along `axis`, spawning a new
    /// session at `grid` for the new pane and giving it focus (tmux semantics:
    /// the new pane becomes `second` and is focused). Returns the new session's
    /// token. A no-op-and-error if there is no active tab or spawn fails. The
    /// new pane shares the tab — no new tab-strip entry is added.
    pub(super) fn split_active(
        &mut self,
        axis: SplitAxis,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        if self.tabs.get(self.active_tab).is_none() {
            return Err(std::io::Error::other("no active tab to split"));
        }
        let new_token = self.insert_spawned_session(grid)?;
        self.split_active_with(axis, new_token);
        Ok(new_token)
    }

    /// Pure tree-mutation half of [`Self::split_active`]: graft `new_token` into
    /// the active tab by splitting its currently focused leaf along `axis` at the
    /// even ratio, then focus the new pane. Assumes `new_token` already exists in
    /// the arena. Factored out so headless tests can exercise the layout-tree
    /// behaviour without spawning a real PTY.
    fn split_active_with(&mut self, axis: SplitAxis, new_token: SessionToken) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let focused = tab.focused;
        let layout = std::mem::replace(&mut tab.layout, PaneNode::leaf(new_token));
        tab.layout = layout.split_leaf(focused, axis, EVEN_RATIO, new_token);
        tab.focused = new_token;
    }

    /// Reset every split ratio in the active tab to even (tmux `Ctrl-b =`).
    pub(super) fn equalize_active(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            let layout = std::mem::replace(&mut tab.layout, PaneNode::leaf(tab.focused));
            tab.layout = layout.equalized();
        }
    }

    /// Cycle focus to the next pane of the active tab in tree order (tmux
    /// `Ctrl-b o`). No geometry needed. Returns true if focus moved.
    pub(super) fn focus_next_pane(&mut self) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        match tab.layout.next_leaf_after(tab.focused) {
            Some(next) if next != tab.focused => {
                tab.focused = next;
                true
            }
            _ => false,
        }
    }

    /// Set the focused pane of the active tab to `token` when it is a pane of
    /// that tab (directional focus / focus-follows-click land the resolved
    /// token here). Returns true if focus changed.
    pub(super) fn set_active_focus(&mut self, token: SessionToken) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        if tab.focused == token || !tab.layout.contains(token) {
            return false;
        }
        tab.focused = token;
        true
    }

    /// The active tab's pane layout tree (for the render/geometry layer).
    pub(super) fn active_layout(&self) -> Option<&PaneNode> {
        self.tabs.get(self.active_tab).map(|tab| &tab.layout)
    }

    /// Number of panes in the active tab (1 ⇒ the byte-identical single path).
    pub(super) fn active_pane_count(&self) -> usize {
        self.tabs
            .get(self.active_tab)
            .map(|tab| tab.layout.pane_count())
            .unwrap_or(1)
    }

    /// True when the active tab holds exactly one pane — the byte-identical
    /// render/resize fast path (design doc §2.3).
    pub(super) fn active_is_single_pane(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .map(|tab| tab.layout.is_single_pane())
            .unwrap_or(true)
    }
}

impl TabBarSource for TabSet {
    fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn tab_title(&self, idx: usize) -> &str {
        let Some(tab) = self.tabs.get(idx) else {
            return "odytty";
        };
        if let Some(name) = &tab.title_override {
            return name.as_str();
        }
        self.sessions
            .get(&tab.focused)
            .map(|session| session.tab_title.as_str())
            .unwrap_or("odytty")
    }

    fn active_tab(&self) -> usize {
        self.active_tab
    }
}

impl Deref for TabSet {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.active()
    }
}

impl DerefMut for TabSet {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.active_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Dimensions;

    fn build_session_with_id(id: SessionToken) -> Session {
        let dims = Dimensions::new(20, 8);
        let pty = PtySession::spawn_shell_command(dims, "sleep 1").expect("spawn test shell");
        let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().expect("writer")));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(pty));
        Session::new(id, terminal, writer, pty, None)
    }

    fn build_session() -> Session {
        build_session_with_id(SessionToken(0))
    }

    #[test]
    fn session_title_defaults_to_odytty() {
        let session = build_session();
        assert_eq!(session.tab_title, "odytty");
    }

    #[test]
    fn session_set_switches_wraps_and_closes() {
        let mut sessions = TabSet::new(build_session(), None);
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
        let mut set = TabSet::new(build_session(), None);
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
        let mut set = TabSet::new(build_session(), None);
        let p1 =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let p2 = set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(2)));
        // Tree leaves in order: 0, 1, 2 (focus currently p2).
        assert_eq!(set.active_id(), p2);
        assert!(set.focus_next_pane());
        assert_eq!(set.active_id(), SessionToken(0)); // wraps to first
        assert!(set.focus_next_pane());
        assert_eq!(set.active_id(), p1);
        // Single-pane tab: no-op.
        let mut single = TabSet::new(build_session(), None);
        assert!(!single.focus_next_pane());
    }

    #[test]
    fn set_active_focus_accepts_panes_and_rejects_strangers() {
        let mut set = TabSet::new(build_session(), None);
        let p1 =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
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
        let mut set = TabSet::new(build_session(), None);
        let p1 =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert_eq!(set.active_pane_count(), 2);
        // Closing one pane collapses the split; the tab survives (not last).
        assert!(!set.close(p1));
        assert_eq!(set.tab_count(), 1);
        assert_eq!(set.active_pane_count(), 1);
        assert!(set.active_is_single_pane());
        assert_eq!(set.active_id(), SessionToken(0));
    }

    #[test]
    fn equalize_active_is_a_noop_on_single_pane() {
        let mut set = TabSet::new(build_session(), None);
        set.equalize_active();
        assert!(set.active_is_single_pane());
        // With a split present, layout tree stays valid (ratios reset).
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        set.equalize_active();
        assert_eq!(set.active_pane_count(), 2);
    }

    #[test]
    fn active_layout_exposes_the_tree() {
        let mut set = TabSet::new(build_session(), None);
        assert!(set.active_layout().is_some_and(PaneNode::is_single_pane));
        set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(1)));
        assert_eq!(set.active_layout().map(PaneNode::pane_count), Some(2));
    }
}
