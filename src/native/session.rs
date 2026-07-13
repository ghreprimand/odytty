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
use crate::ssh_connect::{
    RemoteSshOptions, SshCommand, ssh_command_for_host_with_options, ssh_tab_title,
};
#[cfg(test)]
use crate::text::CellSize;

use winit::event_loop::EventLoopProxy;

use super::app::{CursorBlinkState, HintsUi, SynchronizedOutputHold, TabBarSource};
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
///   * [`WorkspaceSet::insert_local_session_with`] — the split / new-tab path.
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
pub(super) struct RemoteReconnect {
    command: SshCommand,
}

impl RemoteReconnect {
    pub(super) fn new(command: SshCommand) -> Self {
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
pub(super) struct RemoteUpload {
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
    pub(super) fn new(
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

    pub(super) fn destination(&self) -> &str {
        &self.destination
    }

    #[cfg(not(test))]
    pub(super) fn port(&self) -> Option<u16> {
        self.port
    }

    #[cfg(not(test))]
    pub(super) fn control_dir(&self) -> Option<&Path> {
        self.control_dir.as_deref()
    }

    /// A clonable handle to the uploaded-paths list, handed to the async upload
    /// worker so it can record a remote path once the transfer succeeds.
    pub(super) fn uploaded_handle(&self) -> Arc<Mutex<Vec<String>>> {
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
pub(super) struct RemoteUploadJob {
    pub(super) session: SessionToken,
    pub(super) destination: String,
    pub(super) port: Option<u16>,
    pub(super) control_dir: Option<std::path::PathBuf>,
    pub(super) uploaded: Arc<Mutex<Vec<String>>>,
    pub(super) terminal: Arc<Mutex<Terminal>>,
    pub(super) proxy: Option<EventLoopProxy<UserEvent>>,
}

/// What to do when a session's shell reaches EOF, given its child exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExitDisposition {
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
pub(super) fn classify_remote_exit(code: Option<i32>) -> ExitDisposition {
    match code {
        Some(255) => ExitDisposition::Reconnect,
        _ => ExitDisposition::Close,
    }
}

/// The one-line in-pane notice painted when a remote link drops. Written into
/// the terminal model on its own line (leading/trailing CRLF) using standard SGR
/// so it renders in every theme's palette, and kept short enough for a narrow
/// pane. The prompt actions (Enter / Esc) are handled by the App's key path
/// while the session is awaiting reconnect.
/// CLOSE-HANG: the bounded wall-clock budget the whole-app shutdown teardown
/// waits for every session's child to be reaped and its pump thread joined
/// before it detaches the reaper and lets the process exit anyway. Healthy
/// sessions reap in well under this (the wait returns the instant the reaper
/// signals completion); the deadline only bites when a remote is wedged, so a
/// dead `ssh` link caps teardown here instead of freezing it. The OS reaps any
/// still-orphaned child once the process exits.
pub(super) const SHUTDOWN_REAP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long an interactive attach waits for the host's initial snapshot frame,
/// and the aggregate ceiling for a whole reattach batch, before giving up.
/// Bounded so a stalled or misbehaving host cannot hang window startup forever.
/// Defined here (cross-platform) because the restore path is compiled on every
/// platform; the Unix-only attach transport re-exports it for its own use.
pub(super) const SNAPSHOT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Grace before a single-session close forces its output reader to EOF. A
/// healthy session's reader EOFs the instant its slave closes (well inside
/// this), so the forced path only fires when a `setsid`'d grandchild keeps the
/// slave open.
const CLOSE_READER_JOIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
/// Poll interval while a close reaper waits for its pump join to complete.
const CLOSE_JOIN_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Per-connection snapshot budget for one pane in a restore batch: whatever of
/// the shared `batch_deadline` remains at `now`, capped at `cap`. `None` once the
/// batch budget is spent, so the caller skips the handshake and falls through to
/// a fresh shell instead of blocking the UI for the full per-connection deadline.
///
/// The sole call site is the Unix-only reattach path, so the helper is gated to
/// Unix; the Windows lib target compiles that path out.
#[cfg(unix)]
fn per_connection_attach_budget(
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

/// Join a local session's output pump under a bounded deadline, forcing the PTY
/// reader to EOF if it is wedged.
///
/// The pump reader blocks on the PTY master, which reports EOF only once every
/// slave fd is closed. `kill` SIGKILLs the child's process group, but a
/// `setsid`'d grandchild in a foreign group (e.g. an `ssh` ControlMaster /
/// ControlPersist mux, or a disowned daemon) that inherited the slave is out of
/// reach, so the master never EOFs and a plain `join` — plus this reaper, the
/// pump thread, the writer thread, and the master fds — would block forever.
/// After [`CLOSE_READER_JOIN_GRACE`] the reader is forced to EOF through the
/// session's wake pipe, so the pump exits and the join completes; nothing leaks
/// even on the grandchild-holds-the-slave case.
fn bounded_pump_join(pump: JoinHandle<()>, pty: Arc<Mutex<PtySession>>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let done = Arc::new(AtomicBool::new(false));
    let done_signal = done.clone();
    let joiner = std::thread::Builder::new()
        .name("odytty-pump-join".to_owned())
        .spawn(move || {
            let _ = pump.join();
            done_signal.store(true, Ordering::Release);
        });
    let Ok(joiner) = joiner else {
        // Could not spawn the joiner (resource exhaustion): best-effort force the
        // reader to EOF and abandon rather than block the reaper.
        if let Ok(guard) = pty.lock() {
            guard.force_reader_eof();
        }
        return;
    };

    let deadline = Instant::now() + CLOSE_READER_JOIN_GRACE;
    while Instant::now() < deadline {
        if done.load(Ordering::Acquire) {
            let _ = joiner.join();
            return;
        }
        std::thread::sleep(CLOSE_JOIN_POLL);
    }
    // Still wedged after the grace: force the reader to EOF so the pump exits.
    if let Ok(guard) = pty.lock() {
        guard.force_reader_eof();
    }
    let _ = joiner.join();
}

const RECONNECT_BANNER: &str =
    "\r\n\x1b[1;33m connection dropped \x1b[0m  Enter: reconnect · Esc: close\r\n";

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
    /// CTRL-CLICK-OPEN latch: `true` while the left button is held after a
    /// Ctrl/Cmd+click over a resolved span was intercepted and opened. The
    /// paired left release is then swallowed so a mouse-reporting app sees
    /// neither the press nor the release for that gesture (matching
    /// kitty/iTerm2/GNOME Terminal). Cleared at the start of every fresh left
    /// press, so a release lost to focus change never swallows a later click.
    pub(super) swallow_open_left_release: bool,
    pub(super) viewport: Viewport,
    pub(super) search: SearchUi,
    pub(super) hints: Option<HintsUi>,
    pub(super) copy_mode: Option<CopyModeState>,
    pub(super) search_restore_viewport: Option<usize>,
    pub(super) last_scrollback_len: usize,
    /// Last scrollback front-trim epoch reconciled into absolute-coordinate UI
    /// state. A mismatch means row zero moved and stale selections cannot be
    /// trusted to name the same bytes.
    pub(super) last_scrollback_trim_epoch: u64,
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
    /// Sub-row scroll remainder in rows (SCROLL-FEEL Tier 2), invariant
    /// `(-1.0, 1.0)`; whole rows carry into `viewport`. Drives
    /// [`Self::scroll_frac_offset`]. `0.0` at rest.
    pub(super) scroll_frac_rows: f32,
    pub(super) scroll_frac_offset: f32,
    /// SCROLL-GLIDE forward-chase follower. `glide_visual` is the rendered
    /// viewport position in offset-rows; it eases toward the integer
    /// `viewport` offset while `glide_active`. `glide_target` is the logical
    /// offset being chased (a between-frame change of it, e.g. output growth,
    /// snaps the glide). `glide_last_tick` is the previous frame time for the
    /// frame-rate-independent step. Inactive/at rest: `glide_active == false`,
    /// `glide_visual == offset`, and the render path is byte-identical.
    pub(super) glide_visual: f32,
    pub(super) glide_active: bool,
    pub(super) glide_target: usize,
    pub(super) glide_last_tick: Option<Instant>,
    /// Remote reconnect anchor (F6-i4). `Some` only for sessions launched through
    /// the `ssh` connect path; `None` for a local shell, so exit classification
    /// and the reconnect prompt never engage for a local session. See
    /// [`RemoteReconnect`].
    pub(super) reconnect: Option<RemoteReconnect>,
    /// True while this remote session's link has dropped (`ssh` exit 255) and the
    /// in-pane reconnect prompt is showing. Keys drive the prompt (Enter to
    /// reconnect, Esc/Ctrl+D to dismiss) rather than the now-dead shell. Cleared
    /// on a successful reconnect or when the tab is closed.
    pub(super) awaiting_reconnect: bool,
    /// Image paste-through upload descriptor (F6-i7). `Some` only on a remote
    /// *integrated* ssh session; `None` for a local shell or an integration-off
    /// plain-ssh tab, so image paste-through never engages there. See
    /// [`RemoteUpload`].
    pub(super) upload: Option<RemoteUpload>,
    /// The remote host this session is connected to (RESTORE-REMOTE), or
    /// `None` for a local shell. Set by the `ssh` connect path to the
    /// saved-profile alias (when opened from a `hosts.conf` entry) or the
    /// literal `[user@]host[:port]` destination (ad-hoc). Captured into the
    /// shape snapshot so restore respawns the pane through the connect path
    /// instead of a local shell at the remote's cwd. Never set on a local
    /// session, so a local pane's capture/restore is unchanged.
    pub(super) remote_destination: Option<String>,
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
        let last_scrollback_trim_epoch = terminal
            .lock()
            .map(|terminal| terminal.scrollback_trim_epoch())
            .unwrap_or(0);
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
            swallow_open_left_release: false,
            viewport: Viewport::default(),
            search: SearchUi::default(),
            hints: None,
            copy_mode: None,
            search_restore_viewport: None,
            last_scrollback_len: 0,
            last_scrollback_trim_epoch,
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

    pub(super) fn refresh_tab_title(&mut self) {
        self.tab_title = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.title().map(ToOwned::to_owned))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "odytty".to_owned());
    }

    /// Anchor this pane's scrollback viewport across output growth and refresh
    /// its growth baseline, returning the live offset to snapshot at. This is
    /// the single "stay scrolled" bookkeeping shared by the single-pane render
    /// path and the multipane rebuild loop so the two can never diverge: a
    /// scrolled-back pane (foreground, background split, or the focused pane of
    /// a split tab) stays pinned to the same absolute rows as fresh PTY output
    /// arrives, and the baseline stays current so collapsing a split back to a
    /// single pane applies no accumulated jump. A no-op at the live tail
    /// (offset 0) and when nothing grew.
    pub(super) fn anchor_viewport_for_render(&mut self, scrollback_len: usize) -> usize {
        let added = scrollback_len.saturating_sub(self.last_scrollback_len);
        self.viewport.anchor_after_growth(added, scrollback_len);
        self.last_scrollback_len = scrollback_len;
        self.viewport.clamp(scrollback_len);
        self.viewport.offset()
    }

    /// Settle the cursor-animation timers — blink phase, ID1 easing fade, VE4
    /// slide — to their at-rest identity with no scheduled wake. These are the
    /// timers whose ONLY consumer is the render path's per-frame poll
    /// (`cursor_blink.poll` / `update_cursor_easing` / `update_cursor_motion`),
    /// so a pane with no render consumer strands their past toggle deadline in
    /// the wake set and busy-spins. Two panes lack that consumer: a background
    /// pane (never rendered, NF20-B) and the focused pane of a MULTI-pane tab
    /// (`rebuild_multipane` advances no cursor timer, NF21-1). Idempotent;
    /// every animation re-arms from the current frame time when the pane is
    /// next rendered on the single-pane path.
    pub(super) fn park_cursor_timers(&mut self) {
        self.cursor_blink.park();
        self.cursor_anim_alpha = 1.0;
        self.cursor_ease_deadline = None;
        self.cursor_ease_phase_on = true;
        self.cursor_ease_toggle_at = None;
        self.cursor_anim_offset = [0.0, 0.0];
        self.cursor_slide_deadline = None;
        self.cursor_slide_start = None;
        self.cursor_slide_from_px = [0.0, 0.0];
    }

    /// Settle every timer of a never-rendered (background) pane: the cursor
    /// timers above PLUS the synchronized-output hold. A background pane is
    /// never rendered, so none of these has a consumer (NF20-B). The
    /// synchronized-output hold is parked ONLY here: unlike the cursor timers it
    /// is consumed by `should_hold` in the render branch (which runs before the
    /// single/multi split) and its 150 ms deadline is the crash-protection
    /// watchdog that auto-releases a frozen display — so the focused pane of a
    /// multi-pane tab keeps its hold live (parking it would defeat the watchdog)
    /// and parks only its cursor timers.
    pub(super) fn park_animation_timers(&mut self) {
        self.park_cursor_timers();
        self.synchronized_output_hold.clear();
    }

    /// Clear every piece of UI state whose coordinates are tied to the row /
    /// scrollback layout, so a reflow never leaves a selection, hover span,
    /// search match, hint label, or copy-mode caret pointing at cells the text
    /// no longer occupies.
    ///
    /// Run for EVERY session a resize reflows, not just the active one (NF21-3):
    /// [`WorkspaceSet::resize_all_panes`] reflows every tab's panes, but the clear that
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
        self.swallow_open_left_release = false;
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

    /// Drop the transient pointer-input latches so an active-session change
    /// cannot leave them stranded on the outgoing session or phantom-hovering on
    /// the incoming one (NF21-8 / NF21-9). Unlike
    /// [`Self::invalidate_layout_dependent_state`] this deliberately leaves the
    /// selection, viewport, search, hints and copy-mode state untouched — a tab
    /// or workspace switch is not a reflow, so a made selection must survive to
    /// be copied on switch-back. Only the in-flight drag and the last hover cell
    /// are cleared: a mid-drag switch must not resurrect a buttonless
    /// `Selecting` latch, and a stale `pointer_cell` must not paint a phantom
    /// hover (or open a stale Ctrl+click target) before the first real
    /// `CursorMoved` on the new surface.
    pub(super) fn clear_input_latches(&mut self) {
        self.pointer_drag = PointerDrag::None;
        self.pointer_cell = None;
        self.pointer_px = None;
        self.report_button = None;
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

    /// Fire a best-effort remote cleanup for any images this session uploaded
    /// during its life (F6-i7). Runs the `rm -f` over a detached `ssh` (reusing
    /// the live `ControlMaster` when available) and never waits on it, so tab
    /// close stays instant. Best-effort by nature: if the link already dropped
    /// the command cannot run and the remote's own `/tmp` reaper removes the
    /// file. Compiled to a no-op under `cfg(test)` so closing a synthetic remote
    /// tab never spawns a real `ssh`.
    fn fire_upload_cleanup(&self) {
        let Some(upload) = self.upload.as_ref() else {
            return;
        };
        let paths = std::mem::take(&mut *crate::native::lock_recover(&upload.uploaded_handle()));
        // Under `cfg(test)` the paths are simply drained (no real `ssh`); the
        // discard keeps the binding used without a trailing no-op return.
        #[cfg(test)]
        let _ = paths;
        #[cfg(not(test))]
        if !paths.is_empty()
            && let Some(command) = crate::ssh_connect::remote_cleanup_command(
                upload.destination(),
                upload.port(),
                upload.control_dir(),
                &paths,
            )
        {
            let (program, args) = command.into_program_args();
            let _ = std::process::Command::new(program)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }

    fn close(mut self) -> bool {
        self.fire_upload_cleanup();
        let pump_thread = self.pump_thread.take();
        match self.source {
            SessionSource::Local { pty } => {
                // Kill promptly on the calling (main) thread — SIGKILL delivery
                // never waits for the child to actually die.
                if let Ok(mut guard) = pty.lock() {
                    let _ = guard.kill();
                }
                // Defer the blocking reap (`wait`) + reader join to a detached
                // thread. CLOSE-HANG-2: a ControlPersist mux master (or any
                // `setsid`'d grandchild in a foreign process group) can hold the
                // PTY slave open after the child is killed, so the reader never
                // sees EOF on its own and a plain join would block forever —
                // leaking this reaper, the pump thread, the writer thread, and
                // the master fds. `bounded_pump_join` bounds the join and, if the
                // reader is still wedged after the grace, forces it to EOF via the
                // session's wake pipe so the pump exits and everything is
                // released. The close itself returns to the event loop at once.
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut guard) = pty.lock() {
                            let _ = guard.wait();
                        }
                        if let Some(thread) = pump_thread {
                            bounded_pump_join(thread, pty);
                        }
                    });
            }
            // Closing an attached tab is a clean detach: the host keeps the PTY
            // + terminal model alive for later reattach by id. `detach()` is
            // best-effort network I/O and the reader join is unbounded, so run
            // both off the main thread too (`Drop` on the client is the backstop).
            #[cfg(unix)]
            SessionSource::Attached { client } => {
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut guard) = client.lock() {
                            let _ = guard.detach();
                        }
                        if let Some(thread) = pump_thread {
                            let _ = thread.join();
                        }
                    });
            }
        }
        true
    }

    fn close_after_shell_exit(mut self) -> bool {
        self.fire_upload_cleanup();
        let pump_thread = self.pump_thread.take();
        match &self.source {
            SessionSource::Local { pty } => {
                let pty = pty.clone();
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut guard) = pty.lock() {
                            let _ = guard.try_wait();
                        }
                        // Even after the shell self-exits, a `setsid`'d
                        // grandchild can keep the slave open, so bound the pump
                        // join and force the reader to EOF if it is wedged rather
                        // than leak the pump/writer threads and the master fds.
                        if let Some(thread) = pump_thread {
                            bounded_pump_join(thread, pty);
                        }
                    });
            }
            // The host child already exited (or the link dropped). Detach is
            // best-effort network I/O and the reader is ending on its own, but a
            // wedged control socket could still stall an inline detach + join —
            // run both off the main thread, matching the Local arm.
            #[cfg(unix)]
            SessionSource::Attached { client } => {
                let client = client.clone();
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut client) = client.lock() {
                            let _ = client.detach();
                        }
                        if let Some(thread) = pump_thread {
                            let _ = thread.join();
                        }
                    });
            }
        }
        true
    }

    /// Whole-app shutdown teardown for one session. SIGKILL the child *now*
    /// (synchronous but non-blocking — signal delivery never waits) so no
    /// runaway shell or `ssh` client survives the process, then return the
    /// blocking reap (`wait`) + pump-thread join as a deferred closure the
    /// caller runs OFF the main thread under a single bounded deadline
    /// ([`WorkspaceSet::shutdown_all`]). This is the CLOSE-HANG fix: a wedged
    /// remote — an `ssh` client parked in a dead-socket syscall whose `wait()`
    /// blocks for the kernel's network timeout, or a reader thread that never
    /// sees EOF — must never delay window teardown. The process is exiting, so
    /// if the deferred reaper outlives the deadline the OS reaps the orphaned
    /// child and the detached thread dies with the process. [`Self::close`]
    /// (single-tab close in a long-lived process) uses the same off-main-thread
    /// deferral, but its reaper runs to completion — reaping the zombie and
    /// joining the reader — rather than leaning on process exit, so a live
    /// process leaks neither.
    fn shutdown(mut self) -> Box<dyn FnOnce() + Send> {
        self.fire_upload_cleanup();
        let pump_thread = self.pump_thread.take();
        match self.source {
            SessionSource::Local { pty } => {
                // Kill promptly on the calling thread — SIGKILL is delivered
                // without waiting for the child to actually die.
                if let Ok(mut guard) = pty.lock() {
                    let _ = guard.kill();
                }
                Box::new(move || {
                    if let Ok(mut guard) = pty.lock() {
                        let _ = guard.wait();
                    }
                    if let Some(thread) = pump_thread {
                        let _ = thread.join();
                    }
                })
            }
            // A detach is best-effort network I/O; defer it too so a wedged
            // control socket cannot stall teardown.
            #[cfg(unix)]
            SessionSource::Attached { client } => Box::new(move || {
                if let Ok(mut guard) = client.lock() {
                    let _ = guard.detach();
                }
                if let Some(thread) = pump_thread {
                    let _ = thread.join();
                }
            }),
        }
    }
}

/// One tab in the strip. It owns a layout tree of panes (a binary
/// [`PaneNode`]) and tracks which pane within the tab is focused. A fresh tab
/// is a single [`PaneNode::Leaf`], which the render/resize paths treat
/// byte-identically to today's single-session window (design doc §2.3). Pane
/// splitting is wired in later work; for now every tab is a single
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
    /// Unseen-activity latch for the rollup indicator (NF21-6 / ODP-6 v2). Set
    /// when a bell rings in one of this tab's panes while the tab is NOT the
    /// active-visible tab; cleared once the tab is viewed (it is the active tab
    /// of the active workspace). Tab granularity is the finest useful rollup
    /// unit; workspace-level activity is DERIVED from its tabs
    /// ([`WorkspaceSet::workspace_has_activity`]) rather than stored twice. The
    /// rollup UI that renders this flag is deferred to a later cycle; for now
    /// only the signal is landed and maintained, so it has no reader yet.
    #[allow(dead_code)]
    pub(super) activity: bool,
}

impl Tab {
    /// A single-pane tab wrapping one session.
    fn single(token: SessionToken) -> Self {
        Self {
            layout: PaneNode::leaf(token),
            focused: token,
            title_override: None,
            zoomed: false,
            activity: false,
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

/// One workspace: a named, ordered list of tabs with a focused (active) tab
/// (design doc §3.1). The workspace layer sits ABOVE tabs — a [`WorkspaceSet`]
/// owns an ordered list of these plus the single, flat session arena that every
/// tab's panes reference by token. Per the §3.3 naming hazard this layer is
/// never called a "session".
pub(super) struct Workspace {
    /// User-visible, renameable label; defaults to "Workspace N". Read by the
    /// command palette / keyboard layer and the workspace-rail chrome.
    pub(super) name: String,
    /// The tabs of this workspace, in strip order. `Tab` is unchanged.
    pub(super) tabs: Vec<Tab>,
    /// Index into `tabs` of the focused tab.
    pub(super) active_tab: usize,
    /// The host alias this workspace is bound to (F6-W5, ODP-9). When `Some`, a
    /// New Tab opened while this workspace is active routes through the remote
    /// connect path for that host instead of spawning a local shell; the
    /// "New Local Tab" escape hatch always spawns a local shell regardless.
    /// `None` (the default) is byte-identical to the pre-W5 local-only behavior.
    pub(super) default_profile: Option<String>,
}

impl Workspace {
    /// A fresh workspace wrapping a single single-pane tab for `token`.
    fn single(name: String, token: SessionToken) -> Self {
        Self {
            name,
            tabs: vec![Tab::single(token)],
            active_tab: 0,
            default_profile: None,
        }
    }
}

/// The generated default label for the workspace that will sit at zero-based
/// rail `index` ("Workspace 1", "Workspace 2", …). Kept in one place so the
/// spawn sites and the PRISTINE-CONSUME default-name check
/// ([`WorkspaceSet::is_single_pristine_workspace`]) can never disagree about
/// what an untouched, never-renamed workspace is called.
fn default_workspace_name(index: usize) -> String {
    format!("Workspace {}", index + 1)
}

/// The workspace list and the session arena that backs it (design doc §3.1).
///
/// The hierarchy is `WorkspaceSet` -> [`Workspace`] -> [`Tab`] -> pane. Sessions
/// live in ONE arena keyed by [`SessionToken`] so pump-thread lookup by token
/// stays O(1) and never has to walk the hierarchy (§5 rule 1); the workspace /
/// tab / pane tree carries only presentation and focus. Splitting `tabs` /
/// `active_tab` out of the set and into [`Workspace`] is a one-level push-down:
/// the arena, token counter, and every global toggle stay on the set, so the
/// ~35 `Deref` sites and all keyboard/cursor/selection paths are untouched by
/// construction. `Deref`/`DerefMut` resolve to the focused pane of the active
/// tab of the ACTIVE workspace. With a single workspace this is behaviourally
/// identical to the previous single-`Vec<Tab>` model.
pub(super) struct WorkspaceSet {
    sessions: HashMap<SessionToken, Session>,
    workspaces: Vec<Workspace>,
    active_ws: usize,
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

impl WorkspaceSet {
    pub(super) fn new(initial: Session, proxy: Option<EventLoopProxy<UserEvent>>) -> Self {
        let token = initial.id;
        let next_token = token.0.saturating_add(1);
        let mut sessions = HashMap::new();
        sessions.insert(token, initial);
        Self {
            sessions,
            workspaces: vec![Workspace::single(default_workspace_name(0), token)],
            active_ws: 0,
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

    /// The active workspace. A set never holds zero workspaces (the last one
    /// closing exits the app), and `active_ws` is kept in range by every
    /// workspace-removing path; the fallback to the first workspace mirrors
    /// `active_focused_token`'s defensive lookup so a stray index can never
    /// panic.
    fn active_workspace(&self) -> &Workspace {
        self.workspaces
            .get(self.active_ws)
            .or_else(|| self.workspaces.first())
            .expect("WorkspaceSet always holds at least one workspace")
    }

    fn active_workspace_mut(&mut self) -> &mut Workspace {
        let idx = if self.active_ws < self.workspaces.len() {
            self.active_ws
        } else {
            0
        };
        self.workspaces
            .get_mut(idx)
            .expect("WorkspaceSet always holds at least one workspace")
    }

    /// The active tab of the active workspace, if the workspace has one.
    fn active_tab_ref(&self) -> Option<&Tab> {
        let ws = self.active_workspace();
        ws.tabs.get(ws.active_tab)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let ws = self.active_workspace_mut();
        ws.tabs.get_mut(ws.active_tab)
    }

    /// Locate the `(workspace index, tab index)` of the tab whose layout tree
    /// contains `token`, scanning ALL workspaces. Pane close / shell-exit reap a
    /// token that may live in a background workspace, and attach dedup (ODP-10)
    /// deep-switches to whichever workspace owns a token - both need the full
    /// scan, not the active workspace alone.
    fn locate_token(&self, token: SessionToken) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(ws_idx, ws)| {
            ws.tabs
                .iter()
                .position(|tab| tab.layout.contains(token))
                .map(|tab_idx| (ws_idx, tab_idx))
        })
    }

    /// Remove the workspace at `ws_idx` (its last tab just closed - no empty
    /// workspaces, ODP-3) and clamp `active_ws` onto a surviving workspace,
    /// mirroring the tab-removal clamp. Returns `true` iff no workspaces remain,
    /// i.e. the last workspace closed and the caller should signal app exit.
    fn remove_workspace(&mut self, ws_idx: usize) -> bool {
        self.workspaces.remove(ws_idx);
        if self.workspaces.is_empty() {
            self.active_ws = 0;
            return true;
        }
        if self.active_ws == ws_idx {
            self.active_ws = ws_idx.min(self.workspaces.len() - 1);
        } else if self.active_ws > ws_idx {
            self.active_ws -= 1;
        }
        false
    }

    /// Number of workspaces. The app's close-tab exit guard keys on this: the
    /// last tab of the last workspace exits, but the last tab of a non-last
    /// workspace merely closes that workspace.
    pub(super) fn workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    /// PRISTINE-CONSUME: true when the whole set is exactly one untouched,
    /// freshly-spawned workspace — the state a bare launch produces. Used at
    /// open-layout time to decide whether an append should CONSUME this default
    /// workspace (replace it) rather than leave it beside the restored set, so a
    /// layout opened onto a fresh window yields exactly what was saved.
    ///
    /// Judged on SHAPE facts only — never shell activity: one workspace still
    /// bearing its generated ([`default_workspace_name`]) name, no host binding,
    /// a single tab with no title override, and that tab a single leaf pane (no
    /// splits). ANY real state — a second workspace, a rename, a host binding, a
    /// split, or an extra tab — is not pristine and appends as before.
    pub(super) fn is_single_pristine_workspace(&self) -> bool {
        if self.workspaces.len() != 1 {
            return false;
        }
        let ws = &self.workspaces[0];
        ws.name == default_workspace_name(0)
            && ws.default_profile.is_none()
            && ws.tabs.len() == 1
            && ws.tabs[0].title_override.is_none()
            && ws.tabs[0].layout.is_single_pane()
    }

    /// The token of the focused pane of the active tab - the `Deref` target.
    fn active_focused_token(&self) -> SessionToken {
        let ws = self.active_workspace();
        ws.tabs
            .get(ws.active_tab)
            .or_else(|| ws.tabs.first())
            .map(|tab| tab.focused)
            .unwrap_or(SessionToken(0))
    }

    pub(super) fn active(&self) -> &Session {
        let token = self.active_focused_token();
        self.sessions
            .get(&token)
            .or_else(|| self.sessions.values().next())
            .expect("WorkspaceSet always holds at least one session while active() is called")
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
            .expect("WorkspaceSet always holds at least one session while active_mut() is called")
    }

    pub(super) fn active_id(&self) -> SessionToken {
        self.active_focused_token()
    }

    /// A clone of the event-loop proxy, for background workers that need to wake
    /// a redraw when they finish (e.g. the Test Connection probe). `None` in
    /// headless / test builds without a real event loop.
    pub(super) fn event_proxy(&self) -> Option<EventLoopProxy<UserEvent>> {
        self.proxy.clone()
    }

    /// Park the animation / render-hold timers of every pane that has no render
    /// consumer this frame, matching the fan-out of the `next_wake_deadline`
    /// sources with a consumer of equal reach (NF20-B / NF21-1).
    ///
    /// Consumer scope (§5 rule 2): the only pane with live animation timers is
    /// the focused pane of the active tab of the ACTIVE WORKSPACE — everything
    /// else (all background workspaces, all background tabs, all non-focused
    /// panes) is parked. Collectors iterate the flat arena (§5 rule 1), never
    /// the hierarchy; "active" is resolved once through `active_focused_token`
    /// so this and the redraw gate can never disagree about which pane is live.
    ///
    /// - Every pane of an inactive tab (in any workspace) and every non-focused
    ///   pane of the active tab is never rendered → fully parked
    ///   (`park_animation_timers`).
    /// - The focused pane of a **single-pane** active tab keeps ALL its timers:
    ///   the single-pane frame path polls its blink/ease/slide each rebuild and
    ///   `should_hold` consumes its render hold.
    /// - The focused pane of a **multi-pane** active tab renders through
    ///   `rebuild_multipane`, which advances no cursor timer, so its blink /
    ///   ease / slide would strand a past deadline and spin (NF21-1). Park just
    ///   those (`park_cursor_timers`); its synchronized-output hold stays live
    ///   (still consumed by `should_hold`; its deadline is the crash watchdog).
    ///
    /// Idempotent; cheap (few panes).
    pub(super) fn park_background_timers(&mut self) {
        let active = self.active_focused_token();
        let active_multipane = !self.active_is_single_pane();
        for (token, session) in self.sessions.iter_mut() {
            if *token != active {
                session.park_animation_timers();
            } else if active_multipane {
                session.park_cursor_timers();
            }
        }
    }

    /// True when any currently visible pane of the active tab has its
    /// `needs_rebuild` flag set. The render gate ORs this across the whole tab so
    /// output streaming into a non-focused split pane repaints even while the
    /// focused pane is idle (NF21-7) — `self.needs_rebuild` alone is the focused
    /// pane's flag (the `Deref` target). For a single-pane tab this is exactly
    /// the focused pane's flag, so the single-pane gate decision is unchanged.
    pub(super) fn any_visible_pane_needs_rebuild(&self) -> bool {
        self.active_visible_tokens()
            .into_iter()
            .any(|token| self.sessions.get(&token).is_some_and(|s| s.needs_rebuild))
    }

    /// True when any currently visible pane of the active tab has an in-flight
    /// SCROLL-GLIDE follower. The multipane wake path sources a frame-paced
    /// repaint off this so a split's per-pane glide advances every frame until it
    /// settles (mirrors the focused-only `scroll_glide_deadline` for single-pane).
    /// For a single-pane tab this is exactly the focused pane's `glide_active`.
    pub(super) fn any_visible_pane_gliding(&self) -> bool {
        self.active_visible_tokens()
            .into_iter()
            .any(|token| self.sessions.get(&token).is_some_and(|s| s.glide_active))
    }

    /// Clear `needs_rebuild` on every visible pane of the active tab. Paired with
    /// [`Self::any_visible_pane_needs_rebuild`]: `rebuild_multipane` snapshots
    /// every visible pane, so it must clear every visible pane's flag — clearing
    /// only the focused pane's would leave a dirtied background pane's flag set
    /// and re-open the (now tab-wide) gate every frame, a rebuild storm (NF21-7).
    pub(super) fn clear_visible_pane_rebuild_flags(&mut self) {
        for token in self.active_visible_tokens() {
            if let Some(session) = self.sessions.get_mut(&token) {
                session.needs_rebuild = false;
            }
        }
    }

    /// The tokens of the panes currently on screen for the active tab: just the
    /// focused pane while zoomed (only it is rendered), otherwise every leaf of
    /// the active tab's layout. Mirrors [`Self::is_visible_pane`]'s membership.
    fn active_visible_tokens(&self) -> Vec<SessionToken> {
        match self.active_tab_ref() {
            Some(tab) if tab.is_effectively_zoomed() => vec![tab.focused],
            Some(tab) => tab.layout.leaves(),
            None => Vec::new(),
        }
    }

    /// Clear the pointer-input latches on EVERY session in the arena (NF21-8 /
    /// NF21-9). Called from the active-session-change seam, which fires on both
    /// tab and workspace switches post-W1: sweeping the flat arena covers the
    /// outgoing session (whose in-flight drag must not survive), the incoming
    /// session (whose stale hover cell must not paint), and every background
    /// session across all workspaces in one pass. Selection and viewport state
    /// are intentionally preserved — see [`Session::clear_input_latches`].
    pub(super) fn clear_all_input_latches(&mut self) {
        for session in self.sessions.values_mut() {
            session.clear_input_latches();
        }
    }

    /// Clear stale absolute-coordinate state after scrollback front eviction.
    /// The terminal pump mutates the model asynchronously, so the app calls
    /// this at the start of each redraw before clipboard requests or painting.
    pub(super) fn reconcile_scrollback_trims(&mut self) {
        for session in self.sessions.values_mut() {
            let epoch = crate::native::lock_recover(&session.terminal).scrollback_trim_epoch();
            if epoch != session.last_scrollback_trim_epoch {
                session.invalidate_layout_dependent_state();
                session.last_scrollback_trim_epoch = epoch;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn active_position(&self) -> usize {
        self.active_workspace().active_tab
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
        match self.active_tab_ref() {
            // While zoomed only the focused pane is on screen, so background
            // panes' output must not drive a redraw (it would not be visible).
            Some(tab) if tab.is_effectively_zoomed() => tab.focused == token,
            Some(tab) => tab.layout.contains(token),
            None => false,
        }
    }

    /// Reconcile the scrollback-growth baseline (`last_scrollback_len`) of every
    /// pane of the active tab to its terminal's current scrollback length,
    /// WITHOUT anchoring the viewport. Called on activation (tab / workspace
    /// switch): a tab keeps producing output while it is backgrounded, but it is
    /// not rendered, so `anchor_viewport_for_render` never runs and its baseline
    /// freezes at the length from its last on-screen frame. Without this, the
    /// first render after switching back computes `added = current - stale`
    /// (all the backgrounded growth at once) and `anchor_after_growth` yanks a
    /// scrolled-up viewport toward the top of scrollback, stranding fresh output
    /// offscreen below — the user returns to a tab "stuck scrolled up" with new
    /// output invisible. Treating the backgrounded growth as already-past
    /// preserves the pane's scroll position across the switch: a pane at the
    /// live bottom (offset 0) stays live, and a scrolled-up pane keeps its
    /// offset relative to the now-current bottom rather than jumping into deep
    /// history. This is the viewport analogue of the new-output-fade
    /// discontinuity the activation path already clears (NF21-12). A no-op for a
    /// tab that was never backgrounded (its baseline already equals its current
    /// length). Platform-neutral: viewport/scrollback bookkeeping is identical
    /// on Unix and Windows.
    pub(super) fn reconcile_active_tab_scroll_baselines(&mut self) {
        let Some(tab) = self.active_tab_ref() else {
            return;
        };
        for token in tab.layout.leaves() {
            if let Some(session) = self.sessions.get_mut(&token) {
                let len = crate::native::lock_recover(&session.terminal)
                    .screen()
                    .scrollback_len();
                session.last_scrollback_len = len;
            }
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
        match self.active_tab_ref() {
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
        self.active_tab_ref()
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
        self.active_tab_ref()
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
        self.active_tab_ref()
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
        self.active_tab_mut()
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
        self.active_tab_mut().and_then(|tab| {
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
                let (cols, rows) = grid_dims_for_rect(rect, cell_w, cell_h);
                let Some(session) = self.sessions.get_mut(&token) else {
                    continue;
                };
                let mut dimensions_changed = false;
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
                    terminal.set_cell_metrics(cell_w, cell_h);
                }
                if dimensions_changed {
                    session.invalidate_layout_dependent_state();
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
        let Some(tab) = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .find(|tab| tab.layout.contains(token))
        else {
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
        let Some(tab) = self
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.tabs.iter_mut())
            .find(|tab| tab.layout.contains(token))
        else {
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
        self.workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(move |tab| {
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
    fn insert_spawned_session_in(
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
    fn insert_restored_session(
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
    fn insert_exec_session(
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
    pub(super) fn insert_ssh_restored_session(
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

    fn insert_local_session_with(
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
        let writer: PtyWriter = Arc::new(Mutex::new(super::pty_writer::writer_shim(
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
    pub(super) fn spawn(
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
    pub(super) fn connect_ssh_in_new_tab(
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
    pub(super) fn remote_upload_for(
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
        sink: impl super::attach::AttachEventSink,
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
    fn reattach_restored_session(
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
    fn reattach_restored_session(
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
        sink: impl super::attach::AttachEventSink,
    ) -> Result<SessionToken, std::io::Error> {
        let socket =
            resolve_session_socket(runtime_base, session_id).map_err(std::io::Error::other)?;
        self.insert_attached_session(&socket, session_id, sink)
    }

    /// The focused-pane token of the tab at `position` in the strip.
    pub(super) fn token_at_position(&self, position: usize) -> Option<SessionToken> {
        self.active_workspace()
            .tabs
            .get(position)
            .map(|tab| tab.focused)
    }

    /// The strip index of the tab that contains `token` as one of its panes.
    pub(super) fn position_of_token(&self, token: SessionToken) -> Option<usize> {
        self.active_workspace()
            .tabs
            .iter()
            .position(|tab| tab.layout.contains(token))
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

    /// Whole-app shutdown (CLOSE-HANG): drain every session, SIGKILL each child
    /// promptly on the calling thread, then reap + join them OFF the calling
    /// thread under a single bounded `deadline`. Draining first means
    /// [`Self::is_empty`] holds the instant this returns. A wedged remote can no
    /// longer freeze teardown: the blocking `wait()`/join runs on a detached
    /// reaper thread, and the caller waits at most `deadline` before letting the
    /// process exit (the OS reaps any orphaned child). Healthy sessions reap
    /// well within the budget — the wait returns as soon as the reaper signals,
    /// not after the full deadline. Replaces the old serial per-session
    /// `pty.wait()` + `pump_thread.join()` on the main thread, which blocked
    /// indefinitely on a hung `ssh` client and produced the Super+Q
    /// not-responding stall.
    pub(super) fn shutdown_all(&mut self, deadline: std::time::Duration) {
        let drained = std::mem::take(&mut self.sessions);
        self.workspaces.clear();
        self.active_ws = 0;
        // `Session::shutdown` sends SIGKILL synchronously as the reaper closures
        // are built, so every child is signalled before we wait on anything.
        let reapers: Vec<Box<dyn FnOnce() + Send>> =
            drained.into_values().map(Session::shutdown).collect();
        if reapers.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("odytty-shutdown-reap".to_owned())
            .spawn(move || {
                for reap in reapers {
                    reap();
                }
                let _ = tx.send(());
            });
        // If even the reaper thread fails to spawn, don't block — the process is
        // exiting and the OS reaps. Otherwise wait up to the deadline for a
        // clean reap, then detach.
        if spawned.is_ok() {
            let _ = rx.recv_timeout(deadline);
        }
    }

    /// Focus the tab (and pane) that owns `token`, scanning ALL workspaces so a
    /// selection can deep-switch the active workspace + tab + focused pane in
    /// one step (attach dedup / summon deep-focus, ODP-10). With a single
    /// workspace this is byte-identical to the previous same-workspace switch.
    /// Returns true when the focus target actually moved.
    pub(super) fn switch(&mut self, token: SessionToken) -> bool {
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return false;
        };
        let already = self.active_ws == ws_idx
            && self.workspaces[ws_idx].active_tab == tab_idx
            && self.workspaces[ws_idx].tabs[tab_idx].focused == token;
        if already {
            return false;
        }
        self.active_ws = ws_idx;
        self.workspaces[ws_idx].active_tab = tab_idx;
        self.workspaces[ws_idx].tabs[tab_idx].focused = token;
        true
    }

    pub(super) fn next(&mut self) -> bool {
        let ws = self.active_workspace_mut();
        if ws.tabs.len() <= 1 {
            return false;
        }
        ws.active_tab = (ws.active_tab + 1) % ws.tabs.len();
        true
    }

    pub(super) fn prev(&mut self) -> bool {
        let ws = self.active_workspace_mut();
        if ws.tabs.len() <= 1 {
            return false;
        }
        ws.active_tab = if ws.active_tab == 0 {
            ws.tabs.len() - 1
        } else {
            ws.active_tab - 1
        };
        true
    }

    pub(super) fn close(&mut self, token: SessionToken) -> bool {
        self.close_with(token, Session::close)
    }

    pub(super) fn close_shell_exited(&mut self, token: SessionToken) -> bool {
        self.close_with(token, Session::close_after_shell_exit)
    }

    /// Capture the exit code of a session's local PTY child after its reader has
    /// reached EOF. The child is already dead at the EOF fork, so `try_wait()`
    /// returns synchronously — no blocking `wait()` is introduced. `None` means
    /// no code was available: a Unix signal death (`.code() == None`) or, on
    /// Windows, a post-EOF `STILL_ACTIVE` (259) sentinel that `try_wait` maps to
    /// `Ok(None)`; both are treated as "unknown status", never "still running".
    /// An attached session has no local PTY and also yields `None`.
    fn capture_exit_code(&self, token: SessionToken) -> Option<i32> {
        match &self.sessions.get(&token)?.source {
            SessionSource::Local { pty } => {
                pty.lock().ok()?.try_wait().ok()?.and_then(|s| s.code())
            }
            #[cfg(unix)]
            SessionSource::Attached { .. } => None,
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
    pub(super) fn try_arm_reconnect(&mut self, token: SessionToken) -> bool {
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
    pub(super) fn active_awaiting_reconnect(&self) -> bool {
        self.active().awaiting_reconnect
    }

    /// The active session's remote upload destination (`user@host`), or `None`
    /// when the active tab is not a remote *integrated* ssh session (F6-i7). The
    /// App uses this both as the image-paste trigger gate and as the host label
    /// in the confirm prompt.
    pub(super) fn active_remote_upload_target(&self) -> Option<String> {
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
    pub(super) fn remote_upload_job(&self, token: SessionToken) -> Option<RemoteUploadJob> {
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
    pub(super) fn set_active_upload_for_test(&mut self, destination: &str) {
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
    pub(super) fn reconnect(&mut self, token: SessionToken) -> bool {
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
        let Ok(boxed_writer) = super::pty_writer::writer_shim(raw_writer, token) else {
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

    /// Close the **entire active tab** — reap every leaf session in its layout
    /// tree and remove the tab from the strip — regardless of how many panes it
    /// holds. This is the deliberate "Close Tab" semantics: closing a
    /// tab closes the tab you are in even when it holds multiple panes, and it
    /// must not behave like "Close Pane".
    ///
    /// Distinct from [`Self::close`] / `close_focused_pane`, which collapse a
    /// single leaf into its sibling and keep a multi-pane tab alive. For a
    /// single-pane tab this reaps the one session and removes the tab —
    /// byte-identical to the old `close(active_id())` path (the `None` branch of
    /// [`Self::close_with`]).
    ///
    /// Returns `true` iff no workspaces remain afterward, i.e. the last tab of
    /// the last workspace was closed and the caller should signal app exit. A
    /// workspace's last tab closing closes that workspace (ODP-3); only the last
    /// workspace closing exits. Exit keys on the last tab of the last
    /// **workspace**, never on the last pane.
    pub(super) fn close_active_tab(&mut self) -> bool {
        self.close_tab_at(self.active_workspace().active_tab)
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
    /// one leaves the active index unchanged. When the closed tab was the
    /// workspace's last, the workspace is removed too (ODP-3); returns `true`
    /// iff no workspaces remain (signal app exit).
    ///
    /// For a single-pane tab this reaps the one session and removes the tab —
    /// byte-identical to the old `close(token)` path the `×` button used.
    pub(super) fn close_tab_at(&mut self, tab_idx: usize) -> bool {
        // The tab strip shows the active workspace, so a strip index resolves
        // there. Collect every owned leaf token first (owned `Vec`, so the
        // immutable borrow of the workspace's tabs ends before the reap loop
        // mutates `self`).
        let ws_idx = if self.active_ws < self.workspaces.len() {
            self.active_ws
        } else {
            0
        };
        let tokens = match self
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        {
            Some(tab) => tab.layout.leaves(),
            None => return self.workspaces.is_empty(),
        };
        for token in tokens {
            if let Some(session) = self.sessions.remove(&token) {
                let _ = session.close();
            }
        }
        let ws = &mut self.workspaces[ws_idx];
        let was_active = ws.active_tab == tab_idx;
        ws.tabs.remove(tab_idx);
        if ws.tabs.is_empty() {
            // Last tab of the workspace closed -> close the workspace; the last
            // workspace closing signals app exit (ODP-3).
            return self.remove_workspace(ws_idx);
        }
        // Mirror `close_with`'s `None` branch: clamp the active tab index when
        // the active (or an earlier) tab was removed, leave it untouched when a
        // later tab was closed.
        if was_active {
            ws.active_tab = tab_idx.min(ws.tabs.len() - 1);
        } else if ws.active_tab > tab_idx {
            ws.active_tab -= 1;
        }
        false
    }

    fn close_with(
        &mut self,
        token: SessionToken,
        close_session: impl FnOnce(Session) -> bool,
    ) -> bool {
        // A closing pane's session may live in a background workspace (a
        // background shell exiting), so locate it across ALL workspaces, not the
        // active one alone.
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return self.sessions.is_empty();
        };
        // Reap the session itself.
        if let Some(session) = self.sessions.remove(&token) {
            let _ = close_session(session);
        }
        // Remove the pane leaf, collapsing its split parent into the sibling.
        // For a single-pane tab this yields `None`, i.e. the tab closes — the
        // byte-identical analogue of removing a session from the old Vec.
        let ws = &mut self.workspaces[ws_idx];
        match ws.tabs[tab_idx].layout.clone().close_leaf(token) {
            None => {
                let was_active = ws.active_tab == tab_idx;
                ws.tabs.remove(tab_idx);
                if ws.tabs.is_empty() {
                    // Last tab of the workspace closed -> close the workspace;
                    // the last workspace closing signals app exit (ODP-3).
                    return self.remove_workspace(ws_idx);
                }
                if was_active {
                    ws.active_tab = tab_idx.min(ws.tabs.len() - 1);
                } else if ws.active_tab > tab_idx {
                    ws.active_tab -= 1;
                }
                false
            }
            Some(layout) => {
                // The tab survives (a multi-pane tab lost one pane). Refocus a
                // surviving leaf if the closed pane held focus.
                if ws.tabs[tab_idx].focused == token
                    && let Some(first) = layout.leaves().first().copied()
                {
                    ws.tabs[tab_idx].focused = first;
                }
                ws.tabs[tab_idx].layout = layout;
                // Closing a pane changes the tree; un-zoom so the survivor(s)
                // render at their layout geometry. Closing the zoomed pane must
                // un-zoom, and closing a background pane while zoomed also
                // re-tiles, so clear unconditionally.
                ws.tabs[tab_idx].zoomed = false;
                false
            }
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn push(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.insert(id, session);
        self.active_workspace_mut().tabs.push(Tab::single(id));
        id
    }

    /// Insert `session` into the arena and append it as a brand-new
    /// single-pane workspace (test-only), WITHOUT switching to it — the
    /// workspace-level analogue of [`Self::push`], so headless tests can build
    /// multi-workspace sets without an event-loop proxy for `new_workspace`'s
    /// PTY spawn.
    #[cfg(test)]
    pub(in crate::native) fn push_workspace(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.insert(id, session);
        let name = default_workspace_name(self.workspaces.len());
        self.workspaces.push(Workspace::single(name, id));
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
/// They are driven by the keybinding layer (later work) and, for
/// now, by `#[cfg(test)]` seams + the multi-pane render dispatch (1c). The
/// `allow(dead_code)` is scaffolding parity with `layout.rs`: it comes off as
/// the render path (`active_layout`/`active_pane_count`/`active_is_single_pane`)
/// and the keybinding ops wire these in. Single-pane tabs never reach the
/// mutating ops, so the byte-identical path is untouched.
#[allow(dead_code)]
impl WorkspaceSet {
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
        if self.active_tab_ref().is_none() {
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
        let Some(tab) = self.active_tab_mut() else {
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
        if let Some(tab) = self.active_tab_mut() {
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
        let Some(tab) = self.active_tab_mut() else {
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
        self.active_tab_ref()
            .map(Tab::is_effectively_zoomed)
            .unwrap_or(false)
    }

    /// Cycle focus to the next pane of the active tab in tree order (tmux
    /// `Ctrl-b o`). No geometry needed. Returns true if focus moved.
    pub(super) fn focus_next_pane(&mut self) -> bool {
        let Some(tab) = self.active_tab_mut() else {
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
        let Some(tab) = self.active_tab_mut() else {
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
        self.active_tab_ref().map(|tab| &tab.layout)
    }

    /// Number of panes in the active tab (1 ⇒ the byte-identical single path).
    pub(super) fn active_pane_count(&self) -> usize {
        self.active_tab_ref()
            .map(|tab| tab.layout.pane_count())
            .unwrap_or(1)
    }

    /// True when the active tab holds exactly one pane — the byte-identical
    /// render/resize fast path (design doc §2.3).
    pub(super) fn active_is_single_pane(&self) -> bool {
        self.active_tab_ref()
            .map(|tab| tab.layout.is_single_pane())
            .unwrap_or(true)
    }

    /// True when there is exactly one tab and it carries a custom
    /// `title_override` (F4 ODP-7 / F4-NF1). The tab bar's show rule uses this
    /// so a single renamed "workflow" tab is visible even below the usual
    /// two-tab threshold.
    pub(super) fn lone_tab_has_title_override(&self) -> bool {
        let ws = self.active_workspace();
        ws.tabs.len() == 1
            && ws
                .tabs
                .first()
                .is_some_and(|tab| tab.title_override.is_some())
    }
}

/// Workspace-level operations (design doc §3.1, ODP-3/-10). These are the model
/// half of the workspace layer: create / switch / rename / close a workspace and
/// query the workspace list. The keyboard/palette layer (W3) wires these in; the
/// rail chrome (W2) reuses them. None of them run until a second workspace
/// exists, so single-workspace behavior is untouched.
impl WorkspaceSet {
    /// The active workspace index (rail highlight / palette current-marker).
    pub(super) fn active_workspace_index(&self) -> usize {
        self.active_ws
    }

    /// The display name of the workspace at rail index `idx`, or `None` when out
    /// of range.
    pub(super) fn workspace_name(&self, idx: usize) -> Option<&str> {
        self.workspaces.get(idx).map(|ws| ws.name.as_str())
    }

    /// The display names of every workspace, in rail order. Feeds the command
    /// palette's per-workspace "switch to …" rows (W3); the index into this list
    /// is the [`Self::switch_workspace`] target.
    pub(super) fn workspace_names(&self) -> Vec<String> {
        self.workspaces.iter().map(|ws| ws.name.clone()).collect()
    }

    /// The host alias the active workspace is bound to (F6-W5 / ODP-9), or
    /// `None` when it is a plain local workspace. `handle_new_tab` routes New Tab
    /// through the remote connect path when this is `Some`.
    pub(super) fn active_workspace_default_profile(&self) -> Option<&str> {
        self.active_workspace().default_profile.as_deref()
    }

    /// Bind (or, with `None`, unbind) the active workspace to a host alias
    /// (F6-W5). Idempotent; the binding is captured in the shape snapshot so it
    /// survives restore. Returns the previous binding.
    pub(super) fn set_active_workspace_default_profile(
        &mut self,
        profile: Option<String>,
    ) -> Option<String> {
        std::mem::replace(&mut self.active_workspace_mut().default_profile, profile)
    }

    /// The host alias the workspace at rail index `idx` is bound to (RAIL-BIND),
    /// or `None` when out of range or unbound. Read for the rail context menu's
    /// Bind/Unbind conditional, which targets the CLICKED slot rather than the
    /// active workspace.
    pub(super) fn workspace_default_profile_at(&self, idx: usize) -> Option<&str> {
        self.workspaces.get(idx)?.default_profile.as_deref()
    }

    /// Bind (or, with `None`, unbind) the workspace at rail index `idx`
    /// (RAIL-BIND). Same semantics as the active-workspace form, but targets a
    /// specific slot so the rail menu can bind a workspace without first
    /// switching to it. Returns the previous binding; an out-of-range index is a
    /// no-op returning `None`.
    pub(super) fn set_workspace_default_profile_at(
        &mut self,
        idx: usize,
        profile: Option<String>,
    ) -> Option<String> {
        let ws = self.workspaces.get_mut(idx)?;
        std::mem::replace(&mut ws.default_profile, profile)
    }

    /// The workspace list as a [`TabBarSource`] for the rail widget (§7.1): the
    /// same geometry / hit-test / panel code the tab strip uses, now listing
    /// workspaces. Borrows `self`, so it is built per render/hit-test frame.
    pub(super) fn rail_source(&self) -> WorkspaceRailSource<'_> {
        WorkspaceRailSource { set: self }
    }

    /// Spawn a fresh shell in a brand-new workspace appended after the current
    /// list and switch focus to it. The new workspace owns exactly one
    /// single-pane tab (no empty workspaces, ODP-3). Mirrors [`Self::spawn`] one
    /// level up. Returns the new session's token.
    pub(super) fn new_workspace(
        &mut self,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        self.new_workspace_in(grid, None)
    }

    /// Create a fresh workspace whose first shell spawns in `cwd` (the cwd-aware
    /// variant of [`Self::new_workspace`]). Threads the working directory into
    /// the new workspace's single-pane shell exactly as [`Self::spawn`] does for
    /// a new tab, so Duplicate Workspace opens where the active pane already is.
    /// A `None` cwd spawns in the default directory, byte-identical to
    /// `new_workspace`. Cross-platform: the cwd flows through the same
    /// `insert_spawned_session_in` path the cwd-aware tab spawn uses, so ConPTY
    /// honors it on Windows (drive-letter OSC 7 cwds are already normalized).
    pub(super) fn new_workspace_in(
        &mut self,
        grid: crate::core::Dimensions,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<SessionToken, std::io::Error> {
        let token = self.insert_spawned_session_in(grid, cwd)?;
        let name = default_workspace_name(self.workspaces.len());
        self.workspaces.push(Workspace::single(name, token));
        self.active_ws = self.workspaces.len() - 1;
        Ok(token)
    }

    /// Switch the active workspace to rail index `idx` (its active tab's focused
    /// pane becomes the `Deref` target). Returns true when the active workspace
    /// actually changed; out-of-range or same-index requests are no-ops.
    pub(super) fn switch_workspace(&mut self, idx: usize) -> bool {
        if idx == self.active_ws || idx >= self.workspaces.len() {
            return false;
        }
        self.active_ws = idx;
        true
    }

    /// Cycle the active workspace forward (rail order, wrapping). No-op with a
    /// single workspace. Returns true when the active workspace changed.
    pub(super) fn next_workspace(&mut self) -> bool {
        if self.workspaces.len() <= 1 {
            return false;
        }
        self.active_ws = (self.active_ws + 1) % self.workspaces.len();
        true
    }

    /// Cycle the active workspace backward (rail order, wrapping). No-op with a
    /// single workspace. Returns true when the active workspace changed.
    pub(super) fn prev_workspace(&mut self) -> bool {
        if self.workspaces.len() <= 1 {
            return false;
        }
        self.active_ws = if self.active_ws == 0 {
            self.workspaces.len() - 1
        } else {
            self.active_ws - 1
        };
        true
    }

    /// Move the workspace at rail index `idx` one slot toward the front (`up`)
    /// or the back of the rail (RAIL-REORDER). An adjacent swap: the ACTIVE
    /// workspace follows by identity, so reordering never changes which
    /// workspace is focused -- if the active slot is one of the two swapped, its
    /// index moves with it. No-op (returns `false`) when the move would run off
    /// either end (idx 0 up, last idx down) or `idx` is out of range; otherwise
    /// returns `true`. The workspace list is a plain `Vec<Workspace>`, so the
    /// swap carries each workspace's whole state (name, tabs, binding) with it,
    /// and the shape snapshot captures the new order for restore.
    pub(super) fn move_workspace(&mut self, idx: usize, up: bool) -> bool {
        if idx >= self.workspaces.len() {
            return false;
        }
        let target = if up {
            if idx == 0 {
                return false;
            }
            idx - 1
        } else {
            if idx + 1 >= self.workspaces.len() {
                return false;
            }
            idx + 1
        };
        self.workspaces.swap(idx, target);
        // Follow the active workspace by identity across the swap: only the two
        // swapped slots change index, so at most one of them is the active one.
        if self.active_ws == idx {
            self.active_ws = target;
        } else if self.active_ws == target {
            self.active_ws = idx;
        }
        true
    }

    /// Move a tab in the active workspace from strip index `from` to insertion
    /// index `to` (`0..=tab_count`). The active tab follows its `Tab` identity,
    /// so reordering never changes the focused session or pane. Returns `false`
    /// for invalid indices and no-op drops.
    pub(super) fn reorder_tab(&mut self, from: usize, to: usize) -> bool {
        let ws = self.active_workspace_mut();
        if from >= ws.tabs.len() || to > ws.tabs.len() {
            return false;
        }
        let dest = if to > from { to - 1 } else { to };
        if dest == from {
            return false;
        }
        let active_token = ws.tabs.get(ws.active_tab).map(|tab| tab.focused);
        let tab = ws.tabs.remove(from);
        ws.tabs.insert(dest, tab);
        if let Some(token) = active_token
            && let Some(active) = ws.tabs.iter().position(|tab| tab.focused == token)
        {
            ws.active_tab = active;
        }
        true
    }

    /// Rename the workspace at rail index `idx`. Out-of-range requests are
    /// no-ops. Used by the "Rename Workspace" action / palette entry (targeting
    /// the active index) and, later, the rail's in-place rename.
    pub(super) fn rename_workspace(&mut self, idx: usize, name: String) {
        if let Some(ws) = self.workspaces.get_mut(idx) {
            ws.name = name;
        }
    }

    /// Close the ENTIRE active workspace — reap every session of every tab and
    /// remove the workspace from the rail — regardless of tab/pane count. The
    /// "Close Workspace" action (ODP-3). Returns `true` iff no workspaces remain,
    /// i.e. the last workspace was closed and the caller should signal app exit
    /// (the App-level guard that avoids emptying the arena before teardown, as in
    /// `close_active_tab`, is wired when the keybinding/menu lands).
    pub(super) fn close_active_workspace(&mut self) -> bool {
        let ws_idx = if self.active_ws < self.workspaces.len() {
            self.active_ws
        } else {
            0
        };
        let tokens: Vec<SessionToken> = self
            .workspaces
            .get(ws_idx)
            .map(|ws| ws.tabs.iter().flat_map(|tab| tab.layout.leaves()).collect())
            .unwrap_or_default();
        for token in tokens {
            if let Some(session) = self.sessions.remove(&token) {
                let _ = session.close();
            }
        }
        self.remove_workspace(ws_idx)
    }

    /// Move the tab that owns `token` out of its workspace and append it to the
    /// workspace at `dest_idx` (ODP-7, "Move to workspace"). This is a `Tab`
    /// VALUE splice between two `Workspace.tabs` vecs — the sessions never leave
    /// the global arena, so pump-thread lookup by token is untouched. The
    /// destination's active tab is left as-is (v1: move without following), and
    /// the active workspace is unchanged unless the SOURCE workspace empties, in
    /// which case it is removed (no empty workspaces, ODP-3) and `active_ws` is
    /// clamped onto a survivor. Returns `true` when a tab actually moved, and
    /// separately whether the source workspace was closed by the move. No-op
    /// (`(false, false)`) when the token is unknown, `dest_idx` is out of range,
    /// or source == destination.
    pub(super) fn move_tab_to_workspace(
        &mut self,
        token: SessionToken,
        dest_idx: usize,
    ) -> (bool, bool) {
        let Some((src_idx, tab_idx)) = self.locate_token(token) else {
            return (false, false);
        };
        if src_idx == dest_idx || dest_idx >= self.workspaces.len() {
            return (false, false);
        }
        // Splice the tab value out of the source and append it to the
        // destination. Both indices are still valid here — no workspace is
        // removed until after the move.
        let tab = self.workspaces[src_idx].tabs.remove(tab_idx);
        self.workspaces[dest_idx].tabs.push(tab);
        // Source bookkeeping: an empty source workspace closes (ODP-3);
        // otherwise clamp its active-tab index exactly like a tab close.
        if self.workspaces[src_idx].tabs.is_empty() {
            let closed = self.remove_workspace(src_idx);
            // `remove_workspace` returns whether NO workspaces remain; here at
            // least the destination survives, so it is always `false`. The
            // source workspace was nonetheless closed.
            debug_assert!(!closed);
            return (true, true);
        }
        let src = &mut self.workspaces[src_idx];
        if src.active_tab == tab_idx {
            src.active_tab = tab_idx.min(src.tabs.len() - 1);
        } else if src.active_tab > tab_idx {
            src.active_tab -= 1;
        }
        (true, false)
    }

    /// The destination candidates for the "Move to Workspace" picker (W4-v2):
    /// every workspace EXCEPT the one that owns `token`, as `(original rail
    /// index, name)` pairs. The index is the [`Self::move_tab_to_workspace`]
    /// target. Empty when the token is unknown or it is the only workspace
    /// (nothing to move to), which suppresses the picker.
    pub(super) fn move_tab_destinations(&self, token: SessionToken) -> Vec<(usize, String)> {
        let Some((src_idx, _)) = self.locate_token(token) else {
            return Vec::new();
        };
        self.workspaces
            .iter()
            .enumerate()
            .filter(|(idx, _)| *idx != src_idx)
            .map(|(idx, ws)| (idx, ws.name.clone()))
            .collect()
    }

    /// Move the just-appended, currently-active tab so it sits immediately after
    /// the tab that owns `anchor` in the active workspace (ODP-5D "Connect to
    /// host ▸": the new remote tab reads as opening from the clicked tab). The
    /// moved tab is assumed to be the last strip entry — the state left by
    /// `connect_ssh_in_new_tab` + `switch`. A no-op when the anchor is missing,
    /// is itself the last tab, or the moved tab is already directly after it. On
    /// success `active_tab` follows the moved tab to its new index so it stays
    /// focused.
    pub(super) fn reposition_active_tab_after(&mut self, anchor: SessionToken) {
        let ws = self.active_workspace_mut();
        let last = ws.tabs.len().saturating_sub(1);
        let Some(anchor_idx) = ws.tabs.iter().position(|t| t.layout.contains(anchor)) else {
            return;
        };
        // Anchor is the last tab (or the moved tab itself), or the moved tab is
        // already the neighbour just after the anchor — nothing to reorder.
        if anchor_idx + 1 >= last {
            return;
        }
        let dest = anchor_idx + 1;
        let tab = ws.tabs.remove(last);
        ws.tabs.insert(dest, tab);
        ws.active_tab = dest;
    }

    /// Whether any pane of the tab that owns `token` has a running foreground
    /// job (ODP-5D replace gating). Scans that tab's leaf sessions across ALL
    /// workspaces so a background tab can be probed; an attached pane always
    /// reports not-running (its job lives on the remote host). `false` when the
    /// token resolves to no tab.
    pub(super) fn tab_foreground_job_running(&self, token: SessionToken) -> bool {
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return false;
        };
        self.workspaces[ws_idx].tabs[tab_idx]
            .layout
            .leaves()
            .into_iter()
            .any(|leaf| {
                self.sessions
                    .get(&leaf)
                    .is_some_and(Session::foreground_job_running)
            })
    }

    /// Whether a shell exit on `token` would close its ENTIRE workspace: the
    /// session is the sole pane of the sole tab of its workspace, so reaping it
    /// empties the workspace (SHELL-EXIT-CLOSES). Drives the App-mode exit
    /// setting, which escalates a workspace-closing shell exit into an app quit.
    /// `false` when the token is unknown, its tab has sibling panes, or its
    /// workspace has sibling tabs -- those exits close only the pane or tab.
    pub(super) fn shell_exit_closes_workspace(&self, token: SessionToken) -> bool {
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return false;
        };
        let ws = &self.workspaces[ws_idx];
        ws.tabs.len() == 1 && ws.tabs[tab_idx].layout.leaves().len() == 1
    }

    /// Whether any session OTHER than `token` has a running foreground job
    /// (SHELL-EXIT-CLOSES). The App-mode exit quit reuses the window-close
    /// confirmation when this is true, so quitting on a shell exit cannot
    /// silently kill a live job in another workspace. The exiting session itself
    /// is excluded because it has already ended. Attached sessions report
    /// not-running (their job lives on the remote host), matching the
    /// confirm-close semantics elsewhere.
    pub(super) fn any_foreground_job_running_except(&self, token: SessionToken) -> bool {
        self.sessions
            .iter()
            .any(|(id, session)| *id != token && session.foreground_job_running())
    }
}

/// The result of one arena-wide bell / prompt-marks drain
/// ([`WorkspaceSet::drain_bells`]). The App turns this into a viewport flash,
/// window urgency, and a prompt-marks epoch bump; the per-tab activity latch is
/// applied inside the drain (it needs the token->tab mapping).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct BellSweep {
    /// The active-visible focused pane rang this pass — drives today's viewport
    /// flash (byte-identical single-pane behavior).
    pub(super) focused_bell: bool,
    /// The active-visible focused pane's prompt marks changed AND the
    /// command-status gutter is on — drives the prompt-marks epoch bump.
    pub(super) focused_prompt_changed: bool,
    /// At least one NON-focused session rang — drives window urgency
    /// (`request_user_attention`); the specific tabs are latched in the drain.
    pub(super) background_bell: bool,
}

impl WorkspaceSet {
    /// Drain the bell and prompt-marks-changed latches of EVERY session over the
    /// flat arena (design §5 rule 1 — never a hierarchy walk), routing each per
    /// NF21-6:
    ///
    /// - The active-visible focused pane keeps today's behavior: its bell drives
    ///   the viewport flash and its prompt-marks change (when the gutter is on)
    ///   bumps the epoch. The single-pane render fast path no longer drains —
    ///   this does — so that path stays byte-identical.
    /// - Any OTHER session that rang pings window urgency and latches its owning
    ///   tab's activity flag, UNLESS that tab is the active-visible one (a bell
    ///   in a background pane of the tab you are already viewing is "seen").
    ///   Background prompt-marks are drained and discarded so a stale change can
    ///   never bump the epoch spuriously on switch-back.
    ///
    /// The active-visible tab's activity flag is also cleared here every pass:
    /// viewing a tab is what clears its rollup signal.
    pub(super) fn drain_bells(&mut self, gutter_on: bool) -> BellSweep {
        let focused = self.active_focused_token();
        let active_ws = self.active_ws;
        let active_tab = self.active_workspace().active_tab;
        let mut sweep = BellSweep::default();
        let mut background_rang: Vec<SessionToken> = Vec::new();
        for session in self.sessions.values() {
            let Ok(mut terminal) = session.terminal.lock() else {
                continue;
            };
            let bell = terminal.take_bell();
            let prompt_changed = terminal.take_prompt_marks_changed();
            drop(terminal);
            if session.id == focused {
                sweep.focused_bell = bell;
                sweep.focused_prompt_changed = gutter_on && prompt_changed;
            } else if bell {
                background_rang.push(session.id);
            }
        }
        for token in background_rang {
            sweep.background_bell = true;
            if let Some((ws_idx, tab_idx)) = self.locate_token(token)
                && (ws_idx, tab_idx) != (active_ws, active_tab)
                && let Some(tab) = self
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|workspace| workspace.tabs.get_mut(tab_idx))
            {
                tab.activity = true;
            }
        }
        // Viewing the active-visible tab clears its rollup signal.
        if let Some(tab) = self
            .workspaces
            .get_mut(active_ws)
            .and_then(|workspace| workspace.tabs.get_mut(active_tab))
        {
            tab.activity = false;
        }
        sweep
    }

    /// Whether any tab of the workspace at `ws_idx` carries an unseen-activity
    /// latch (the DERIVED workspace-level rollup signal; the rail rollup UI will
    /// read this). No reader outside tests yet — the rollup UI is deferred.
    #[allow(dead_code)]
    pub(super) fn workspace_has_activity(&self, ws_idx: usize) -> bool {
        self.workspaces
            .get(ws_idx)
            .is_some_and(|workspace| workspace.tabs.iter().any(|tab| tab.activity))
    }

    /// The unseen-activity latch of the tab at `(ws_idx, tab_idx)` (test seam).
    #[cfg(test)]
    pub(super) fn tab_activity(&self, ws_idx: usize, tab_idx: usize) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.get(tab_idx))
            .is_some_and(|tab| tab.activity)
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
pub(super) enum RestoreReport {
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
    pub(super) fn capture_shape(&self) -> crate::native::persistence::ShapeSnapshot {
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
    pub(super) fn restore_from_snapshot_remote(
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
    pub(super) fn restore_from_snapshot_with(
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
    pub(super) fn append_from_snapshot_remote(
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
    pub(super) fn append_from_snapshot_with(
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
                let pty = crate::native::test_support::spawn_test_pause_shell(dims).ok()?;
                let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().ok()?));
                let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
                let pty = Arc::new(Mutex::new(pty));
                let token = SessionToken(set.next_token);
                set.next_token = set.next_token.saturating_add(1);
                set.sessions
                    .insert(token, Session::new(token, terminal, writer, pty, None));
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
    pub(super) fn structural_fingerprint(&self) -> u64 {
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

impl TabBarSource for WorkspaceSet {
    fn tab_count(&self) -> usize {
        self.active_workspace().tabs.len()
    }

    fn tab_title(&self, idx: usize) -> &str {
        let Some(tab) = self.active_workspace().tabs.get(idx) else {
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
        self.active_workspace().active_tab
    }
}

/// A borrow of the workspace list presented through [`TabBarSource`] so the F4
/// rail widget renders and hit-tests the WORKSPACES (name / active / count)
/// rather than the active workspace's tabs (design doc §7.1). The rail's
/// `TabHit::Switch(idx)` then dispatches to [`WorkspaceSet::switch_workspace`]
/// instead of `switch`; `TabHit::NewTab` (the `+` slot) creates a workspace.
/// Presentation-only: it reads `workspaces` directly, so it carries no per-tab
/// title-override or session lookup — a workspace's label is its `name`.
pub(in crate::native) struct WorkspaceRailSource<'a> {
    set: &'a WorkspaceSet,
}

impl TabBarSource for WorkspaceRailSource<'_> {
    fn tab_count(&self) -> usize {
        self.set.workspaces.len()
    }

    fn tab_bound(&self, idx: usize) -> bool {
        self.set
            .workspaces
            .get(idx)
            .is_some_and(|ws| ws.default_profile.is_some())
    }

    fn tab_title(&self, idx: usize) -> &str {
        self.set
            .workspaces
            .get(idx)
            .map(|ws| ws.name.as_str())
            .unwrap_or("workspace")
    }

    fn active_tab(&self) -> usize {
        self.set.active_ws
    }
}

impl Deref for WorkspaceSet {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.active()
    }
}

impl DerefMut for WorkspaceSet {
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

    #[cfg(unix)]
    #[test]
    fn per_connection_attach_budget_bounds_the_whole_restore_batch() {
        use std::time::Duration;
        let cap = Duration::from_secs(5);
        let now = Instant::now();

        // Budget remaining and under the cap: the pane gets what is left, so a
        // batch of K slow panes shares one 5s budget instead of K * 5s.
        let mid_batch = now + Duration::from_millis(1500);
        let budget =
            per_connection_attach_budget(mid_batch, now, cap).expect("budget available mid-batch");
        assert!(budget <= cap && budget > Duration::from_millis(1400));

        // Plenty of budget: capped at the per-connection maximum.
        let fresh = now + Duration::from_secs(30);
        assert_eq!(per_connection_attach_budget(fresh, now, cap), Some(cap));

        // Batch budget spent: no handshake attempted (fast-fail to a fresh shell).
        let spent = now - Duration::from_millis(1);
        assert_eq!(per_connection_attach_budget(spent, now, cap), None);
        assert_eq!(per_connection_attach_budget(now, now, cap), None);
    }

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

    fn test_selection() -> AbsoluteSelectionRange {
        AbsoluteSelectionRange {
            start: crate::selection::AbsoluteCellPoint { row: 0, column: 0 },
            end: crate::selection::AbsoluteCellPoint { row: 0, column: 1 },
        }
    }

    #[test]
    fn scrollback_front_trim_clears_absolute_coordinate_state() {
        let mut set = WorkspaceSet::new(build_session(), None);
        {
            let mut terminal = set.active().terminal.lock().expect("terminal lock");
            terminal.set_scrollback_limit(2);
            terminal.advance(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\nj\r\n");
        }
        // Reconcile output that arrived before the selection was made.
        set.reconcile_scrollback_trims();
        set.active_mut().selection.set_range(test_selection());
        assert!(set.active().selection.range().is_some());

        set.active()
            .terminal
            .lock()
            .expect("terminal lock")
            .advance(b"k\r\nl\r\nm\r\n");
        set.reconcile_scrollback_trims();

        assert!(
            set.active().selection.range().is_none(),
            "front eviction must clear rather than silently retarget a selection"
        );
    }

    /// Build a session whose pump (reader) thread is PARKED and will not exit on
    /// its own for `park` — the shape a wedged remote leaves behind (the ssh
    /// child never delivers EOF, so the reader never returns). Used to prove the
    /// shutdown teardown does not block the caller on that join.
    fn build_session_with_parked_reader(id: SessionToken, park: std::time::Duration) -> Session {
        let dims = Dimensions::new(20, 8);
        let pty = spawn_test_pause_shell(dims).expect("spawn test shell");
        let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().expect("writer")));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(pty));
        let parked = std::thread::Builder::new()
            .name("test-wedged-reader".to_owned())
            .spawn(move || std::thread::sleep(park))
            .expect("spawn parked reader");
        Session::new(id, terminal, writer, pty, Some(parked))
    }

    /// CLOSE-HANG regression: whole-app shutdown must stay bounded even when a
    /// session's reader thread is wedged (no EOF), the shape that hung Super+Q
    /// with remote workspaces. The old serial path joined the pump thread on the
    /// caller, so a parked reader would block shutdown for the full park time;
    /// `shutdown_all` offloads the reap + join to a detached thread and returns
    /// within the bounded deadline. Fail-before: the caller would take ~the park
    /// duration (5s) and blow the assertion; pass-after: it returns in ~the
    /// deadline.
    #[test]
    fn shutdown_all_is_bounded_when_a_reader_is_wedged() {
        let park = std::time::Duration::from_secs(5);
        let session = build_session_with_parked_reader(SessionToken(1), park);
        let mut set = WorkspaceSet::new(session, None);

        let deadline = std::time::Duration::from_millis(200);
        let start = Instant::now();
        set.shutdown_all(deadline);
        let elapsed = start.elapsed();

        assert!(set.is_empty(), "shutdown drains the session set");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "shutdown must be bounded, not block on the wedged reader join (took {elapsed:?})"
        );
    }

    /// CLOSE-HANG-2 regression: an interactive tab/workspace close must not
    /// block the caller on a wedged reader join. A ControlPersist mux master can
    /// hold the PTY slave open after the client is killed, so the reader never
    /// sees EOF and its join would otherwise block for the full park. The
    /// parked-reader session reproduces that shape; `close` kills synchronously
    /// and defers wait + join to a detached reaper, returning promptly while the
    /// reaper finishes later. Fail-before: the old inline `pty.wait()` + join
    /// blocked the caller ~the park (3s) and blew the assertion; pass-after: it
    /// returns well under a second.
    #[test]
    fn interactive_close_is_bounded_when_a_reader_is_wedged() {
        let park = std::time::Duration::from_secs(3);
        let mut set = WorkspaceSet::new(build_session(), None);
        let wedged = SessionToken(1);
        set.push(build_session_with_parked_reader(wedged, park));

        let start = Instant::now();
        let last = set.close(wedged);
        let elapsed = start.elapsed();

        assert!(
            !last,
            "a live tab remains, so close does not signal app exit"
        );
        assert_eq!(set.len(), 1, "the wedged session leaves the arena");
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "interactive close must not block on the wedged reader join (took {elapsed:?})"
        );
    }

    /// CLOSE-HANG-2 regression: closing several wedged sessions in quick
    /// succession (the "closed several remote workspaces fast" freeze) must not
    /// compound serially. Each close defers its reap, so N closes return in
    /// aggregate well under a second even though every reader is parked.
    /// Fail-before: the closes summed to N x park (~15s) on the caller.
    #[test]
    fn rapid_successive_closes_do_not_compound() {
        let park = std::time::Duration::from_secs(3);
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut wedged = Vec::new();
        for i in 1..=5u64 {
            let token = SessionToken(i);
            set.push(build_session_with_parked_reader(token, park));
            wedged.push(token);
        }

        let start = Instant::now();
        for token in wedged {
            let _ = set.close(token);
        }
        let elapsed = start.elapsed();

        assert_eq!(set.len(), 1, "every wedged session left the arena");
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "rapid successive closes must not compound serially (took {elapsed:?})"
        );
    }

    fn tabset_with_proxy_for_test() -> Option<(WorkspaceSet, EventLoop<UserEvent>)> {
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
        Some((WorkspaceSet::new(build_session(), Some(proxy)), event_loop))
    }

    #[test]
    fn session_title_defaults_to_odytty() {
        let session = build_session();
        assert_eq!(session.tab_title, "odytty");
    }

    #[test]
    fn session_set_switches_wraps_and_closes() {
        let mut sessions = WorkspaceSet::new(build_session(), None);
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
            .spawn(Dimensions::new(20, 8), None)
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
            .spawn(Dimensions::new(20, 8), None)
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

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[test]
    fn spawn_inherits_an_explicit_working_directory() {
        // F1 cwd inheritance / Duplicate Tab: a new tab spawned with an explicit
        // cwd seeds the pane's advisory directory to that path (before any OSC 7),
        // so New Tab / Duplicate Tab opens where the active pane was. Distinct
        // from the None path, which falls back to the process cwd.
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let inherited = std::path::PathBuf::from("/tmp/odytty-inherited-cwd");
        let token = sessions
            .spawn(Dimensions::new(20, 8), Some(inherited.clone()))
            .expect("spawn local session in cwd");
        assert!(sessions.switch(token));

        let session = sessions.active();
        {
            let terminal = session.terminal.lock().expect("terminal lock");
            assert_eq!(
                terminal.current_working_directory(),
                Some("/tmp/odytty-inherited-cwd"),
                "a new tab seeded with an explicit cwd reports it before the first OSC 7"
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

        // Shared setup: build a single-pane WorkspaceSet, force the pane's terminal to
        // the wrapped 4x3 state, and return the incoming (pre-resize) cursor.
        let setup = |shell_owns: bool| -> (WorkspaceSet, Position) {
            let sessions = WorkspaceSet::new(build_session(), None);
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
    fn classify_remote_exit_maps_255_to_reconnect_and_everything_else_to_close() {
        // The transport-drop discriminator: OpenSSH exits 255 on its own
        // connection failures, so 255 (and only 255) offers reconnect.
        assert_eq!(classify_remote_exit(Some(255)), ExitDisposition::Reconnect);
        // Clean exit, ordinary remote-command failures, and a signal/unknown
        // (`None` — Unix signal death or a Windows post-EOF STILL_ACTIVE
        // sentinel) all close normally.
        for code in [Some(0), Some(1), Some(126), Some(127), Some(130), None] {
            assert_eq!(
                classify_remote_exit(code),
                ExitDisposition::Close,
                "code {code:?} must close, not reconnect"
            );
        }
    }

    /// A short-lived local child masquerading as an ssh session, whose exit code
    /// is `code`. Used to drive the reconnect classifier without a live ssh.
    #[cfg(not(windows))]
    fn exit_code_command(code: i32) -> SshCommand {
        SshCommand::new(
            "/bin/sh",
            vec![OsString::from("-c"), OsString::from(format!("exit {code}"))],
        )
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn ssh_session_stores_reconnect_anchor_but_a_local_shell_does_not() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let ssh = sessions
            .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(0))
            .expect("ssh stub session");
        assert!(
            sessions.get(ssh).expect("ssh session").reconnect.is_some(),
            "an ssh-launched session carries a reconnect anchor"
        );
        // A plain local shell (the startup session at token 0) never does, so
        // classification and the reconnect prompt never engage for it.
        assert!(
            sessions
                .get(SessionToken(0))
                .expect("local session")
                .reconnect
                .is_none()
        );
        assert!(!sessions.close(ssh));
        assert!(sessions.close(SessionToken(0)));
    }

    /// RESTORE-UPLOAD: image paste-through (F6-i7) is a remote *integrated*
    /// feature, so the shared upload-descriptor builder engages only when
    /// integration is on and yields the ssh destination; a plain-ssh host leaves
    /// it unset. Pure logic, so it also covers the Windows client (where
    /// `control_dir` is always `None` but the descriptor is otherwise identical).
    #[test]
    fn remote_upload_for_engages_only_on_integrated_hosts() {
        let host = crate::connection_hosts::parse_adhoc_target("root@host.example.invalid")
            .expect("adhoc target parses")
            .to_connection_host();
        let integrated = RemoteSshOptions {
            integration: true,
            ..RemoteSshOptions::default()
        };
        let plain = RemoteSshOptions {
            integration: false,
            ..RemoteSshOptions::default()
        };
        assert_eq!(
            WorkspaceSet::remote_upload_for(&host, &integrated)
                .map(|upload| upload.destination().to_owned()),
            Some("root@host.example.invalid".to_owned()),
            "an integrated host carries the paste-through upload descriptor"
        );
        assert!(
            WorkspaceSet::remote_upload_for(&host, &plain).is_none(),
            "a plain-ssh host leaves paste-through unset"
        );
    }

    /// RESTORE-UPLOAD regression: a restored *integrated* remote pane exposes its
    /// image paste-through target exactly like a freshly-connected one, so
    /// pasting a screenshot into a restored remote tab offers the upload.
    /// Fail-before: `insert_ssh_restored_session` never set `session.upload`, so
    /// `active_remote_upload_target()` was `None` on every restored pane and the
    /// paste bailed silently. Pass-after: the descriptor flows through.
    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn restored_integrated_remote_pane_exposes_its_upload_target() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let host = crate::connection_hosts::parse_adhoc_target("root@host.example.invalid")
            .expect("adhoc target parses")
            .to_connection_host();
        let integrated = RemoteSshOptions {
            integration: true,
            ..RemoteSshOptions::default()
        };
        let upload = WorkspaceSet::remote_upload_for(&host, &integrated);
        let token = sessions
            .insert_ssh_restored_session(
                Dimensions::new(20, 8),
                exit_code_command(0),
                "root@host.example.invalid".to_owned(),
                upload,
            )
            .expect("restored ssh session");
        // Restore inserts into the arena without tab wiring (the rebuild owns the
        // pane tree); graft + focus so the active_* API resolves to it.
        sessions
            .active_workspace_mut()
            .tabs
            .push(Tab::single(token));
        assert!(sessions.switch(token));
        assert_eq!(
            sessions.active_remote_upload_target(),
            Some("root@host.example.invalid".to_owned()),
            "a restored integrated remote pane engages image paste-through"
        );
        assert!(!sessions.close(token));
        assert!(sessions.close(SessionToken(0)));
    }

    /// RESTORE-UPLOAD: a restored plain-ssh (integration-off) remote pane leaves
    /// paste-through unset — byte-identical to before — so a restored pane never
    /// gains a capability its freshly-connected twin lacks.
    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn restored_plain_remote_pane_leaves_paste_through_unset() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let host = crate::connection_hosts::parse_adhoc_target("root@host.example.invalid")
            .expect("adhoc target parses")
            .to_connection_host();
        let plain = RemoteSshOptions {
            integration: false,
            ..RemoteSshOptions::default()
        };
        let upload = WorkspaceSet::remote_upload_for(&host, &plain);
        let token = sessions
            .insert_ssh_restored_session(
                Dimensions::new(20, 8),
                exit_code_command(0),
                "root@host.example.invalid".to_owned(),
                upload,
            )
            .expect("restored ssh session");
        sessions
            .active_workspace_mut()
            .tabs
            .push(Tab::single(token));
        assert!(sessions.switch(token));
        assert_eq!(
            sessions.active_remote_upload_target(),
            None,
            "a plain-ssh restored pane stays byte-identical: no paste-through"
        );
        assert!(!sessions.close(token));
        assert!(sessions.close(SessionToken(0)));
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn arm_reconnect_holds_the_tab_open_on_a_255_drop_and_paints_the_banner() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let ssh = sessions
            .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(40, 8), exit_code_command(255))
            .expect("ssh stub session");
        // Poll until the child has exited (255): while it is still running,
        // `try_wait` returns `Ok(None)` non-destructively, so a `false` here just
        // means "not dead yet" — retry. Once dead, the code is captured once and
        // the tab is held open.
        let armed = (0..200).any(|_| {
            if sessions.try_arm_reconnect(ssh) {
                true
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            }
        });
        assert!(armed, "a 255 drop must arm reconnect within the timeout");
        assert!(sessions.get(ssh).expect("ssh session").awaiting_reconnect);
        assert!(sessions.switch(ssh));
        assert!(sessions.active_awaiting_reconnect());
        // The in-pane banner was painted into the terminal model.
        let text: String = sessions
            .get(ssh)
            .expect("ssh session")
            .terminal
            .lock()
            .expect("terminal lock")
            .snapshot()
            .cells
            .iter()
            .map(|cell| cell.ch)
            .collect();
        assert!(
            text.contains("connection dropped"),
            "the dropped banner must be visible, got: {text:?}"
        );
        assert!(!sessions.close(ssh));
        assert!(sessions.close(SessionToken(0)));
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn arm_reconnect_declines_a_clean_exit() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let ssh = sessions
            .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(0))
            .expect("ssh stub session");
        // Reap the child up-front so its status is consumed; the subsequent
        // `try_arm_reconnect` sees an unknown (`None`) code — which, like the
        // clean 0 it exited with, must NOT arm reconnect.
        let _ = sessions
            .get(ssh)
            .expect("ssh session")
            .local_pty()
            .expect("local ssh pty")
            .lock()
            .expect("pty lock")
            .wait();
        assert!(
            !sessions.try_arm_reconnect(ssh),
            "a clean exit must not hold the tab open"
        );
        assert!(!sessions.get(ssh).expect("ssh session").awaiting_reconnect);
        assert!(!sessions.close(ssh));
        assert!(sessions.close(SessionToken(0)));
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn a_local_shell_never_arms_reconnect() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        // The startup session at token 0 is a plain local shell with no
        // reconnect anchor: even a 255-shaped exit can never arm reconnect.
        assert!(!sessions.try_arm_reconnect(SessionToken(0)));
        assert!(sessions.close(SessionToken(0)));
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn reconnect_respawns_into_the_same_token_and_clears_the_prompt() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        // A slightly longer-lived stub so the first spawn is comfortably alive,
        // then drops 255 to arm the prompt.
        let ssh = sessions
            .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
            .expect("ssh stub session");
        let tabs_before = sessions.tab_count();
        let sessions_before = sessions.len();
        let armed = (0..200).any(|_| {
            if sessions.try_arm_reconnect(ssh) {
                true
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            }
        });
        assert!(armed, "drop must arm reconnect");
        // Reconnect re-runs the stored argv into the SAME token/tab.
        assert!(sessions.reconnect(ssh), "reconnect respawns the session");
        assert_eq!(sessions.tab_count(), tabs_before, "no new tab is created");
        assert_eq!(sessions.len(), sessions_before, "same session count");
        assert!(
            !sessions.get(ssh).expect("ssh session").awaiting_reconnect,
            "the prompt is cleared after a successful reconnect"
        );
        // The reconnect anchor is retained so a second drop can reconnect again.
        assert!(sessions.get(ssh).expect("ssh session").reconnect.is_some());
        assert!(!sessions.close(ssh));
        assert!(sessions.close(SessionToken(0)));
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[cfg(not(windows))]
    #[test]
    fn reconnect_resets_stale_input_reporting_modes() {
        let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        let ssh = sessions
            .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
            .expect("ssh stub session");
        // A pre-drop remote shell (or a TUI) latches bracketed paste on the
        // reused model. Reconnect must clear it so a paste into the FRESH shell
        // is not wrapped in \e[200~/\e[201~ markers the new readline never
        // enabled — otherwise they echo literally into the command line.
        crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
            .advance(b"\x1b[?2004h");
        assert!(
            crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
                .bracketed_paste_enabled(),
            "the pre-drop shell latched bracketed paste on the reused model"
        );

        let armed = (0..200).any(|_| {
            if sessions.try_arm_reconnect(ssh) {
                true
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            }
        });
        assert!(armed, "drop must arm reconnect");
        assert!(sessions.reconnect(ssh), "reconnect respawns the session");
        assert!(
            !crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
                .bracketed_paste_enabled(),
            "reconnect must clear the pre-drop bracketed-paste latch so a paste \
             into the fresh shell is not wrapped in markers it never enabled"
        );
        assert!(!sessions.close(ssh));
        assert!(sessions.close(SessionToken(0)));
    }

    #[test]
    fn split_active_grows_a_pane_within_the_same_tab() {
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut single = WorkspaceSet::new(build_session(), None);
        assert!(!single.focus_next_pane());
    }

    #[test]
    fn set_active_focus_accepts_panes_and_rejects_strangers() {
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
            let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut via_close_tab = WorkspaceSet::new(build_session(), None);
        via_close_tab.push(build_session_with_id(SessionToken(1)));
        let mut via_close_id = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
        set.equalize_active();
        assert!(set.active_is_single_pane());
        // With a split present, layout tree stays valid (ratios reset).
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        set.equalize_active();
        assert_eq!(set.active_pane_count(), 2);
    }

    #[test]
    fn toggle_zoom_flips_and_is_a_noop_on_single_pane() {
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert!(set.toggle_active_zoom());
        assert!(set.active_is_zoomed());
        set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(2)));
        assert!(!set.active_is_zoomed(), "split clears zoom");
    }

    #[test]
    fn resize_sizes_the_zoomed_focused_pane_to_full_content() {
        let mut set = WorkspaceSet::new(build_session(), None);
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
    fn zoom_resize_invalidates_only_the_pane_that_reflows() {
        let mut set = WorkspaceSet::new(build_session(), None);
        let focused =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
        set.resize_all_panes(content, 10, 20, 1.0);
        set.get_mut(SessionToken(0))
            .expect("background pane")
            .selection
            .set_range(test_selection());
        set.get_mut(focused)
            .expect("focused pane")
            .selection
            .set_range(test_selection());

        assert!(set.toggle_active_zoom());
        set.resize_all_panes(content, 10, 20, 1.0);

        assert!(
            set.get(focused)
                .expect("focused pane")
                .selection
                .range()
                .is_none(),
            "zoom reflow clears the focused pane's stale selection"
        );
        assert!(
            set.get(SessionToken(0))
                .expect("unchanged background pane")
                .selection
                .range()
                .is_some(),
            "a pane whose grid dimensions did not change keeps its selection"
        );
    }

    #[test]
    fn zoom_hides_background_panes_from_visibility() {
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
        assert!(set.active_layout().is_some_and(PaneNode::is_single_pane));
        set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(1)));
        assert_eq!(set.active_layout().map(PaneNode::pane_count), Some(2));
    }

    fn pane_dims(set: &WorkspaceSet, token: SessionToken) -> (usize, usize) {
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
        let mut set = WorkspaceSet::new(build_session(), None);
        // 800x400 content, 10x20 cell → 80 cols, 20 rows; one pane fills it.
        let content = PaneRect::new(0.0, 0.0, 800.0, 400.0);
        set.resize_all_panes(content, 10, 20, 1.0);
        assert_eq!(pane_dims(&set, SessionToken(0)), (80, 20));
    }

    #[test]
    fn default_session_source_is_local() {
        // BYTE-IDENTITY GUARD: a normally-spawned session is `Local`, so the
        // source generalization is a no-op for the default path.
        let set = WorkspaceSet::new(build_session(), None);
        assert!(matches!(set.active().source, SessionSource::Local { .. }));
    }

    #[test]
    #[cfg(unix)]
    fn local_session_resize_routes_to_pty_unchanged() {
        // BYTE-IDENTITY GUARD: resizing a local session must push the exact same
        // TIOCSWINSZ to the concrete PTY as before Phase 2 — the `Local` match
        // arm is the identical `pty.lock().resize(...)` call.
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
    fn visible_pane_rebuild_flag_helpers_span_the_whole_active_tab() {
        // NF21-7: the render gate ORs `needs_rebuild` across every visible pane
        // of the active tab (not just the focused one), and the multi-pane
        // rebuild clears every visible pane's flag. A dirtied NON-focused split
        // pane must therefore be both SEEN by the OR and CLEARED by the sweep —
        // otherwise its output freezes (gate never opens) or storms (flag never
        // clears).
        let mut set = WorkspaceSet::new(build_session(), None);
        let sibling =
            set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        // A second, non-visible tab whose flag must be ignored by both helpers.
        let other_tab = SessionToken(2);
        set.push(build_session_with_id(other_tab));

        set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = false;
        set.get_mut(sibling).expect("sibling").needs_rebuild = false;
        set.get_mut(other_tab).expect("other tab").needs_rebuild = false;
        assert!(
            !set.any_visible_pane_needs_rebuild(),
            "no visible pane dirty → gate stays closed"
        );

        // Output into the NON-focused visible pane (focus is on `sibling`).
        set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = true;
        assert!(
            set.any_visible_pane_needs_rebuild(),
            "a dirtied non-focused visible pane opens the tab-wide gate"
        );

        // A dirty pane on an INACTIVE tab must not open the active tab's gate.
        set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = false;
        set.get_mut(other_tab).expect("other tab").needs_rebuild = true;
        assert!(
            !set.any_visible_pane_needs_rebuild(),
            "an off-tab pane's flag is not a visible-pane rebuild"
        );

        // The sweep clears every visible pane, leaving the off-tab flag alone.
        set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = true;
        set.get_mut(sibling).expect("sibling").needs_rebuild = true;
        set.clear_visible_pane_rebuild_flags();
        assert!(!set.get(SessionToken(0)).expect("pane 0").needs_rebuild);
        assert!(!set.get(sibling).expect("sibling").needs_rebuild);
        assert!(
            set.get(other_tab).expect("other tab").needs_rebuild,
            "the sweep clears only the active tab's visible panes"
        );
    }

    #[test]
    fn active_pane_rects_tiles_the_content_without_overlap() {
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let single = WorkspaceSet::new(build_session(), None);
        assert_eq!(
            single.active_divider_at_point(content, 1.0, 402.0, 50.0, 6.0),
            None
        );
    }

    #[test]
    fn drag_active_divider_reflows_the_active_split_ratio() {
        let mut set = WorkspaceSet::new(build_session(), None);
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
        let mut set = WorkspaceSet::new(build_session(), None);
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

    // --- Workspace hierarchy (W1; design doc §3.1, ODP-3/-10) ---

    #[test]
    fn a_fresh_set_holds_one_named_workspace() {
        let set = WorkspaceSet::new(build_session(), None);
        assert_eq!(set.workspace_count(), 1);
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.workspace_name(0), Some("Workspace 1"));
        assert_eq!(set.workspace_name(1), None);
    }

    #[test]
    fn switching_workspaces_isolates_each_workspaces_tab_list() {
        // ws0: two tabs (sessions 0, 1). ws1: one tab (session 2).
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        assert_eq!(set.tab_count(), 2, "ws0 has two tabs");
        set.push_workspace(build_session_with_id(SessionToken(2)));
        assert_eq!(set.workspace_count(), 2);
        // push_workspace never switches: ws0 stays active with its two tabs.
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.tab_count(), 2);
        assert_eq!(set.active_id(), SessionToken(0));

        // Switch to ws1: its own single-tab list, its own active session.
        assert!(set.switch_workspace(1));
        assert_eq!(set.active_workspace_index(), 1);
        assert_eq!(set.tab_count(), 1);
        assert_eq!(set.active_id(), SessionToken(2));

        // A same-index / out-of-range switch is a no-op.
        assert!(!set.switch_workspace(1));
        assert!(!set.switch_workspace(9));

        // Switch back: ws0's two-tab list and prior active session are intact.
        assert!(set.switch_workspace(0));
        assert_eq!(set.tab_count(), 2);
        assert_eq!(set.active_id(), SessionToken(0));
    }

    #[test]
    fn move_workspace_reorders_and_follows_the_active_by_identity() {
        // Three workspaces (tokens 0/1/2), ws1 active.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        assert!(set.switch_workspace(1));
        assert_eq!(
            set.workspace_names(),
            vec!["Workspace 1", "Workspace 2", "Workspace 3"]
        );
        assert_eq!(set.active_workspace_index(), 1);
        assert_eq!(set.active_id(), SessionToken(1));

        // Move the active workspace (idx 1) up: it swaps with idx 0, and the
        // active index follows it to 0 -- same workspace stays focused.
        assert!(set.move_workspace(1, true));
        assert_eq!(
            set.workspace_names(),
            vec!["Workspace 2", "Workspace 1", "Workspace 3"]
        );
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(
            set.active_id(),
            SessionToken(1),
            "active workspace unchanged by the move"
        );

        // Move a NON-active workspace (idx 2 = "Workspace 3") up: it swaps into
        // idx 1, which does NOT touch the active slot (idx 0), so the active
        // index is unchanged and still points at the same workspace.
        assert!(set.move_workspace(2, true));
        assert_eq!(
            set.workspace_names(),
            vec!["Workspace 2", "Workspace 3", "Workspace 1"]
        );
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.active_id(), SessionToken(1));

        // The last slot (idx 2) cannot move down: a no-op past the end.
        assert!(
            !set.move_workspace(2, false),
            "cannot move the last slot down"
        );
        assert_eq!(set.active_workspace_index(), 0);

        // Reorder back to the original rail order via the down direction: move
        // "Workspace 2" (the active, at idx 0) down twice.
        assert!(set.move_workspace(0, false));
        assert_eq!(
            set.active_workspace_index(),
            1,
            "active follows its slot down"
        );
        assert!(set.move_workspace(1, false));
        assert_eq!(set.active_workspace_index(), 2);
        assert_eq!(
            set.workspace_names(),
            vec!["Workspace 3", "Workspace 1", "Workspace 2"]
        );
        assert_eq!(
            set.active_id(),
            SessionToken(1),
            "same workspace focused throughout"
        );
    }

    #[test]
    fn move_workspace_guards_the_ends_and_bad_indices() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        // Top guard: idx 0 cannot move up. Bottom guard: last idx cannot move down.
        assert!(!set.move_workspace(0, true));
        assert!(!set.move_workspace(1, false));
        // Out-of-range index is a no-op.
        assert!(!set.move_workspace(9, true));
        assert!(!set.move_workspace(9, false));
        // Order untouched by every rejected move.
        assert_eq!(set.workspace_names(), vec!["Workspace 1", "Workspace 2"]);
    }

    #[test]
    fn move_workspace_order_round_trips_through_the_shape_snapshot() {
        // Reorder, then confirm the captured shape preserves the new rail order
        // (the autosave/restore path serializes `workspaces` in this order).
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        set.rename_workspace(0, "alpha".to_owned());
        set.rename_workspace(1, "beta".to_owned());
        set.rename_workspace(2, "gamma".to_owned());
        assert!(set.switch_workspace(2)); // gamma active
        // Move gamma (idx 2) up to the front.
        assert!(set.move_workspace(2, true));
        assert!(set.move_workspace(1, true));
        assert_eq!(set.workspace_names(), vec!["gamma", "alpha", "beta"]);
        assert_eq!(
            set.active_workspace_index(),
            0,
            "gamma still active after the move"
        );

        let shape = set.capture_shape();
        let order: Vec<&str> = shape.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(
            order,
            vec!["gamma", "alpha", "beta"],
            "snapshot preserves the reordered rail"
        );
        assert_eq!(
            shape.active_workspace, 0,
            "active index captured after the reorder"
        );
    }

    #[test]
    fn shell_exit_closes_workspace_only_for_a_sole_pane_sole_tab() {
        // SHELL-EXIT-CLOSES: the predicate is true only when the exiting session
        // is the sole pane of the sole tab of its workspace (reaping it empties
        // the workspace). Sibling panes or tabs make it false -- those exits
        // close only a pane or tab, never the workspace.
        let mut set = WorkspaceSet::new(build_session(), None);
        // ws0: one single-pane tab (token 0). The predicate holds.
        assert!(set.shell_exit_closes_workspace(SessionToken(0)));

        // Give ws0 a second tab (token 1): now token 0 has a sibling tab.
        set.push(build_session_with_id(SessionToken(1)));
        assert!(
            !set.shell_exit_closes_workspace(SessionToken(0)),
            "a sibling tab means the exit closes only the tab"
        );
        assert!(
            !set.shell_exit_closes_workspace(SessionToken(1)),
            "the sibling tab itself closes only the tab"
        );

        // A second workspace with a SPLIT tab (tokens 2 + 3 in one tab): a
        // sibling pane means the exit closes only the pane.
        set.push_workspace(build_session_with_id(SessionToken(2)));
        assert!(set.switch_workspace(1));
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(3)));
        assert!(
            !set.shell_exit_closes_workspace(SessionToken(2)),
            "a sibling pane means the exit closes only the pane"
        );
        assert!(!set.shell_exit_closes_workspace(SessionToken(3)));

        // An unknown token is never a workspace-closing exit.
        assert!(!set.shell_exit_closes_workspace(SessionToken(999)));
    }

    #[test]
    fn any_foreground_job_running_except_excludes_the_named_session() {
        // SHELL-EXIT-CLOSES: the "except" scan skips the exiting session. With a
        // lone workspace, excluding its only session leaves nothing to scan, so
        // the result is false regardless of that session's (already-ended) job.
        let set = WorkspaceSet::new(build_session(), None);
        assert!(!set.any_foreground_job_running_except(SessionToken(0)));
        // An unknown exclusion token scans every real session; the test shells
        // are idle (`ForegroundJob::None`), so still false -- and never panics.
        assert!(!set.any_foreground_job_running_except(SessionToken(999)));
    }

    #[test]
    fn closing_a_workspaces_last_tab_closes_the_workspace_and_switches_out() {
        // ws0 (token 0), ws1 (token 1); ws1 active.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        assert!(set.switch_workspace(1));
        assert_eq!(set.workspace_count(), 2);

        // Closing ws1's only tab removes ws1 entirely — not app exit, because
        // ws0 survives — and clamps the active workspace back onto ws0.
        let exit = set.close_active_tab();
        assert!(!exit, "another workspace survives, so not app exit");
        assert_eq!(set.workspace_count(), 1);
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.active_id(), SessionToken(0));
        assert_eq!(set.len(), 1, "ws1's session was reaped");
    }

    #[test]
    fn closing_the_last_workspaces_last_tab_signals_app_exit() {
        let mut set = WorkspaceSet::new(build_session(), None);
        assert_eq!(set.workspace_count(), 1);
        let exit = set.close_active_tab();
        assert!(exit, "the last tab of the last workspace exits the app");
        assert!(set.is_empty());
        assert_eq!(set.workspace_count(), 0);
    }

    #[test]
    fn a_background_workspaces_shell_exit_reaps_it_without_disturbing_the_active_one() {
        // ws0 active (token 0); ws1 background (token 1, its sole tab).
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.workspace_count(), 2);

        // The background workspace's shell exits: its tab (and thus the now-empty
        // workspace) is reaped without app exit, and the active workspace is
        // untouched. This is the NF21 §5 background-workspace polarity: a
        // producer in a non-active workspace still serviced correctly.
        let exit = set.close_shell_exited(SessionToken(1));
        assert!(!exit);
        assert_eq!(set.workspace_count(), 1);
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.active_id(), SessionToken(0));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn switch_deep_focuses_across_workspaces_for_attach_dedup() {
        // ws0: tabs for tokens 0 and 1. ws1: token 2, active.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        assert!(set.switch_workspace(1));
        assert_eq!(set.active_workspace_index(), 1);

        // Selecting a token that lives in ws0 (the attach-dedup deep-switch,
        // ODP-10) moves the active workspace + tab + focused pane in one step.
        assert!(set.switch(SessionToken(1)));
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.active_id(), SessionToken(1));

        // Re-selecting the already-focused token is a no-op; an unknown token
        // never switches.
        assert!(!set.switch(SessionToken(1)));
        assert!(!set.switch(SessionToken(99)));
    }

    #[test]
    fn close_active_workspace_reaps_every_tab_and_pane() {
        // ws0: two tabs, one multi-pane (tokens 0+3 split, token 1). ws1: token 2.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(3)));
        set.push(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.len(), 4);

        let exit = set.close_active_workspace();
        assert!(!exit, "ws1 survives");
        assert_eq!(set.workspace_count(), 1);
        assert_eq!(set.active_id(), SessionToken(2), "ws1 is now active");
        assert_eq!(set.len(), 1, "all of ws0's sessions were reaped");
    }

    #[test]
    fn close_active_workspace_on_the_last_workspace_signals_exit() {
        let mut set = WorkspaceSet::new(build_session(), None);
        let exit = set.close_active_workspace();
        assert!(exit);
        assert!(set.is_empty());
        assert_eq!(set.workspace_count(), 0);
    }

    #[test]
    fn renaming_a_workspace_updates_only_that_rail_name() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.rename_workspace(0, "infra".to_owned());
        assert_eq!(set.workspace_name(0), Some("infra"));
        set.push_workspace(build_session_with_id(SessionToken(1)));
        assert!(set.switch_workspace(1));
        set.rename_workspace(set.active_workspace_index(), "app".to_owned());
        assert_eq!(set.workspace_name(0), Some("infra"));
        assert_eq!(set.workspace_name(1), Some("app"));
    }

    #[test]
    fn next_and_prev_workspace_wrap_in_rail_order() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        assert_eq!(set.active_workspace_index(), 0);
        assert!(set.next_workspace());
        assert_eq!(set.active_workspace_index(), 1);
        assert!(set.prev_workspace());
        assert_eq!(set.active_workspace_index(), 0);
        // Wrap backward from the first to the last, then forward wraps to the first.
        assert!(set.prev_workspace());
        assert_eq!(set.active_workspace_index(), 2);
        assert!(set.next_workspace());
        assert_eq!(set.active_workspace_index(), 0);
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[test]
    fn a_new_workspace_appends_switches_and_holds_one_tab() {
        // new_workspace needs a real event-loop proxy for the PTY spawn.
        let Some((mut set, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        assert_eq!(set.workspace_count(), 1);
        let grid = Dimensions::new(20, 8);
        let token = set.new_workspace(grid).expect("spawn new workspace");
        assert_eq!(set.workspace_count(), 2);
        // The new workspace is active and holds exactly one single-pane tab.
        assert_eq!(set.active_workspace_index(), 1);
        assert_eq!(set.tab_count(), 1);
        assert!(set.active_is_single_pane());
        assert_eq!(set.active_id(), token);
        assert_eq!(set.workspace_name(1), Some("Workspace 2"));
    }

    #[cfg_attr(
        target_os = "macos",
        ignore = "winit EventLoop cannot be built off the main thread on macOS"
    )]
    #[test]
    fn new_workspace_in_threads_cwd_and_appends_like_new_workspace() {
        // Duplicate Workspace threads the active pane's cwd through the cwd-aware
        // `new_workspace_in`, which spawns via the SAME `insert_spawned_session_in`
        // path New Tab's cwd inheritance uses. A `Some(cwd)` still appends,
        // switches to, and holds exactly one single-pane tab -- identical shape to
        // the cwd-less `new_workspace`. (The spawn honors the directory the same
        // way the tab path does; the pty's cwd is not observable here without
        // shell integration, so this pins the workspace-level behavior.)
        let Some((mut set, _event_loop)) = tabset_with_proxy_for_test() else {
            return;
        };
        assert_eq!(set.workspace_count(), 1);
        let grid = Dimensions::new(20, 8);
        let cwd = Some(std::env::temp_dir());
        let token = set
            .new_workspace_in(grid, cwd)
            .expect("spawn new workspace in cwd");
        assert_eq!(set.workspace_count(), 2);
        assert_eq!(set.active_workspace_index(), 1);
        assert_eq!(set.tab_count(), 1);
        assert!(set.active_is_single_pane());
        assert_eq!(set.active_id(), token);
        assert_eq!(set.workspace_name(1), Some("Workspace 2"));
    }

    #[test]
    fn move_tab_to_workspace_splices_the_tab_without_touching_the_active() {
        // ws0 holds tokens [0, 1]; ws1 holds [2]. Active stays ws0.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.tab_count(), 2);

        // Move the background tab (token 1) into ws1.
        let (moved, source_closed) = set.move_tab_to_workspace(SessionToken(1), 1);
        assert!(moved);
        assert!(!source_closed, "ws0 still has token 0");
        // Active workspace unchanged (v1: move without following) and now holds
        // only token 0.
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.tab_count(), 1);
        assert_eq!(set.active_id(), SessionToken(0));
        // The moved tab landed at the END of ws1.
        assert!(set.switch_workspace(1));
        assert_eq!(set.tab_count(), 2);
        assert_eq!(set.token_at_position(1), Some(SessionToken(1)));
        // No session left the arena.
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn moving_the_last_tab_out_closes_the_source_workspace() {
        // ws0 holds [0]; ws1 holds [1]. Moving token 0 out empties and closes ws0.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        assert_eq!(set.workspace_count(), 2);
        assert_eq!(set.active_workspace_index(), 0);

        let (moved, source_closed) = set.move_tab_to_workspace(SessionToken(0), 1);
        assert!(moved);
        assert!(source_closed, "the emptied source workspace closes (ODP-3)");
        assert_eq!(set.workspace_count(), 1);
        // The surviving workspace (old ws1, now index 0) holds both tabs: its
        // own token 1 then the moved token 0.
        assert_eq!(set.active_workspace_index(), 0);
        assert_eq!(set.tab_count(), 2);
        assert_eq!(set.token_at_position(0), Some(SessionToken(1)));
        assert_eq!(set.token_at_position(1), Some(SessionToken(0)));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn move_tab_destinations_excludes_the_source_and_is_empty_alone() {
        // Single workspace: no destinations, so the picker never opens (W4-v2).
        let mut set = WorkspaceSet::new(build_session(), None);
        assert!(
            set.move_tab_destinations(SessionToken(0)).is_empty(),
            "one workspace = nowhere to move"
        );

        // Three workspaces named in order; from ws2 the destinations are ws0 and
        // ws1 (the source ws2 is excluded), carrying their ORIGINAL indices.
        set.push_workspace(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        set.rename_workspace(0, "alpha".to_owned());
        set.rename_workspace(1, "beta".to_owned());
        set.rename_workspace(2, "gamma".to_owned());
        assert!(set.switch_workspace(2));
        let token = set.active_id();
        let dests = set.move_tab_destinations(token);
        assert_eq!(
            dests,
            vec![(0, "alpha".to_owned()), (1, "beta".to_owned())],
            "source workspace excluded; original indices + names preserved"
        );
    }

    #[test]
    fn reposition_active_tab_after_slides_new_tab_next_to_a_non_active_anchor() {
        // ODP-5D: the connect flow appends the remote tab last + switches to it;
        // reposition then slides it to sit right after the CLICKED (anchor) tab,
        // even when the anchor is neither the active nor the last tab.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        set.push(build_session_with_id(SessionToken(2)));
        // The freshly-appended remote tab, made active (mirrors connect + switch).
        set.push(build_session_with_id(SessionToken(3)));
        assert!(set.switch(SessionToken(3)));
        // Strip is [0, 1, 2, 3]; anchor on the clicked tab 1 (≠ active, ≠ last).
        set.reposition_active_tab_after(SessionToken(1));
        assert_eq!(set.token_at_position(0), Some(SessionToken(0)));
        assert_eq!(set.token_at_position(1), Some(SessionToken(1)));
        assert_eq!(set.token_at_position(2), Some(SessionToken(3)));
        assert_eq!(set.token_at_position(3), Some(SessionToken(2)));
        // The moved tab stays active/focused at its new index.
        assert_eq!(set.active_id(), SessionToken(3));
    }

    #[test]
    fn reorder_tab_splices_with_insertion_semantics_and_preserves_active_identity() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        set.push(build_session_with_id(SessionToken(2)));
        assert!(set.switch(SessionToken(1)));

        assert!(set.reorder_tab(0, 3));
        assert_eq!(set.token_at_position(0), Some(SessionToken(1)));
        assert_eq!(set.token_at_position(1), Some(SessionToken(2)));
        assert_eq!(set.token_at_position(2), Some(SessionToken(0)));
        assert_eq!(set.active_id(), SessionToken(1));

        assert!(!set.reorder_tab(2, 3), "drop after itself is a no-op");
        assert!(!set.reorder_tab(9, 0), "invalid source is a no-op");
        assert!(!set.reorder_tab(0, 9), "invalid insertion is a no-op");
    }

    #[test]
    fn reposition_active_tab_after_is_a_noop_when_already_adjacent_or_anchor_missing() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        set.push(build_session_with_id(SessionToken(2)));
        assert!(set.switch(SessionToken(2)));
        // Anchor is the tab right before the moved (last) tab: already in place.
        set.reposition_active_tab_after(SessionToken(1));
        assert_eq!(set.token_at_position(2), Some(SessionToken(2)));
        // Unknown anchor: nothing moves.
        set.reposition_active_tab_after(SessionToken(99));
        assert_eq!(set.token_at_position(2), Some(SessionToken(2)));
    }

    #[test]
    fn tab_foreground_job_running_resolves_tokens_and_defaults_false_when_idle() {
        // ODP-5D replace gating: a resolvable idle/headless tab reports
        // not-running (→ replace-direct path), and an unknown token is false.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        assert!(!set.tab_foreground_job_running(SessionToken(1)));
        assert!(!set.tab_foreground_job_running(SessionToken(99)));
    }

    #[test]
    fn move_tab_rejects_unknown_token_out_of_range_and_same_workspace() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        // Unknown token.
        assert_eq!(
            set.move_tab_to_workspace(SessionToken(99), 1),
            (false, false)
        );
        // Out-of-range destination.
        assert_eq!(
            set.move_tab_to_workspace(SessionToken(0), 9),
            (false, false)
        );
        // Same workspace (token 0 already in ws0 → dest 0).
        assert_eq!(
            set.move_tab_to_workspace(SessionToken(0), 0),
            (false, false)
        );
        // Nothing changed.
        assert_eq!(set.workspace_count(), 2);
        assert_eq!(set.tab_count(), 2);
    }

    // ---- WP2 restore-on-launch (design §10.6) ----

    /// A fake leaf spawner for restore tests: records the resolved cwd it is
    /// handed and inserts a headless session under a freshly minted token, so
    /// the rebuild runs without an event-loop proxy.
    #[cfg(test)]
    fn fake_spawner(
        handed: &mut Vec<Option<std::path::PathBuf>>,
    ) -> impl FnMut(&mut WorkspaceSet, Option<std::path::PathBuf>) -> Option<SessionToken> + '_
    {
        move |set: &mut WorkspaceSet, cwd: Option<std::path::PathBuf>| {
            handed.push(cwd.clone());
            let token = SessionToken(set.next_token);
            set.next_token = set.next_token.saturating_add(1);
            set.sessions.insert(token, build_session_with_id(token));
            Some(token)
        }
    }

    /// A remote spawner that never resolves a host — the default for the
    /// pre-RESTORE-REMOTE round-trip tests, which use only local leaves. A leaf
    /// carrying a `remote_host` would fall through to the local spawner.
    fn no_remote_spawner() -> impl FnMut(&mut WorkspaceSet, &str) -> Option<SessionToken> {
        |_, _| None
    }

    /// A headless remote spawner (RESTORE-REMOTE): records each identity it is
    /// asked to reconnect and inserts a placeholder session, standing in for the
    /// real `ssh` connect path so a test can assert remote leaves route here
    /// with the right host string.
    fn fake_remote_spawner(
        seen: &mut Vec<String>,
    ) -> impl FnMut(&mut WorkspaceSet, &str) -> Option<SessionToken> + '_ {
        move |set: &mut WorkspaceSet, identity: &str| {
            seen.push(identity.to_owned());
            let token = SessionToken(set.next_token);
            set.next_token = set.next_token.saturating_add(1);
            set.sessions.insert(token, build_session_with_id(token));
            Some(token)
        }
    }

    /// F6-W5: binding the active workspace to a host alias is observable through
    /// the accessor, idempotent, and unbinding returns the previous alias.
    #[test]
    fn workspace_host_binding_set_and_clear() {
        let mut set = WorkspaceSet::new(build_session(), None);
        assert_eq!(set.active_workspace_default_profile(), None);
        assert_eq!(
            set.set_active_workspace_default_profile(Some("prod".to_owned())),
            None,
            "first bind returns the previous (empty) binding"
        );
        assert_eq!(set.active_workspace_default_profile(), Some("prod"));
        assert_eq!(
            set.set_active_workspace_default_profile(None),
            Some("prod".to_owned()),
            "unbind returns the alias that was bound"
        );
        assert_eq!(set.active_workspace_default_profile(), None);
    }

    /// RAIL-BIND: the by-index bind/query pair targets a specific slot and is a
    /// safe no-op out of range.
    #[test]
    fn workspace_host_binding_by_index() {
        let mut set = WorkspaceSet::new(build_session(), None);
        assert_eq!(set.workspace_default_profile_at(0), None);
        assert_eq!(
            set.set_workspace_default_profile_at(0, Some("edge".to_owned())),
            None
        );
        assert_eq!(set.workspace_default_profile_at(0), Some("edge"));
        // The active accessor sees the same binding (slot 0 is active here).
        assert_eq!(set.active_workspace_default_profile(), Some("edge"));
        // Out-of-range index is a no-op returning None.
        assert_eq!(
            set.set_workspace_default_profile_at(9, Some("x".to_owned())),
            None
        );
        assert_eq!(set.workspace_default_profile_at(9), None);
    }

    /// F6-W5: a workspace host binding survives the capture -> restore round
    /// trip so a restored remote workspace routes New Tab through the host again.
    #[test]
    fn workspace_host_binding_survives_restore() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.set_active_workspace_default_profile(Some("edge-1".to_owned()));
        let snapshot = set.capture_shape();
        assert_eq!(
            snapshot.workspaces[0].default_profile.as_deref(),
            Some("edge-1"),
            "capture carries the binding into the snapshot"
        );

        let mut restored = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        restored.restore_from_snapshot_with(
            &snapshot,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert_eq!(
            restored.active_workspace_default_profile(),
            Some("edge-1"),
            "restore re-applies the binding"
        );
    }

    /// WP3 / 8e: instantiating a layout APPENDS its workspace(s) after the
    /// current list and focuses the first appended one — the live workspaces are
    /// untouched (never clobbered).
    #[test]
    fn append_from_snapshot_appends_without_clobbering() {
        // Start with two live workspaces.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        assert_eq!(set.workspace_count(), 2);
        set.rename_workspace(0, "live-a".to_owned());
        set.rename_workspace(1, "live-b".to_owned());

        // A one-workspace layout snapshot.
        let layout = crate::native::persistence::ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![crate::native::persistence::WorkspaceShape {
                name: "from-layout".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![crate::native::persistence::TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: crate::native::persistence::PaneShape::Leaf {
                        cwd: None,
                        session_host_id: None,
                        remote_host: None,
                    },
                }],
            }],
        };

        let mut handed = Vec::new();
        let report = set.append_from_snapshot_with(
            &layout,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert!(matches!(
            report,
            RestoreReport::Restored { workspaces: 1, .. }
        ));
        // The two live workspaces survive; the layout is appended as a third and
        // becomes active.
        assert_eq!(set.workspace_count(), 3);
        assert_eq!(set.workspace_name(0), Some("live-a"));
        assert_eq!(set.workspace_name(1), Some("live-b"));
        assert_eq!(set.workspace_name(2), Some("from-layout"));
        assert_eq!(set.active_workspace_index(), 2);
    }

    /// SAVE-ALL-LAYOUT: `capture_shape` records EVERY workspace (not just the
    /// active one), preserving rail order and the active-workspace index. This is
    /// the whole-app save side — the same snapshot the single-workspace save then
    /// slices down to one.
    #[test]
    fn capture_shape_records_every_workspace() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        set.rename_workspace(0, "one".to_owned());
        set.rename_workspace(1, "two".to_owned());
        set.rename_workspace(2, "three".to_owned());
        // Focus the middle workspace so the captured active index is non-zero.
        set.switch_workspace(1);

        let snapshot = set.capture_shape();
        assert_eq!(
            snapshot.workspaces.len(),
            3,
            "captures all three workspaces"
        );
        assert_eq!(snapshot.workspaces[0].name, "one");
        assert_eq!(snapshot.workspaces[1].name, "two");
        assert_eq!(snapshot.workspaces[2].name, "three");
        assert_eq!(
            snapshot.active_workspace, 1,
            "the active-workspace index is preserved in the whole-app capture"
        );
    }

    /// SAVE-ALL-LAYOUT: opening a whole-app layout (a multi-workspace snapshot)
    /// APPENDS every one of its workspaces after the live list, never just the
    /// first — the open side of the whole-app save.
    #[test]
    fn append_from_snapshot_appends_all_workspaces_of_a_multi_workspace_layout() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        set.rename_workspace(0, "live-a".to_owned());
        set.rename_workspace(1, "live-b".to_owned());
        assert_eq!(set.workspace_count(), 2);

        // A three-workspace layout snapshot (the whole-app save output shape).
        let leaf = || crate::native::persistence::TabShape {
            title: None,
            focused_leaf: 0,
            layout: crate::native::persistence::PaneShape::Leaf {
                cwd: None,
                session_host_id: None,
                remote_host: None,
            },
        };
        let ws = |name: &str| crate::native::persistence::WorkspaceShape {
            name: name.to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![leaf()],
        };
        let layout = crate::native::persistence::ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![ws("lay-1"), ws("lay-2"), ws("lay-3")],
        };

        let mut handed = Vec::new();
        let report = set.append_from_snapshot_with(
            &layout,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert!(
            matches!(report, RestoreReport::Restored { workspaces: 3, .. }),
            "all three layout workspaces are appended"
        );
        // The two live workspaces survive; the three layout workspaces follow.
        assert_eq!(set.workspace_count(), 5);
        assert_eq!(set.workspace_name(0), Some("live-a"));
        assert_eq!(set.workspace_name(1), Some("live-b"));
        assert_eq!(set.workspace_name(2), Some("lay-1"));
        assert_eq!(set.workspace_name(3), Some("lay-2"));
        assert_eq!(set.workspace_name(4), Some("lay-3"));
        // The first appended workspace becomes active (8e focus rule).
        assert_eq!(set.active_workspace_index(), 2);
    }

    /// PRISTINE-CONSUME: a bare launch is exactly one untouched default
    /// workspace, so the predicate reads `true`.
    #[test]
    fn is_single_pristine_workspace_true_for_a_fresh_launch() {
        let set = WorkspaceSet::new(build_session(), None);
        assert!(set.is_single_pristine_workspace());
    }

    /// PRISTINE-CONSUME: every kind of real state defeats the pristine check —
    /// a second workspace, a rename, a host binding, a split, or an extra tab.
    #[test]
    fn is_single_pristine_workspace_false_for_any_real_state() {
        // A second workspace.
        let mut two = WorkspaceSet::new(build_session(), None);
        two.push_workspace(build_session_with_id(SessionToken(1)));
        assert!(!two.is_single_pristine_workspace(), "two workspaces");

        // A renamed sole workspace.
        let mut renamed = WorkspaceSet::new(build_session(), None);
        renamed.rename_workspace(0, "prod".to_owned());
        assert!(!renamed.is_single_pristine_workspace(), "renamed");

        // A host-bound sole workspace.
        let mut bound = WorkspaceSet::new(build_session(), None);
        bound.set_active_workspace_default_profile(Some("edge".to_owned()));
        assert!(!bound.is_single_pristine_workspace(), "host-bound");

        // A split sole workspace (two panes in the one tab).
        let mut split = WorkspaceSet::new(build_session(), None);
        split.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
        assert!(!split.is_single_pristine_workspace(), "split");

        // A sole workspace with a second tab.
        let mut two_tabs = WorkspaceSet::new(build_session(), None);
        let extra = two_tabs.push_arena_only(build_session_with_id(SessionToken(1)));
        two_tabs.workspaces[0].tabs.push(Tab::single(extra));
        assert!(!two_tabs.is_single_pristine_workspace(), "two tabs");

        // A renamed tab title on the sole tab.
        let mut titled = WorkspaceSet::new(build_session(), None);
        let token = titled.workspaces[0].tabs[0].focused;
        titled.set_title_override(token, Some("build".to_owned()));
        assert!(!titled.is_single_pristine_workspace(), "tab titled");
    }

    /// PRISTINE-CONSUME: opening a layout onto a pristine launch replaces the
    /// default workspace with the saved set — no stray "Workspace 1" left over,
    /// and the pristine session is reaped from the arena.
    #[test]
    fn append_consumes_a_pristine_workspace_on_open() {
        let mut set = WorkspaceSet::new(build_session(), None);
        let pristine_token = set.workspaces[0].tabs[0].focused;
        assert!(set.sessions.contains_key(&pristine_token));

        let leaf = || crate::native::persistence::TabShape {
            title: None,
            focused_leaf: 0,
            layout: crate::native::persistence::PaneShape::Leaf {
                cwd: None,
                session_host_id: None,
                remote_host: None,
            },
        };
        let ws = |name: &str| crate::native::persistence::WorkspaceShape {
            name: name.to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![leaf()],
        };
        let layout = crate::native::persistence::ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![ws("saved-a"), ws("saved-b")],
        };

        let mut handed = Vec::new();
        let report = set.append_from_snapshot_with(
            &layout,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert!(matches!(
            report,
            RestoreReport::Restored { workspaces: 2, .. }
        ));
        // Exactly the saved set — the pristine workspace is gone, not appended.
        assert_eq!(set.workspace_count(), 2);
        assert_eq!(set.workspace_name(0), Some("saved-a"));
        assert_eq!(set.workspace_name(1), Some("saved-b"));
        assert_eq!(set.active_workspace_index(), 0);
        // The consumed workspace's session was reaped.
        assert!(!set.sessions.contains_key(&pristine_token));
    }

    /// PRISTINE-CONSUME: a single but NON-pristine workspace (here, renamed) is
    /// NOT consumed — the layout appends beside it, never clobbering real state.
    #[test]
    fn append_does_not_consume_a_single_but_renamed_workspace() {
        let mut set = WorkspaceSet::new(build_session(), None);
        set.rename_workspace(0, "live".to_owned());

        let layout = crate::native::persistence::ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![crate::native::persistence::WorkspaceShape {
                name: "saved".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![crate::native::persistence::TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: crate::native::persistence::PaneShape::Leaf {
                        cwd: None,
                        session_host_id: None,
                        remote_host: None,
                    },
                }],
            }],
        };

        let mut handed = Vec::new();
        set.append_from_snapshot_with(
            &layout,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        // The renamed workspace survives; the layout is appended as a second.
        assert_eq!(set.workspace_count(), 2);
        assert_eq!(set.workspace_name(0), Some("live"));
        assert_eq!(set.workspace_name(1), Some("saved"));
        assert_eq!(set.active_workspace_index(), 1);
    }

    /// LAYOUT-OPEN-MODE (Replace): instantiating a layout via the restore path
    /// onto a populated multi-workspace window installs EXACTLY the saved set —
    /// every prior workspace and its sessions are reaped (no survivors in the
    /// arena), and the saved active-workspace index is honored.
    #[test]
    fn replace_via_restore_leaves_no_survivors() {
        // A populated window: three workspaces, one session each.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push_workspace(build_session_with_id(SessionToken(1)));
        set.push_workspace(build_session_with_id(SessionToken(2)));
        set.rename_workspace(0, "old-a".to_owned());
        set.rename_workspace(1, "old-b".to_owned());
        set.rename_workspace(2, "old-c".to_owned());
        assert_eq!(set.workspace_count(), 3);
        assert_eq!(set.len(), 3, "three live sessions before replace");

        // A two-workspace layout with a non-zero active index.
        let leaf = || crate::native::persistence::TabShape {
            title: None,
            focused_leaf: 0,
            layout: crate::native::persistence::PaneShape::Leaf {
                cwd: None,
                session_host_id: None,
                remote_host: None,
            },
        };
        let ws = |name: &str| crate::native::persistence::WorkspaceShape {
            name: name.to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![leaf()],
        };
        let layout = crate::native::persistence::ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 1,
            workspaces: vec![ws("saved-a"), ws("saved-b")],
        };

        let mut handed = Vec::new();
        let report = set.restore_from_snapshot_with(
            &layout,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert!(matches!(
            report,
            RestoreReport::Restored { workspaces: 2, .. }
        ));
        // Exactly the saved set — no old workspaces survive.
        assert_eq!(set.workspace_count(), 2);
        assert_eq!(set.workspace_name(0), Some("saved-a"));
        assert_eq!(set.workspace_name(1), Some("saved-b"));
        // Every prior session was reaped: the arena holds only the two new panes.
        assert_eq!(set.len(), 2, "no survivor sessions from the old set");
        // The saved active-workspace index is honored.
        assert_eq!(set.active_workspace_index(), 1);
    }

    /// WP3 / 8h: a pane carrying a session-host id whose host is not alive (no
    /// runtime dir in the test) is counted as a reattach attempt but falls back
    /// to a fresh shell — never a dead pane. Verifies the "N of M" accounting.
    #[test]
    fn reattach_counts_attempt_and_falls_back_to_fresh_when_host_is_dead() {
        let snapshot = crate::native::persistence::ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![crate::native::persistence::WorkspaceShape {
                name: "w".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![crate::native::persistence::TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: crate::native::persistence::PaneShape::Leaf {
                        cwd: None,
                        session_host_id: Some("odytty-nonexistent-host".to_owned()),
                        remote_host: None,
                    },
                }],
            }],
        };
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        let report = set.restore_from_snapshot_with(
            &snapshot,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert!(
            matches!(
                report,
                RestoreReport::Restored {
                    panes: 1,
                    reattached: 0,
                    reattach_attempted: 1,
                    ..
                }
            ),
            "report was {report:?}"
        );
        // A fresh shell was spawned for the pane despite the dead host id.
        assert_eq!(handed.len(), 1);
    }

    /// Capture a rich shape, restore it into a fresh set, and assert the rebuilt
    /// shape equals the captured one (structural equality; the fake sessions
    /// carry no cwd, so every captured cwd here is `None` too). Exercises the
    /// end-to-end capture -> restore round trip headlessly.
    #[test]
    fn restore_rebuilds_the_captured_shape() {
        // ws0: tab0 split (Rows) into two panes; tab1 a titled single pane.
        // ws1: one single-pane tab, renamed. Active stays ws0 / tab0.
        let mut set = WorkspaceSet::new(build_session(), None);
        set.push(build_session_with_id(SessionToken(1)));
        set.set_title_override(SessionToken(1), Some("build".to_owned()));
        set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(2)));
        set.push_workspace(build_session_with_id(SessionToken(3)));
        set.rename_workspace(1, "logs".to_owned());

        let snapshot = set.capture_shape();
        assert_eq!(snapshot.workspaces.len(), 2);

        let mut restored = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        let report = restored.restore_from_snapshot_with(
            &snapshot,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );

        assert!(
            matches!(
                report,
                RestoreReport::Restored {
                    workspaces: 2,
                    panes: 4,
                    stale_cwd: 0,
                    ..
                }
            ),
            "report was {report:?}"
        );
        // The launch session was reaped; only the 4 restored leaves remain.
        assert_eq!(restored.len(), 4);
        // The rebuilt shape mirrors the captured one exactly.
        assert_eq!(restored.capture_shape(), snapshot);
    }

    /// A captured directory that no longer exists lands the pane at home and is
    /// counted stale; an unknown (`None`) cwd also lands at home but is NOT
    /// counted (a quiet fallback). Both drive the resolved cwd handed to spawn.
    #[test]
    fn restore_lands_stale_and_unknown_cwds_at_home() {
        use crate::native::persistence::{
            PaneShape, ShapeSnapshot, SplitAxisShape, TabShape, WorkspaceShape,
        };
        let snapshot = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![WorkspaceShape {
                name: "W".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: PaneShape::Split {
                        axis: SplitAxisShape::Columns,
                        ratio: 0.5,
                        first: Box::new(PaneShape::Leaf {
                            cwd: Some("/definitely/not/a/real/dir/odytty-wp2".to_owned()),
                            session_host_id: None,
                            remote_host: None,
                        }),
                        second: Box::new(PaneShape::Leaf {
                            cwd: None,
                            session_host_id: None,
                            remote_host: None,
                        }),
                    },
                }],
            }],
        };
        let home = std::env::temp_dir();
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        let report = set.restore_from_snapshot_with(
            &snapshot,
            Some(&home),
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );

        assert!(
            matches!(
                report,
                RestoreReport::Restored {
                    panes: 2,
                    stale_cwd: 1,
                    ..
                }
            ),
            "report was {report:?}"
        );
        // Both leaves (stale and unknown) were handed the home directory.
        assert_eq!(handed, vec![Some(home.clone()), Some(home)]);
    }

    /// A spawn failure mid-rebuild aborts the whole restore, reaping anything
    /// already spawned and leaving the launch layout untouched (sub-ODP 8f:
    /// never a broken/empty window).
    #[test]
    fn restore_aborts_cleanly_when_a_leaf_fails_to_spawn() {
        use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
        let leaf = |cwd: Option<&str>| PaneShape::Leaf {
            cwd: cwd.map(str::to_owned),
            session_host_id: None,
            remote_host: None,
        };
        let snapshot = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![WorkspaceShape {
                name: "W".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![
                    TabShape {
                        title: None,
                        focused_leaf: 0,
                        layout: leaf(None),
                    },
                    TabShape {
                        title: None,
                        focused_leaf: 0,
                        layout: leaf(None),
                    },
                ],
            }],
        };
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut spawned = 0u32;
        let report = set.restore_from_snapshot_with(
            &snapshot,
            None,
            |inner, _cwd| {
                spawned += 1;
                if spawned >= 2 {
                    return None; // second leaf fails
                }
                let token = SessionToken(inner.next_token);
                inner.next_token = inner.next_token.saturating_add(1);
                inner.sessions.insert(token, build_session_with_id(token));
                Some(token)
            },
            no_remote_spawner(),
        );

        assert_eq!(report, RestoreReport::Skipped);
        // Launch layout intact: one workspace, one (launch) session; the partial
        // spawn was reaped.
        assert_eq!(set.workspace_count(), 1);
        assert_eq!(set.len(), 1);
    }

    /// H4: a snapshot that PARSES cleanly but carries semantically hostile
    /// values — out-of-range active/focused indices reachable by hand-editing
    /// `workspaces.json` — must never panic the launch-time rebuild. Every index
    /// is clamped or falls back into range, so restore lands one valid, focused
    /// workspace. Belt-and-suspenders over the audited guards
    /// (`active_workspace.min(len-1)`, `active_tab.min(len-1)`, and the
    /// `focused_leaf` first-leaf fallback). Platform-neutral: the rebuild is
    /// index math with no OS surface.
    #[test]
    fn out_of_range_indices_in_a_snapshot_clamp_never_panic() {
        use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
        let leaf = || PaneShape::Leaf {
            cwd: None,
            session_host_id: None,
            remote_host: None,
        };
        let snapshot = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            // Far past the (one) workspace, the (one) tab, and the (one) leaf.
            active_workspace: 999,
            workspaces: vec![WorkspaceShape {
                name: "w".to_owned(),
                default_profile: None,
                active_tab: 999,
                tabs: vec![TabShape {
                    title: None,
                    focused_leaf: 999,
                    layout: leaf(),
                }],
            }],
        };
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        let report = set.restore_from_snapshot_with(
            &snapshot,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        // One valid workspace restored; the runaway active index clamps to the
        // last real workspace rather than panicking a bounds check.
        assert!(matches!(
            report,
            RestoreReport::Restored {
                workspaces: 1,
                panes: 1,
                ..
            }
        ));
        assert_eq!(
            set.active_workspace_index(),
            0,
            "active_workspace clamps to the last real index"
        );
    }

    /// H4: a snapshot with nothing restorable — zero workspaces, or workspaces
    /// that are all tab-less — must degrade to `Skipped` (the launch layout is
    /// left untouched, so the caller keeps its fresh session) and never panic on
    /// the empty vectors. Both are reachable by hand-editing the file.
    #[test]
    fn empty_and_tabless_snapshots_are_skipped_never_panic() {
        use crate::native::persistence::{ShapeSnapshot, WorkspaceShape};

        // (i) zero workspaces: nothing to build -> Skipped, layout intact.
        let empty = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![],
        };
        let mut set = WorkspaceSet::new(build_session(), None);
        let before = set.workspace_count();
        let mut handed = Vec::new();
        let report = set.restore_from_snapshot_with(
            &empty,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert_eq!(report, RestoreReport::Skipped);
        assert_eq!(
            set.workspace_count(),
            before,
            "a no-op restore leaves the live layout intact"
        );

        // (ii) every workspace has empty tabs: each is `continue`d, nothing is
        // built -> Skipped, and no `active_tab.min(len-1)` underflow on len 0.
        let tabless = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![
                WorkspaceShape {
                    name: "a".to_owned(),
                    default_profile: None,
                    active_tab: 3,
                    tabs: vec![],
                },
                WorkspaceShape {
                    name: "b".to_owned(),
                    default_profile: None,
                    active_tab: 0,
                    tabs: vec![],
                },
            ],
        };
        let mut set2 = WorkspaceSet::new(build_session(), None);
        let mut handed2 = Vec::new();
        let report2 = set2.restore_from_snapshot_with(
            &tabless,
            None,
            fake_spawner(&mut handed2),
            no_remote_spawner(),
        );
        assert_eq!(report2, RestoreReport::Skipped);
    }

    /// H4: absurd split ratios and a deep split spine must not panic the
    /// recursive rebuild. Out-of-[0,1] ratios (negative, > 1, huge) are
    /// reachable by hand-editing the file; NaN / infinity are not parser-
    /// reachable (JSON has no such literals) but are constructed here to prove
    /// the rebuild is total over any `f32` it is handed. The ratios are stored
    /// verbatim into the layout tree; geometry that consumes them is a separate,
    /// later concern — restore itself only builds the tree.
    #[test]
    fn absurd_ratios_and_deep_nesting_restore_without_panicking() {
        use crate::native::persistence::{
            PaneShape, ShapeSnapshot, SplitAxisShape, TabShape, WorkspaceShape,
        };
        let leaf = || PaneShape::Leaf {
            cwd: None,
            session_host_id: None,
            remote_host: None,
        };
        // A right-leaning split spine 40 deep, each level carrying a
        // pathological ratio. 40 added leaves + the original = 41 leaves.
        let mut node = leaf();
        for i in 0..40 {
            let ratio = match i % 4 {
                0 => f32::NAN,
                1 => -3.0,
                2 => 17.0,
                _ => f32::INFINITY,
            };
            node = PaneShape::Split {
                axis: SplitAxisShape::Columns,
                ratio,
                first: Box::new(leaf()),
                second: Box::new(node),
            };
        }
        let snapshot = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![WorkspaceShape {
                name: "deep".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: node,
                }],
            }],
        };
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        let report = set.restore_from_snapshot_with(
            &snapshot,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        // The whole spine spawned; the rebuild survived the ratios and depth.
        assert!(matches!(report, RestoreReport::Restored { panes: 41, .. }));
    }

    /// RESTORE-REMOTE: a leaf carrying a `remote_host` reconnects through the
    /// remote spawner (with the exact stored identity), while a local leaf beside
    /// it still routes to the local spawner. No pane falls back.
    #[test]
    fn restore_reconnects_remote_leaves_and_keeps_local_leaves_local() {
        use crate::native::persistence::{
            PaneShape, ShapeSnapshot, SplitAxisShape, TabShape, WorkspaceShape,
        };
        let snapshot = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![WorkspaceShape {
                name: "W".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: PaneShape::Split {
                        axis: SplitAxisShape::Columns,
                        ratio: 0.5,
                        first: Box::new(PaneShape::Leaf {
                            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                            session_host_id: None,
                            remote_host: None,
                        }),
                        second: Box::new(PaneShape::Leaf {
                            // A remote pane captured the REMOTE cwd; it must not be
                            // used to chdir a local shell — this leaf reconnects.
                            cwd: Some("/root".to_owned()),
                            session_host_id: None,
                            remote_host: Some("prod".to_owned()),
                        }),
                    },
                }],
            }],
        };
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut local_handed = Vec::new();
        let mut remote_seen = Vec::new();
        let report = set.restore_from_snapshot_with(
            &snapshot,
            None,
            fake_spawner(&mut local_handed),
            fake_remote_spawner(&mut remote_seen),
        );
        // The remote leaf reached the connect spawner with its stored identity;
        // the local leaf reached the local spawner with its own cwd — and the
        // remote leaf never touched the local spawner (no /root local shell).
        assert_eq!(remote_seen, vec!["prod".to_owned()]);
        assert_eq!(local_handed, vec![Some(std::env::temp_dir())]);
        assert!(
            matches!(
                report,
                RestoreReport::Restored {
                    panes: 2,
                    remote_fallback: 0,
                    ..
                }
            ),
            "{report:?}"
        );
    }

    /// RESTORE-REMOTE: a remote leaf whose host cannot be resolved (the spawner
    /// returns `None`) falls back to a local shell, counted in `remote_fallback`,
    /// and the restore still succeeds — never a wholesale abort.
    #[test]
    fn restore_falls_back_to_local_when_remote_host_unresolvable() {
        use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
        let snapshot = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![WorkspaceShape {
                name: "W".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: PaneShape::Leaf {
                        // cwd None so the local fallback lands at home cleanly
                        // (no stale-cwd noise) — this asserts the remote_fallback
                        // count in isolation.
                        cwd: None,
                        session_host_id: None,
                        remote_host: Some("gone.example.invalid".to_owned()),
                    },
                }],
            }],
        };
        let home = std::env::temp_dir();
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut local_handed = Vec::new();
        let report = set.restore_from_snapshot_with(
            &snapshot,
            Some(&home),
            fake_spawner(&mut local_handed),
            no_remote_spawner(),
        );
        assert_eq!(local_handed, vec![Some(home)]);
        assert!(
            matches!(
                report,
                RestoreReport::Restored {
                    panes: 1,
                    remote_fallback: 1,
                    stale_cwd: 0,
                    ..
                }
            ),
            "{report:?}"
        );
    }

    /// RESTORE-REMOTE / sub-ODP 8f: a local leaf whose captured directory exists
    /// but denies the spawn (the EACCES a real `chdir` would hit, e.g. a remote
    /// `/root` that exists locally at mode 700) retries once at home — counted as
    /// stale_cwd — instead of aborting the whole restore.
    #[test]
    fn restore_retries_at_home_when_spawn_fails_at_an_existing_cwd() {
        use crate::native::persistence::{PaneShape, ShapeSnapshot, TabShape, WorkspaceShape};
        // A real directory that EXISTS (so resolve_cwd does not pre-fall-back to
        // home) but that the spawner will refuse, standing in for a live chdir
        // EACCES on a mode-000/700 directory.
        let bad = std::env::temp_dir().join(format!("odytty-eacces-{}", std::process::id()));
        std::fs::create_dir_all(&bad).unwrap();
        let bad_str = bad.to_string_lossy().into_owned();
        let home = std::env::temp_dir();
        let snapshot = ShapeSnapshot {
            version: crate::native::persistence::SNAPSHOT_VERSION,
            active_workspace: 0,
            workspaces: vec![WorkspaceShape {
                name: "W".to_owned(),
                default_profile: None,
                active_tab: 0,
                tabs: vec![TabShape {
                    title: None,
                    focused_leaf: 0,
                    layout: PaneShape::Leaf {
                        cwd: Some(bad_str.clone()),
                        session_host_id: None,
                        remote_host: None,
                    },
                }],
            }],
        };
        let mut set = WorkspaceSet::new(build_session(), None);
        let mut handed: Vec<Option<std::path::PathBuf>> = Vec::new();
        let bad_path = bad.clone();
        let report = set.restore_from_snapshot_with(
            &snapshot,
            Some(&home),
            |inner: &mut WorkspaceSet, cwd: Option<std::path::PathBuf>| {
                handed.push(cwd.clone());
                // Refuse the captured directory (the simulated EACCES); accept the
                // home retry.
                if cwd.as_deref() == Some(bad_path.as_path()) {
                    return None;
                }
                let token = SessionToken(inner.next_token);
                inner.next_token = inner.next_token.saturating_add(1);
                inner.sessions.insert(token, build_session_with_id(token));
                Some(token)
            },
            no_remote_spawner(),
        );
        let _ = std::fs::remove_dir_all(&bad);
        // First tried the captured dir, then retried at home; counted stale, and
        // the restore succeeded rather than aborting.
        assert_eq!(handed, vec![Some(bad), Some(home)]);
        assert!(
            matches!(
                report,
                RestoreReport::Restored {
                    panes: 1,
                    stale_cwd: 1,
                    ..
                }
            ),
            "{report:?}"
        );
    }

    /// RESTORE-REMOTE: a session's remote destination is captured into the shape
    /// as the leaf's `remote_host`; a local session leaves it `None`.
    #[test]
    fn capture_records_remote_destination_as_remote_host() {
        let mut set = WorkspaceSet::new(build_session(), None);
        let token = set.active_id();
        set.sessions.get_mut(&token).unwrap().remote_destination = Some("prod".to_owned());
        let snapshot = set.capture_shape();
        match &snapshot.workspaces[0].tabs[0].layout {
            crate::native::persistence::PaneShape::Leaf { remote_host, .. } => {
                assert_eq!(remote_host.as_deref(), Some("prod"));
            }
            other => panic!("expected a leaf, got {other:?}"),
        }
    }

    /// The structural fingerprint changes when the shape changes and is stable
    /// otherwise (the debounce trigger, sub-ODP 8c).
    #[test]
    fn structural_fingerprint_tracks_shape_changes() {
        let mut set = WorkspaceSet::new(build_session(), None);
        let base = set.structural_fingerprint();
        assert_eq!(base, set.structural_fingerprint(), "stable when unchanged");

        set.push(build_session_with_id(SessionToken(1)));
        let after_tab = set.structural_fingerprint();
        assert_ne!(after_tab, base, "adding a tab changes the fingerprint");

        set.rename_workspace(0, "renamed".to_owned());
        assert_ne!(
            set.structural_fingerprint(),
            after_tab,
            "renaming a workspace changes the fingerprint"
        );
    }
}
