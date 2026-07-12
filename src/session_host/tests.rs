// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::{
    Dimensions, SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal,
};

use super::protocol::{
    ClientHello, HOST_PROTOCOL_VERSION, HostFrame, HostHello, ProtocolError, read_client_hello,
    read_host_hello, versions_compatible, write_client_hello, write_host_hello,
};
use super::{
    HostCommand, HostConfig, HostExitReason, SessionHostClient, cleanup_stale_socket, kill_session,
    prepare_runtime_dir, run_host, session_socket_path, validate_runtime_dir,
};

// Frame-read / output-wait budget. Generous so a cold, heavily-parallel CI
// runner (especially macOS) that is slow to schedule the host thread and pump
// frames does not lose the race. Polling stays at a fine interval, so a healthy
// host still completes near-instantly — this only raises the failure ceiling.
const SHORT_WAIT: Duration = Duration::from_secs(15);

// Socket-bind readiness budget. The host thread must spawn its PTY child and
// bind the per-user socket before the first connect; on a cold macOS CI runner
// that cold-start can take several seconds under load. Separated from
// `SHORT_WAIT` and set very generously so `wait_for_socket` never panics with
// "socket did not become ready" on a slow runner.
const SOCKET_READY_WAIT: Duration = Duration::from_secs(30);

// Upper bound on awaiting a spawned helper thread (the session-host) to finish.
// The host is expected to exit in well under a second once the test drives it to
// its terminal condition (child exit or detached idle timeout). This is purely a
// safety net: if `run_host` ever fails to return — the intermittent macOS
// non-exit we are hardening against — the test FAILS FAST with a clear message
// instead of hanging the whole suite (and CI) indefinitely.
const JOIN_DEADLINE: Duration = Duration::from_secs(30);

/// Join a spawned thread with a hard deadline. Returns the thread's value, or
/// panics with a clear message if it does not finish in `JOIN_DEADLINE`
/// (re-raising the thread's own panic if it panicked). A timed-out helper thread
/// is intentionally left detached: the point is to turn a silent infinite hang
/// into a loud, attributable test failure.
fn join_within<T: Send + 'static>(handle: thread::JoinHandle<T>, what: &str) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    match rx.recv_timeout(JOIN_DEADLINE) {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!(
            "{what} did not finish within {JOIN_DEADLINE:?} — the session-host \
             loop is wedged (run_host never returned). Failing fast instead of \
             hanging the suite."
        ),
    }
}

#[test]
fn protocol_handshake_round_trip_accepts_current_versions() {
    let (mut client, mut server) = UnixStream::pair().expect("socket pair");
    let server_thread = thread::spawn(move || {
        let hello = read_client_hello(&mut server).expect("read client hello");
        assert!(versions_compatible(&hello));
        write_host_hello(&mut server, &HostHello::accepted()).expect("write host hello");
    });

    write_client_hello(&mut client, &ClientHello::current("demo")).expect("write client hello");
    let hello = read_host_hello(&mut client)
        .expect("read host hello")
        .into_result()
        .expect("accepted");

    assert_eq!(hello.host_protocol_version, HOST_PROTOCOL_VERSION);
    server_thread.join().expect("server thread");
}

#[test]
fn protocol_handshake_rejects_version_mismatch() {
    let (mut client, mut server) = UnixStream::pair().expect("socket pair");
    let server_thread = thread::spawn(move || {
        let mut hello = read_client_hello(&mut server).expect("read client hello");
        hello.host_protocol_version += 1;
        assert!(!versions_compatible(&hello));
        write_host_hello(&mut server, &HostHello::rejected("version mismatch"))
            .expect("write rejection");
    });

    let mut hello = ClientHello::current("demo");
    hello.host_protocol_version += 1;
    write_client_hello(&mut client, &hello).expect("write client hello");
    let error = read_host_hello(&mut client)
        .expect("read host hello")
        .into_result()
        .unwrap_err();

    assert!(matches!(error, ProtocolError::Rejected(message) if message == "version mismatch"));
    server_thread.join().expect("server thread");
}

#[test]
fn connect_to_a_wedged_host_that_never_accepts_fails_within_the_deadline() {
    // C-2 regression: `UnixStream::connect` succeeds against a bound host that
    // never `accept()`s (the listen backlog absorbs the connection); the client
    // hello lands in the socket buffer. Before the fix, `read_host_hello` had no
    // timeout and blocked the caller forever. The bounded hello read must now
    // surface an error within the deadline.
    let temp = TempDir::new("sh-wedged");
    let runtime_dir = prepare_runtime_dir(temp.path()).expect("prepare runtime dir");
    let socket_path = session_socket_path(&runtime_dir, "wedged").expect("socket path");
    // Bind but NEVER accept: the listener sits idle for the whole test.
    let _listener = UnixListener::bind(&socket_path).expect("bind wedged host");

    let start = Instant::now();
    let result = SessionHostClient::connect_within_deadline(
        &socket_path,
        "wedged",
        Duration::from_millis(300),
    );
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "connect to a wedged (never-accepting) host must error, not hang"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "connect to a wedged host took {elapsed:?}; the hello read was not bounded"
    );
}

#[test]
fn runtime_dir_validation_requires_owner_private_mode() {
    let temp = TempDir::new("sh-perm");
    let runtime_dir = prepare_runtime_dir(temp.path()).expect("prepare runtime dir");
    validate_runtime_dir(&runtime_dir).expect("runtime dir valid");

    fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o755))
        .expect("loosen runtime dir");
    let error = validate_runtime_dir(&runtime_dir).unwrap_err();
    assert!(
        error.to_string().contains("expected 700"),
        "unexpected error: {error}"
    );
}

#[test]
fn stale_socket_cleanup_keeps_live_peer_and_removes_dead_socket() {
    let temp = TempDir::new("sh-stale");
    let runtime_dir = prepare_runtime_dir(temp.path()).expect("prepare runtime dir");
    let socket_path = session_socket_path(&runtime_dir, "demo").expect("socket path");

    let listener = UnixListener::bind(&socket_path).expect("bind live socket");
    let error = cleanup_stale_socket(&socket_path).unwrap_err();
    assert!(
        error.to_string().contains("live peer"),
        "unexpected error: {error}"
    );
    drop(listener);

    // Dropping the listener closes it, but under heavy parallel scheduling the
    // kernel's teardown of the listening socket can lag a beat -- a `connect()`
    // probe may still briefly succeed against the soon-to-be-dead inode, which
    // `cleanup_stale_socket` correctly reports as a live peer. The path is
    // per-test unique, so nothing else can be binding it; this races only the
    // kernel, not another test. Wait until the socket genuinely refuses
    // connections before asserting the stale-cleanup path removes it. Pure test
    // timing -- the production cleanup logic is exercised unchanged.
    wait_until_socket_refuses(&socket_path);

    cleanup_stale_socket(&socket_path).expect("remove stale socket");
    assert!(!socket_path.exists());
}

/// Poll until a `connect()` to `socket_path` fails, i.e. no listener answers.
/// Used after dropping a test listener to wait out the kernel's asynchronous
/// listening-socket teardown before probing for a stale socket.
fn wait_until_socket_refuses(socket_path: &Path) {
    let deadline = Instant::now() + SHORT_WAIT;
    loop {
        match UnixStream::connect(socket_path) {
            // Still connectable: the listener teardown is not yet visible.
            Ok(_) => {}
            // Refused / not found: the socket is now stale, safe to clean up.
            Err(_) => return,
        }
        if Instant::now() >= deadline {
            panic!(
                "socket {} still answers connections {SHORT_WAIT:?} after listener drop",
                socket_path.display()
            );
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn host_detach_keeps_session_alive_then_idle_timeout_exits() {
    let temp = TempDir::new("sh-life");
    // The child blocks on `read` until this client (once attached) sends a byte,
    // then prints its marker. This makes the ordering attach-THEN-output
    // deterministic on every platform: without it, a fast scheduler (notably
    // macOS) can run `printf ready` before the attach completes, so "ready" lands
    // in the attach SNAPSHOT and never arrives as a live Output frame, and
    // `wait_for_output` waits forever. Mirrors the input-driven child the
    // real-process e2e test already relies on.
    let config = host_config(
        temp.path(),
        "keepalive",
        "read x; printf ready; sleep 2",
        Duration::from_millis(500),
    );
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    let mut client = SessionHostClient::connect(&socket_path, "keepalive").expect("attach");
    let snapshot = expect_snapshot(&mut client);
    let decoded =
        SnapshotEnvelope::decode(&snapshot, SnapshotEnvelopeCaps::default()).expect("snapshot");
    assert_eq!(decoded.terminal.dimensions, Dimensions::new(80, 24));
    client
        .send_input(b"\n")
        .expect("trigger child output after attach");
    wait_for_output(&mut client, "ready");
    client.detach().expect("detach");
    drop(client);

    thread::sleep(Duration::from_millis(100));
    let mut reattached = SessionHostClient::connect(&socket_path, "keepalive").expect("reattach");
    let snapshot = expect_snapshot(&mut reattached);
    let decoded =
        SnapshotEnvelope::decode(&snapshot, SnapshotEnvelopeCaps::default()).expect("snapshot");
    let restored = Terminal::from_snapshot_envelope(&decoded).expect("restore");
    assert!(restored.screen().plain_text().contains("ready"));
    reattached.detach().expect("detach reattached");
    drop(reattached);

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::DetachedIdleTimeout);
}

#[test]
fn host_reattach_replays_output_produced_while_detached() {
    let temp = TempDir::new("sh-reat");
    // `read x` gates the first marker on this client's post-attach input so
    // "before" deterministically arrives as a live Output frame (not folded into
    // the attach snapshot by a fast macOS scheduler). "after" is still produced
    // while detached and is asserted via the reattach snapshot below.
    let config = host_config(
        temp.path(),
        "reattach",
        "read x; printf before; sleep 0.2; printf after; sleep 2",
        Duration::from_millis(800),
    );
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    let mut client = SessionHostClient::connect(&socket_path, "reattach").expect("attach");
    expect_snapshot(&mut client);
    client
        .send_input(b"\n")
        .expect("trigger child output after attach");
    wait_for_output(&mut client, "before");
    client.detach().expect("detach");
    drop(client);

    // Poll the reattach snapshot until the detached-produced "after" lands,
    // instead of betting a fixed sleep is long enough (flaky on loaded macOS CI).
    // "after" implies "before" already replayed, but assert both for clarity.
    let snapshot = reattach_snapshot_with_text(&socket_path, "reattach", "after");
    let text = snapshot_plain_text(&snapshot);
    assert!(
        text.contains("before") && text.contains("after"),
        "reattach snapshot did not replay detached output: {text:?}"
    );

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::DetachedIdleTimeout);
}

#[test]
fn host_exits_when_child_exits_even_with_client_attached() {
    let temp = TempDir::new("sh-exit");
    // `read x` keeps the child alive until this client is attached and sends a
    // byte; only then does it print "done" and exit 7. Without this gate the
    // child can `printf done; exit 7` and the host can reap it + tear down BEFORE
    // the attach completes (observed deterministically on macOS: the connect then
    // reads EOF mid-handshake), and even when the connect wins the race "done"
    // may already be in the snapshot rather than a live Output frame. Gating on
    // input makes "attach, observe live output, observe SessionExit" reliable on
    // every platform while testing exactly the same propagation path.
    let config = host_config(
        temp.path(),
        "exits",
        "read x; printf done; exit 7",
        Duration::from_secs(10),
    );
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    let mut client = SessionHostClient::connect(&socket_path, "exits").expect("attach");
    expect_snapshot(&mut client);
    client
        .send_input(b"\n")
        .expect("trigger child output after attach");
    wait_for_output(&mut client, "done");
    expect_session_exit(&mut client, Some(7));

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::SessionExited);
    assert_eq!(exit.exit_code, Some(7));
    assert!(!socket_path.exists(), "host socket must be removed on exit");
}

#[test]
fn host_shutdown_frame_terminates_session_and_cleans_up_socket() {
    let temp = TempDir::new("sh-kill");
    // A long-lived child + long idle timeout: nothing but the Shutdown frame can
    // end this host within the test window, so the assertion is unambiguous.
    let config = host_config(temp.path(), "killme", "sleep 30", Duration::from_secs(3600));
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    let mut client = SessionHostClient::connect(&socket_path, "killme").expect("attach");
    expect_snapshot(&mut client);
    client.shutdown().expect("send shutdown frame");
    drop(client);

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::Killed);
    assert!(
        !socket_path.exists(),
        "host socket must be removed after a kill"
    );
}

#[test]
fn host_survives_hostile_resize_dimensions() {
    let temp = TempDir::new("sh-rsz");
    // The child blocks on `read` until this client sends a byte, then prints
    // its marker — proving the host is still serving traffic AFTER the hostile
    // resize frame has been drained.
    let config = host_config(
        temp.path(),
        "resize",
        "read x; printf ready; sleep 2",
        Duration::from_millis(500),
    );
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    let mut client = SessionHostClient::connect(&socket_path, "resize").expect("attach");
    expect_snapshot(&mut client);
    // Hostile/buggy client: columns=rows=0xFFFFFFFF. The wire protocol carries
    // raw u32 dimensions; unclamped, the host drove `Terminal::resize` into a
    // ~1.8e19-cell grid allocation (capacity-overflow panic / OOM), killing the
    // session for every attached client. The host must clamp and carry on.
    client
        .resize(u32::MAX, u32::MAX)
        .expect("send hostile resize frame");
    client
        .send_input(b"\n")
        .expect("input after hostile resize");
    wait_for_output(&mut client, "ready");
    client.shutdown().expect("send shutdown frame");
    drop(client);

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::Killed);
}

#[test]
fn metadata_with_unknown_version_is_ignored() {
    // audit C27: `read_session_metadata` skipped the `version=` line entirely,
    // so a future-format (or corrupted-version) file would be parsed with v1
    // semantics instead of being treated as unreadable. An unknown version must
    // read as `None` (same graceful fallback as a missing file).
    let temp = TempDir::new("sh-c27");
    let runtime_dir = prepare_runtime_dir(temp.path()).expect("prepare runtime dir");
    super::write_session_metadata(
        &runtime_dir,
        &super::SessionMetadata {
            id: "v".to_owned(),
            name: "Versioned".to_owned(),
            created_unix_ms: 42,
            pane_count: 2,
        },
    )
    .expect("write metadata");
    // Sanity: the v1 file reads back.
    assert!(
        super::read_session_metadata(&runtime_dir, "v")
            .expect("read v1")
            .is_some()
    );

    let path = super::session_metadata_path(&runtime_dir, "v").expect("metadata path");
    let future = fs::read_to_string(&path)
        .expect("read metadata file")
        .replace("version=1", "version=2");
    fs::write(&path, future).expect("rewrite metadata file");
    assert!(
        super::read_session_metadata(&runtime_dir, "v")
            .expect("read v2")
            .is_none(),
        "future-version metadata must not be parsed with v1 semantics"
    );
}

#[test]
fn spawn_socket_timeout_reaps_the_spawned_child() {
    // audit C26: when the freshly spawned host child never binds its socket
    // within the startup timeout, `spawn_host_on_demand` returned the error and
    // DROPPED the `Child` — leaking a live orphan host process (or a zombie if
    // it exited) with no owner ever calling kill/wait. The timeout path must
    // kill and reap the child before surfacing the error.
    let temp = TempDir::new("sh-c26");
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn stand-in child");
    let missing_socket = temp.path().join("never-bound.sock");
    let error =
        super::host::await_host_socket(&mut child, &missing_socket, Duration::from_millis(100))
            .expect_err("socket never appears");
    assert!(
        error.to_string().contains("never-bound.sock"),
        "error should name the socket: {error:#}"
    );
    let status = child
        .try_wait()
        .expect("query stand-in child after timeout");
    assert!(
        status.is_some(),
        "timed-out spawn must kill and reap the child, not leak it"
    );
}

#[test]
fn host_survives_client_that_vanishes_mid_handshake() {
    // audit C6: a per-connection failure during the attach handshake must
    // log-and-drop THAT connection, not tear down the whole host. The classic
    // trigger: a client that connects, sends a valid hello, and dies before
    // reading the reply — the host's hello/snapshot writes then fail with
    // BrokenPipe, which previously propagated out of accept_pending_clients and
    // killed the session for everyone.
    let temp = TempDir::new("sh-c6");
    let config = host_config(
        temp.path(),
        "c6",
        "read x; printf ready; sleep 2",
        Duration::from_millis(800),
    );
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    // The dying client: hello, then close both halves without reading the
    // reply. Connect+write+drop completes long before the host's 10ms accept
    // poll picks the connection up, so by the time the host answers the peer
    // is definitively closed.
    {
        let mut bad = UnixStream::connect(&socket_path).expect("connect dying client");
        write_client_hello(&mut bad, &ClientHello::current("c6")).expect("write hello");
    }

    // A real client must still be able to attach and drive the session.
    let mut client =
        SessionHostClient::connect(&socket_path, "c6").expect("attach after dying client");
    expect_snapshot(&mut client);
    client.send_input(b"\n").expect("input after bad handshake");
    wait_for_output(&mut client, "ready");
    client.shutdown().expect("send shutdown frame");
    drop(client);

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::Killed);
}

#[test]
fn kill_session_terminates_a_live_host() {
    let temp = TempDir::new("sh-rk");
    let config = host_config(temp.path(), "rk", "sleep 30", Duration::from_secs(3600));
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    kill_session(Some(temp.path()), "rk").expect("kill_session");

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::Killed);
    assert!(
        !socket_path.exists(),
        "host socket must be removed after kill_session"
    );
}

#[test]
fn kill_session_on_missing_socket_is_ok() {
    let temp = TempDir::new("sh-reg-gone");
    // The runtime dir exists (owner-private) but no host is bound for this id, so
    // the connect fails and kill_session treats it as already-gone → Ok.
    prepare_runtime_dir(temp.path()).expect("prepare runtime dir");
    kill_session(Some(temp.path()), "never-existed").expect("kill of absent session is Ok");
}

#[test]
fn host_enforces_configured_scrollback_bound_on_reattach() {
    let temp = TempDir::new("sh-scrl");
    let mut config = host_config_with_sh(
        temp.path(),
        "bounded",
        "i=0; while [ \"$i\" -lt 24 ]; do printf 'line%02d\\n' \"$i\"; i=$((i + 1)); done; sleep 2",
        Duration::from_millis(500),
    );
    config.dimensions = Dimensions::new(8, 3);
    config.snapshot_limits = SnapshotCaptureLimits {
        max_scrollback_rows: 4,
    };
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    let mut client = SessionHostClient::connect(&socket_path, "bounded").expect("attach");
    expect_snapshot(&mut client);
    client.detach().expect("detach");
    drop(client);

    let snapshot = reattach_snapshot_with_text(&socket_path, "bounded", "line");
    let decoded =
        SnapshotEnvelope::decode(&snapshot, SnapshotEnvelopeCaps::default()).expect("snapshot");
    assert!(
        decoded.terminal.scrollback_rows.len() <= 4,
        "snapshot scrollback exceeded host cap: {}",
        decoded.terminal.scrollback_rows.len()
    );
    let restored = Terminal::from_snapshot_envelope(&decoded).expect("restore");
    assert!(
        restored.screen().scrollback_len() <= 4,
        "restored scrollback exceeded host cap: {}",
        restored.screen().scrollback_len()
    );

    let exit = join_within(host, "session-host thread").expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::DetachedIdleTimeout);
}

fn host_config(
    runtime_base: &Path,
    session_id: &str,
    command: &str,
    idle_timeout: Duration,
) -> HostConfig {
    let mut config = HostConfig::new(session_id);
    config.runtime_base = Some(runtime_base.to_owned());
    config.command = HostCommand::ShellCommand(command.to_owned());
    config.detached_idle_timeout = idle_timeout;
    config.dimensions = Dimensions::new(80, 24);
    config
}

fn host_config_with_sh(
    runtime_base: &Path,
    session_id: &str,
    command: &str,
    idle_timeout: Duration,
) -> HostConfig {
    let mut config = host_config(runtime_base, session_id, ":", idle_timeout);
    config.command = HostCommand::Exec {
        program: "/bin/sh".into(),
        args: vec!["-lc".into(), command.into()],
        working_directory: None,
    };
    config
}

fn expect_snapshot(client: &mut SessionHostClient) -> Vec<u8> {
    let deadline = Instant::now() + SHORT_WAIT;
    loop {
        match client
            .read_frame(Duration::from_millis(50))
            .expect("read frame")
        {
            Some(HostFrame::Snapshot(bytes)) => return bytes,
            Some(_) | None if Instant::now() < deadline => {}
            other => panic!("missing snapshot, got {other:?}"),
        }
    }
}

fn wait_for_output(client: &mut SessionHostClient, needle: &str) {
    let deadline = Instant::now() + SHORT_WAIT;
    let mut bytes = Vec::new();
    while Instant::now() < deadline {
        if let Some(HostFrame::Output(chunk)) = client
            .read_frame(Duration::from_millis(50))
            .expect("read frame")
        {
            bytes.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return;
            }
        }
    }
    panic!(
        "timed out waiting for {needle:?}; output={}",
        String::from_utf8_lossy(&bytes)
    );
}

fn expect_session_exit(client: &mut SessionHostClient, expected_code: Option<i32>) {
    let deadline = Instant::now() + SHORT_WAIT;
    loop {
        match client
            .read_frame(Duration::from_millis(50))
            .expect("read frame")
        {
            Some(HostFrame::SessionExit { exit_code }) => {
                assert_eq!(exit_code, expected_code);
                return;
            }
            Some(_) | None if Instant::now() < deadline => {}
            other => panic!("missing session exit, got {other:?}"),
        }
    }
}

fn snapshot_plain_text(snapshot: &[u8]) -> String {
    let decoded =
        SnapshotEnvelope::decode(snapshot, SnapshotEnvelopeCaps::default()).expect("snapshot");
    Terminal::from_snapshot_envelope(&decoded)
        .expect("restore")
        .screen()
        .plain_text()
}

fn reattach_snapshot_with_text(socket_path: &Path, session_id: &str, needle: &str) -> Vec<u8> {
    let deadline = Instant::now() + SHORT_WAIT;
    loop {
        let mut client = SessionHostClient::connect(socket_path, session_id).expect("reattach");
        let snapshot = expect_snapshot(&mut client);
        let text = snapshot_plain_text(&snapshot);
        client.detach().expect("detach reattached");
        drop(client);
        if text.contains(needle) {
            return snapshot;
        }
        if Instant::now() >= deadline {
            panic!("reattach snapshot never contained {needle:?}; last text={text:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_socket(socket_path: &Path) {
    let deadline = Instant::now() + SOCKET_READY_WAIT;
    loop {
        match UnixStream::connect(socket_path) {
            Ok(_) => return,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("socket did not become ready: {error}"),
        }
    }
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        // Per-test isolation: pid + a process-global monotonic counter guarantee a
        // unique directory across processes (pid) and within one (the counter is
        // the deciding factor). We deliberately keep this name SHORT and omit a
        // nanos timestamp: the host appends `<base>/odytty/session-<id>.sock`, and
        // on macOS the temp base is a long `/var/folders/.../T/` path, so an
        // `AF_UNIX` socket under a verbose, timestamped base overflows the 104-byte
        // `sun_path` limit and `bind()` fails. Keep `prefix` to a short tag.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{seq}", std::process::id()));
        fs::create_dir(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
