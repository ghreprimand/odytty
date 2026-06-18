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

pub(super) type SessionId = usize;

pub(super) struct Session {
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

pub(super) struct SessionSet {
    sessions: Vec<Session>,
    active: SessionId,
    proxy: Option<EventLoopProxy<UserEvent>>,
}

impl SessionSet {
    pub(super) fn new(initial: Session, proxy: Option<EventLoopProxy<UserEvent>>) -> Self {
        Self {
            sessions: vec![initial],
            active: 0,
            proxy,
        }
    }

    pub(super) fn active(&self) -> &Session {
        &self.sessions[self.active]
    }

    pub(super) fn active_mut(&mut self) -> &mut Session {
        &mut self.sessions[self.active]
    }

    pub(super) fn active_id(&self) -> SessionId {
        self.active
    }

    pub(super) fn get_mut(&mut self, index: SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(index)
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
    ) -> Result<SessionId, std::io::Error> {
        let Some(proxy) = self.proxy.clone() else {
            return Err(std::io::Error::other(
                "session spawn unavailable without event loop proxy",
            ));
        };
        let session_id = self.sessions.len();
        let session = PtySession::spawn_default_shell(grid).map_err(std::io::Error::other)?;
        let reader = session.try_clone_reader().map_err(std::io::Error::other)?;
        let writer: PtyWriter = Arc::new(Mutex::new(
            session.take_writer().map_err(std::io::Error::other)?,
        ));
        let terminal = Arc::new(Mutex::new(Terminal::new(grid.columns, grid.rows)));
        let pump_thread =
            spawn_pty_pump(reader, writer.clone(), terminal.clone(), proxy, session_id);
        let pty = Arc::new(Mutex::new(session));
        self.sessions
            .push(Session::new(terminal, writer, pty, Some(pump_thread)));
        Ok(session_id)
    }

    pub(super) fn switch(&mut self, index: SessionId) -> bool {
        if index >= self.sessions.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    pub(super) fn next(&mut self) -> bool {
        if self.sessions.len() <= 1 {
            return false;
        }
        self.active = (self.active + 1) % self.sessions.len();
        true
    }

    pub(super) fn prev(&mut self) -> bool {
        if self.sessions.len() <= 1 {
            return false;
        }
        self.active = if self.active == 0 {
            self.sessions.len() - 1
        } else {
            self.active - 1
        };
        true
    }

    pub(super) fn close(&mut self, index: SessionId) -> bool {
        self.close_with(index, Session::close)
    }

    pub(super) fn close_shell_exited(&mut self, index: SessionId) -> bool {
        self.close_with(index, Session::close_after_shell_exit)
    }

    fn close_with(
        &mut self,
        index: SessionId,
        close_session: impl FnOnce(Session) -> bool,
    ) -> bool {
        if index >= self.sessions.len() {
            return self.sessions.is_empty();
        }
        let session = self.sessions.remove(index);
        let _ = close_session(session);
        if self.sessions.is_empty() {
            self.active = 0;
            return true;
        }
        if self.active > index {
            self.active -= 1;
        } else if self.active >= self.sessions.len() {
            self.active = self.sessions.len() - 1;
        }
        false
    }

    #[cfg(test)]
    pub(in crate::native) fn push(&mut self, session: Session) -> SessionId {
        let id = self.sessions.len();
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
            .map(|session| session.tab_title.as_str())
            .unwrap_or("odytty")
    }

    fn active_tab(&self) -> usize {
        self.active
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

    fn build_session() -> Session {
        let dims = Dimensions::new(20, 8);
        let pty = PtySession::spawn_shell_command(dims, "sleep 1").expect("spawn test shell");
        let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().expect("writer")));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(pty));
        Session::new(terminal, writer, pty, None)
    }

    #[test]
    fn session_title_defaults_to_odytty() {
        let session = build_session();
        assert_eq!(session.tab_title, "odytty");
    }

    #[test]
    fn session_set_switches_wraps_and_closes() {
        let mut sessions = SessionSet::new(build_session(), None);
        sessions.push(build_session());
        sessions.push(build_session());

        assert_eq!(sessions.active_id(), 0);
        assert!(sessions.next());
        assert_eq!(sessions.active_id(), 1);
        assert!(sessions.prev());
        assert_eq!(sessions.active_id(), 0);
        assert!(sessions.switch(2));
        assert_eq!(sessions.active_id(), 2);

        let last = sessions.close(2);
        assert!(!last);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.active_id(), 1);

        assert!(!sessions.close(1));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions.active_id(), 0);

        assert!(sessions.close(0));
        assert!(sessions.is_empty());
    }
}
