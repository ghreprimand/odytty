// SPDX-License-Identifier: GPL-3.0-only
use std::collections::HashMap;
use std::ffi::OsString;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::connection_hosts::ConnectionHost;
use crate::core::{LinkId, Snapshot, Terminal};
#[cfg(test)]
use crate::native::WindowPadding;
use crate::pty::{ForegroundJob, PtySession};
use crate::selection::{
    AbsoluteSelectionRange, AbsoluteSelectionState, CellPoint, ClickTracker, PointerDrag,
};
use crate::ssh_connect::{SshCommand, ssh_command_for_host};
#[cfg(test)]
use crate::text::CellSize;

use winit::event_loop::EventLoopProxy;

use super::app::{
    CursorBlinkState, HintsUi, SessionScrollAnimState, SynchronizedOutputHold, TabBarSource,
};
#[cfg(unix)]
use super::attach::{AttachClient, attach_input_writer, resolve_session_socket, spawn_attach_pump};
use super::copy_mode::CopyModeState;
use super::layout::{
    EVEN_RATIO, FocusDir, PaneNode, PaneRect, SplitAxis, divider_at_point, divider_axis_at_point,
    divider_rects_with_axis, drag_divider_to, focus_move, grid_dims_for_rect, layout_rects,
    pane_at_point, snap_divider_to_cells,
};
use super::output_recorder::RecorderHandle;
use super::pty::{PtyWriter, UserEvent, spawn_pty_pump};
use super::render_helpers::RenderSignature;
use super::search_ui::SearchUi;
use super::viewport::Viewport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct SessionToken(pub(super) u64);

/// Apply the local-PTY backend capabilities onto a freshly-created terminal
/// model. Called from EVERY local-pane creation path so the wiring can't drift:
///   * [`SessionSet::insert_local_session_with`] — the split / new-tab path.
///   * [`super::run_native`] — the startup pane (hand-built in `run_native`).
///
/// Currently propagates one capability: whether the backend's shell repaints
/// the cursor with absolute positioning on resize (ConPTY/Windows = true), so
/// the terminal defers resize cursor placement to the shell instead of
/// translating it. On a POSIX PTY this is false (= the model default), which is
/// why a missing call is invisible on Linux/macOS and only Windows exposes a
/// drift — keep this funnel the single source of truth and guard it on Windows
/// CI via the setter/getter/behavior tie + the per-path pane tests.
pub(super) fn apply_local_backend_caps(model: &mut Terminal, session: &PtySession) {
    model.set_shell_owns_cursor_on_resize(session.shell_repaints_on_resize());
}

pub(super) fn seed_initial_working_directory(model: &mut Terminal, cwd: Option<&Path>) {
    let cwd = match cwd {
        Some(path) => path.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(path) => path,
            Err(_) => return,
        },
    };
    model.seed_working_directory(cwd.to_string_lossy().into_owned());
}

/// What backs a session's I/O. The default is a locally-spawned PTY (the
/// byte-identical path that everything used before Phase 2); an attached session
/// is instead backed by a socket to a detached session-host. Input is *not*
/// routed here — it flows through `Session::writer`, which for an attached
/// session is an [`AttachInputWriter`](super::attach::AttachInputWriter) boxed
/// into the same [`PtyWriter`] type, so the app-side input path is identical.
/// This enum routes the two operations that genuinely differ by backing:
/// **resize** (TIOCSWINSZ vs. a `Resize` frame) and **close** (kill+reap vs. a
/// clean `Detach` that keeps the host session alive for later reattach).
pub(super) enum SessionSource {
    /// Locally-spawned PTY — the default, byte-identical path.
    Local { pty: Arc<Mutex<PtySession>> },
    /// Attached to a detached session-host over a per-user unix socket. The
    /// client is shared with the input writer so input/resize/detach serialize
    /// through one socket lock. Unix-only: the detached session-host transport is
    /// `#[cfg(unix)]`, so on Windows a session is always `Local` and every match
    /// on this enum is exhaustive with the `Local` arm alone.
    #[cfg(unix)]
    Attached { client: Arc<Mutex<AttachClient>> },
}

pub(super) struct Session {
    pub(super) id: SessionToken,
    pub(super) terminal: Arc<Mutex<Terminal>>,
    pub(super) writer: PtyWriter,
    pub(super) source: SessionSource,
    /// The host session-id string this session was attached by (Phase 14), or
    /// `None` for a locally-spawned PTY. Drives attach dedup: selecting a
    /// session already open in a tab switches to it instead of appending a
    /// duplicate. Set only on the attached construction path; the local path
    /// leaves it `None`, so the default behavior is unchanged.
    pub(super) attached_session_id: Option<String>,
    pub(super) pump_thread: Option<JoinHandle<()>>,
    /// Bounded ring of recorded screen frames for the replay overlay (Phase 2).
    /// A clonable handle shared with this session's pump thread, which writes
    /// frames into it while recording is enabled (`session_replay`, default
    /// off). Empty and disabled by default, so it costs nothing on the plain
    /// path. For an attached session this handle exists but is not yet wired to
    /// the attach pump (recording an attached session is a documented
    /// follow-up), so it stays empty.
    pub(super) recorder: RecorderHandle,
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
    /// Headless multi-pane geometry seam: a `(surface_px, padding)` override so
    /// `multipane_geometry()` (and the divider hover/drag cursor path it feeds)
    /// can run without a GPU/window. `None` in production builds — the field
    /// only exists under `cfg(test)` — so the live path is unchanged.
    #[cfg(test)]
    pub(super) test_surface: Option<((u32, u32), WindowPadding)>,
    /// Headless scale-factor seam: a display scale override so DPI-aware pointer
    /// geometry (the F4-P3 rail reveal zone) can be exercised without a
    /// GPU/window. `None` in production (the field only exists under
    /// `cfg(test)`); the live path reads `GpuState::scale`.
    #[cfg(test)]
    pub(super) test_scale: Option<f32>,
    pub(super) hovered_hyperlink: Option<LinkId>,
    /// INTERACTIVE-PATHS (Phase 7): the path span currently under the pointer
    /// that resolved to a real filesystem entry, or `None`. Drives the pointer
    /// (hand) cursor exactly like `hovered_hyperlink`. Permanently `None` while
    /// the `interactive_paths` setting is off (the scanner is gated off before
    /// it can ever run), so the default hover path is byte-identical.
    pub(super) hovered_path: Option<crate::paths::Resolved>,
    /// UX-A (Phase 11): the visible-cell span of `hovered_path`, captured in the
    /// same hover computation so the Ctrl+hover armed underline can decorate
    /// exactly those cells without re-scanning the row at paint time. Kept in
    /// lockstep with `hovered_path` (set/cleared together); `None` whenever
    /// `hovered_path` is `None`, so it is permanently `None` while the feature is
    /// off and the default hover path is byte-identical.
    pub(super) hovered_path_cells: Option<super::app::click_hint::HoverPathCells>,
    /// INTERACTIVE-URLS: the bare (non-OSC-8) URL currently under the pointer
    /// whose scheme is openable, or `None`. The full URI string to open; drives
    /// the pointer (hand) cursor and the Ctrl+click open exactly like an OSC 8
    /// hyperlink. Permanently `None` while the `interactive_urls` setting is off
    /// (the scanner is gated off before it runs), so the default hover path is
    /// byte-identical. Always `None` when the hovered cell already carries an
    /// OSC 8 hyperlink — that explicit path wins, so a cell is never
    /// double-decorated.
    pub(super) hovered_url: Option<String>,
    /// INTERACTIVE-URLS: the visible-cell span of `hovered_url`, captured in the
    /// same hover computation so the Ctrl+hover armed underline can decorate
    /// exactly those cells. Kept in lockstep with `hovered_url` (set/cleared
    /// together), so it is permanently `None` while the feature is off.
    pub(super) hovered_url_cells: Option<super::app::click_hint::HoverPathCells>,
    /// Test seam (INTERACTIVE-PATHS): synthetic stat-gate so headless hover
    /// tests resolve path spans against an injected fs map, never the real
    /// filesystem. Production builds compile this out and use `FsResolveProbe`.
    #[cfg(test)]
    pub(super) test_path_probe: super::app::interactive_paths::MapProbe,
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
    /// Construct a locally-spawned (PTY-backed) session — the byte-identical
    /// path. The signature is unchanged from before Phase 2; the `pty` is
    /// wrapped into a [`SessionSource::Local`]. Test-only: the production local
    /// construction sites use [`Self::new_local_with_recorder`] so the pump and
    /// the session share one recorder handle. Tests that do not record use this
    /// shorter form (it mints its own empty, disabled handle).
    #[cfg(test)]
    pub(super) fn new(
        id: SessionToken,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
        pump_thread: Option<JoinHandle<()>>,
    ) -> Self {
        Self::from_parts(
            id,
            terminal,
            writer,
            SessionSource::Local { pty },
            pump_thread,
        )
    }

    /// Construct a locally-spawned session that shares a pre-built recorder
    /// handle with its pump thread (so the pump's frames land in the same ring
    /// the App later scrubs). Used by the startup path and `insert_spawned_
    /// session`; the plain `Session::new` (which mints its own empty handle)
    /// stays for call sites that do not record.
    pub(super) fn new_local_with_recorder(
        id: SessionToken,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
        pump_thread: Option<JoinHandle<()>>,
        recorder: RecorderHandle,
    ) -> Self {
        Self::from_parts_with_recorder(
            id,
            terminal,
            writer,
            SessionSource::Local { pty },
            pump_thread,
            recorder,
        )
    }

    /// Construct an attached (session-host-backed) session. Input flows through
    /// `writer` (an [`AttachInputWriter`](super::attach::AttachInputWriter)); the
    /// `client` backs resize/detach. Unix-only (the session-host transport is
    /// `#[cfg(unix)]`).
    #[cfg(unix)]
    pub(super) fn new_attached(
        id: SessionToken,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        client: Arc<Mutex<AttachClient>>,
        session_id: &str,
        pump_thread: Option<JoinHandle<()>>,
    ) -> Self {
        let mut session = Self::from_parts(
            id,
            terminal,
            writer,
            SessionSource::Attached { client },
            pump_thread,
        );
        // Record the host id so attach dedup (Phase 14) can match a re-selected
        // session to its already-open tab.
        session.attached_session_id = Some(session_id.to_owned());
        session
    }

    #[cfg(any(test, unix))]
    fn from_parts(
        id: SessionToken,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        source: SessionSource,
        pump_thread: Option<JoinHandle<()>>,
    ) -> Self {
        Self::from_parts_with_recorder(
            id,
            terminal,
            writer,
            source,
            pump_thread,
            RecorderHandle::new(),
        )
    }

    fn from_parts_with_recorder(
        id: SessionToken,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        source: SessionSource,
        pump_thread: Option<JoinHandle<()>>,
        recorder: RecorderHandle,
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
            source,
            attached_session_id: None,
            pump_thread,
            recorder,
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
            #[cfg(test)]
            test_surface: None,
            #[cfg(test)]
            test_scale: None,
            hovered_hyperlink: None,
            hovered_path: None,
            hovered_path_cells: None,
            hovered_url: None,
            hovered_url_cells: None,
            #[cfg(test)]
            test_path_probe: super::app::interactive_paths::MapProbe::default(),
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

    /// Settle every cursor-animation / render-hold timer to its at-rest identity,
    /// with no scheduled wake. Called on a session that is not the active pane
    /// (NF20-B): a background pane is never rendered, so any live blink /
    /// ease / slide / synchronized-output-hold deadline it holds would be a wake
    /// source with **no** consumer — the loop would schedule `WaitUntil` on a
    /// boundary that never advances and busy-spin. Idempotent (parking an
    /// already-parked session is a no-op); every animation re-arms naturally from
    /// the current frame time when the pane is activated and rendered again.
    pub(super) fn park_animation_timers(&mut self) {
        self.cursor_blink.park();
        self.synchronized_output_hold.clear();
        self.cursor_anim_alpha = 1.0;
        self.cursor_ease_deadline = None;
        self.cursor_ease_phase_on = true;
        self.cursor_ease_toggle_at = None;
        self.cursor_anim_offset = [0.0, 0.0];
        self.cursor_slide_deadline = None;
        self.cursor_slide_start = None;
        self.cursor_slide_from_px = [0.0, 0.0];
    }

    /// Clear every piece of UI state whose coordinates are tied to the row /
    /// scrollback layout, so a reflow never leaves a selection, hover span,
    /// search match, hint label, or copy-mode caret pointing at cells the text
    /// no longer occupies.
    ///
    /// Run for EVERY session a resize reflows, not just the active one (NF21-3):
    /// [`TabSet::resize_all_panes`] reflows every tab's panes, but the clear that
    /// followed it in `App::apply_grid_resize` went through `Deref` = the ACTIVE
    /// session only. A background tab that crossed the reflow keeping stale
    /// absolute-row coordinates would, on switch-back, highlight the wrong text
    /// and copy the wrong bytes. The field set and order match that former
    /// active-only block exactly, so the active-tab path stays byte-identical.
    pub(super) fn invalidate_layout_dependent_state(&mut self) {
        self.selection.clear();
        self.selection_block = false;
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.report_button = None;
        self.pointer_cell = None;
        self.pointer_px = None;
        self.hovered_hyperlink = None;
        self.hovered_path = None;
        // UX-A (Phase 11): drop the armed-underline span alongside the hovered
        // path it mirrors; a reflow makes its old row coords stale.
        self.hovered_path_cells = None;
        // INTERACTIVE-URLS: drop the hovered-URL span too; its row coords are
        // equally stale after a reflow.
        self.hovered_url = None;
        self.hovered_url_cells = None;
        // Reflow changes the row/scrollback layout; return to the live bottom so
        // the offset is never stale against the new geometry.
        self.viewport.reset_to_live();
        // Search closes because its absolute row matches were computed against
        // the old layout.
        self.search.reset_for_reflow();
        self.search_restore_viewport = None;
        // HINTS label spans are absolute rows against the old layout; a reflow
        // makes them stale, so close the modal.
        self.hints = None;
        // COPY-MODE (C13): the caret + selection anchor are absolute-buffer
        // coords computed against the old scrollback/row layout; a reflow
        // re-wraps those rows and leaves them stale. Close the modal alongside
        // the other absolute-row overlays.
        self.copy_mode = None;
    }

    /// The local PTY backing this session, or `None` for an attached session.
    /// Test-only seam (the production foreground-job query uses
    /// [`Self::foreground_job_running`]); tests read the concrete PTY through
    /// this, and an attached session has no local PTY.
    ///
    /// Available in every test build (not just Unix): the cross-platform
    /// pane-wiring guard reads the backend's resize cursor-authority capability
    /// through this on Windows CI, where the value (`true`) differs from the
    /// model default — the case Linux can't exercise. On Windows a session is
    /// always `Local` (the `Attached` variant is `#[cfg(unix)]`), so the match
    /// is exhaustive with the `Local` arm alone.
    #[cfg(test)]
    pub(super) fn local_pty(&self) -> Option<&Arc<Mutex<PtySession>>> {
        match &self.source {
            SessionSource::Local { pty } => Some(pty),
            #[cfg(unix)]
            SessionSource::Attached { .. } => None,
        }
    }

    /// True only when this is a local session whose foreground job is running.
    /// An attached session reports `false` (the foreground job lives in the
    /// remote host and cannot be queried locally), so confirm-close never blocks
    /// closing an attached window — closing it cleanly detaches anyway.
    pub(super) fn foreground_job_running(&self) -> bool {
        match &self.source {
            SessionSource::Local { pty } => pty
                .lock()
                .is_ok_and(|pty| pty.foreground_job() == ForegroundJob::Running),
            #[cfg(unix)]
            SessionSource::Attached { .. } => false,
        }
    }

    fn close(mut self) -> bool {
        match &self.source {
            SessionSource::Local { pty } => {
                if let Ok(mut pty) = pty.lock() {
                    let _ = pty.kill();
                    let _ = pty.wait();
                }
            }
            // Closing an attached tab is a clean detach: the host keeps the PTY
            // + terminal model alive for later reattach by id. (`Drop` on the
            // client is the backstop; this makes the intent explicit.)
            #[cfg(unix)]
            SessionSource::Attached { client } => {
                if let Ok(mut client) = client.lock() {
                    let _ = client.detach();
                }
            }
        }
        if let Some(thread) = self.pump_thread.take() {
            let _ = thread.join();
        }
        true
    }

    fn close_after_shell_exit(mut self) -> bool {
        let pump_thread = self.pump_thread.take();
        match &self.source {
            SessionSource::Local { pty } => {
                let pty = pty.clone();
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
            }
            // The host child already exited (or the link dropped). Detach is
            // best-effort and the pump thread is ending on its own; reap it.
            #[cfg(unix)]
            SessionSource::Attached { client } => {
                if let Ok(mut client) = client.lock() {
                    let _ = client.detach();
                }
                if let Some(thread) = pump_thread {
                    let _ = thread.join();
                }
            }
        }
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
    /// Zoom / toggle-fullscreen-pane state (tmux `Ctrl-b z`, §7 K2-zoom). When
    /// `true` the focused pane is rendered full-bleed across the whole content
    /// rect while the **layout tree underneath is preserved**, so un-zoom
    /// restores the exact prior geometry. A structural change (split, close,
    /// equalize) clears it. Zoom on a single-pane tab is meaningless and never
    /// set (the toggle is a no-op there), but every zoom-aware path also guards
    /// on pane count so a stray flag can never perturb the single-pane render.
    pub(super) zoomed: bool,
}

impl Tab {
    /// A single-pane tab wrapping one session.
    fn single(token: SessionToken) -> Self {
        Self {
            layout: PaneNode::leaf(token),
            focused: token,
            title_override: None,
            zoomed: false,
        }
    }

    /// True when this tab should render/resize as a single full-bleed pane: it
    /// is in zoom mode AND zoom is meaningful (multi-pane, focused leaf present).
    /// A zoomed single-pane tab is impossible (the toggle is a no-op there), but
    /// the pane-count guard keeps the single-pane fast path byte-identical even
    /// if the flag were ever set spuriously.
    fn is_effectively_zoomed(&self) -> bool {
        self.zoomed && !self.layout.is_single_pane() && self.layout.contains(self.focused)
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
    /// Whether output recording is currently enabled (`session_replay`). Newly
    /// spawned sessions inherit this so recording follows the live setting;
    /// [`Self::set_recording_enabled`] fans a toggle out to every session's
    /// recorder handle. Default off ⇒ the plain path is untouched.
    recording_enabled: bool,
    /// Local hostname injected into every terminal model so OSC 7
    /// `file://host/path` URLs from the local shell can update cwd while remote
    /// hosts remain rejected by the core.
    local_hostname: Option<String>,
    /// Whether newly spawned local default shells should receive OdyTTY's OSC
    /// 133 integration wrapper. Existing sessions are not modified.
    shell_integration_enabled: bool,
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
            recording_enabled: false,
            local_hostname: None,
            shell_integration_enabled: false,
        }
    }

    pub(super) fn set_local_hostname(&mut self, local_hostname: Option<String>) {
        self.local_hostname = local_hostname;
        for session in self.sessions.values() {
            if let Ok(mut terminal) = session.terminal.lock() {
                terminal.set_local_hostname(self.local_hostname.clone());
            }
        }
    }

    /// Enable or disable output recording across every session, and remember the
    /// state so later-spawned sessions inherit it. Off by default; toggling off
    /// clears each session's ring (freeing memory). Cheap and idempotent — the
    /// plain path never calls this with `true`.
    pub(super) fn set_recording_enabled(&mut self, on: bool) {
        self.recording_enabled = on;
        for session in self.sessions.values() {
            session.recorder.set_enabled(on);
        }
    }

    pub(super) fn set_shell_integration_enabled(&mut self, on: bool) {
        self.shell_integration_enabled = on;
    }

    /// A decoupled clone of the **focused** session's recorded frames, oldest
    /// first, for the replay overlay to scrub. Empty when recording is off or
    /// nothing has been recorded yet.
    pub(super) fn active_recorder_frames(&self) -> Vec<Snapshot> {
        self.active().recorder.frames_clone()
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

    /// Park the cursor-animation / render-hold timers of every pane that is not
    /// the active (rendered) one (NF20-B). The active pane keeps its live timers;
    /// its consumer (the frame path + about-to-wait maintenance, both operating
    /// on the `Deref` active session) advances them. Background panes get no such
    /// consumer, so their timers are settled here to keep them out of the wake
    /// set — the fan-out of the deadline sources in `next_wake_deadline` is thus
    /// matched by a consumer of equal reach. Idempotent; cheap (few panes).
    pub(super) fn park_background_timers(&mut self) {
        let active = self.active_focused_token();
        for (token, session) in self.sessions.iter_mut() {
            if *token != active {
                session.park_animation_timers();
            }
        }
    }

    /// Invalidate the layout-dependent UI state of EVERY session after a resize
    /// reflow (NF21-3). [`Self::resize_all_panes`] reflows all tabs' panes, so
    /// all their stale row/scrollback-coordinate state must be cleared too — the
    /// active-only clear in `App::apply_grid_resize` left background tabs with a
    /// selection / search / hints / copy-mode caret mapped to the pre-reflow
    /// layout, whose worst case is a silent wrong-bytes copy on switch-back.
    /// Mirrors [`Self::park_background_timers`]' all-session fan-out; unlike it,
    /// the active pane is included (its clear was the byte-identical old
    /// behavior, now sourced from the shared per-session helper).
    pub(super) fn invalidate_all_layout_dependent_state(&mut self) {
        for session in self.sessions.values_mut() {
            session.invalidate_layout_dependent_state();
        }
    }

    #[cfg(test)]
    pub(super) fn active_position(&self) -> usize {
        self.active_tab
    }

    pub(super) fn get_mut(&mut self, token: SessionToken) -> Option<&mut Session> {
        self.sessions.get_mut(&token)
    }

    /// Read access to a session by token (multi-pane render dispatch snapshots
    /// each visible pane's terminal through this).
    pub(super) fn get(&self, token: SessionToken) -> Option<&Session> {
        self.sessions.get(&token)
    }

    /// True when `token` is a currently visible pane of the **active** tab —
    /// i.e. its output should drive a redraw even when it is not the focused
    /// pane (design doc §2.5 audit row #4: redraw suppression must key on "any
    /// visible pane of the active tab", not just the focused one). For a
    /// single-pane tab this is exactly `active_id() == token`, so the
    /// single-pane redraw decision is unchanged.
    pub(super) fn is_visible_pane(&self, token: SessionToken) -> bool {
        match self.tabs.get(self.active_tab) {
            // While zoomed only the focused pane is on screen, so background
            // panes' output must not drive a redraw (it would not be visible).
            Some(tab) if tab.is_effectively_zoomed() => tab.focused == token,
            Some(tab) => tab.layout.contains(token),
            None => false,
        }
    }

    /// The (token, pixel-rect) layout of the **active** tab's panes within
    /// `content`, for the multi-pane render dispatch. Single-pane tabs yield one
    /// entry spanning the whole content rect — identical geometry to the
    /// single-pane path, which never calls this.
    pub(super) fn active_pane_rects(
        &self,
        content: PaneRect,
        divider_px: f32,
    ) -> Vec<(SessionToken, PaneRect)> {
        match self.tabs.get(self.active_tab) {
            // Zoomed tab: only the focused pane is rendered, spanning the whole
            // content rect (the layout tree underneath is untouched, so un-zoom
            // restores the prior geometry exactly).
            Some(tab) if tab.is_effectively_zoomed() => vec![(tab.focused, content)],
            Some(tab) => layout_rects(&tab.layout, content, divider_px),
            None => Vec::new(),
        }
    }

    /// The pane of the active tab under a pixel point, or `None` in a divider
    /// gap / outside content. Focus-follows-click resolves the clicked pane
    /// through this (design doc §4.3 / audit row #6).
    pub(super) fn active_pane_at_point(
        &self,
        content: PaneRect,
        divider_px: f32,
        x: f32,
        y: f32,
    ) -> Option<SessionToken> {
        let rects = self.active_pane_rects(content, divider_px);
        pane_at_point(&rects, x, y)
    }

    /// Move focus within the active tab to the spatial neighbor of the focused
    /// pane in direction `dir` (tmux `Ctrl-b` arrows, §4.3 / §7). Builds the
    /// pane rects within `content` and resolves the neighbor via
    /// [`layout::focus_move`]. Returns true if focus changed.
    pub(super) fn focus_move_active(
        &mut self,
        content: PaneRect,
        divider_px: f32,
        dir: FocusDir,
    ) -> bool {
        let focused = self.active_id();
        let rects = self.active_pane_rects(content, divider_px);
        match focus_move(&rects, focused, dir) {
            Some(target) => self.set_active_focus(target),
            None => false,
        }
    }

    /// The tree-order index of the active tab's divider under a pixel point
    /// (widened by `grab_px`), to start a divider drag. `None` when no divider
    /// is grabbed.
    pub(super) fn active_divider_at_point(
        &self,
        content: PaneRect,
        divider_px: f32,
        x: f32,
        y: f32,
        grab_px: f32,
    ) -> Option<usize> {
        self.tabs
            .get(self.active_tab)
            // No dividers are drawn while zoomed, so none can be grabbed.
            .filter(|tab| !tab.is_effectively_zoomed())
            .and_then(|tab| divider_at_point(&tab.layout, content, divider_px, x, y, grab_px))
    }

    /// The [`SplitAxis`] of the active tab's divider under a pixel point (widened
    /// by `grab_px`), or `None` when the point is over no divider. Drives the
    /// hover resize-cursor affordance (`ColResize` for a column split's vertical
    /// divider, `RowResize` for a row split's horizontal one). Mirrors
    /// [`Self::active_divider_at_point`]'s zoom and hit-test gating so hover and
    /// grab agree. A single-pane tab has no dividers, so this is always `None`
    /// there — the byte-identical path never sees a resize cursor.
    pub(super) fn active_divider_axis_at_point(
        &self,
        content: PaneRect,
        divider_px: f32,
        x: f32,
        y: f32,
        grab_px: f32,
    ) -> Option<SplitAxis> {
        self.tabs
            .get(self.active_tab)
            .filter(|tab| !tab.is_effectively_zoomed())
            .and_then(|tab| divider_axis_at_point(&tab.layout, content, divider_px, x, y, grab_px))
    }

    /// The [`SplitAxis`] of the active tab's divider at tree-order `idx` (the
    /// index a divider drag started from), or `None` when no such divider
    /// exists. Lets an in-progress drag keep showing the matching resize cursor
    /// even when the pointer strays off the hairline. Same pre-order numbering
    /// as [`Self::active_divider_at_point`].
    pub(super) fn active_divider_axis(
        &self,
        content: PaneRect,
        divider_px: f32,
        idx: usize,
    ) -> Option<SplitAxis> {
        self.tabs
            .get(self.active_tab)
            .and_then(|tab| {
                divider_rects_with_axis(&tab.layout, content, divider_px)
                    .into_iter()
                    .nth(idx)
            })
            .map(|(_, axis)| axis)
    }

    /// Drag the active tab's divider at tree-order `target` to a pixel point,
    /// re-deriving and clamping that split's ratio. Returns the new ratio when
    /// the split exists. Caller reflows the affected panes afterward.
    pub(super) fn drag_active_divider(
        &mut self,
        content: PaneRect,
        divider_px: f32,
        target: usize,
        x: f32,
        y: f32,
    ) -> Option<f32> {
        self.tabs
            .get_mut(self.active_tab)
            .and_then(|tab| drag_divider_to(&mut tab.layout, content, divider_px, target, x, y))
    }

    /// Snap the active tab's `target` divider onto a whole-cell boundary,
    /// returning the snapped ratio when the split exists. Called once on drag
    /// release so every rest position leaves identical outer margins; the caller
    /// reflows the affected panes afterward (same path the drag uses).
    pub(super) fn snap_active_divider(
        &mut self,
        content: PaneRect,
        divider_px: f32,
        target: usize,
        cell_w: u32,
        cell_h: u32,
    ) -> Option<f32> {
        self.tabs.get_mut(self.active_tab).and_then(|tab| {
            snap_divider_to_cells(&mut tab.layout, content, divider_px, target, cell_w, cell_h)
        })
    }

    /// Resize **every pane of every tab** to its laid-out cell dimensions within
    /// `content`, reflowing each pane's terminal model and PTY. For an all-
    /// single-pane world every tab's lone leaf spans `content`, so each session
    /// is sized to exactly the dimensions the old per-session resize loop
    /// produced — the single-pane path stays byte-identical. Multi-pane tabs get
    /// each pane sized to its own sub-rect (design doc §2.5 audit row #1).
    pub(super) fn resize_all_panes(
        &mut self,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
    ) {
        self.resize_all_panes_impl(content, cell_w, cell_h, divider_px, true);
    }

    /// Reflow every pane's terminal **model + cell metrics** to the laid-out
    /// dimensions WITHOUT issuing the kernel-side PTY (`TIOCSWINSZ`) / attached
    /// (`Resize` frame) resize. The on-screen grid reflows live, but the shell
    /// is NOT told its size changed.
    ///
    /// COALESCING (Phase H decision gate → option (a), flush-on-release): a
    /// divider drag fires one pointer-move event per pixel. Routing each through
    /// the full `resize_all_panes` flooded the shell with one
    /// `ResizePseudoConsole`/`SIGWINCH` per move — on Windows ConPTY that
    /// scrambles PSReadLine's prompt as it repaints mid-resize. So the live
    /// per-move drag reflows the model only (this method) and the left-release
    /// handler flushes exactly one real `resize_all_panes` (PTY included) at
    /// drag-end, when the final size is settled. See `App::drag_divider_to_pointer`
    /// and the RELEASE-SNAP block in `app::pointer`.
    pub(super) fn reflow_all_panes_for_drag(
        &mut self,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
    ) {
        self.resize_all_panes_impl(content, cell_w, cell_h, divider_px, false);
    }

    /// Shared body of [`resize_all_panes`] (`resize_pty = true`) and
    /// [`reflow_all_panes_for_drag`] (`resize_pty = false`). The model + cell-
    /// metrics reflow is identical for both; only the kernel-side resize is
    /// gated, so every existing `resize_all_panes` caller stays byte-identical.
    fn resize_all_panes_impl(
        &mut self,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
        resize_pty: bool,
    ) {
        for tab in &self.tabs {
            // A zoomed tab sizes its focused pane to the whole content rect
            // (it is rendered full-bleed); background panes keep their layout
            // sub-rect so un-zoom is instantly correct without a second reflow.
            let zoomed = tab.is_effectively_zoomed();
            for (token, rect) in layout_rects(&tab.layout, content, divider_px) {
                let rect = if zoomed && token == tab.focused {
                    content
                } else {
                    rect
                };
                let (cols, rows) = grid_dims_for_rect(rect, cell_w, cell_h);
                let Some(session) = self.sessions.get(&token) else {
                    continue;
                };
                if let Ok(mut terminal) = session.terminal.lock() {
                    // A resize to identical grid dimensions MUST be a model
                    // no-op. `resize_all_panes` runs on every structural change
                    // (split / close / equalize / window resize), and for a
                    // split it touches EVERY pane of the tab — including panes
                    // the split did not actually resize. Calling
                    // `terminal.resize(cols, rows)` with unchanged dimensions
                    // still drives the column reflow, whose trailing-blank trim
                    // shifts a shell's printed prompt-trailing space and drags
                    // the cursor one column left (the v0.3.0 fish `❯ ` bug). The
                    // PTY size is unchanged for such a pane, so no SIGWINCH
                    // reaches its shell to repaint and self-correct; the offset
                    // sticks until an unrelated real resize. Guarding the resize
                    // on a genuine dimension change keeps the untouched pane's
                    // cells + cursor byte-identical. Cell *pixel* metrics are
                    // still applied unconditionally — they never reflow columns.
                    let current = terminal.screen().dimensions();
                    if current.columns != cols || current.rows != rows {
                        terminal.resize(cols, rows);
                    }
                    terminal.set_cell_metrics(cell_w, cell_h);
                }
                // Route the kernel-side resize to whichever source backs the
                // session: a local PTY gets TIOCSWINSZ (byte-identical to before
                // Phase 2); an attached session forwards a `Resize` frame so the
                // host applies TIOCSWINSZ + reflow on its side. Skipped entirely
                // for the live divider-drag path (`resize_pty = false`), which
                // reflows the model only and lets the release handler issue the
                // single coalesced kernel resize at drag-end.
                if resize_pty {
                    match &session.source {
                        SessionSource::Local { pty } => {
                            if let Ok(pty) = pty.lock() {
                                // Feed the live cell metric so TIOCSWINSZ reports
                                // a real ws_xpixel/ws_ypixel (C23), then resize.
                                pty.set_cell_metrics(crate::core::CellMetrics::new(cell_w, cell_h));
                                let _ = pty.resize(crate::core::Dimensions::new(cols, rows));
                            }
                        }
                        #[cfg(unix)]
                        SessionSource::Attached { client } => {
                            if let Ok(mut client) = client.lock() {
                                let _ = client.resize(cols as u32, rows as u32);
                            }
                        }
                    }
                }
            }
        }
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

    /// Spawn a shell + terminal at `grid` and insert it into the arena,
    /// **without** attaching it to any tab. Shared by [`Self::spawn`] (which
    /// then opens a new tab) and [`Self::split_active`] (which then grafts the
    /// session into the active tab's layout tree as a new pane). The caller owns
    /// tab/pane wiring.
    fn insert_spawned_session(
        &mut self,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        let shell_integration = self.shell_integration_enabled;
        self.insert_local_session_with(grid, |grid| {
            let settings = crate::settings::Settings {
                shell_integration,
                ..crate::settings::Settings::default()
            };
            PtySession::spawn_default_shell_in_with_settings(grid, None, &settings)
        })
    }

    /// Spawn an explicit child command in a local PTY and insert it into the
    /// arena without attaching it to a tab. Used by the SSH connect action; the
    /// shell/new-pane path above remains unchanged.
    fn insert_exec_session(
        &mut self,
        grid: crate::core::Dimensions,
        program: OsString,
        args: Vec<OsString>,
    ) -> Result<SessionToken, std::io::Error> {
        self.insert_local_session_with(grid, |grid| {
            PtySession::spawn_exec(grid, program, args, None)
        })
    }

    fn insert_local_session_with(
        &mut self,
        grid: crate::core::Dimensions,
        spawn: impl FnOnce(crate::core::Dimensions) -> anyhow::Result<PtySession>,
    ) -> Result<SessionToken, std::io::Error> {
        let Some(proxy) = self.proxy.clone() else {
            return Err(std::io::Error::other(
                "session spawn unavailable without event loop proxy",
            ));
        };
        let session_id = SessionToken(self.next_token);
        self.next_token = self.next_token.saturating_add(1);
        let session = spawn(grid).map_err(std::io::Error::other)?;
        let reader = session.try_clone_reader().map_err(std::io::Error::other)?;
        let writer: PtyWriter = Arc::new(Mutex::new(
            session.take_writer().map_err(std::io::Error::other)?,
        ));
        let mut model = Terminal::new(grid.columns, grid.rows);
        model.set_local_hostname(self.local_hostname.clone());
        seed_initial_working_directory(&mut model, None);
        // Defer resize cursor placement to the shell when the backend repaints
        // absolutely (ConPTY on Windows). The POSIX PTY backend returns false,
        // so Linux/macOS keep translating the cursor on resize as today. Funneled
        // through the shared helper so the startup-pane path stays in lockstep.
        apply_local_backend_caps(&mut model, &session);
        let terminal = Arc::new(Mutex::new(model));
        // One recorder handle shared between the pump (writer) and the session
        // (reader). Inherits the current recording-enabled state so a session
        // spawned while replay is on starts recording immediately; otherwise it
        // is a no-op handle. The pump only records while enabled.
        let recorder = RecorderHandle::new();
        recorder.set_enabled(self.recording_enabled);
        let diagnostic = session.pending_diagnostic_slot();
        let pump_thread = spawn_pty_pump(
            reader,
            writer.clone(),
            terminal.clone(),
            proxy,
            session_id,
            recorder.clone(),
            diagnostic,
        );
        let pty = Arc::new(Mutex::new(session));
        self.sessions.insert(
            session_id,
            Session::from_parts_with_recorder(
                session_id,
                terminal,
                writer,
                SessionSource::Local { pty },
                Some(pump_thread),
                recorder,
            ),
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

    /// Spawn `ssh` for a resolved connection entry in a brand-new single-pane
    /// tab. The argv is built by `crate::ssh_connect` from name-only fields and
    /// execs the system `ssh` binary directly: OdyTTY never reads, stores,
    /// prompts for, or forwards credentials/key material.
    pub(super) fn connect_ssh_in_new_tab(
        &mut self,
        host: &ConnectionHost,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        let command = ssh_command_for_host(host).map_err(std::io::Error::other)?;
        let title = host.title.clone().unwrap_or_else(|| host.alias.clone());
        self.spawn_ssh_command_in_new_tab(grid, command, Some(title))
    }

    fn spawn_ssh_command_in_new_tab(
        &mut self,
        grid: crate::core::Dimensions,
        command: SshCommand,
        title_override: Option<String>,
    ) -> Result<SessionToken, std::io::Error> {
        let (program, args) = command.into_program_args();
        let session_id = self.insert_exec_session(grid, program, args)?;
        self.tabs.push(Tab::single(session_id));
        if let Some(title) = title_override {
            self.set_title_override(session_id, Some(title));
        }
        Ok(session_id)
    }

    /// Attach to a detached, session-host-backed session by id and present it as
    /// a new single-pane tab — the live "close window, reopen by id, full
    /// scrollback intact" path. Resolves the id to its per-user socket (CLI
    /// parity), connects + restores the mirror terminal from the host snapshot,
    /// spawns the read pump, and inserts an attached [`Session`] whose input
    /// writer forwards to the same socket. `runtime_base` is `None` in
    /// production; tests pass an explicit base. The new tab is appended and not
    /// focused here (the caller switches to it), matching `spawn`.
    #[cfg(unix)]
    pub(super) fn attach_in_new_tab(
        &mut self,
        runtime_base: Option<&Path>,
        session_id: &str,
    ) -> Result<SessionToken, std::io::Error> {
        let Some(proxy) = self.proxy.clone() else {
            return Err(std::io::Error::other(
                "session attach unavailable without event loop proxy",
            ));
        };
        let socket =
            resolve_session_socket(runtime_base, session_id).map_err(std::io::Error::other)?;
        self.insert_attached_session(&socket, session_id, proxy)
    }

    /// Connect to a hosted session over `socket`, restore the mirror terminal,
    /// spawn the read pump driven by `sink`, and graft it in as a new single-pane
    /// tab. Shared by production [`Self::attach_in_new_tab`] (sink = winit proxy)
    /// and the headless test seam (sink = channel), so the present/repaint path is
    /// exercisable without an event loop.
    #[cfg(unix)]
    fn insert_attached_session(
        &mut self,
        socket: &Path,
        session_id: &str,
        sink: impl super::attach::AttachEventSink,
    ) -> Result<SessionToken, std::io::Error> {
        let (client, reader, terminal) =
            AttachClient::connect(socket, session_id).map_err(std::io::Error::other)?;

        let token = SessionToken(self.next_token);
        self.next_token = self.next_token.saturating_add(1);

        let mut terminal = terminal;
        terminal.set_local_hostname(self.local_hostname.clone());
        let terminal = Arc::new(Mutex::new(terminal));
        let client = Arc::new(Mutex::new(client));
        let writer = attach_input_writer(client.clone());
        let pump_thread = spawn_attach_pump(reader, terminal.clone(), sink, token);
        self.sessions.insert(
            token,
            Session::new_attached(
                token,
                terminal,
                writer,
                client,
                session_id,
                Some(pump_thread),
            ),
        );
        self.tabs.push(Tab::single(token));
        Ok(token)
    }

    /// Headless test driver for the live attach-by-id path: resolves the id to a
    /// socket (CLI parity) and presents an attached tab driven by a caller-
    /// provided sink, so the full resolve → connect → restore → present flow is
    /// testable without a winit event loop.
    #[cfg(all(test, unix))]
    pub(in crate::native) fn attach_in_new_tab_for_test(
        &mut self,
        runtime_base: Option<&Path>,
        session_id: &str,
        sink: impl super::attach::AttachEventSink,
    ) -> Result<SessionToken, std::io::Error> {
        let socket =
            resolve_session_socket(runtime_base, session_id).map_err(std::io::Error::other)?;
        self.insert_attached_session(&socket, session_id, sink)
    }

    /// The focused-pane token of the tab at `position` in the strip.
    pub(super) fn token_at_position(&self, position: usize) -> Option<SessionToken> {
        self.tabs.get(position).map(|tab| tab.focused)
    }

    /// The strip index of the tab that contains `token` as one of its panes.
    pub(super) fn position_of_token(&self, token: SessionToken) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.layout.contains(token))
    }

    /// The token of an already-open session attached by host id `session_id`, if
    /// any (Phase 14 attach dedup). Scans every session for an attached one whose
    /// recorded host id matches, so a re-selected session can switch to its
    /// existing tab instead of appending a duplicate. `None` when no open tab
    /// mirrors that host.
    pub(super) fn find_attached_tab(&self, session_id: &str) -> Option<SessionToken> {
        self.sessions
            .iter()
            .find(|(_, session)| session.attached_session_id.as_deref() == Some(session_id))
            .map(|(token, _)| *token)
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

    /// Close the **entire active tab** — reap every leaf session in its layout
    /// tree and remove the tab from the strip — regardless of how many panes it
    /// holds. This is the "Close Tab" semantics (Director, explicit): closing a
    /// tab closes the tab you are in even when it holds multiple panes, and it
    /// must not behave like "Close Pane".
    ///
    /// Distinct from [`Self::close`] / `close_focused_pane`, which collapse a
    /// single leaf into its sibling and keep a multi-pane tab alive. For a
    /// single-pane tab this reaps the one session and removes the tab —
    /// byte-identical to the old `close(active_id())` path (the `None` branch of
    /// [`Self::close_with`]).
    ///
    /// Returns `true` iff no tabs remain afterward, i.e. the last tab was
    /// closed and the caller should signal app exit. Exit keys on the **last
    /// tab**, never on the last pane.
    pub(super) fn close_active_tab(&mut self) -> bool {
        self.close_tab_at(self.active_tab)
    }

    /// Close the tab at strip index `tab_idx` — reap every leaf session in its
    /// layout tree and remove the tab — regardless of pane count. This is the
    /// index-aware reap shared by every "Close Tab" entry point: the menu /
    /// keyboard ([`Self::close_active_tab`], which delegates here with the
    /// active index) and the tab-strip `×` button, which can target a
    /// **non-active** tab.
    ///
    /// Fixes `active_tab` exactly like `close_with`'s `None` branch: when the
    /// closed tab was the active one (or to its left) the active index shifts so
    /// it still points at a live tab; closing a tab to the right of the active
    /// one leaves the active index unchanged. Returns `true` iff no tabs remain.
    ///
    /// For a single-pane tab this reaps the one session and removes the tab —
    /// byte-identical to the old `close(token)` path the `×` button used.
    pub(super) fn close_tab_at(&mut self, tab_idx: usize) -> bool {
        // Collect every owned leaf token first (owned `Vec`, so the immutable
        // borrow of `self.tabs` ends before the reap loop mutates `self`).
        let tokens = match self.tabs.get(tab_idx) {
            Some(tab) => tab.layout.leaves(),
            None => return self.tabs.is_empty(),
        };
        for token in tokens {
            if let Some(session) = self.sessions.remove(&token) {
                let _ = session.close();
            }
        }
        let was_active = self.active_tab == tab_idx;
        self.tabs.remove(tab_idx);
        if self.tabs.is_empty() {
            self.active_tab = 0;
            return true;
        }
        // Mirror `close_with`'s `None` branch: clamp the active index when the
        // active (or an earlier) tab was removed, leave it untouched when a
        // later tab was closed.
        if was_active {
            self.active_tab = tab_idx.min(self.tabs.len() - 1);
        } else if self.active_tab > tab_idx {
            self.active_tab -= 1;
        }
        false
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
                // Closing a pane changes the tree; un-zoom so the survivor(s)
                // render at their layout geometry. Closing the zoomed pane must
                // un-zoom (Director's explicit case), and closing a background
                // pane while zoomed also re-tiles, so clear unconditionally.
                self.tabs[tab_idx].zoomed = false;
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
    /// production [`Self::split_active`] minus the PTY spawn. `pub(in
    /// crate::native)` so App-level seams can seed a multi-pane tab headlessly
    /// (the production split needs an event-loop proxy to spawn a PTY).
    #[cfg(test)]
    pub(in crate::native) fn split_active_for_test(
        &mut self,
        axis: SplitAxis,
        session: Session,
    ) -> SessionToken {
        let token = self.push_arena_only(session);
        self.split_active_with(axis, token);
        token
    }

    #[cfg(test)]
    pub(in crate::native) fn spawn_ssh_command_in_new_tab_for_test(
        &mut self,
        grid: crate::core::Dimensions,
        command: SshCommand,
    ) -> Result<SessionToken, std::io::Error> {
        self.spawn_ssh_command_in_new_tab(grid, command, Some("synthetic ssh".to_owned()))
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
        // Splitting changes the tree, so any prior zoom no longer applies
        // (tmux un-zooms on split).
        tab.zoomed = false;
    }

    /// Reset every split ratio in the active tab to even (tmux `Ctrl-b =`).
    pub(super) fn equalize_active(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            let layout = std::mem::replace(&mut tab.layout, PaneNode::leaf(tab.focused));
            tab.layout = layout.equalized();
            // Equalize re-tiles every pane, so a zoom (which hides all but one)
            // is cleared — the user asked to see the balanced layout.
            tab.zoomed = false;
        }
    }

    /// Toggle zoom (tmux `Ctrl-b z`) on the active tab. A no-op on a single-pane
    /// tab (zoom is meaningless), where it returns `false` so the caller skips
    /// the reflow/redraw. Returns `true` when the zoom state flipped. The layout
    /// tree is never mutated — only the `zoomed` flag — so un-zoom restores the
    /// prior geometry exactly.
    pub(super) fn toggle_active_zoom(&mut self) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        if tab.layout.is_single_pane() {
            return false;
        }
        tab.zoomed = !tab.zoomed;
        true
    }

    /// True when the active tab is rendering one pane full-bleed (zoom mode).
    /// Drives the render path's divider suppression and the redraw decision.
    pub(super) fn active_is_zoomed(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .map(Tab::is_effectively_zoomed)
            .unwrap_or(false)
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

    /// True when there is exactly one tab and it carries a custom
    /// `title_override` (F4 ODP-7 / F4-NF1). The tab bar's show rule uses this
    /// so a single renamed "workflow" tab is visible even below the usual
    /// two-tab threshold.
    pub(super) fn lone_tab_has_title_override(&self) -> bool {
        self.tabs.len() == 1
            && self
                .tabs
                .first()
                .is_some_and(|tab| tab.title_override.is_some())
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
    use crate::native::test_support::spawn_test_pause_shell;
    use winit::event_loop::EventLoop;
    #[cfg(target_os = "linux")]
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    #[cfg(target_os = "windows")]
    use winit::platform::windows::EventLoopBuilderExtWindows;
    #[cfg(target_os = "linux")]
    use winit::platform::x11::EventLoopBuilderExtX11;

    fn build_session_with_id(id: SessionToken) -> Session {
        let dims = Dimensions::new(20, 8);
        let pty = spawn_test_pause_shell(dims).expect("spawn test shell");
        let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().expect("writer")));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(pty));
        Session::new(id, terminal, writer, pty, None)
    }

    fn build_session() -> Session {
        build_session_with_id(SessionToken(0))
    }

    fn tabset_with_proxy_for_test() -> Option<(TabSet, EventLoop<UserEvent>)> {
        let mut builder = EventLoop::<UserEvent>::with_user_event();
        #[cfg(target_os = "linux")]
        {
            EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
            EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
        }
        #[cfg(target_os = "windows")]
        {
            EventLoopBuilderExtWindows::with_any_thread(&mut builder, true);
        }
        let event_loop = builder.build().ok()?;
        let proxy = event_loop.create_proxy();
        Some((TabSet::new(build_session(), Some(proxy)), event_loop))
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

    // macOS forbids constructing a winit `EventLoop` off the main thread
    // (winit panics: "Initializing the event loop outside of the main thread is
    // a significant cross-platform compatibility hazard"). `cargo test` runs
    // each test on a worker thread, and Linux/Windows offer
    // `with_any_thread(true)` to opt out of that check while macOS does not.
    // This test only needs a real `EventLoopProxy` so the connect action can
    // spawn a PTY-backed session; there is no headless seam for the concrete
    // winit proxy type without abstracting the whole PTY-pump wake path, so it
    // is ignored on macOS as an accepted v0.3.0 stopgap. The connect/spawn
    // logic stays covered on Linux CI, with a Windows command arm ready for
    // Phase 4 CI once the remaining Windows compile gates clear.
    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[test]
    fn spawned_local_pane_wires_shell_owns_cursor_from_backend() {
        // Guards the split/new-tab local-pane path (`insert_local_session_with`,
        // via `spawn`): the pane's terminal must carry the backend's resize
        // cursor-authority capability. On Windows CI the spawned ConPTY backend
        // returns true (≠ the model default false), so a missing/incorrect wire
        // FAILS here; on Linux both are false (byte-identical), so this is the
        // cross-platform funnel guard that only Windows can fully exercise.
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let token = sessions
            .spawn(Dimensions::new(20, 8))
            .expect("spawn local session");
        assert!(sessions.switch(token));

        let session = sessions.active();
        let expected = session
            .local_pty()
            .expect("spawned pane is local")
            .lock()
            .expect("pty lock")
            .shell_repaints_on_resize();
        let wired = session
            .terminal
            .lock()
            .expect("terminal lock")
            .shell_owns_cursor_on_resize();
        assert_eq!(
            wired, expected,
            "spawned pane must wire shell_owns_cursor_on_resize from the backend capability"
        );

        assert!(!sessions.close(token));
        assert!(sessions.close(SessionToken(0)));
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[test]
    fn spawned_local_pane_seeds_working_directory_before_osc7() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let expected = std::env::current_dir()
            .expect("current dir")
            .to_string_lossy()
            .into_owned();

        let token = sessions
            .spawn(Dimensions::new(20, 8))
            .expect("spawn local session");
        assert!(sessions.switch(token));

        let session = sessions.active();
        {
            let terminal = session.terminal.lock().expect("terminal lock");
            assert_eq!(
                terminal.current_working_directory(),
                Some(expected.as_str()),
                "new local panes must know their inherited spawn cwd before the first OSC 7"
            );
        }

        {
            let mut terminal = session.terminal.lock().expect("terminal lock");
            terminal.advance(b"\x1b]7;file:///tmp/odytty-osc7-updated\x07");
            assert_eq!(
                terminal.current_working_directory(),
                Some("/tmp/odytty-osc7-updated"),
                "OSC 7 remains authoritative after the spawn cwd seed"
            );
        }

        assert!(!sessions.close(token));
        assert!(sessions.close(SessionToken(0)));
    }

    #[test]
    fn resize_all_panes_honors_shell_owns_cursor_through_app_entry_point() {
        // END-TO-END guard for the path the OPERATOR'S window actually drives.
        // The Screen-unit `shell_owns_cursor_setter_getter_behavior_tie` proves
        // `Terminal::resize` honors the flag; this proves the flag survives and
        // is honored when the resize is driven through `resize_all_panes` — the
        // exact entry point the App calls on every `Resized` event. It closes
        // the gap between "the flag is SET on the session terminal at creation"
        // (the spawned-pane wire test) and "the flag is HONORED in the resize
        // the operator sees," which the Windows on-device cursor-translation
        // trace put in question.
        //
        // Both arms set up the identical wrapped buffer at 4x3 ("$ hello" →
        // "$ he" / "llo", cursor on the continuation row), then drive a
        // width-changing resize to 20x3 THROUGH `resize_all_panes` (cell 10x20,
        // content 200x60 → 20 cols x 3 rows for the single pane). A translation
        // would land the cursor at end-of-content (0,7); a clamp keeps it at the
        // incoming continuation position (1,3). The two outcomes differ, so the
        // assertion cannot pass by coincidence.
        use crate::core::Position;
        let content = PaneRect::new(0.0, 0.0, 200.0, 60.0);
        let (cell_w, cell_h, divider_px) = (10u32, 20u32, 1.0f32);

        // Shared setup: build a single-pane TabSet, force the pane's terminal to
        // the wrapped 4x3 state, and return the incoming (pre-resize) cursor.
        let setup = |shell_owns: bool| -> (TabSet, Position) {
            let sessions = TabSet::new(build_session(), None);
            let incoming = {
                let session = sessions.active();
                let mut terminal = session.terminal.lock().expect("terminal lock");
                terminal.set_shell_owns_cursor_on_resize(shell_owns);
                terminal.resize(4, 3);
                terminal.advance(b"$ hello");
                terminal.screen().cursor()
            };
            (sessions, incoming)
        };

        // DEFAULT (false): the App resize path TRANSLATES the cursor to
        // end-of-content — the historical Linux/POSIX behavior and the exact
        // symptom captured on Windows on-device when the flag is not live at
        // resize time.
        let (mut translate, incoming) = setup(false);
        assert_eq!(
            incoming,
            Position { row: 1, column: 3 },
            "pre-resize wrapped state must put the cursor on the continuation row"
        );
        translate.resize_all_panes(content, cell_w, cell_h, divider_px);
        let translated = {
            let session = translate.active();
            let terminal = session.terminal.lock().expect("terminal lock");
            terminal.screen().cursor()
        };
        assert_eq!(
            translated,
            Position { row: 0, column: 7 },
            "default path must translate the cursor through resize_all_panes \
             (this reproduces the on-device symptom when the flag is false)"
        );

        // SHELL-OWNS (true, the ConPTY/Windows capability): the App resize path
        // must DEFER — keep the incoming cursor clamped to the new dims for the
        // shell's absolute repaint to own. This is the assertion that fails if
        // any layer between the wired terminal and `resize_all_panes` drops or
        // ignores the flag.
        let (mut defer, incoming_defer) = setup(true);
        assert_eq!(incoming_defer, incoming, "identical pre-resize state");
        defer.resize_all_panes(content, cell_w, cell_h, divider_px);
        let (deferred, flag_survived) = {
            let session = defer.active();
            let terminal = session.terminal.lock().expect("terminal lock");
            (
                terminal.screen().cursor(),
                terminal.shell_owns_cursor_on_resize(),
            )
        };
        assert_eq!(
            deferred,
            Position {
                row: incoming.row.min(2),
                column: incoming.column.min(19),
            },
            "shell-owns path must defer (clamp) the cursor through resize_all_panes, \
             not translate it"
        );
        assert!(
            flag_survived,
            "resize_all_panes must not clobber the shell_owns_cursor capability"
        );
        assert_ne!(
            deferred, translated,
            "clamp and translate must differ, or the guard is vacuous"
        );
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[test]
    fn connect_action_spawns_new_session_with_stub_command() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        #[cfg(not(windows))]
        let command = SshCommand::new(
            "/bin/sh",
            vec![
                OsString::from("-lc"),
                OsString::from("printf 'synthetic ssh child\\n'; sleep 1"),
            ],
        );
        #[cfg(windows)]
        let command = SshCommand::new(
            "cmd.exe",
            vec![
                OsString::from("/C"),
                OsString::from("echo synthetic ssh child & ping -n 2 127.0.0.1 >NUL"),
            ],
        );

        let token = sessions
            .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), command)
            .expect("stub command session");

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.tab_count(), 2);
        assert_eq!(sessions.effective_tab_title(token), "synthetic ssh");
        assert!(sessions.switch(token));
        assert_eq!(sessions.active_id(), token);

        assert!(!sessions.close(token));
        assert!(sessions.close(SessionToken(0)));
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
    fn close_active_tab_reaps_the_whole_multi_pane_tab() {
        // tab0 = two panes (sessions 0 + 1); tab1 = single pane (session 2).
        let mut set = TabSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        set.push(build_session_with_id(SessionToken(2)));
        assert_eq!(set.tab_count(), 2);
        assert_eq!(set.len(), 3, "three sessions across two tabs");
        // The active tab (tab0) is multi-pane.
        assert!(!set.active_is_single_pane());

        // "Close Tab" removes the ENTIRE active tab — both leaf sessions reaped,
        // the tab gone — not just the focused pane.
        let last = set.close_active_tab();
        assert!(!last, "another tab remains, so not the last tab");
        assert_eq!(set.tab_count(), 1, "the whole multi-pane tab was removed");
        assert_eq!(set.len(), 1, "both panes of the closed tab were reaped");
        // The survivor is tab1's session, now the active single-pane tab.
        assert_eq!(set.active_id(), SessionToken(2));
        assert!(set.active_is_single_pane());
    }

    #[test]
    fn close_active_tab_differs_from_close_pane_on_a_multi_pane_tab() {
        // Two structurally identical multi-pane sets; one gets Close Tab, the
        // other Close Pane. Prove the outcomes differ (the operator's core bug:
        // Close Tab must not behave like Close Pane).
        let build = || {
            let mut set = TabSet::new(build_session(), None);
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
            set.push(build_session_with_id(SessionToken(2)));
            set
        };

        // Close Tab: the multi-pane tab is gone entirely.
        let mut close_tab = build();
        close_tab.close_active_tab();
        assert_eq!(close_tab.tab_count(), 1);
        assert_eq!(close_tab.active_pane_count(), 1);

        // Close Pane: collapses one leaf, the multi-pane tab SURVIVES as single.
        let mut close_pane = build();
        close_pane.close(close_pane.active_id());
        assert_eq!(close_pane.tab_count(), 2, "Close Pane keeps the tab");
        // The formerly multi-pane tab is now single-pane but still present.
        assert!(close_pane.active_is_single_pane());

        // The defining contrast: same starting state, different tab counts.
        assert_ne!(close_tab.tab_count(), close_pane.tab_count());
    }

    #[test]
    fn close_active_tab_on_the_last_tab_signals_exit_even_when_multi_pane() {
        // A single tab holding multiple panes: Close Tab on it is the last tab,
        // so it empties the set (the App maps this to app exit). Exit keys on
        // the last TAB, never on the last pane.
        let mut set = TabSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert_eq!(set.tab_count(), 1);
        assert!(!set.active_is_single_pane());

        let last = set.close_active_tab();
        assert!(last, "closing the sole tab empties the set");
        assert!(set.is_empty());
    }

    #[test]
    fn close_active_tab_on_a_single_pane_tab_matches_close_active_id() {
        // Single-pane byte-identical proof: Close Tab on a single-pane tab does
        // exactly what the old `close(active_id())` path did — same surviving
        // session, same active token, same tab count.
        let mut via_close_tab = TabSet::new(build_session(), None);
        via_close_tab.push(build_session_with_id(SessionToken(1)));
        let mut via_close_id = TabSet::new(build_session(), None);
        via_close_id.push(build_session_with_id(SessionToken(1)));

        let last_a = via_close_tab.close_active_tab();
        let last_b = via_close_id.close(via_close_id.active_id());
        assert_eq!(last_a, last_b);
        assert_eq!(via_close_tab.tab_count(), via_close_id.tab_count());
        assert_eq!(via_close_tab.active_id(), via_close_id.active_id());
        assert_eq!(via_close_tab.len(), via_close_id.len());
    }

    #[test]
    fn close_tab_at_reaps_a_non_active_multi_pane_tab_and_leaves_active_untouched() {
        // tab0 = single pane (session 0, active); tab1 = two panes (sessions 2
        // + 1), NON-active. The tab-strip `×` can target tab1 while tab0 is
        // active — it must reap the WHOLE tab1 and leave tab0 (and the active
        // index) untouched.
        let mut set = TabSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(2))); // tab1, single
        assert!(set.switch(SessionToken(2))); // activate tab1
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert!(set.switch(SessionToken(0))); // back to tab0 (active)
        assert_eq!(set.tab_count(), 2);
        assert_eq!(set.len(), 3);
        assert_eq!(set.active_id(), SessionToken(0));
        assert!(set.active_is_single_pane());

        // Close the NON-active multi-pane tab1 by index.
        let last = set.close_tab_at(1);
        assert!(!last, "tab0 remains");
        assert_eq!(set.tab_count(), 1, "the whole non-active tab was removed");
        assert_eq!(set.len(), 1, "both panes of tab1 were reaped");
        // The active tab0 is unchanged: same session, still active, still single.
        assert_eq!(set.active_id(), SessionToken(0));
        assert!(set.active_is_single_pane());
    }

    #[test]
    fn close_tab_at_a_later_index_keeps_the_active_index_stable() {
        // active = tab0; closing tab2 (to the right) must not shift the active
        // index, and closing tab0 (the active one) clamps the active index.
        let mut set = TabSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1))); // tab1
        set.push(build_session_with_id(SessionToken(2))); // tab2
        assert_eq!(set.active_id(), SessionToken(0)); // tab0 active
        // Close the rightmost tab: active stays on tab0.
        assert!(!set.close_tab_at(2));
        assert_eq!(set.active_id(), SessionToken(0));
        assert_eq!(set.tab_count(), 2);
        // Close the active tab0: active clamps onto the survivor (old tab1).
        assert!(!set.close_tab_at(0));
        assert_eq!(set.active_id(), SessionToken(1));
        assert_eq!(set.tab_count(), 1);
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
    fn toggle_zoom_flips_and_is_a_noop_on_single_pane() {
        let mut set = TabSet::new(build_session(), None);
        // Single pane: zoom is meaningless, toggle is a no-op.
        assert!(!set.toggle_active_zoom());
        assert!(!set.active_is_zoomed());

        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        // Multi-pane: toggle on, then off.
        assert!(set.toggle_active_zoom());
        assert!(set.active_is_zoomed());
        assert!(set.toggle_active_zoom());
        assert!(!set.active_is_zoomed());
    }

    #[test]
    fn zoomed_tab_renders_only_the_focused_leaf_full_content() {
        let mut set = TabSet::new(build_session(), None);
        let right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let content = PaneRect::new(5.0, 7.0, 401.0, 200.0);
        // Un-zoomed: two tiled rects.
        assert_eq!(set.active_pane_rects(content, 1.0).len(), 2);

        assert!(set.toggle_active_zoom());
        let rects = set.active_pane_rects(content, 1.0);
        assert_eq!(rects.len(), 1, "only the focused pane renders while zoomed");
        let (token, rect) = rects[0];
        assert_eq!(token, right, "the focused pane is the one shown");
        // It spans the whole content rect.
        assert!((rect.x - content.x).abs() < f32::EPSILON);
        assert!((rect.y - content.y).abs() < f32::EPSILON);
        assert!((rect.w - content.w).abs() < f32::EPSILON);
        assert!((rect.h - content.h).abs() < f32::EPSILON);
    }

    #[test]
    fn unzoom_restores_the_prior_pane_rects_exactly() {
        let mut set = TabSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let content = PaneRect::new(5.0, 7.0, 401.0, 200.0);
        let before = set.active_pane_rects(content, 1.0);

        assert!(set.toggle_active_zoom());
        assert!(set.toggle_active_zoom());
        let after = set.active_pane_rects(content, 1.0);

        assert_eq!(before.len(), after.len());
        for ((tb, rb), (ta, ra)) in before.iter().zip(after.iter()) {
            assert_eq!(tb, ta);
            assert!((rb.x - ra.x).abs() < f32::EPSILON);
            assert!((rb.y - ra.y).abs() < f32::EPSILON);
            assert!((rb.w - ra.w).abs() < f32::EPSILON);
            assert!((rb.h - ra.h).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn closing_a_pane_unzooms_the_tab() {
        let mut set = TabSet::new(build_session(), None);
        let right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert!(set.toggle_active_zoom());
        assert!(set.active_is_zoomed());
        // Close the zoomed (focused) pane: the tab survives and is no longer
        // zoomed.
        assert!(!set.close(right));
        assert!(!set.active_is_zoomed());
        assert!(set.active_is_single_pane());
    }

    #[test]
    fn splitting_a_zoomed_tab_unzooms_it() {
        let mut set = TabSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert!(set.toggle_active_zoom());
        assert!(set.active_is_zoomed());
        set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(2)));
        assert!(!set.active_is_zoomed(), "split clears zoom");
    }

    #[test]
    fn resize_sizes_the_zoomed_focused_pane_to_full_content() {
        let mut set = TabSet::new(build_session(), None);
        let right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
        assert!(set.toggle_active_zoom());
        set.resize_all_panes(content, 10, 20, 1.0);
        // The focused (zoomed) pane fills the whole content → 80 cols, 20 rows.
        assert_eq!(pane_dims(&set, right), (80, 20));
        // The background pane keeps its split sub-rect (40 cols) so un-zoom is
        // instantly correct.
        assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));
    }

    #[test]
    fn zoom_hides_background_panes_from_visibility() {
        let mut set = TabSet::new(build_session(), None);
        let right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert!(set.toggle_active_zoom());
        // While zoomed only the focused pane is visible (drives redraw).
        assert!(set.is_visible_pane(right));
        assert!(!set.is_visible_pane(SessionToken(0)));
        // Un-zoom restores both as visible.
        assert!(set.toggle_active_zoom());
        assert!(set.is_visible_pane(right));
        assert!(set.is_visible_pane(SessionToken(0)));
    }

    #[test]
    fn active_layout_exposes_the_tree() {
        let mut set = TabSet::new(build_session(), None);
        assert!(set.active_layout().is_some_and(PaneNode::is_single_pane));
        set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(1)));
        assert_eq!(set.active_layout().map(PaneNode::pane_count), Some(2));
    }

    fn pane_dims(set: &TabSet, token: SessionToken) -> (usize, usize) {
        let dims = set
            .get(token)
            .expect("pane present")
            .terminal
            .lock()
            .expect("terminal lock")
            .screen()
            .dimensions();
        (dims.columns, dims.rows)
    }

    #[test]
    fn resize_all_panes_sizes_a_single_pane_to_the_full_content() {
        let mut set = TabSet::new(build_session(), None);
        // 800x400 content, 10x20 cell → 80 cols, 20 rows; one pane fills it.
        let content = PaneRect::new(0.0, 0.0, 800.0, 400.0);
        set.resize_all_panes(content, 10, 20, 1.0);
        assert_eq!(pane_dims(&set, SessionToken(0)), (80, 20));
    }

    #[test]
    fn default_session_source_is_local() {
        // BYTE-IDENTITY GUARD: a normally-spawned session is `Local`, so the
        // source generalization is a no-op for the default path.
        let set = TabSet::new(build_session(), None);
        assert!(matches!(set.active().source, SessionSource::Local { .. }));
    }

    #[test]
    #[cfg(unix)]
    fn local_session_resize_routes_to_pty_unchanged() {
        // BYTE-IDENTITY GUARD: resizing a local session must push the exact same
        // TIOCSWINSZ to the concrete PTY as before Phase 2 — the `Local` match
        // arm is the identical `pty.lock().resize(...)` call.
        let mut set = TabSet::new(build_session(), None);
        let content = PaneRect::new(0.0, 0.0, 800.0, 400.0);
        set.resize_all_panes(content, 10, 20, 1.0);
        let pty_dims = set
            .active()
            .local_pty()
            .expect("local session has a PTY")
            .lock()
            .expect("pty lock")
            .dimensions_for_test()
            .expect("pty dimensions");
        assert_eq!((pty_dims.columns, pty_dims.rows), (80, 20));
    }

    #[test]
    fn resize_all_panes_gives_each_split_pane_its_sub_rect() {
        let mut set = TabSet::new(build_session(), None);
        let right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        // 801px wide, 1px divider → 800 usable, even split → 400/400 → 40 cols
        // each at a 10px cell. Heights are the full 400px → 20 rows.
        let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
        set.resize_all_panes(content, 10, 20, 1.0);
        assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));
        assert_eq!(pane_dims(&set, right), (40, 20));
    }

    #[test]
    fn resize_all_panes_same_dims_preserves_cursor_and_trailing_blank() {
        // v0.3.0 regression guard (the fish `❯ ` cursor-offset bug). A split
        // runs `resize_all_panes` over EVERY pane of the tab, including panes
        // the split did not actually resize. When such a pane's grid dimensions
        // are unchanged, the model resize must be a no-op: re-running the column
        // reflow would trim the trailing blank the shell printed after its
        // prompt and drag the cursor one column left, and because the PTY size
        // is unchanged no SIGWINCH reaches the shell to repaint and self-correct.
        let mut set = TabSet::new(build_session(), None);
        let _right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        // Settle both panes at 40x20 (801px wide, 1px divider, 10x20 cell).
        let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
        set.resize_all_panes(content, 10, 20, 1.0);
        assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));

        // Print a fish-style prompt with its trailing space into the left pane:
        // `❯` at column 0, a space at column 1, cursor parked at column 2.
        set.get(SessionToken(0))
            .expect("left pane")
            .terminal
            .lock()
            .expect("terminal lock")
            .advance("❯ ".as_bytes());

        let before = set
            .get(SessionToken(0))
            .expect("left pane")
            .terminal
            .lock()
            .expect("terminal lock")
            .snapshot();
        assert_eq!(before.cursor.column, 2, "prompt parks the cursor at col 2");
        assert_eq!(before.cells[0].ch, '❯');
        assert_eq!(before.cells[1].ch, ' ', "trailing prompt space present");

        // Re-run the exact same layout pass (what a split of the OTHER column
        // does to this untouched pane: identical 40x20 dims). With the no-op
        // guard the cursor and the trailing blank are byte-identical.
        set.resize_all_panes(content, 10, 20, 1.0);
        assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));

        let after = set
            .get(SessionToken(0))
            .expect("left pane")
            .terminal
            .lock()
            .expect("terminal lock")
            .snapshot();
        assert_eq!(
            after.cursor.column, 2,
            "same-dims resize must not shift the cursor"
        );
        assert_eq!(after.cells[0].ch, '❯');
        assert_eq!(
            after.cells[1].ch, ' ',
            "same-dims resize must not trim the trailing prompt space"
        );
    }

    #[test]
    fn is_visible_pane_covers_every_pane_of_the_active_tab_only() {
        let mut set = TabSet::new(build_session(), None);
        let sibling =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        // Both panes of the active tab are visible (focused + background).
        assert!(set.is_visible_pane(SessionToken(0)));
        assert!(set.is_visible_pane(sibling));
        // A pane that does not exist is never visible.
        assert!(!set.is_visible_pane(SessionToken(99)));

        // Open a second tab; its pane is not visible while tab 0 is active.
        let other_tab = SessionToken(2);
        set.push(build_session_with_id(other_tab));
        assert!(!set.is_visible_pane(other_tab));
        assert!(set.is_visible_pane(SessionToken(0)));
    }

    #[test]
    fn active_pane_rects_tiles_the_content_without_overlap() {
        let mut set = TabSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let content = PaneRect::new(5.0, 7.0, 401.0, 200.0);
        let rects = set.active_pane_rects(content, 1.0);
        assert_eq!(rects.len(), 2);
        let (_, left) = rects[0];
        let (_, right) = rects[1];
        // Left + divider + right spans exactly the content width; no overlap.
        assert!((left.x - content.x).abs() < f32::EPSILON);
        assert!((right.x - (left.x + left.w + 1.0)).abs() < f32::EPSILON);
        assert!(((right.x + right.w) - (content.x + content.w)).abs() < f32::EPSILON);
    }

    #[test]
    fn active_pane_at_point_resolves_focus_follows_click() {
        let mut set = TabSet::new(build_session(), None);
        let right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        // 801px wide, 1px divider at x=400 → left pane [0,400), right [401,801).
        let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
        assert_eq!(
            set.active_pane_at_point(content, 1.0, 100.0, 50.0),
            Some(SessionToken(0))
        );
        assert_eq!(
            set.active_pane_at_point(content, 1.0, 600.0, 50.0),
            Some(right)
        );
        // The 1px divider gap (x=400) belongs to no pane.
        assert_eq!(set.active_pane_at_point(content, 1.0, 400.0, 50.0), None);
    }

    #[test]
    fn active_divider_at_point_grabs_only_near_the_divider() {
        let mut set = TabSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
        // Within the 6px grab band of the x=400 divider → index 0.
        assert_eq!(
            set.active_divider_at_point(content, 1.0, 402.0, 50.0, 6.0),
            Some(0)
        );
        // Far from the divider → no grab.
        assert_eq!(
            set.active_divider_at_point(content, 1.0, 100.0, 50.0, 6.0),
            None
        );
        // A single-pane active tab has no dividers to grab.
        let single = TabSet::new(build_session(), None);
        assert_eq!(
            single.active_divider_at_point(content, 1.0, 402.0, 50.0, 6.0),
            None
        );
    }

    #[test]
    fn drag_active_divider_reflows_the_active_split_ratio() {
        let mut set = TabSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
        // Drag the column divider to x=200 → ratio ≈ 200/800.
        let new = set
            .drag_active_divider(content, 1.0, 0, 200.0, 50.0)
            .expect("active split exists");
        assert!((new - 200.0 / 800.0).abs() < 1e-3);
        // The new ratio re-tiles the panes: left pane now ~200px wide.
        let rects = set.active_pane_rects(content, 1.0);
        let (_, left) = rects[0];
        assert!((left.w - 200.0).abs() < 1.0);
        // An out-of-range divider index leaves the tree unchanged.
        assert_eq!(set.drag_active_divider(content, 1.0, 9, 50.0, 50.0), None);
    }

    #[test]
    fn focus_move_active_lands_on_the_spatial_neighbor() {
        let mut set = TabSet::new(build_session(), None);
        let right =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        // After the split focus is on the right pane.
        assert_eq!(set.active_id(), right);
        let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
        // Move focus left → the original pane; returns true (focus changed).
        assert!(set.focus_move_active(content, 1.0, FocusDir::Left));
        assert_eq!(set.active_id(), SessionToken(0));
        // No neighbor to the left of the leftmost pane → no change, false.
        assert!(!set.focus_move_active(content, 1.0, FocusDir::Left));
        assert_eq!(set.active_id(), SessionToken(0));
        // Move right → back to the right pane.
        assert!(set.focus_move_active(content, 1.0, FocusDir::Right));
        assert_eq!(set.active_id(), right);
    }
}
