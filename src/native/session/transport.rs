// SPDX-License-Identifier: GPL-3.0-only
//! Session transports: what backs a session's I/O, how one is constructed, and
//! every operation that speaks to a backend.
//!
//! A session is backed by a locally spawned PTY (the default on every platform,
//! and the only production backing on Windows, where it is ConPTY), by an
//! attached detached-session host over a Unix socket, or - in test builds only -
//! by a headless stand-in with no OS child. This module owns source selection,
//! construction, the local and remote spawn paths, attach and reattach, image
//! upload, reconnect classification, and the backend half of resize. The model
//! is always resized before its backend.

#[cfg(unix)]
use super::SNAPSHOT_DEADLINE;
use super::model::{Session, SessionToken, Tab, WorkspaceSet};
#[cfg(unix)]
use super::persistence::per_connection_attach_budget;
use crate::connection_hosts::ConnectionHost;
use crate::core::{Snapshot, Terminal};
use crate::native::app::{CursorBlinkState, SynchronizedOutputHold};
#[cfg(unix)]
use crate::native::attach::{
    AttachClient, attach_input_writer, resolve_session_socket, spawn_attach_pump,
};
use crate::native::layout::{PaneRect, grid_dims_for_rect, layout_rects, pane_inner_rect};
use crate::native::output_recorder::RecorderHandle;
#[cfg(not(test))]
use crate::native::pty::UserEvent;
use crate::native::pty::{PtyWriter, spawn_pty_pump};
use crate::native::search_ui::SearchUi;
use crate::native::viewport::Viewport;
use crate::pty::{ForegroundJob, PtySession};
use crate::selection::{AbsoluteSelectionState, ClickTracker, PointerDrag};
use crate::ssh_connect::{
    RemoteSshOptions, SshCommand, ssh_command_for_host_with_options, ssh_tab_title,
};
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
#[cfg(not(test))]
use winit::event_loop::EventLoopProxy;

/// Apply the local-PTY backend capabilities onto a freshly-created terminal
/// model. Called from EVERY local-pane creation path so the wiring can't drift:
///   * [`WorkspaceSet::insert_local_session_with`] — the split / new-tab path.
///   * [`crate::native::run_native`] — the startup pane (hand-built in `run_native`).
///
/// Currently propagates one capability: whether the backend's shell repaints
/// the cursor with absolute positioning on resize (ConPTY/Windows = true), so
/// the terminal defers resize cursor placement to the shell instead of
/// translating it. On a POSIX PTY this is false (= the model default), which is
/// why a missing call is invisible on Linux/macOS and only Windows exposes a
/// drift — keep this funnel the single source of truth and guard it on Windows
/// CI via the setter/getter/behavior tie + the per-path pane tests.
pub(in crate::native) fn apply_local_backend_caps(model: &mut Terminal, session: &PtySession) {
    model.set_shell_owns_cursor_on_resize(session.shell_repaints_on_resize());
}

pub(in crate::native) fn seed_initial_working_directory(model: &mut Terminal, cwd: Option<&Path>) {
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
/// session is an [`AttachInputWriter`](crate::native::attach::AttachInputWriter) boxed
/// into the same [`PtyWriter`] type, so the app-side input path is identical.
/// This enum routes the two operations that genuinely differ by backing:
/// **resize** (TIOCSWINSZ vs. a `Resize` frame) and **close** (kill+reap vs. a
/// clean `Detach` that keeps the host session alive for later reattach).
pub(in crate::native) enum SessionSource {
    /// Locally-spawned PTY — the default, byte-identical path.
    Local { pty: Arc<Mutex<PtySession>> },
    /// Attached to a detached session-host over a per-user unix socket. The
    /// client is shared with the input writer so input/resize/detach serialize
    /// through one socket lock. Unix-only: the detached session-host transport is
    /// `#[cfg(unix)]`, so on Windows a session is always `Local` and every match
    /// on this enum is exhaustive with the `Local` arm alone.
    #[cfg(unix)]
    Attached { client: Arc<Mutex<AttachClient>> },
    /// Test-only headless source (no OS child, PTY, pump, wake pipe, or platform
    /// API). Pure App/UI tests need only a terminal model and a writable sink to
    /// satisfy [`App::new`], not a real shell; owning one made those tests
    /// inherit a real `PtySession`'s synchronous kill+wait teardown, which was
    /// the root cause of the macOS CI PTY-teardown wedge. This variant records
    /// resize geometry and reports an injectable foreground job so migrated
    /// tests keep their geometry/close assertions, while close/shutdown are
    /// immediate no-ops. Compiled out of production builds entirely.
    #[cfg(test)]
    Headless { session: Arc<HeadlessSession> },
}

/// Test-only backing state for [`SessionSource::Headless`]. Interior-mutable so
/// one `Arc` handle shared with a test can observe resize calls without a lock
/// dance, mirroring how a real fixture shares `Arc<Mutex<PtySession>>`.
#[cfg(test)]
pub(in crate::native) struct HeadlessSession {
    dimensions: Mutex<crate::core::Dimensions>,
    resize_calls: std::sync::atomic::AtomicUsize,
    cell_metrics: Mutex<Option<crate::core::CellMetrics>>,
    foreground_job: Mutex<ForegroundJob>,
}

#[cfg(test)]
impl HeadlessSession {
    pub(in crate::native) fn new(dimensions: crate::core::Dimensions) -> Self {
        Self {
            dimensions: Mutex::new(dimensions),
            resize_calls: std::sync::atomic::AtomicUsize::new(0),
            cell_metrics: Mutex::new(None),
            foreground_job: Mutex::new(ForegroundJob::Unknown),
        }
    }

    /// Record a kernel-side resize exactly as the real PTY resize dispatch would,
    /// without any syscall: store the new dimensions and bump the call counter.
    pub(in crate::native) fn record_resize(&self, dimensions: crate::core::Dimensions) {
        if let Ok(mut current) = self.dimensions.lock() {
            *current = dimensions;
        }
        self.resize_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(in crate::native) fn record_cell_metrics(&self, metrics: crate::core::CellMetrics) {
        if let Ok(mut current) = self.cell_metrics.lock() {
            *current = Some(metrics);
        }
    }

    pub(in crate::native) fn dimensions(&self) -> crate::core::Dimensions {
        self.dimensions
            .lock()
            .map(|d| *d)
            .unwrap_or_else(|_| crate::core::Dimensions::new(1, 1))
    }

    pub(in crate::native) fn resize_call_count(&self) -> usize {
        self.resize_calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(in crate::native) fn cell_metrics(&self) -> Option<crate::core::CellMetrics> {
        self.cell_metrics.lock().ok().and_then(|m| *m)
    }

    pub(in crate::native) fn foreground_job(&self) -> ForegroundJob {
        self.foreground_job
            .lock()
            .map(|j| *j)
            .unwrap_or(ForegroundJob::Unknown)
    }

    /// Inject a foreground job so a migrated confirm-close test can exercise the
    /// running-job branch without a real child.
    pub(in crate::native) fn set_foreground_job(&self, job: ForegroundJob) {
        if let Ok(mut current) = self.foreground_job.lock() {
            *current = job;
        }
    }
}

/// A remote session's reconnect anchor (F6-i4). Holds what is needed to
/// re-establish the same connection into the SAME tab slot after a transport
/// drop: the resolved `ssh` argv (so a reconnect is byte-identical to the
/// original launch and survives the host being edited or removed from the saved
/// hosts mid-session) plus the tab title to restore.
///
/// REATTACH ANCHOR — this is the per-session hook for re-establishing a remote
/// connection. It is deliberately the same concept as a detached session-host's
/// per-pane reattach id: both answer "how does this pane come back". Keep them
/// unified as one reconnect notion rather than two parallel fields. The tab
/// title is not stored here — reconnect reuses the same tab, whose title
/// override already persists across the drop.
pub(in crate::native) struct RemoteReconnect {
    command: SshCommand,
}

impl RemoteReconnect {
    pub(in crate::native) fn new(command: SshCommand) -> Self {
        Self { command }
    }
}

/// What the image paste-through path (F6-i7) needs to upload a pasted clipboard
/// image to this session's remote host and clean up afterward.
///
/// Present ONLY on a remote *integrated* ssh session — that presence is the
/// trigger gate: a local shell and an integration-off plain-ssh tab both leave
/// it `None`, so image paste-through never engages there and their paste path
/// stays byte-identical. Holds the ssh destination + port and the connect
/// path's `ControlMaster` dir (so the upload multiplexes over the live master
/// with no second auth), plus the temp paths uploaded during the tab's life for
/// best-effort cleanup on close.
pub(in crate::native) struct RemoteUpload {
    destination: String,
    // Read only by the not-`test` upload/cleanup argv builders; under `cfg(test)`
    // the confirm flow records the intended upload instead of building an argv.
    #[cfg_attr(test, allow(dead_code))]
    port: Option<u16>,
    #[cfg_attr(test, allow(dead_code))]
    control_dir: Option<std::path::PathBuf>,
    /// Remote temp paths uploaded during this tab's life. Shared with the async
    /// upload worker (it appends a path on a successful upload) and drained on
    /// close to fire best-effort remote cleanup.
    uploaded: Arc<Mutex<Vec<String>>>,
}

impl RemoteUpload {
    pub(in crate::native) fn new(
        destination: String,
        port: Option<u16>,
        control_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            destination,
            port,
            control_dir,
            uploaded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(in crate::native) fn destination(&self) -> &str {
        &self.destination
    }

    #[cfg(not(test))]
    pub(in crate::native) fn port(&self) -> Option<u16> {
        self.port
    }

    #[cfg(not(test))]
    pub(in crate::native) fn control_dir(&self) -> Option<&Path> {
        self.control_dir.as_deref()
    }

    /// A clonable handle to the uploaded-paths list, handed to the async upload
    /// worker so it can record a remote path once the transfer succeeds.
    pub(in crate::native) fn uploaded_handle(&self) -> Arc<Mutex<Vec<String>>> {
        self.uploaded.clone()
    }
}

/// The self-contained bundle an image paste-through upload worker needs (F6-i7).
/// Every field is a cheap clone/handle, so the worker owns everything on its own
/// thread and never borrows the session set: it uploads over `ssh`, then writes
/// the remote path (on success) into `writer` or a one-line failure notice into
/// `terminal`, and wakes a redraw through `proxy`. Compiled out under
/// `cfg(test)`, where the confirm flow records the intended upload rather than
/// running the worker.
#[cfg(not(test))]
pub(in crate::native) struct RemoteUploadJob {
    pub(in crate::native) session: SessionToken,
    pub(in crate::native) destination: String,
    pub(in crate::native) port: Option<u16>,
    pub(in crate::native) control_dir: Option<std::path::PathBuf>,
    pub(in crate::native) uploaded: Arc<Mutex<Vec<String>>>,
    pub(in crate::native) terminal: Arc<Mutex<Terminal>>,
    pub(in crate::native) proxy: Option<EventLoopProxy<UserEvent>>,
}

/// What to do when a session's shell reaches EOF, given its child exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum ExitDisposition {
    /// Close the tab normally — the byte-identical path everything used before.
    Close,
    /// A remote transport drop: hold the tab open and offer reconnect.
    Reconnect,
}

/// Classify a remote session's shell exit from its child exit code.
///
/// OpenSSH's `ssh` client exits **255** on its own transport failures
/// (connection refused, host-key mismatch, or a link that drops mid-session), so
/// 255 is the reconnect trigger. A clean `0` (the user typed `exit`/`logout`),
/// any other remote-command code, and a missing code (`None` — a Unix signal
/// death, or a post-EOF Windows `STILL_ACTIVE` sentinel treated as "unknown")
/// all close normally. The rare case of the remote command itself exiting 255 is
/// an accepted false positive: the reconnect prompt is dismissable and
/// self-correcting, which is strictly better than today's silent close on every
/// drop. Classification only runs for sessions that carry reconnect state, so a
/// local shell is never affected.
pub(in crate::native) fn classify_remote_exit(code: Option<i32>) -> ExitDisposition {
    match code {
        Some(255) => ExitDisposition::Reconnect,
        _ => ExitDisposition::Close,
    }
}

const RECONNECT_BANNER: &str =
    "\r\n\x1b[1;33m connection dropped \x1b[0m  Enter: reconnect · Esc: close\r\n";

#[derive(Clone, Copy, PartialEq, Eq)]
enum PtyResizePolicy {
    FlushDirty,
    Never,
}

impl Session {
    /// Construct a locally-spawned (PTY-backed) session — the byte-identical
    /// path. The signature is unchanged from before Phase 2; the `pty` is
    /// wrapped into a [`SessionSource::Local`]. Test-only: the production local
    /// construction sites use [`Self::new_local_with_recorder`] so the pump and
    /// the session share one recorder handle. Tests that do not record use this
    /// shorter form (it mints its own empty, disabled handle).
    #[cfg(test)]
    pub(in crate::native) fn new(
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

    /// Construct a test-only headless session backed by a [`HeadlessSession`]
    /// instead of a real PTY. No OS child, master/slave, pump thread, or wake
    /// pipe is created, so the session's teardown is a synchronous no-op and the
    /// fixture cannot inherit a real shell's kill+wait. The returned handle lets
    /// a test observe resize geometry or inject a foreground job.
    #[cfg(test)]
    pub(in crate::native) fn new_headless(
        id: SessionToken,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        headless: Arc<HeadlessSession>,
    ) -> Self {
        Self::from_parts(
            id,
            terminal,
            writer,
            SessionSource::Headless { session: headless },
            None,
        )
    }

    /// Construct a locally-spawned session that shares a pre-built recorder
    /// handle with its pump thread (so the pump's frames land in the same ring
    /// the App later scrubs). Used by the startup path and `insert_spawned_
    /// session`; the plain `Session::new` (which mints its own empty handle)
    /// stays for call sites that do not record.
    pub(in crate::native) fn new_local_with_recorder(
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
    /// `writer` (an [`AttachInputWriter`](crate::native::attach::AttachInputWriter)); the
    /// `client` backs resize/detach. Unix-only (the session-host transport is
    /// `#[cfg(unix)]`).
    #[cfg(unix)]
    pub(in crate::native) fn new_attached(
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
        let last_scrollback_trim_epoch = terminal
            .lock()
            .map(|terminal| terminal.scrollback_trim_epoch())
            .unwrap_or(0);
        Self {
            id,
            terminal,
            writer,
            source,
            pty_resize_dirty: false,
            attached_session_id: None,
            pump_thread,
            recorder,
            tab_title,
            attention: crate::native::notifications::PaneAttention::default(),
            notify_when_command_finishes: false,
            monitors: crate::native::notifications::PaneMonitors::default(),
            needs_rebuild: true,
            last_render_signature: None,
            synchronized_output_hold: SynchronizedOutputHold::default(),
            last_presented_snapshot: None,
            last_cursor_comparison_snapshot: None,
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
            hovered_button: None,
            #[cfg(test)]
            test_path_probe: crate::native::app::interactive_paths::MapProbe::default(),
            pointer_drag: PointerDrag::None,
            selection_block: false,
            drag_anchor_unit: None,
            clicks: ClickTracker::default(),
            last_selection_autoscroll: None,
            report_button: None,
            swallow_open_left_release: false,
            pressed_button: None,
            viewport: Viewport::default(),
            search: SearchUi::default(),
            hints: None,
            copy_mode: None,
            search_restore_viewport: None,
            last_scrollback_len: 0,
            last_scrollback_trim_epoch,
            cursor_blink: CursorBlinkState::new(crate::native::app::CURSOR_BLINK_INTERVAL),
            cursor_anim_alpha: 1.0,
            cursor_ease_deadline: None,
            cursor_ease_phase_on: true,
            cursor_ease_toggle_at: None,
            cursor_anim_offset: [0.0, 0.0],
            cursor_slide_deadline: None,
            cursor_slide_start: None,
            cursor_slide_from_px: [0.0, 0.0],
            cursor_streak: crate::native::app::cursor_streak::CursorStreakState::default(),
            row_fade_starts: Vec::new(),
            last_scrollback_len_for_fade: 0,
            row_fade_epoch: 0,
            scroll_frac_rows: 0.0,
            scroll_frac_offset: 0.0,
            glide_visual: 0.0,
            glide_active: false,
            glide_target: 0,
            glide_last_tick: None,
            reconnect: None,
            awaiting_reconnect: false,
            upload: None,
            remote_destination: None,
        }
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
    pub(in crate::native) fn local_pty(&self) -> Option<&Arc<Mutex<PtySession>>> {
        match &self.source {
            SessionSource::Local { pty } => Some(pty),
            #[cfg(unix)]
            SessionSource::Attached { .. } => None,
            SessionSource::Headless { .. } => None,
        }
    }

    /// The headless backing state, or `None` for a real-PTY or attached session.
    /// Test-only seam so a migrated fixture can assert recorded resize geometry
    /// or inject a foreground job without owning a real shell.
    #[cfg(test)]
    pub(in crate::native) fn headless_session(&self) -> Option<&Arc<HeadlessSession>> {
        match &self.source {
            SessionSource::Headless { session } => Some(session),
            SessionSource::Local { .. } => None,
            #[cfg(unix)]
            SessionSource::Attached { .. } => None,
        }
    }

    /// True only when this is a local session whose foreground job is running.
    /// An attached session reports `false` (the foreground job lives in the
    /// remote host and cannot be queried locally), so confirm-close never blocks
    /// closing an attached window — closing it cleanly detaches anyway.
    pub(in crate::native) fn foreground_job_running(&self) -> bool {
        match &self.source {
            SessionSource::Local { pty } => pty
                .lock()
                .is_ok_and(|pty| pty.foreground_job() == ForegroundJob::Running),
            #[cfg(unix)]
            SessionSource::Attached { .. } => false,
            #[cfg(test)]
            SessionSource::Headless { session } => {
                session.foreground_job() == ForegroundJob::Running
            }
        }
    }
}

impl WorkspaceSet {
    pub(in crate::native) fn set_local_hostname(&mut self, local_hostname: Option<String>) {
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
    pub(in crate::native) fn set_recording_enabled(&mut self, on: bool) {
        self.recording_enabled = on;
        for session in self.sessions.values() {
            session.recorder.set_enabled(on);
        }
    }

    pub(in crate::native) fn set_shell_integration_enabled(&mut self, on: bool) {
        self.shell_integration_enabled = on;
    }

    /// A decoupled clone of the **focused** session's recorded frames, oldest
    /// first, for the replay overlay to scrub. Empty when recording is off or
    /// nothing has been recorded yet.
    pub(in crate::native) fn active_recorder_frames(&self) -> Vec<Snapshot> {
        self.active().recorder.frames_clone()
    }

    /// Reconcile **every pane of every tab** to its laid-out cell dimensions
    /// within `content`, reflowing changed terminal models and flushing only
    /// backends whose grid/metrics became dirty. For an all-single-pane world
    /// every tab's lone leaf still spans `content`; multi-pane tabs derive each
    /// pane from its own sub-rect (design doc §2.5 audit row #1). An unchanged
    /// session is inert instead of receiving a redundant PTY/ConPTY resize.
    ///
    /// `pad` is the physical window-padding inset applied to every divider-facing
    /// pane edge (`0.0` and the single-pane path stay byte-identical); it sizes
    /// each pane's PTY/grid to its padded drawable rect so glyphs never sit flush
    /// against a divider.
    pub(in crate::native) fn resize_all_panes(
        &mut self,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
        pad: f32,
    ) {
        self.resize_all_panes_impl(
            content,
            cell_w,
            cell_h,
            divider_px,
            pad,
            PtyResizePolicy::FlushDirty,
        );
    }

    /// Reconcile pane models and PTYs after a surface configure, returning
    /// whether any pane's grid or cell metrics changed. A duplicate same-grid
    /// configure does not signal unchanged backends; the final debounced
    /// configure can still resize an individual ratio-derived child when the
    /// aggregate window grid is stable.
    pub(in crate::native) fn reconcile_all_panes_for_surface(
        &mut self,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
        pad: f32,
    ) -> bool {
        self.resize_all_panes_impl(
            content,
            cell_w,
            cell_h,
            divider_px,
            pad,
            PtyResizePolicy::FlushDirty,
        )
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
    pub(in crate::native) fn reflow_all_panes_for_drag(
        &mut self,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
        pad: f32,
    ) {
        self.resize_all_panes_impl(
            content,
            cell_w,
            cell_h,
            divider_px,
            pad,
            PtyResizePolicy::Never,
        );
    }

    /// Shared body for structural resize, surface reconciliation, and live drag
    /// reflow. The model + cell-metrics path is identical; only the kernel-side
    /// resize policy differs.
    fn resize_all_panes_impl(
        &mut self,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
        pad: f32,
        pty_policy: PtyResizePolicy,
    ) -> bool {
        let mut any_geometry_changed = false;
        for tab in self.workspaces.iter().flat_map(|ws| ws.tabs.iter()) {
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
                // PANE-PADDING: size the PTY/grid to the pane's padded drawable
                // rect (inset by `pad` on divider-facing edges), so the reported
                // "first glyph flush against the divider" regression is fixed and
                // the grid never overflows into the divider gap. `pad == 0`, the
                // single-pane path, and a zoomed full-bleed pane all yield an
                // inner rect equal to `rect`, so those paths stay byte-identical.
                let rect = pane_inner_rect(rect, content, pad);
                let (drawable_cols, drawable_rows) = grid_dims_for_rect(rect, cell_w, cell_h);
                // A heavily padded or aggressively narrowed leaf can have no
                // drawable cell on one axis. Terminal models, Unix PTYs,
                // ConPTY, and the attached-session protocol all require
                // non-zero dimensions, so retain a valid 1x1 backing grid while
                // the render and pointer paths skip the collapsed leaf.
                let cols = drawable_cols.max(1);
                let rows = drawable_rows.max(1);
                let Some(session) = self.sessions.get_mut(&token) else {
                    continue;
                };
                let mut dimensions_changed = false;
                let mut metrics_changed = false;
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
                        dimensions_changed = true;
                    }
                    let metrics = crate::core::CellMetrics::new(cell_w, cell_h);
                    metrics_changed = terminal.cell_metrics() != metrics;
                    terminal.set_cell_metrics(cell_w, cell_h);
                }
                let geometry_changed = dimensions_changed || metrics_changed;
                any_geometry_changed |= geometry_changed;
                if dimensions_changed {
                    session.invalidate_layout_dependent_state();
                }
                let backend_geometry_changed = match &session.source {
                    SessionSource::Local { .. } => geometry_changed,
                    #[cfg(unix)]
                    SessionSource::Attached { .. } => dimensions_changed,
                    #[cfg(test)]
                    SessionSource::Headless { .. } => geometry_changed,
                };
                session.pty_resize_dirty |= backend_geometry_changed;
                // Route the kernel-side resize to whichever source backs the
                // session. A local PTY still refreshes its pixel metrics even
                // when the cell grid is unchanged. An attached session has no
                // cell-metric payload, so it forwards `Resize` only for a real
                // dimension change; this keeps reconciliation transport-neutral
                // when the host snapshot already matches the window. Skipped
                // entirely for the live divider-drag path. Surface configure
                // reconciliation signals only panes whose grid or metrics
                // changed, preserving duplicate-event idempotence. The release
                // handler still flushes the single coalesced backend resize for
                // every dirty pane at drag-end.
                if pty_policy == PtyResizePolicy::FlushDirty && session.pty_resize_dirty {
                    let resize_succeeded = match &session.source {
                        SessionSource::Local { pty } => {
                            if let Ok(pty) = pty.lock() {
                                // Feed the live cell metric so TIOCSWINSZ reports
                                // a real ws_xpixel/ws_ypixel (C23), then resize.
                                pty.set_cell_metrics(crate::core::CellMetrics::new(cell_w, cell_h));
                                pty.resize(crate::core::Dimensions::new(cols, rows)).is_ok()
                            } else {
                                false
                            }
                        }
                        #[cfg(unix)]
                        SessionSource::Attached { client } => {
                            if let Ok(mut client) = client.lock() {
                                client.resize(cols as u32, rows as u32).is_ok()
                            } else {
                                false
                            }
                        }
                        // Record the resize geometry without any syscall so a
                        // migrated geometry test can still assert it.
                        #[cfg(test)]
                        SessionSource::Headless { session } => {
                            session
                                .record_cell_metrics(crate::core::CellMetrics::new(cell_w, cell_h));
                            session.record_resize(crate::core::Dimensions::new(cols, rows));
                            true
                        }
                    };
                    if resize_succeeded {
                        session.pty_resize_dirty = false;
                    }
                }
            }
        }
        any_geometry_changed
    }

    /// C18: push new per-cell pixel metrics to every pane WITHOUT a column
    /// reflow or a PTY resize. A DPI scale change can alter the cell's
    /// physical-pixel size while the grid still floors to the same cols/rows,
    /// which the debounced grid resize skips entirely; pixel-space consumers
    /// (SGR-pixel mouse reports, inline-image sizing) would otherwise stay on the
    /// stale metric. No `terminal.resize` (no reflow) and no `pty.resize` (no
    /// SIGWINCH), so the shell sees nothing — only the metric changes.
    pub(in crate::native) fn apply_cell_metrics_all(&mut self, cell_w: u32, cell_h: u32) {
        let tokens: Vec<SessionToken> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.layout.leaves())
            .collect();
        for token in tokens {
            let Some(session) = self.sessions.get_mut(&token) else {
                continue;
            };
            if let Ok(mut terminal) = session.terminal.lock() {
                terminal.set_cell_metrics(cell_w, cell_h);
            }
            match &session.source {
                SessionSource::Local { pty } => {
                    if let Ok(pty) = pty.lock() {
                        pty.set_cell_metrics(crate::core::CellMetrics::new(cell_w, cell_h));
                    }
                }
                #[cfg(unix)]
                SessionSource::Attached { .. } => {}
                #[cfg(test)]
                SessionSource::Headless { session } => {
                    session.record_cell_metrics(crate::core::CellMetrics::new(cell_w, cell_h));
                }
            }
        }
    }

    /// Spawn a shell + terminal at `grid` and insert it into the arena,
    /// **without** attaching it to any tab. Shared by [`Self::spawn`] (which
    /// then opens a new tab) and [`Self::split_active`] (which then grafts the
    /// session into the active tab's layout tree as a new pane). The caller owns
    /// tab/pane wiring.
    pub(super) fn insert_spawned_session(
        &mut self,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        self.insert_spawned_session_in(grid, None)
    }

    /// Like [`Self::insert_spawned_session`] but seeds the new shell at `cwd`
    /// (F1 cwd inheritance / Duplicate Tab). Threads the directory to BOTH the
    /// shell spawn (so the child process starts there) and the terminal model's
    /// advisory cwd (so the pane reports the right directory — and tab title —
    /// from the first frame, before any OSC 7 arrives), mirroring
    /// [`Self::insert_restored_session`]. `cwd == None` is byte-identical to the
    /// legacy `insert_spawned_session` path (spawn wherever the process already
    /// is). Cross-platform: the working directory is honored by the POSIX PTY
    /// and Windows ConPTY spawns alike.
    pub(super) fn insert_spawned_session_in(
        &mut self,
        grid: crate::core::Dimensions,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<SessionToken, std::io::Error> {
        let shell_integration = self.shell_integration_enabled;
        let spawn_cwd = cwd.clone();
        self.insert_local_session_with(grid, cwd, move |grid| {
            let settings = crate::settings::Settings {
                shell_integration,
                ..crate::settings::Settings::default()
            };
            PtySession::spawn_default_shell_in_with_settings(grid, spawn_cwd, &settings)
        })
    }

    /// Spawn a restored local shell at `cwd` and insert it into the arena
    /// without attaching it to a tab (WP2 restore path). Mirrors
    /// [`Self::insert_spawned_session`] but (1) hands the captured cwd to the
    /// shell spawn so the child starts there, and (2) SEEDS the terminal model's
    /// advisory cwd to the same value so the restored pane reports the right
    /// directory (and tab title) from the first frame, before any OSC 7 arrives.
    /// `cwd` is `None` when the pane had no restorable directory, which spawns
    /// the shell wherever the process already is.
    pub(super) fn insert_restored_session(
        &mut self,
        grid: crate::core::Dimensions,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<SessionToken, std::io::Error> {
        let shell_integration = self.shell_integration_enabled;
        let spawn_cwd = cwd.clone();
        self.insert_local_session_with(grid, cwd, move |grid| {
            let settings = crate::settings::Settings {
                shell_integration,
                ..crate::settings::Settings::default()
            };
            PtySession::spawn_default_shell_in_with_settings(grid, spawn_cwd, &settings)
        })
    }

    /// Spawn an explicit child command in a local PTY and insert it into the
    /// arena without attaching it to a tab. Used by the SSH connect action; the
    /// shell/new-pane path above remains unchanged.
    pub(super) fn insert_exec_session(
        &mut self,
        grid: crate::core::Dimensions,
        program: OsString,
        args: Vec<OsString>,
    ) -> Result<SessionToken, std::io::Error> {
        self.insert_local_session_with(grid, None, |grid| {
            PtySession::spawn_exec(grid, program, args, None)
        })
    }

    /// Spawn a restored remote `ssh` session (RESTORE-REMOTE) and insert it into
    /// the arena WITHOUT tab/pane wiring — the restore rebuild owns tree
    /// assembly. Mirrors the connect path's per-session bookkeeping so a restored
    /// remote pane behaves exactly like a freshly-connected one: the
    /// reconnect-anchor (a mid-session drop re-runs this argv), the
    /// remote-destination (the next shape capture records it as remote again),
    /// and the image paste-through `upload` descriptor (built by the caller with
    /// [`Self::remote_upload_for`], `Some` only for an integrated host — a
    /// plain-ssh restored pane passes `None` and its paste path stays
    /// byte-identical). Without it a restored integrated pane could not upload a
    /// pasted image, unlike its freshly-connected twin. The remote shell lands at
    /// its own default directory (the captured remote cwd is not `chdir`'d in
    /// v1). Windows uses the same `ssh.exe` argv with no ControlMaster options;
    /// `upload` is still set for an integrated pane (its `scp.exe` path is
    /// unaffected, `control_dir` simply always `None` there).
    pub(in crate::native) fn insert_ssh_restored_session(
        &mut self,
        grid: crate::core::Dimensions,
        command: SshCommand,
        remote_destination: String,
        upload: Option<RemoteUpload>,
    ) -> Result<SessionToken, std::io::Error> {
        let reconnect = RemoteReconnect::new(command.clone());
        let (program, args) = command.into_program_args();
        let session_id = self.insert_exec_session(grid, program, args)?;
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.reconnect = Some(reconnect);
            session.remote_destination = Some(remote_destination);
            session.upload = upload;
        }
        Ok(session_id)
    }

    pub(super) fn insert_local_session_with(
        &mut self,
        grid: crate::core::Dimensions,
        seed_cwd: Option<std::path::PathBuf>,
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
        let writer: PtyWriter = Arc::new(Mutex::new(crate::native::pty_writer::writer_shim(
            session.take_writer().map_err(std::io::Error::other)?,
            session_id,
        )?));
        let mut model = Terminal::new(grid.columns, grid.rows);
        model.set_local_hostname(self.local_hostname.clone());
        seed_initial_working_directory(&mut model, seed_cwd.as_deref());
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
        )?;
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

    /// Spawn a new session in a brand-new single-pane tab. Tab order is
    /// append-to-end, unchanged. `cwd == None` is the legacy new-tab behaviour
    /// (spawn wherever the process already is); a `Some` path seeds the new
    /// shell — and the pane's advisory cwd — there (F1 cwd inheritance /
    /// Duplicate Tab). Cross-platform: the POSIX PTY and Windows ConPTY spawns
    /// both honor the working directory.
    pub(in crate::native) fn spawn(
        &mut self,
        grid: crate::core::Dimensions,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<SessionToken, std::io::Error> {
        let session_id = self.insert_spawned_session_in(grid, cwd)?;
        self.active_workspace_mut()
            .tabs
            .push(Tab::single(session_id));
        Ok(session_id)
    }

    /// Spawn `ssh` for a resolved connection entry in a brand-new single-pane
    /// tab. The argv is built by `crate::ssh_connect` from name-only fields and
    /// execs the system `ssh` binary directly: OdyTTY never reads, stores,
    /// prompts for, or forwards credentials/key material.
    pub(in crate::native) fn connect_ssh_in_new_tab(
        &mut self,
        host: &ConnectionHost,
        grid: crate::core::Dimensions,
        opts: &RemoteSshOptions,
    ) -> Result<SessionToken, std::io::Error> {
        let command =
            ssh_command_for_host_with_options(host, opts).map_err(std::io::Error::other)?;
        let title = host.title.clone().unwrap_or_else(|| ssh_tab_title(host));
        let session_id = self.spawn_ssh_command_in_new_tab(grid, command, Some(title))?;
        // Image paste-through (F6-i7) engages only on a remote *integrated*
        // session. Capture the upload descriptor here where the resolved host and
        // options are known; a plain-ssh (integration-off) tab leaves it unset so
        // its paste path stays byte-identical. Built by the shared helper so the
        // restore path stays in lockstep (RESTORE-UPLOAD).
        if let Some(upload) = Self::remote_upload_for(host, opts)
            && let Some(session) = self.sessions.get_mut(&session_id)
        {
            session.upload = Some(upload);
        }
        Ok(session_id)
    }

    /// The image paste-through upload descriptor (F6-i7) for a resolved host +
    /// options, or `None` when paste-through does not engage. Paste-through is a
    /// remote *integrated* feature, so a plain-ssh (integration-off) session gets
    /// `None` and its paste path stays byte-identical. The `ControlMaster` dir is
    /// carried only when reuse established a master, so the upload multiplexes
    /// over the live session rather than pointing at a socket that never opened.
    /// Shared by the fresh-connect ([`Self::connect_ssh_in_new_tab`]) and restore
    /// ([`Self::insert_ssh_restored_session`]) paths so a restored integrated
    /// pane is configured identically to a freshly-connected one — the two must
    /// not drift (RESTORE-UPLOAD). Windows: `control_dir` is always `None` there
    /// (no socket multiplexing); the descriptor is otherwise identical and the
    /// `scp.exe` upload path is unaffected.
    pub(in crate::native) fn remote_upload_for(
        host: &ConnectionHost,
        opts: &RemoteSshOptions,
    ) -> Option<RemoteUpload> {
        if !opts.integration {
            return None;
        }
        let destination = crate::ssh_connect::ssh_destination(host).ok()?;
        let control_dir = if opts.reuse {
            opts.control_dir.clone()
        } else {
            None
        };
        Some(RemoteUpload::new(destination, host.port, control_dir))
    }

    fn spawn_ssh_command_in_new_tab(
        &mut self,
        grid: crate::core::Dimensions,
        command: SshCommand,
        title_override: Option<String>,
    ) -> Result<SessionToken, std::io::Error> {
        // Keep the resolved argv as the session's reconnect anchor (F6-i4) before
        // it is consumed into (program, args): a mid-session transport drop
        // re-runs exactly this argv into the same tab slot.
        let reconnect = RemoteReconnect::new(command.clone());
        let (program, args) = command.into_program_args();
        let session_id = self.insert_exec_session(grid, program, args)?;
        self.active_workspace_mut()
            .tabs
            .push(Tab::single(session_id));
        if let Some(title) = title_override {
            self.set_title_override(session_id, Some(title));
        }
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.reconnect = Some(reconnect);
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
    pub(in crate::native) fn attach_in_new_tab(
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
        sink: impl crate::native::attach::AttachEventSink,
    ) -> Result<SessionToken, std::io::Error> {
        // Interactive single attach: the user is waiting on exactly one handshake,
        // so it gets the full per-connection snapshot deadline.
        let token =
            self.insert_attached_session_arena(socket, session_id, sink, SNAPSHOT_DEADLINE)?;
        self.active_workspace_mut().tabs.push(Tab::single(token));
        Ok(token)
    }

    /// Arena-only half of [`Self::insert_attached_session`]: connect to the
    /// hosted session, restore the mirror terminal, spawn the read pump, and
    /// insert the attached [`Session`] into the arena — WITHOUT grafting a tab.
    /// The new-tab attach pushes a tab on top of this; the WP3 restore/append
    /// reattach path grafts the returned token into the pane tree it is building
    /// instead.
    #[cfg(unix)]
    fn insert_attached_session_arena(
        &mut self,
        socket: &Path,
        session_id: &str,
        sink: impl crate::native::attach::AttachEventSink,
        snapshot_deadline: std::time::Duration,
    ) -> Result<SessionToken, std::io::Error> {
        let (client, reader, terminal) =
            AttachClient::connect_within(socket, session_id, snapshot_deadline)
                .map_err(std::io::Error::other)?;

        let token = SessionToken(self.next_token);
        self.next_token = self.next_token.saturating_add(1);

        let mut terminal = terminal;
        terminal.set_local_hostname(self.local_hostname.clone());
        let terminal = Arc::new(Mutex::new(terminal));
        let client = Arc::new(Mutex::new(client));
        let writer = attach_input_writer(client.clone(), token)?;
        let pump_thread = spawn_attach_pump(reader, terminal.clone(), sink, token)?;
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
        Ok(token)
    }

    /// WP3 / 8h reattach: if the detached session-host `session_id` is still
    /// alive, connect to it and insert the attached session into the arena,
    /// returning its token (no tab — the restore/append path grafts it into the
    /// tree). Returns `None` when the host is gone, unreachable, already
    /// reattached in this build (attach dedup, ODP-10), or the event loop proxy
    /// is unavailable — the caller then spawns a fresh shell instead. Unix-only;
    /// on any other platform this always returns `None`, so a snapshot copied
    /// from a Unix box degrades cleanly to all-fresh.
    #[cfg(unix)]
    pub(super) fn reattach_restored_session(
        &mut self,
        session_id: &str,
        attach_batch_deadline: Instant,
    ) -> Option<SessionToken> {
        if self.find_attached_tab(session_id).is_some() {
            return None;
        }
        // Bound each pane's handshake by whatever remains of the aggregate
        // restore budget. Once the batch budget is spent, further reattaches
        // fast-fail here (returning None) and fall through to a fresh shell,
        // instead of each pane blocking the UI for the full per-connection
        // deadline (K panes -> up to K * 5s of frozen startup).
        let per_connection =
            per_connection_attach_budget(attach_batch_deadline, Instant::now(), SNAPSHOT_DEADLINE)?;
        let proxy = self.proxy.clone()?;
        let socket = resolve_session_socket(None, session_id).ok()?;
        self.insert_attached_session_arena(&socket, session_id, proxy, per_connection)
            .ok()
    }

    #[cfg(not(unix))]
    pub(super) fn reattach_restored_session(
        &mut self,
        _session_id: &str,
        _attach_batch_deadline: Instant,
    ) -> Option<SessionToken> {
        None
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
        sink: impl crate::native::attach::AttachEventSink,
    ) -> Result<SessionToken, std::io::Error> {
        let socket =
            resolve_session_socket(runtime_base, session_id).map_err(std::io::Error::other)?;
        self.insert_attached_session(&socket, session_id, sink)
    }

    /// Capture the exit code of a session's local PTY child after its reader has
    /// reached EOF. Closing the PTY can become visible a few scheduler ticks
    /// before the child status becomes waitable, so a bounded post-EOF poll
    /// closes that race without an unbounded `wait()` on the event-loop thread.
    /// `None` means no code became available within the bound: a Unix signal
    /// death (`.code() == None`), a backend error, or a child whose status did
    /// not settle. An attached session has no local PTY and also yields `None`.
    pub(super) fn capture_exit_code(&self, token: SessionToken) -> Option<i32> {
        match &self.sessions.get(&token)?.source {
            SessionSource::Local { pty } => capture_local_exit_code(pty),
            #[cfg(unix)]
            SessionSource::Attached { .. } => None,
            #[cfg(test)]
            SessionSource::Headless { .. } => None,
        }
    }

    /// On a session's shell EOF, decide whether the tab should be held open for
    /// reconnect. For a remote session (one that carries a [`RemoteReconnect`]
    /// anchor) whose child exited 255, this arms the in-pane reconnect prompt —
    /// painting a one-line banner into the pane via the write-once-into-terminal
    /// precedent and flagging the session awaiting-reconnect — and returns
    /// `true` so the caller leaves the tab open. Every other case (a local
    /// shell, a clean exit, a non-transport exit code) returns `false` and the
    /// caller closes normally, byte-identically to before.
    pub(in crate::native) fn try_arm_reconnect(&mut self, token: SessionToken) -> bool {
        if self
            .sessions
            .get(&token)
            .is_none_or(|s| s.reconnect.is_none())
        {
            return false;
        }
        let code = self.capture_exit_code(token);
        if classify_remote_exit(code) != ExitDisposition::Reconnect {
            return false;
        }
        if let Some(session) = self.sessions.get_mut(&token) {
            session.awaiting_reconnect = true;
            crate::native::lock_recover(&session.terminal).advance(RECONNECT_BANNER.as_bytes());
        }
        true
    }

    /// Whether the active session is showing the dropped-connection reconnect
    /// prompt. When true the App routes keys to the prompt (Enter reconnects,
    /// Esc/Ctrl+D dismisses) instead of the dead shell.
    pub(in crate::native) fn active_awaiting_reconnect(&self) -> bool {
        self.active().awaiting_reconnect
    }

    /// The active session's remote upload destination (`user@host`), or `None`
    /// when the active tab is not a remote *integrated* ssh session (F6-i7). The
    /// App uses this both as the image-paste trigger gate and as the host label
    /// in the confirm prompt.
    pub(in crate::native) fn active_remote_upload_target(&self) -> Option<String> {
        self.active()
            .upload
            .as_ref()
            .map(|upload| upload.destination().to_owned())
    }

    /// Assemble the everything-the-worker-needs bundle to upload a pasted image
    /// to `token`'s remote host on a background thread (F6-i7): the ssh
    /// destination/port/`ControlMaster` dir plus clonable handles to the
    /// session's input writer, terminal model, uploaded-paths list, and the
    /// event-loop proxy (to wake a redraw when the worker finishes). `None` when
    /// the session is gone or is not a remote integrated tab.
    #[cfg(not(test))]
    pub(in crate::native) fn remote_upload_job(
        &self,
        token: SessionToken,
    ) -> Option<RemoteUploadJob> {
        let session = self.sessions.get(&token)?;
        let upload = session.upload.as_ref()?;
        Some(RemoteUploadJob {
            session: token,
            destination: upload.destination().to_owned(),
            port: upload.port(),
            control_dir: upload.control_dir().map(Path::to_path_buf),
            uploaded: upload.uploaded_handle(),
            terminal: session.terminal.clone(),
            proxy: self.proxy.clone(),
        })
    }

    /// Test seam (F6-i7): mark the active session as a remote *integrated*
    /// upload target, as the connect path does, so the App-level image
    /// paste-through flow can be exercised without a real ssh connection.
    #[cfg(test)]
    pub(in crate::native) fn set_active_upload_for_test(&mut self, destination: &str) {
        self.active_mut().upload = Some(RemoteUpload::new(destination.to_owned(), None, None));
    }

    /// Re-establish a dropped remote session in the SAME tab slot: respawn the
    /// stored `ssh` argv, swap the session's I/O (PTY source, input writer, and
    /// read pump) in place, and clear the awaiting-reconnect flag. The token,
    /// tab, and pane layout are unchanged and the terminal model is reused, so
    /// the reconnected shell reappears exactly where it dropped with the prior
    /// scrollback (and the dropped banner) intact. The reconnect anchor is kept,
    /// so a second drop can reconnect again. Returns `true` on success; on spawn
    /// failure the session stays in the awaiting-reconnect state so the prompt
    /// can be retried or dismissed.
    pub(in crate::native) fn reconnect(&mut self, token: SessionToken) -> bool {
        let Some(proxy) = self.proxy.clone() else {
            return false;
        };
        let Some(session) = self.sessions.get(&token) else {
            return false;
        };
        let Some(reconnect) = session.reconnect.as_ref() else {
            return false;
        };
        let (program, args) = reconnect.command.clone().into_program_args();
        let terminal = session.terminal.clone();
        let recorder = session.recorder.clone();
        let grid = crate::native::lock_recover(&terminal).screen().dimensions();
        let Ok(spawned) = PtySession::spawn_exec(grid, program, args, None) else {
            return false;
        };
        let Ok(reader) = spawned.try_clone_reader() else {
            return false;
        };
        let Ok(raw_writer) = spawned.take_writer() else {
            return false;
        };
        let Ok(boxed_writer) = crate::native::pty_writer::writer_shim(raw_writer, token) else {
            return false;
        };
        let writer: PtyWriter = Arc::new(Mutex::new(boxed_writer));
        let diagnostic = spawned.pending_diagnostic_slot();
        // Respawning into a FRESH remote login shell: the terminal model is
        // reused to preserve scrollback, but the dropped session's latched
        // input-reporting modes must not survive the respawn. A stale bracketed
        // paste (DEC 2004) would wrap the next paste in \e[200~ / \e[201~
        // markers the fresh readline never enabled, so it would echo them
        // literally into the command line. The reconnected shell re-emits
        // whatever modes it wants at its first prompt.
        crate::native::lock_recover(&terminal).reset_input_reporting_modes();
        let Ok(pump_thread) = spawn_pty_pump(
            reader,
            writer.clone(),
            terminal,
            proxy,
            token,
            recorder,
            diagnostic,
        ) else {
            return false;
        };
        let pty = Arc::new(Mutex::new(spawned));
        let Some(session) = self.sessions.get_mut(&token) else {
            return false;
        };
        // The old read pump already ended at EOF; drop its handle. The prior PTY
        // child is reaped by `Drop for PtySession` when the old source is
        // replaced below.
        session.pump_thread = Some(pump_thread);
        session.source = SessionSource::Local { pty };
        session.writer = writer;
        session.awaiting_reconnect = false;
        session.needs_rebuild = true;
        true
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

const EXIT_STATUS_SETTLE_TIMEOUT: Duration = Duration::from_millis(50);
const EXIT_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(1);

fn capture_local_exit_code(pty: &Arc<Mutex<PtySession>>) -> Option<i32> {
    let deadline = Instant::now() + EXIT_STATUS_SETTLE_TIMEOUT;
    let mut pty = pty.lock().ok()?;
    loop {
        match pty.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(EXIT_STATUS_POLL_INTERVAL);
            }
            Ok(None) | Err(_) => return None,
        }
    }
}
