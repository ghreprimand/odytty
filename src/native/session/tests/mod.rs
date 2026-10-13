// SPDX-License-Identifier: GPL-3.0-only
//! Session tests, grouped by responsibility. Shared fixtures live here; the
//! per-responsibility suites sit in the sibling modules.

use super::lifecycle::held_exit_banner;
use super::model::{Session, SessionToken, Tab, WorkspaceSet};
use super::persistence::RestoreReport;
#[cfg(unix)]
use super::persistence::per_connection_attach_budget;
use super::transport::{ExitDisposition, HeadlessSession, SessionSource, classify_remote_exit};
use crate::core::Dimensions;
use crate::core::Terminal;
use crate::native::app::TabBarSource;
use crate::native::layout::{FocusDir, PaneNode, PaneRect, SplitAxis, layout_rects};
use crate::native::pty::PtyWriter;
use crate::native::test_support::spawn_test_pause_shell;
use crate::selection::AbsoluteSelectionRange;
use crate::ssh_connect::{RemoteSshOptions, SshCommand};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Instant;

mod floating_layout;
mod lifecycle;
mod persistence;
mod presentation;
mod reparent;
mod resize_retry;
mod scrollback_trim;
mod stable_identity;
mod transport;
mod window_merge;

fn build_session_with_id(id: SessionToken) -> Session {
    // Pure WorkspaceSet bookkeeping: no PTY behavior is asserted, so a
    // headless session avoids owning a real shell. The CLOSE-HANG / shutdown
    // reaper tests use `build_session_with_parked_reader`, which keeps a real
    // pump-thread shape.
    let dims = Dimensions::new(20, 8);
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let headless = Arc::new(HeadlessSession::new(dims));
    Session::new_headless(id, terminal, writer, headless)
}

fn build_session() -> Session {
    build_session_with_id(SessionToken(0))
}

/// A real-PTY-backed local session for the small set of tests that assert
/// `SessionSource::Local` behavior or route a resize to a concrete PTY.
/// Every other session test uses the headless builders.
fn build_local_session_with_id(id: SessionToken) -> Session {
    let dims = Dimensions::new(20, 8);
    let pty = spawn_test_pause_shell(dims).expect("spawn test shell");
    let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().expect("writer")));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(pty));
    Session::new(id, terminal, writer, pty, None)
}

fn test_selection() -> AbsoluteSelectionRange {
    AbsoluteSelectionRange {
        start: crate::selection::AbsoluteCellPoint { row: 0, column: 0 },
        end: crate::selection::AbsoluteCellPoint { row: 0, column: 1 },
    }
}

/// Holds a parked reader from [`build_session_with_parked_reader`]. The
/// reader stays parked until this gate is dropped (or a minute passes), and
/// [`Self::released`] reports whether it has finished.
struct ReaderGate {
    _release: std::sync::mpsc::Sender<()>,
    finished: Arc<std::sync::atomic::AtomicBool>,
}

impl ReaderGate {
    fn released(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// How long a parked reader waits for its gate before it gives up. A teardown
/// that joined the reader on the caller would block this long, and then see
/// it finished.
const PARKED_READER_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Build a session whose pump (reader) thread is PARKED and will not exit
/// until its [`ReaderGate`] is dropped - the shape a wedged remote leaves
/// behind (the ssh child never delivers EOF, so the reader never returns).
/// Used to prove the teardown returns before that join completes, without a
/// wall-clock bound.
fn build_session_with_parked_reader(id: SessionToken) -> (Session, ReaderGate) {
    let dims = Dimensions::new(20, 8);
    let pty = spawn_test_pause_shell(dims).expect("spawn test shell");
    let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().expect("writer")));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(pty));
    let (release, parked_on) = std::sync::mpsc::channel::<()>();
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished_signal = finished.clone();
    let parked = std::thread::Builder::new()
        .name("test-wedged-reader".to_owned())
        .spawn(move || {
            let _ = parked_on.recv_timeout(PARKED_READER_LIMIT);
            finished_signal.store(true, std::sync::atomic::Ordering::Release);
        })
        .expect("spawn parked reader");
    let gate = ReaderGate {
        _release: release,
        finished,
    };
    (Session::new(id, terminal, writer, pty, Some(parked)), gate)
}

/// A workspace set whose spawns get a wake handle from
/// `event_waker_for_test`: the process-wide test event loop's proxy where one can
/// be built, a discarding waker on macOS. `None` (with the reason already
/// printed) when this environment offers no loop. Building a fresh loop per test
/// only ever worked for the first case in the process: winit refuses to recreate
/// one, so every later case returned early without asserting anything.
fn tabset_with_proxy_for_test() -> Option<WorkspaceSet> {
    let proxy = crate::native::tests::event_waker_for_test().ok()?;
    Some(WorkspaceSet::new(build_session(), Some(proxy)))
}

/// A short-lived local child masquerading as an ssh session, whose exit code
/// is `code`. Used to drive the reconnect classifier without a live ssh. The
/// command text is a fixed test constant on both platforms.
fn exit_code_command(code: i32) -> SshCommand {
    shell_command(&format!("exit {code}"))
}

/// Run `script` through the platform's stock command interpreter.
fn shell_command(script: &str) -> SshCommand {
    #[cfg(not(windows))]
    let (program, flag) = ("/bin/sh", "-c");
    #[cfg(windows)]
    let (program, flag) = ("cmd.exe", "/C");
    SshCommand::new(program, vec![OsString::from(flag), OsString::from(script)])
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

/// A fake leaf spawner for restore tests: records the resolved cwd it is
/// handed and inserts a headless session under a freshly minted token, so
/// the rebuild runs without an event-loop proxy.
#[cfg(test)]
fn fake_spawner(
    handed: &mut Vec<Option<std::path::PathBuf>>,
) -> impl FnMut(&mut WorkspaceSet, crate::native::session::RestoredLocalLeaf) -> Option<SessionToken> + '_
{
    move |set: &mut WorkspaceSet, leaf: crate::native::session::RestoredLocalLeaf| {
        handed.push(leaf.cwd.clone());
        let token = SessionToken(set.next_token);
        set.next_token = set.next_token.saturating_add(1);
        set.sessions.insert(token, build_session_with_id(token));
        Some(token)
    }
}

/// A remote spawner that rejects every remote restore identity; fixtures with
/// only local leaves use it. A leaf carrying a `remote_host` would fall through
/// to the local spawner.
fn no_remote_spawner() -> impl FnMut(&mut WorkspaceSet, &str) -> Option<SessionToken> {
    |_, _| None
}

/// A headless remote spawner: records each identity it is asked to reconnect
/// and inserts a placeholder session, standing in for the real `ssh` connect
/// path so a test can assert remote leaves route here with the right host
/// string.
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
