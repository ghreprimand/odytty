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
use crate::native::pty::{PtyWriter, UserEvent};
use crate::native::test_support::spawn_test_pause_shell;
use crate::selection::AbsoluteSelectionRange;
use crate::ssh_connect::{RemoteSshOptions, SshCommand};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use winit::event_loop::EventLoop;
#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

mod lifecycle;
mod persistence;
mod presentation;
mod transport;

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

/// A short-lived local child masquerading as an ssh session, whose exit code
/// is `code`. Used to drive the reconnect classifier without a live ssh.
#[cfg(not(windows))]
fn exit_code_command(code: i32) -> SshCommand {
    SshCommand::new(
        "/bin/sh",
        vec![OsString::from("-c"), OsString::from(format!("exit {code}"))],
    )
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
) -> impl FnMut(&mut WorkspaceSet, Option<std::path::PathBuf>) -> Option<SessionToken> + '_ {
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
