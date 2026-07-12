// SPDX-License-Identifier: GPL-3.0-only
//! Hermetic tests for the native window-as-client attach path.
//!
//! Each test stands up an in-process fake session-host: a `UnixListener` under a
//! synthetic `0700` runtime dir that speaks the public wire protocol. It does the
//! handshake plus the initial snapshot, then runs a per-test script. No real
//! session-host process and no host-specific data are involved.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::*;
use crate::core::{Dimensions, SnapshotCaptureLimits, Terminal};
use crate::native::layout::PaneRect;
use crate::native::session::{Session, WorkspaceSet};
use crate::pty::PtySession;
use crate::session_host::protocol::{
    ClientFrame, HostFrame, HostHello, read_client_frame, read_client_hello, write_host_frame,
    write_host_hello,
};
use crate::session_host::{runtime_dir_path, session_socket_path};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

/// A `0700` runtime dir owned by the current uid, satisfying
/// `validate_socket_parent`. Best-effort cleanup is left to the OS temp reaper.
fn unique_runtime_dir() -> PathBuf {
    // Keep this base SHORT: the resolved socket is `<base>/odytty/session-<id>.sock`
    // and on macOS the temp base is a long `/var/folders/.../T/` path, so a verbose
    // unique dir overflows the 104-byte `AF_UNIX` sun_path limit and `bind()` fails.
    let dir = std::env::temp_dir().join(format!(
        "oda_{}_{}",
        std::process::id(),
        UNIQUE.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create runtime dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700 runtime dir");
    dir
}

/// A known hosted terminal: 20x4 grid with three printed lines and the cursor
/// parked after a prompt, so live output appends predictably.
fn sample_host_terminal() -> Terminal {
    let mut terminal = Terminal::new(20, 4);
    terminal.set_scrollback_limit(100);
    terminal.advance(b"line-one\r\nline-two\r\nPROMPT$ ");
    terminal
}

fn snapshot_bytes(terminal: &Terminal) -> Vec<u8> {
    SnapshotEnvelope::from_terminal(terminal, SnapshotCaptureLimits::default()).encode()
}

/// The trimmed text of one visible row of a terminal's snapshot.
fn row_text(terminal: &Terminal, row: usize) -> String {
    let snapshot = terminal.snapshot();
    let cols = snapshot.dimensions.columns;
    snapshot.cells[row * cols..(row + 1) * cols]
        .iter()
        .map(|cell| cell.ch)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Stand up the fake host: bind the socket, then on a thread accept one client,
/// complete the handshake, send `snapshot`, and run `script` against the accepted
/// stream. Returns the socket path and the host thread handle (its join value is
/// whatever `script` returns).
fn spawn_fake_host<T, F>(snapshot: Vec<u8>, script: F) -> (PathBuf, JoinHandle<T>)
where
    T: Send + 'static,
    F: FnOnce(UnixStream) -> T + Send + 'static,
{
    let dir = unique_runtime_dir();
    let socket_path = dir.join("attach.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind fake host");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept attach client");
        let _hello = read_client_hello(&mut stream).expect("read client hello");
        write_host_hello(&mut stream, &HostHello::accepted()).expect("write host hello");
        write_host_frame(&mut stream, &HostFrame::Snapshot(snapshot)).expect("write snapshot");
        script(stream)
    });
    (socket_path, handle)
}

fn test_caps() -> SnapshotEnvelopeCaps {
    SnapshotEnvelopeCaps::default()
}

fn test_deadline() -> Duration {
    Duration::from_secs(5)
}

/// Upper bound on awaiting a spawned helper thread (the attach pump or the fake
/// host) to finish. Generous; purely a safety net so a wedged pump that never
/// EOFs fails the test fast with a clear message instead of hanging the suite.
const JOIN_DEADLINE: Duration = Duration::from_secs(30);

/// Join a spawned thread with a hard deadline, re-raising its panic if it
/// panicked, or panicking with a clear message if it does not finish in time. A
/// timed-out thread is left detached on purpose: the goal is to convert a silent
/// infinite hang into a loud, attributable failure.
fn join_within<T: Send + 'static>(handle: JoinHandle<T>, what: &str) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    match rx.recv_timeout(JOIN_DEADLINE) {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!(
            "{what} did not finish within {JOIN_DEADLINE:?} — the attach pump or \
             fake host is wedged (never EOFed). Failing fast instead of hanging \
             the suite."
        ),
    }
}

/// Poll `cond` every 5ms until it returns true or a generous CI-aware timeout
/// elapses; returns whether it became true. Replaces fixed `sleep` + assert-once
/// so a slow runner that pumps a frame late still observes the expected state.
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return cond();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Host-side teardown that holds the connection open until the attach client
/// disconnects (a clean `Detach` frame or EOF), then returns so the stream drops.
/// Because the host does not close until the client has finished, the client's
/// read pump can never EOF mid-frame on a slow runner — every host-written frame
/// is consumed before the socket closes (the ACK-based teardown the flaky fixed
/// `sleep(150ms); drop` lacked).
fn drain_until_disconnect(mut stream: UnixStream) {
    loop {
        match read_client_frame(&mut stream) {
            // A clean `Detach` is the client signaling it is done. It arrives
            // before the client's write half is dropped (the app's
            // `Session::close` sends `Detach` and then *joins* the pump before
            // the socket drops), so we must return here rather than waiting for
            // EOF — otherwise the host never drops, the pump never EOFs, and the
            // join deadlocks.
            Ok(ClientFrame::Detach) => return,
            // Input / resize before the detach: keep holding the link open.
            Ok(_) => continue,
            // EOF / disconnect / protocol end: the client is gone, safe to drop.
            Err(_) => return,
        }
    }
}

/// Channel-backed event sink so the pump is exercisable without a winit loop.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ev {
    Redraw(SessionToken),
    Exited(SessionToken),
}

#[derive(Clone)]
struct ChannelSink(mpsc::Sender<Ev>);

impl AttachEventSink for ChannelSink {
    fn redraw(&self, session: SessionToken) {
        let _ = self.0.send(Ev::Redraw(session));
    }
    fn exited(&self, session: SessionToken) {
        let _ = self.0.send(Ev::Exited(session));
    }
}

#[test]
fn snapshot_decode_repaints_full_state() {
    let host_terminal = sample_host_terminal();
    let expected = host_terminal.snapshot();
    let (socket_path, handle) = spawn_fake_host(snapshot_bytes(&host_terminal), |stream| {
        // Hold the connection open briefly so the client finishes decoding.
        std::thread::sleep(Duration::from_millis(20));
        drop(stream);
    });

    let (_client, _reader, terminal) =
        AttachClient::connect_with(&socket_path, "snap", test_caps(), test_deadline())
            .expect("attach connects and decodes snapshot");

    assert_eq!(terminal.snapshot().dimensions, expected.dimensions);
    assert_eq!(row_text(&terminal, 0), "line-one");
    assert_eq!(row_text(&terminal, 1), "line-two");
    assert_eq!(row_text(&terminal, 2), "PROMPT$");
    // Full mirror equality: the restored client terminal matches the host grid.
    assert_eq!(terminal.snapshot().cells, expected.cells);
    handle.join().expect("host thread");
}

#[test]
fn live_output_incremental_repaint() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            write_host_frame(&mut stream, &HostFrame::Output(b"XYZ".to_vec()))
                .expect("write output");
            // Close so the pump sees EOF and returns.
            drop(stream);
        });

    let (_client, reader, terminal) =
        AttachClient::connect_with(&socket_path, "live", test_caps(), test_deadline())
            .expect("attach connects");
    let terminal = Arc::new(Mutex::new(terminal));
    let (tx, rx) = mpsc::channel();
    let token = SessionToken(7);
    // Run the pump on its own thread and join it under a deadline: a wedged pump
    // that never EOFs must fail this test fast, not hang the suite.
    let pump_terminal = terminal.clone();
    let pump = std::thread::spawn(move || {
        run_attach_pump(reader, pump_terminal, ChannelSink(tx), token);
    });
    join_within(pump, "attach pump");

    // Output landed and appended at the restored cursor (after "PROMPT$ ").
    let term = terminal.lock().unwrap();
    assert_eq!(row_text(&term, 2), "PROMPT$ XYZ");
    drop(term);

    let events: Vec<Ev> = rx.try_iter().collect();
    assert!(
        events.contains(&Ev::Redraw(token)),
        "live output must trigger a redraw: {events:?}"
    );
    assert!(
        events.contains(&Ev::Exited(token)),
        "EOF after output must signal exit: {events:?}"
    );
    join_within(handle, "fake host thread");
}

#[test]
fn session_exit_is_handled() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            write_host_frame(&mut stream, &HostFrame::SessionExit { exit_code: Some(0) })
                .expect("write session exit");
            drop(stream);
        });

    let (_client, reader, terminal) =
        AttachClient::connect_with(&socket_path, "exit", test_caps(), test_deadline())
            .expect("attach connects");
    let (tx, rx) = mpsc::channel();
    let token = SessionToken(3);
    // Pump must return (not hang) after SessionExit. Run it on its own thread and
    // join under a deadline so a regression here fails fast instead of hanging.
    let pump = std::thread::spawn(move || {
        run_attach_pump(
            reader,
            Arc::new(Mutex::new(terminal)),
            ChannelSink(tx),
            token,
        );
    });
    join_within(pump, "attach pump");

    let events: Vec<Ev> = rx.try_iter().collect();
    assert_eq!(events, vec![Ev::Exited(token)]);
    join_within(handle, "fake host thread");
}

#[test]
fn window_close_detaches_and_host_survives() {
    // The host reads one client frame after the handshake and reports it. A clean
    // Detach (not an abrupt EOF) is what lets the host keep the session alive.
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            read_client_frame(&mut stream).expect("read client frame")
        });

    let (mut client, _reader, _terminal) =
        AttachClient::connect_with(&socket_path, "detach", test_caps(), test_deadline())
            .expect("attach connects");
    client.detach().expect("send detach");
    // A second detach is a no-op (idempotent), and Drop must not double-send.
    client.detach().expect("idempotent detach");

    let frame = handle.join().expect("host thread");
    assert_eq!(frame, ClientFrame::Detach);
}

#[test]
fn resize_is_forwarded() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            read_client_frame(&mut stream).expect("read client frame")
        });

    let (mut client, _reader, _terminal) =
        AttachClient::connect_with(&socket_path, "resize", test_caps(), test_deadline())
            .expect("attach connects");
    client.resize(80, 24).expect("send resize");

    let frame = handle.join().expect("host thread");
    assert_eq!(
        frame,
        ClientFrame::Resize {
            columns: 80,
            rows: 24
        }
    );
}

#[test]
fn input_is_forwarded() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            read_client_frame(&mut stream).expect("read client frame")
        });

    let (mut client, _reader, _terminal) =
        AttachClient::connect_with(&socket_path, "input", test_caps(), test_deadline())
            .expect("attach connects");
    client.send_input(b"ls\n").expect("send input");

    let frame = handle.join().expect("host thread");
    assert_eq!(frame, ClientFrame::Input(b"ls\n".to_vec()));
}

#[test]
fn empty_input_sends_no_frame() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            // Expect the first real frame to be the resize, never an empty input.
            read_client_frame(&mut stream).expect("read client frame")
        });

    let (mut client, _reader, _terminal) =
        AttachClient::connect_with(&socket_path, "empty", test_caps(), test_deadline())
            .expect("attach connects");
    client.send_input(b"").expect("empty input is a no-op");
    client.resize(10, 5).expect("send resize");

    let frame = handle.join().expect("host thread");
    assert_eq!(
        frame,
        ClientFrame::Resize {
            columns: 10,
            rows: 5
        }
    );
}

#[test]
fn rejected_attach_errors_without_terminal() {
    let dir = unique_runtime_dir();
    let socket_path = dir.join("attach.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind fake host");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let _hello = read_client_hello(&mut stream).expect("read client hello");
        write_host_hello(&mut stream, &HostHello::rejected("busy")).expect("write reject");
    });

    let result = AttachClient::connect_with(&socket_path, "reject", test_caps(), test_deadline());
    assert!(result.is_err(), "rejected attach must surface an error");
    handle.join().expect("host thread");
}

#[test]
fn wedged_host_that_never_accepts_fails_within_the_deadline() {
    // C-2 regression: `UnixStream::connect` succeeds against a host that is bound
    // but never `accept()`s (the listen backlog absorbs the connection), and the
    // client hello lands in the socket buffer. Before the fix, the subsequent
    // `read_host_hello` had no timeout and blocked the calling thread — the MAIN
    // thread, including launch restore — forever. The bounded hello read must now
    // surface an error within the attach deadline instead of hanging.
    let dir = unique_runtime_dir();
    let socket_path = dir.join("attach.sock");
    // Bind but NEVER accept: the listener sits idle for the whole test.
    let _listener = UnixListener::bind(&socket_path).expect("bind wedged host");

    let short_deadline = Duration::from_millis(300);
    let start = Instant::now();
    let result = AttachClient::connect_with(&socket_path, "wedged", test_caps(), short_deadline);
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "attach to a wedged (never-accepting) host must error, not hang"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "attach to a wedged host took {elapsed:?}; the hello read was not bounded"
    );
}

#[test]
fn resize_rejects_zero_dimensions() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |stream| {
            std::thread::sleep(Duration::from_millis(20));
            drop(stream);
        });
    let (mut client, _reader, _terminal) =
        AttachClient::connect_with(&socket_path, "zero", test_caps(), test_deadline())
            .expect("attach connects");
    assert!(client.resize(0, 24).is_err());
    assert!(client.resize(80, 0).is_err());
    handle.join().expect("host thread");
}

// ---------------------------------------------------------------------------
// Live-wiring tests: an attached session is a real `Session`/`WorkspaceSet` source.
// These prove the source generalization routes resize/input/close to the
// socket while the local-PTY path stays byte-identical (the latter is guarded
// by `local_session_resize_routes_to_pty_unchanged` in `session.rs`).
// ---------------------------------------------------------------------------

/// Build an attached [`Session`] (no pump) connected to a fake host at `socket`,
/// for tests that drive resize/input/close directly and read the single frame
/// the host receives.
fn attached_session_over(socket: &std::path::Path, id: &str) -> Session {
    let (client, _reader, terminal) =
        AttachClient::connect_with(socket, id, test_caps(), test_deadline())
            .expect("attach connects");
    let terminal = Arc::new(Mutex::new(terminal));
    let client = Arc::new(Mutex::new(client));
    let writer =
        attach_input_writer(client.clone(), SessionToken(0)).expect("writer thread spawns");
    Session::new_attached(SessionToken(0), terminal, writer, client, id, None)
}

/// Like [`attached_session_over`] but with an explicit token, so several
/// attached sessions can coexist in one [`WorkspaceSet`] without a token collision
/// (the test-only `push` does not mint fresh tokens). Used by the Phase 14
/// dedup / replace tests.
fn attached_session_token(socket: &std::path::Path, id: &str, token: u64) -> Session {
    let (client, _reader, terminal) =
        AttachClient::connect_with(socket, id, test_caps(), test_deadline())
            .expect("attach connects");
    let terminal = Arc::new(Mutex::new(terminal));
    let client = Arc::new(Mutex::new(client));
    let writer =
        attach_input_writer(client.clone(), SessionToken(token)).expect("writer thread spawns");
    Session::new_attached(SessionToken(token), terminal, writer, client, id, None)
}

/// A real local-PTY [`Session`] to seed a [`WorkspaceSet`] for the present-live-tab
/// test (the attached tab is grafted on top of it). Mirrors the session-arena
/// test builder.
fn build_local_session() -> Session {
    let dims = Dimensions::new(20, 8);
    let pty = PtySession::spawn_shell_command(dims, "sleep 1").expect("spawn test shell");
    let writer: PtyWriter = Arc::new(Mutex::new(pty.take_writer().expect("writer")));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(pty));
    Session::new(SessionToken(0), terminal, writer, pty, None)
}

#[test]
fn attached_tab_resize_forwards_to_socket() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            read_client_frame(&mut stream).expect("read client frame")
        });
    let session = attached_session_over(&socket_path, "resize-tab");
    let mut set = WorkspaceSet::new(session, None);
    // 800x400 content, 10x20 cell → 80 cols, 20 rows: routed to a `Resize` frame.
    set.resize_all_panes(PaneRect::new(0.0, 0.0, 800.0, 400.0), 10, 20, 1.0);
    let frame = handle.join().expect("host thread");
    assert_eq!(
        frame,
        ClientFrame::Resize {
            columns: 80,
            rows: 20
        }
    );
}

#[test]
fn attached_input_writer_forwards_to_socket() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            read_client_frame(&mut stream).expect("read client frame")
        });
    let session = attached_session_over(&socket_path, "input-tab");
    // The app-side input path writes to `session.writer`; for an attached session
    // that is the `AttachInputWriter`, so bytes arrive as an `Input` frame — the
    // proof the input path is byte-identical (same `writer`, different sink).
    {
        let mut writer = session.writer.lock().expect("writer lock");
        writer.write_all(b"ls\n").expect("write input");
        writer.flush().expect("flush input");
    }
    let frame = handle.join().expect("host thread");
    assert_eq!(frame, ClientFrame::Input(b"ls\n".to_vec()));
}

#[test]
fn window_close_detaches_attached_tab_host_survives() {
    let (socket_path, handle) =
        spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut stream| {
            read_client_frame(&mut stream).expect("read client frame")
        });
    let session = attached_session_over(&socket_path, "close-tab");
    let mut set = WorkspaceSet::new(session, None);
    // Closing the (only) attached session sends a clean Detach, not a kill — the
    // host keeps the session alive for later reattach.
    let was_last = set.close(SessionToken(0));
    assert!(was_last, "closing the only session reports last");
    let frame = handle.join().expect("host thread");
    assert_eq!(frame, ClientFrame::Detach);
}

// --- Phase 14: attach dedup + new-tab/replace orchestration (WorkspaceSet level) ---

/// Dedup (the reported triple-open fix): an attached session records its host
/// id, `find_attached_tab` locates the open tab, and re-selecting it switches
/// instead of appending a duplicate.
#[test]
fn attach_dedup_finds_open_tab_and_switch_adds_no_tab() {
    let (sock, host) = spawn_fake_host(
        snapshot_bytes(&sample_host_terminal()),
        drain_until_disconnect,
    );
    let mut set = WorkspaceSet::new(build_local_session(), None); // local tab, token 0
    let tok = set.push(attached_session_token(&sock, "s-0001-aaaa", 1)); // + attached
    assert_eq!(set.len(), 2);

    // The host id is recorded, so dedup can locate the open tab; an unrelated id
    // is not found.
    assert_eq!(set.find_attached_tab("s-0001-aaaa"), Some(tok));
    assert_eq!(
        set.find_attached_tab("s-9999-zzzz"),
        None,
        "an id with no open tab is not found"
    );

    // Dedup path: re-selecting an already-open session switches to its tab and
    // adds NO duplicate tab.
    let before = set.len();
    let found = set.find_attached_tab("s-0001-aaaa").expect("already open");
    set.switch(found);
    assert_eq!(
        set.len(),
        before,
        "selecting an already-open session adds no tab"
    );
    assert_eq!(set.active_id(), tok, "and focuses the existing tab");

    set.close(tok); // clean Detach so the fake host EOFs
    join_within(host, "dedup fake host");
}

/// Cross-workspace dedup (ODP-10): an attached session living in a BACKGROUND
/// workspace is still located by `find_attached_tab` (the arena scan spans every
/// workspace), and re-selecting it deep-switches the active workspace + tab +
/// pane focus rather than appending a duplicate.
#[test]
fn attach_dedup_deep_switches_across_workspaces() {
    let (sock, host) = spawn_fake_host(
        snapshot_bytes(&sample_host_terminal()),
        drain_until_disconnect,
    );
    // ws0: the local current tab (token 0). ws1: an attached session (token 1).
    let mut set = WorkspaceSet::new(build_local_session(), None);
    let attached = set.push_workspace(attached_session_token(&sock, "s-0002-bbbb", 1));
    assert_eq!(set.workspace_count(), 2);
    // ws0 stays active (push_workspace never switches).
    assert_eq!(set.active_workspace_index(), 0);
    assert_eq!(set.len(), 2);

    // The arena scan finds the attached session even though it lives in a
    // background workspace.
    assert_eq!(set.find_attached_tab("s-0002-bbbb"), Some(attached));

    // Re-selecting it deep-switches to ws1 and adds no duplicate tab.
    let before = set.len();
    let found = set.find_attached_tab("s-0002-bbbb").expect("already open");
    assert!(set.switch(found));
    assert_eq!(set.len(), before, "no duplicate tab");
    assert_eq!(set.active_workspace_index(), 1, "deep-switched to ws1");
    assert_eq!(set.active_id(), attached, "and focuses the attached pane");

    set.close(attached); // clean Detach so the fake host EOFs
    join_within(host, "cross-workspace dedup fake host");
}

/// "Replace current" over a LOCAL current tab: attach appends exactly one tab,
/// then the previously-active local tab is closed directly (its PTY reaped),
/// netting one tab with the attached session focused.
#[test]
fn attach_replace_closes_local_current_and_focuses_attached() {
    let (sock, host) = spawn_fake_host(
        snapshot_bytes(&sample_host_terminal()),
        drain_until_disconnect,
    );
    let mut set = WorkspaceSet::new(build_local_session(), None); // local current, token 0
    let old_active = set.active_id();
    assert_eq!(old_active, SessionToken(0));

    // Mirror App::attach_session_replacing_current: capture old active → attach
    // new (appends + focus) → close the old tab.
    let attached = set.push(attached_session_token(&sock, "s-0003-cccc", 1));
    assert_eq!(set.len(), 2, "attach adds exactly one tab");
    set.switch(attached);
    let old_idx = set.position_of_token(old_active).expect("old tab present");
    let _ = set.close_tab_at(old_idx);

    assert_eq!(set.len(), 1, "replace nets exactly one tab");
    assert_eq!(set.active_id(), attached, "the attached tab stays focused");
    assert_eq!(set.find_attached_tab("s-0003-cccc"), Some(attached));

    set.close(attached);
    join_within(host, "replace-local fake host");
}

/// "Replace current" over a HOSTED/attached current tab: the replaced tab is
/// closed via the same path, which sends a clean `Detach` (the host keeps the
/// PTY alive, so the replaced session stays reattachable) rather than a kill.
#[test]
fn attach_replace_detaches_hosted_current_host_survives() {
    let (sock_cur, host_cur) = spawn_fake_host(snapshot_bytes(&sample_host_terminal()), |mut s| {
        read_client_frame(&mut s).expect("read client frame")
    });
    let (sock_new, host_new) = spawn_fake_host(
        snapshot_bytes(&sample_host_terminal()),
        drain_until_disconnect,
    );

    // The CURRENT tab is itself a hosted/attached session (token 0).
    let current = attached_session_token(&sock_cur, "s-cur-0001", 0);
    let mut set = WorkspaceSet::new(current, None);
    let old_active = set.active_id();

    let new_tok = set.push(attached_session_token(&sock_new, "s-new-0002", 1));
    set.switch(new_tok);
    let old_idx = set.position_of_token(old_active).expect("old tab present");
    let _ = set.close_tab_at(old_idx);

    assert_eq!(set.len(), 1, "replace nets one tab");
    assert_eq!(set.active_id(), new_tok, "the new attached tab is focused");
    // Replacing a hosted current sends a clean Detach — host survives.
    let frame = join_within(host_cur, "replaced hosted current");
    assert_eq!(
        frame,
        ClientFrame::Detach,
        "hosted current detaches cleanly, host survives"
    );

    set.close(new_tok);
    join_within(host_new, "replace-hosted new host");
}

// --- Packet 2: Detach & switch orchestration (WorkspaceSet level) ---
//
// The App reads the focused pane's cwd, spawns a FRESH managed session in it,
// attaches + focuses it, then (Swap only) closes the ORIGINAL focused pane via
// the existing close path. These tests stand the spawned managed session in
// with an attached session over a fake host (the spawn itself is exercised at
// the App level via the spawn-failure seam; here the focus is the close/keep
// orchestration shape). The capture-original → attach → close-original ordering
// mirrors Phase 14 Replace, but the close is PANE-scoped (`close`), not
// whole-tab.

/// "Swap" over a single-pane original: after the managed session is attached and
/// focused, closing the original focused pane removes its (single-pane) tab,
/// netting exactly one tab with the managed session focused.
#[test]
fn detach_switch_swap_closes_single_pane_original_and_focuses_managed() {
    let (sock, host) = spawn_fake_host(
        snapshot_bytes(&sample_host_terminal()),
        drain_until_disconnect,
    );
    let mut set = WorkspaceSet::new(build_local_session(), None); // original, token 0
    let original = set.active_id();
    assert_eq!(original, SessionToken(0));

    // Capture original → attach managed (appends + focus) → close original pane.
    let managed = set.push(attached_session_token(&sock, "s-0001-aaaa", 1));
    assert_eq!(set.len(), 2, "attach adds exactly one tab");
    set.switch(managed);
    let was_last = set.close(original);
    assert!(!was_last, "the managed tab keeps the set non-empty");

    assert_eq!(set.len(), 1, "swap nets exactly one tab");
    assert_eq!(
        set.active_id(),
        managed,
        "the managed session stays focused"
    );

    set.close(managed);
    join_within(host, "detach-switch swap fake host");
}

/// "Keep both": after the managed session is attached and focused, the original
/// pane is left untouched — two tabs, managed focused, original still present.
#[test]
fn detach_switch_keep_both_leaves_original_and_adds_one() {
    let (sock, host) = spawn_fake_host(
        snapshot_bytes(&sample_host_terminal()),
        drain_until_disconnect,
    );
    let mut set = WorkspaceSet::new(build_local_session(), None); // original, token 0
    let original = set.active_id();

    let managed = set.push(attached_session_token(&sock, "s-0002-bbbb", 1));
    set.switch(managed);
    // Keep both: no close.

    assert_eq!(set.len(), 2, "keep both adds exactly one tab");
    assert_eq!(set.active_id(), managed, "the managed session is focused");
    assert!(
        set.position_of_token(original).is_some(),
        "the original pane is left untouched"
    );

    set.close(managed);
    set.close(original);
    join_within(host, "detach-switch keep-both fake host");
}

#[test]
fn attach_by_id_presents_live_tab_and_repaints() {
    // Lay out the runtime tree the id resolver expects:
    // <base>/<runtime-dir>/<id>.sock, all 0700-owned by this uid.
    let base = unique_runtime_dir();
    let runtime_dir = runtime_dir_path(&base);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700 runtime dir");
    let id = "live-tab";
    let socket_path = session_socket_path(&runtime_dir, id).expect("socket path");
    let listener = UnixListener::bind(&socket_path).expect("bind fake host");

    let snapshot = snapshot_bytes(&sample_host_terminal());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let _hello = read_client_hello(&mut stream).expect("read client hello");
        write_host_hello(&mut stream, &HostHello::accepted()).expect("write host hello");
        write_host_frame(&mut stream, &HostFrame::Snapshot(snapshot)).expect("write snapshot");
        // Live output after the snapshot, then hold the link open until the
        // client detaches — ACK-based teardown, never an abrupt drop that could
        // EOF the pump mid-frame on a slow runner.
        write_host_frame(&mut stream, &HostFrame::Output(b"XYZ".to_vec())).expect("write output");
        drain_until_disconnect(stream);
    });

    let mut set = WorkspaceSet::new(build_local_session(), None);
    let (tx, rx) = mpsc::channel();
    let token = set
        .attach_in_new_tab_for_test(Some(&base), id, ChannelSink(tx))
        .expect("attach-by-id presents a tab");

    // A second (attached) session/tab now exists.
    assert_eq!(set.len(), 2, "attach grafts a new tab");
    // Full scrollback restored from the host snapshot at connect time.
    {
        let term = set
            .get(token)
            .expect("attached session")
            .terminal
            .lock()
            .unwrap();
        // The snapshot is restored synchronously at `connect` time, but the live
        // `Output("XYZ")` frame is applied by the async pump thread — so by the
        // time we read here the pump may already have appended it. Row 0 is never
        // touched by the append, so it stays an exact-equality proof of snapshot
        // restore. Row 2 is the prompt line the live output appends to: assert
        // the restored PREFIX ("PROMPT$"), which holds both before and after the
        // append, to prove the prompt line was restored without racing the pump.
        // The exact appended form ("PROMPT$ XYZ") is asserted by the poll below.
        assert_eq!(row_text(&term, 0), "line-one");
        assert!(
            row_text(&term, 2).starts_with("PROMPT$"),
            "snapshot must restore the prompt line (row 2 = {:?})",
            row_text(&term, 2)
        );
    }

    // Live output repaints the mirror. Wait for the pump's redraw event AND
    // poll the mirror until the appended bytes actually land — a redraw can be
    // observed independently of the lock-ordering of the apply, so the content
    // poll is the deterministic gate (not a single assert after the first
    // event). Both together prove the pump signaled a redraw and the output was
    // applied.
    let saw_redraw = wait_until(|| {
        while let Ok(ev) = rx.try_recv() {
            if ev == Ev::Redraw(token) {
                return true;
            }
        }
        false
    });
    assert!(saw_redraw, "live output must repaint the attached tab");
    let applied = wait_until(|| {
        let term = set
            .get(token)
            .expect("attached session")
            .terminal
            .lock()
            .unwrap();
        row_text(&term, 2) == "PROMPT$ XYZ"
    });
    assert!(
        applied,
        "live output must append to the mirror at the cursor"
    );

    // Closing the attached tab cleanly detaches and reaps its pump; the clean
    // Detach also unblocks the host's `drain_until_disconnect`.
    assert!(!set.close(token), "two tabs: closing one is not last");
    handle.join().expect("host thread");
}

/// Regression: audit P1 — a mid-frame read timeout must not desync the snapshot
/// poll loop. `read_initial_snapshot` polls with `SNAPSHOT_POLL` (50ms) and
/// retries on `WouldBlock`; with a stateless `read_exact`-based reader, a frame
/// whose payload arrives split with an inter-arrival gap longer than the poll
/// timeout loses the bytes consumed before the timeout, and the retry parses
/// leftover payload as a fresh frame header (permanent desync). The writer here
/// stalls 150ms mid-payload on an Output frame and again mid-payload on the
/// Snapshot frame; the reader must resume both frames and return the intact
/// snapshot bytes.
#[test]
fn initial_snapshot_survives_mid_frame_stall() {
    fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(5 + payload.len());
        bytes.push(kind);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    let (mut writer_end, mut reader_end) = UnixStream::pair().expect("socketpair");
    let snapshot_payload: Vec<u8> = (0..64u8).cycle().take(4096).collect();
    let expected = snapshot_payload.clone();

    let writer = std::thread::spawn(move || {
        use std::io::Write;
        // Output frame (tolerated + ignored before the snapshot), split
        // mid-payload with a stall longer than SNAPSHOT_POLL.
        let output = encode_frame(2, b"FIRSThALF!");
        writer_end.write_all(&output[..10]).expect("output half 1");
        writer_end.flush().expect("flush");
        std::thread::sleep(Duration::from_millis(150));
        writer_end.write_all(&output[10..]).expect("output half 2");
        // Snapshot frame, also split mid-payload with a stall.
        let snapshot = encode_frame(1, &snapshot_payload);
        let split = 5 + snapshot_payload.len() / 2;
        writer_end
            .write_all(&snapshot[..split])
            .expect("snapshot half 1");
        writer_end.flush().expect("flush");
        std::thread::sleep(Duration::from_millis(150));
        writer_end
            .write_all(&snapshot[split..])
            .expect("snapshot half 2");
        writer_end.flush().expect("flush");
        // Keep the write end open until the reader is done; dropping early
        // could race an EOF into the poll loop.
        std::thread::sleep(Duration::from_millis(500));
    });

    let result = read_initial_snapshot(&mut reader_end, Duration::from_secs(5));
    writer.join().expect("writer thread");
    let bytes = result.expect("snapshot must survive mid-frame stalls without desync");
    assert_eq!(bytes, expected, "snapshot payload must arrive intact");
}
