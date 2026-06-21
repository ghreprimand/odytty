// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::{Dimensions, SnapshotEnvelope, SnapshotEnvelopeCaps};

use super::protocol::{
    ClientHello, HOST_PROTOCOL_VERSION, HostFrame, HostHello, ProtocolError, read_client_hello,
    read_host_hello, versions_compatible, write_client_hello, write_host_hello,
};
use super::{
    HostCommand, HostConfig, HostExitReason, SessionHostClient, cleanup_stale_socket,
    prepare_runtime_dir, run_host, session_socket_path, validate_runtime_dir,
};

const SHORT_WAIT: Duration = Duration::from_secs(3);

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
fn runtime_dir_validation_requires_owner_private_mode() {
    let temp = TempDir::new("odytty-host-perms");
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
    let temp = TempDir::new("odytty-host-stale");
    let runtime_dir = prepare_runtime_dir(temp.path()).expect("prepare runtime dir");
    let socket_path = session_socket_path(&runtime_dir, "demo").expect("socket path");

    let listener = UnixListener::bind(&socket_path).expect("bind live socket");
    let error = cleanup_stale_socket(&socket_path).unwrap_err();
    assert!(
        error.to_string().contains("live peer"),
        "unexpected error: {error}"
    );
    drop(listener);

    cleanup_stale_socket(&socket_path).expect("remove stale socket");
    assert!(!socket_path.exists());
}

#[test]
fn host_detach_keeps_session_alive_then_idle_timeout_exits() {
    let temp = TempDir::new("odytty-host-lifecycle");
    let config = host_config(
        temp.path(),
        "keepalive",
        "printf ready; sleep 2",
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
    wait_for_output(&mut client, "ready");
    drop(client);

    thread::sleep(Duration::from_millis(100));
    let mut reattached = SessionHostClient::connect(&socket_path, "keepalive").expect("reattach");
    let snapshot = expect_snapshot(&mut reattached);
    let decoded =
        SnapshotEnvelope::decode(&snapshot, SnapshotEnvelopeCaps::default()).expect("snapshot");
    let restored = crate::core::Terminal::from_snapshot_envelope(&decoded).expect("restore");
    assert!(restored.screen().plain_text().contains("ready"));
    drop(reattached);

    let exit = host
        .join()
        .expect("host thread")
        .expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::DetachedIdleTimeout);
}

#[test]
fn host_exits_when_child_exits_and_no_clients_remain() {
    let temp = TempDir::new("odytty-host-exit");
    let config = host_config(temp.path(), "exits", "printf done", Duration::from_secs(10));
    let socket_path = config.runtime_paths().expect("runtime paths").socket;
    let host = thread::spawn(move || run_host(config));

    wait_for_socket(&socket_path);
    let mut client = SessionHostClient::connect(&socket_path, "exits").expect("attach");
    expect_snapshot(&mut client);
    wait_for_output(&mut client, "done");
    drop(client);

    let exit = host
        .join()
        .expect("host thread")
        .expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::SessionExited);
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

fn wait_for_socket(socket_path: &Path) {
    let deadline = Instant::now() + SHORT_WAIT;
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
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
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
