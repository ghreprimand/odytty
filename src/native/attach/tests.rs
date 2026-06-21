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
use std::time::Duration;

use super::*;
use crate::core::{SnapshotCaptureLimits, Terminal};
use crate::session_host::protocol::{
    ClientFrame, HostFrame, HostHello, read_client_frame, read_client_hello, write_host_frame,
    write_host_hello,
};

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
