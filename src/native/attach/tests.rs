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
use crate::native::session::{Session, TabSet};
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
    let dir = std::env::temp_dir().join(format!(
        "odytty_attach_{}_{}",
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
    run_attach_pump(reader, terminal.clone(), ChannelSink(tx), token);

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
    handle.join().expect("host thread");
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
    // Pump must return (not hang) after SessionExit.
    run_attach_pump(
        reader,
        Arc::new(Mutex::new(terminal)),
        ChannelSink(tx),
        token,
    );

    let events: Vec<Ev> = rx.try_iter().collect();
    assert_eq!(events, vec![Ev::Exited(token)]);
    handle.join().expect("host thread");
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
// Live-wiring tests: an attached session is a real `Session`/`TabSet` source.
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
    let writer = attach_input_writer(client.clone());
    Session::new_attached(SessionToken(0), terminal, writer, client, None)
}

/// A real local-PTY [`Session`] to seed a [`TabSet`] for the present-live-tab
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
    let mut set = TabSet::new(session, None);
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
    let mut set = TabSet::new(session, None);
    // Closing the (only) attached session sends a clean Detach, not a kill — the
    // host keeps the session alive for later reattach.
    let was_last = set.close(SessionToken(0));
    assert!(was_last, "closing the only session reports last");
    let frame = handle.join().expect("host thread");
    assert_eq!(frame, ClientFrame::Detach);
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

    let mut set = TabSet::new(build_local_session(), None);
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
        assert_eq!(row_text(&term, 0), "line-one");
        assert_eq!(row_text(&term, 2), "PROMPT$");
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
