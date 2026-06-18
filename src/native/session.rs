// SPDX-License-Identifier: GPL-3.0-only
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
    pub(super) title_override: Option<String>,
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
            title_override: None,
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

    pub(super) fn effective_tab_title(&self) -> &str {
        self.title_override
            .as_deref()
            .unwrap_or(self.tab_title.as_str())
    }

    pub(super) fn set_title_override(&mut self, name: Option<String>) {
        self.title_override = name;
        self.needs_rebuild = true;
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

pub(super) struct SessionSet {
    sessions: Vec<Session>,
    active_token: SessionToken,
    next_token: u64,
    proxy: Option<EventLoopProxy<UserEvent>>,
}

impl SessionSet {
    pub(super) fn new(initial: Session, proxy: Option<EventLoopProxy<UserEvent>>) -> Self {
        let active_token = initial.id;
        Self {
            sessions: vec![initial],
            active_token,
            next_token: active_token.0.saturating_add(1),
            proxy,
        }
    }

    pub(super) fn active(&self) -> &Session {
        if let Some(position) = self.position_of_token(self.active_token) {
            &self.sessions[position]
        } else {
            &self.sessions[0]
        }
    }

    pub(super) fn active_mut(&mut self) -> &mut Session {
        if let Some(position) = self.position_of_token(self.active_token) {
            &mut self.sessions[position]
        } else {
            &mut self.sessions[0]
        }
    }

    pub(super) fn active_id(&self) -> SessionToken {
        self.active_token
    }

    pub(super) fn active_position(&self) -> usize {
        self.position_of_token(self.active_token).unwrap_or(0)
    }

    pub(super) fn get_mut(&mut self, token: SessionToken) -> Option<&mut Session> {
        let position = self.position_of_token(token)?;
        self.sessions.get_mut(position)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Session> {
        self.sessions.iter()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.sessions.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub(super) fn spawn(
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
        self.sessions.push(Session::new(
            session_id,
            terminal,
            writer,
            pty,
            Some(pump_thread),
        ));
        Ok(session_id)
    }

    pub(super) fn token_at_position(&self, position: usize) -> Option<SessionToken> {
        self.sessions.get(position).map(|session| session.id)
    }

    pub(super) fn position_of_token(&self, token: SessionToken) -> Option<usize> {
        self.sessions.iter().position(|session| session.id == token)
    }

    pub(super) fn switch(&mut self, token: SessionToken) -> bool {
        if self.position_of_token(token).is_none() || token == self.active_token {
            return false;
        }
        self.active_token = token;
        true
    }

    pub(super) fn next(&mut self) -> bool {
        if self.sessions.len() <= 1 {
            return false;
        }
        let next_position = (self.active_position() + 1) % self.sessions.len();
        self.active_token = self.sessions[next_position].id;
        true
    }

    pub(super) fn prev(&mut self) -> bool {
        if self.sessions.len() <= 1 {
            return false;
        }
        let active = self.active_position();
        let previous_position = if active == 0 {
            self.sessions.len() - 1
        } else {
            active - 1
        };
        self.active_token = self.sessions[previous_position].id;
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
        let Some(index) = self.position_of_token(token) else {
            return self.sessions.is_empty();
        };
        let session = self.sessions.remove(index);
        let _ = close_session(session);
        if self.sessions.is_empty() {
            self.active_token = token;
            return true;
        }
        let next_position = index.min(self.sessions.len() - 1);
        if self.active_token == token || self.position_of_token(self.active_token).is_none() {
            self.active_token = self.sessions[next_position].id;
        }
        false
    }

    #[cfg(test)]
    pub(in crate::native) fn push(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.push(session);
        id
    }
}

impl TabBarSource for SessionSet {
    fn tab_count(&self) -> usize {
        self.sessions.len()
    }

    fn tab_title(&self, idx: usize) -> &str {
        self.sessions
            .get(idx)
            .map(Session::effective_tab_title)
            .unwrap_or("odytty")
    }

    fn active_tab(&self) -> usize {
        self.active_position()
    }
}

impl Deref for SessionSet {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.active()
    }
}

impl DerefMut for SessionSet {
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
        let mut sessions = SessionSet::new(build_session(), None);
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
}
