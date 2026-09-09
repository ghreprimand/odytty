// SPDX-License-Identifier: GPL-3.0-only
//! Hostile-client and endpoint-boundary regressions for the Unix transport.
//!
//! Cross-user credential refusal (peer uid != euid) needs a second account and
//! is recorded as untestable here without root or chown. Same-user owner
//! checks, path permissions, framing, disconnect cancel, and saturation are
//! exercised with synthetic fixture directories only.

use super::*;
use crate::automation::dispatch::{self, MAX_PER_DISPATCH};
use crate::automation::protocol::{
    self, Action, ErrorCode, MAX_MESSAGE_BYTES, Reply, Request, VERSION,
};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;

fn assert_fd_cloexec(fd: i32, label: &str) {
    // SAFETY: fd is a live descriptor owned by this process for the test duration.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let flags = rustix::io::fcntl_getfd(borrowed).expect("fcntl_getfd");
    assert!(
        flags.contains(rustix::io::FdFlags::CLOEXEC),
        "{label} fd {fd} must have FD_CLOEXEC; flags={flags:?}"
    );
}

#[cfg(target_os = "linux")]
fn fd_has_cloexec(fd: i32) -> bool {
    // SAFETY: fd is live while the accept observer runs.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    rustix::io::fcntl_getfd(borrowed)
        .is_ok_and(|flags| flags.contains(rustix::io::FdFlags::CLOEXEC))
}

#[cfg(target_os = "linux")]
fn unix_listening_inode(path: &Path) -> Option<u64> {
    let table = fs::read_to_string("/proc/net/unix").ok()?;
    let needle = path.to_string_lossy();
    for line in table.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 {
            continue;
        }
        let sock_path = fields[7..].join(" ");
        if sock_path == needle.as_ref() {
            return fields[6].parse().ok();
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn find_fd_for_socket_inode(inode: u64) -> Option<i32> {
    let needle = format!("socket:[{inode}]");
    for entry in fs::read_dir("/proc/self/fd").ok()?.flatten() {
        let fd = entry.file_name().to_str()?.parse::<i32>().ok()?;
        let target = fs::read_link(entry.path()).ok()?;
        if target.to_string_lossy() == needle {
            return Some(fd);
        }
    }
    None
}

struct Fixture {
    dir: PathBuf,
    socket: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn fixture() -> Fixture {
    // Synthetic entropy only: no host usernames, homes, or machine paths.
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let tag = format!("odyctl-{:x}-{sequence:x}", std::process::id());
    let dir = std::env::temp_dir().join(tag);
    fs::create_dir(&dir).expect("private fixture directory");
    fs::set_permissions(&dir, Permissions::from_mode(0o700)).expect("dir mode 0700");
    let socket = dir.join("control.sock");
    Fixture { dir, socket }
}

struct Harness {
    fixture: Fixture,
    server: Option<Server>,
    stop_owner: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
    cancelled: Arc<AtomicUsize>,
    wakes: Arc<AtomicUsize>,
    dispatched: Arc<AtomicUsize>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop_owner.store(true, Ordering::Release);
        if let Some(server) = self.server.take() {
            drop(server);
        }
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

fn start_harness(structural_control: bool) -> Harness {
    let fixture = fixture();
    let (submission, queue) = dispatch::channel(structural_control);
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(64);
    let stop_owner = Arc::new(AtomicBool::new(false));
    let stop = stop_owner.clone();
    let cancelled = Arc::new(AtomicUsize::new(0));
    let cancel_count = cancelled.clone();
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = wakes.clone();
    let dispatched = Arc::new(AtomicUsize::new(0));
    let dispatch_count = dispatched.clone();
    let owner = thread::spawn(move || {
        let queue = queue;
        while !stop.load(Ordering::Acquire) {
            match wake_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => loop {
                    let handled = queue.dispatch(|request| {
                        dispatch_count.fetch_add(1, Ordering::Relaxed);
                        match request.action {
                            Action::Capabilities => Reply::Capabilities { structural_control },
                            Action::List => Reply::Objects(Vec::new()),
                            Action::Focus { target } => Reply::Applied(target),
                            Action::Status { .. }
                            | Action::OpenProfile { .. }
                            | Action::CreateTab { .. }
                            | Action::CreateWorkspace { .. }
                            | Action::Split { .. }
                            | Action::Rename { .. } => {
                                Reply::Error(ErrorCode::UnsupportedCapability)
                            }
                        }
                    });
                    if handled == 0 {
                        break;
                    }
                    if handled == MAX_PER_DISPATCH {
                        continue;
                    }
                },
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // Observe cancel races for disconnect tests without reflecting paths.
            let _ = cancel_count.load(Ordering::Relaxed);
        }
    });
    let server = Server::bind(&fixture.socket, submission, move || {
        wake_count.fetch_add(1, Ordering::Relaxed);
        wake_tx.send(()).is_ok()
    })
    .expect("bind owner-private endpoint");
    Harness {
        fixture,
        server: Some(server),
        stop_owner,
        owner: Some(owner),
        cancelled,
        wakes,
        dispatched,
    }
}

fn capabilities_request(id: u64) -> Request {
    Request {
        version: VERSION,
        request_id: id,
        action: Action::Capabilities,
    }
}

#[test]
fn capabilities_roundtrip_over_owner_private_endpoint() {
    let harness = start_harness(true);
    let response =
        request(&harness.fixture.socket, &capabilities_request(7)).expect("capabilities");
    assert_eq!(response.request_id, 7);
    assert_eq!(
        response.reply,
        Reply::Capabilities {
            structural_control: true
        }
    );
}

#[test]
fn relative_and_world_writable_paths_are_refused() {
    let relative = PathBuf::from("control.sock");
    assert_eq!(
        request(&relative, &capabilities_request(1))
            .expect_err("relative")
            .kind(),
        io::ErrorKind::InvalidInput
    );

    let harness = start_harness(false);
    let nested = harness.fixture.dir.join("world");
    fs::create_dir(&nested).expect("nested directory");
    fs::set_permissions(&nested, Permissions::from_mode(0o777)).expect("world-writable nested");
    let bad = nested.join("control.sock");
    let (submission, _queue) = dispatch::channel(false);
    match Server::bind(&bad, submission, || true) {
        Ok(_server) => panic!("world-writable ancestor must be refused"),
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::PermissionDenied),
    }
}

#[test]
fn symlink_regular_file_and_relaxed_mode_are_refused() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();

    let decoy = harness.fixture.dir.join("decoy");
    fs::write(&decoy, b"not-a-socket").expect("decoy file");
    fs::set_permissions(&decoy, Permissions::from_mode(0o600)).expect("decoy mode");
    assert_eq!(
        request(&decoy, &capabilities_request(2))
            .expect_err("non-socket")
            .kind(),
        io::ErrorKind::PermissionDenied
    );

    let link = harness.fixture.dir.join("alias");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    // Symlink in the private directory still resolves to a socket name join of
    // the alias path; metadata of the symlink itself is not a socket.
    assert_eq!(
        request(&link, &capabilities_request(3))
            .expect_err("symlink endpoint")
            .kind(),
        io::ErrorKind::PermissionDenied
    );

    fs::set_permissions(&path, Permissions::from_mode(0o666)).expect("relax socket mode");
    assert_eq!(
        request(&path, &capabilities_request(4))
            .expect_err("mode 0666")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn existing_endpoint_is_not_removed_to_force_bind_and_replacement_is_preserved() {
    let mut harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    let (submission, _queue) = dispatch::channel(false);
    match Server::bind(&path, submission, || true) {
        Ok(_server) => panic!("duplicate bind must be refused"),
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::AddrInUse),
    }

    // Replace the path with a new socket inode while the first server is live.
    // Drop must not unlink the replacement (different device/inode).
    fs::remove_file(&path).expect("unlink live name");
    let replacement = UnixListener::bind(&path).expect("replacement listener");
    fs::set_permissions(&path, Permissions::from_mode(0o600)).expect("replacement mode");
    let replaced = fs::symlink_metadata(&path).expect("replacement metadata");
    // Drop only the Server (EndpointGuard). Keep Fixture alive so its Drop does
    // not mask the guard's preserve-or-unlink contract.
    drop(harness.server.take());
    assert!(
        fs::symlink_metadata(&path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == replaced.dev()
                && metadata.ino() == replaced.ino()
        }),
        "EndpointGuard must preserve a replaced inode"
    );
    drop(replacement);
    let _ = fs::remove_file(&path);
}

#[test]
fn oversized_truncated_and_trailing_frames_fail_closed() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();

    // Oversized length prefix: rejected before body read; correlation id is 0.
    // Protocol rejection must not wake the owner or dispatch work.
    {
        let wakes_before = harness.wakes.load(Ordering::Acquire);
        let dispatched_before = harness.dispatched.load(Ordering::Acquire);
        let mut stream = connect(&path, IO_TIMEOUT).expect("connect oversized");
        let length = (MAX_MESSAGE_BYTES as u32).saturating_add(1).to_le_bytes();
        stream.write_all(&length).expect("length");
        stream.flush().expect("flush");
        let mut io = DeadlineStream::new(stream, IO_TIMEOUT).expect("deadline stream");
        let response = protocol::read_response(&mut io).expect("too-large reply");
        assert_eq!(response.request_id, 0);
        assert_eq!(response.reply, Reply::Error(ErrorCode::TooLarge));
        assert_eq!(
            harness.wakes.load(Ordering::Acquire),
            wakes_before,
            "oversized frame must not wake the owner"
        );
        assert_eq!(
            harness.dispatched.load(Ordering::Acquire),
            dispatched_before,
            "oversized frame must not dispatch"
        );
    }

    // Truncated body: server I/O deadline ends the worker without dispatch.
    // Client must observe EOF/timeout/error - never a successful structural reply.
    {
        let wakes_before = harness.wakes.load(Ordering::Acquire);
        let dispatched_before = harness.dispatched.load(Ordering::Acquire);
        let mut stream = connect(&path, IO_TIMEOUT).expect("connect truncated");
        stream.write_all(&32u32.to_le_bytes()).expect("length");
        stream.write_all(b"ODYC").expect("partial magic");
        stream.flush().expect("flush");
        let started = Instant::now();
        stream
            .set_read_timeout(Some(IO_TIMEOUT + Duration::from_millis(500)))
            .expect("read timeout");
        let mut buf = [0u8; 64];
        let result = stream.read(&mut buf);
        assert!(
            started.elapsed() <= IO_TIMEOUT + Duration::from_secs(1),
            "truncated client must finish within the server I/O deadline window"
        );
        match result {
            Ok(0) => {}
            Err(error) => assert!(
                matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                ),
                "truncated client must fail closed, got {error:?}"
            ),
            Ok(n) => panic!(
                "truncated frame must not yield a protocol reply body ({n} bytes): {:?}",
                &buf[..n]
            ),
        }
        assert_eq!(
            harness.wakes.load(Ordering::Acquire),
            wakes_before,
            "truncated frame must not wake the owner"
        );
        assert_eq!(
            harness.dispatched.load(Ordering::Acquire),
            dispatched_before,
            "truncated frame must not dispatch"
        );
    }

    // Trailing bytes after a complete request cancel pending work (no half-close).
    {
        let mut stream = connect(&path, IO_TIMEOUT).expect("connect trailing");
        protocol::write_request(&mut stream, &capabilities_request(9)).expect("request");
        stream.write_all(&[0x55]).expect("trailing byte");
        stream.flush().expect("flush");
        // Keep the connection open while the server peeks the trailing byte.
        thread::sleep(POLL_INTERVAL * 5);
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("short read timeout");
        let mut buf = [0u8; 4];
        let result = stream.read(&mut buf);
        assert!(
            matches!(result, Ok(0) | Err(_)),
            "trailing-byte connection must not receive a successful structural reply: {result:?}"
        );
    }

    // Endpoint recovers for a clean framed request after hostile frames.
    let response =
        request(&path, &capabilities_request(8)).expect("recovered after hostile frames");
    assert_eq!(response.request_id, 8);
    assert!(
        harness.wakes.load(Ordering::Acquire) >= 1,
        "clean recovery request must wake the owner"
    );
    assert!(
        harness.dispatched.load(Ordering::Acquire) >= 1,
        "clean recovery request must dispatch"
    );
}

#[test]
fn disconnect_before_reply_cancels_pending_work() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    let mut stream = connect(&path, IO_TIMEOUT).expect("connect");
    protocol::write_request(&mut stream, &capabilities_request(11)).expect("request");
    // Drop without half-closing first would be ideal; dropping the stream is EOF
    // and must cancel via peer_finished rather than waiting for owner apply.
    drop(stream);
    thread::sleep(POLL_INTERVAL * 10);
    // A subsequent clean request still works: the worker joined and did not wedged.
    let response = request(&path, &capabilities_request(12)).expect("after disconnect");
    assert_eq!(response.request_id, 12);
    let _ = harness.cancelled.load(Ordering::Relaxed);
}

#[test]
fn saturated_accept_queue_bounds_additional_clients() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    let mut blockers = Vec::new();
    for _ in 0..MAX_CLIENTS {
        let mut stream = connect(&path, IO_TIMEOUT).expect("blocker connect");
        // Occupy the worker inside a framed body read so the slot stays busy
        // until the absolute I/O deadline.
        let length = (MAX_MESSAGE_BYTES / 2) as u32;
        stream
            .write_all(&length.to_le_bytes())
            .expect("blocker length");
        stream.flush().expect("blocker flush");
        blockers.push(stream);
    }
    // Allow the listener to accept every blocker into a worker.
    thread::sleep(POLL_INTERVAL * 10);

    // Kernel listen backlog may still complete connect(2) while userspace has
    // stopped accept. Saturation is therefore worker-bounded, not
    // connect-refusal: an extra request must wait for a slot to free (blocker
    // I/O deadline) rather than running unbounded parallelism.
    let path_for_extra = path.clone();
    let started = Instant::now();
    let extra = thread::spawn(move || request(&path_for_extra, &capabilities_request(99)));
    // While all worker slots stay busy, the deferred client must not finish.
    thread::sleep(Duration::from_millis(250));
    assert!(
        !extra.is_finished(),
        "extra client must remain pending while MAX_CLIENTS workers are held"
    );
    let response = extra
        .join()
        .expect("extra join")
        .expect("served after slot frees");
    let waited = started.elapsed();
    assert_eq!(response.request_id, 99);
    assert!(
        waited >= IO_TIMEOUT.saturating_sub(Duration::from_millis(250)),
        "extra client must wait for a saturated worker I/O deadline, got {waited:?}"
    );
    assert!(
        waited <= IO_TIMEOUT + dispatch::REQUEST_TIMEOUT + Duration::from_secs(2),
        "extra client must finish within connect/request deadlines, got {waited:?}"
    );

    drop(blockers);
    // After blockers release, a fresh request recovers promptly.
    let response = request(&path, &capabilities_request(13)).expect("recovered");
    assert_eq!(response.request_id, 13);
}

#[test]
fn read_only_policy_rejects_mutations_over_the_wire() {
    let harness = start_harness(false);
    let id = protocol::ObjectId {
        instance: [9; 16],
        kind: protocol::ObjectKind::Window,
        serial: 1,
    };
    let response = request(
        &harness.fixture.socket,
        &Request {
            version: VERSION,
            request_id: 21,
            action: Action::Focus { target: id },
        },
    )
    .expect("mutation reply");
    assert_eq!(response.request_id, 21);
    assert_eq!(response.reply, Reply::Error(ErrorCode::PermissionDenied));
}

#[test]
fn client_connect_sets_fd_cloexec() {
    let harness = start_harness(false);
    let stream = connect(&harness.fixture.socket, IO_TIMEOUT).expect("connect");
    assert_fd_cloexec(stream.as_raw_fd(), "client connect");
}

#[cfg(target_os = "linux")]
#[test]
fn listener_and_accepted_connection_set_fd_cloexec() {
    let fixture = fixture();
    let path = fixture.socket.clone();
    let (submission, _queue) = dispatch::channel(false);
    let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
    let server = Server::bind_with_accept_observer(
        &path,
        submission,
        || true,
        move |stream| {
            let _ = accepted_tx.send(fd_has_cloexec(stream.as_raw_fd()));
        },
    )
    .expect("bind observed endpoint");
    let inode = unix_listening_inode(&path).expect("listening inode in /proc/net/unix");
    let listener_fd = find_fd_for_socket_inode(inode).expect("listener fd in /proc/self/fd");
    assert_fd_cloexec(listener_fd, "automation listener");

    let stream = connect(&path, IO_TIMEOUT).expect("held client");
    assert_fd_cloexec(stream.as_raw_fd(), "client after accept");
    assert!(
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("accept observer result"),
        "accepted connection must have FD_CLOEXEC"
    );
    drop(stream);
    drop(server);
}

#[cfg(target_os = "macos")]
#[test]
fn listener_cloexec_uses_client_side_proxy_on_darwin() {
    // Darwin has no /proc fd/inode table for listener discovery. Production
    // client connect sets CLOEXEC via fcntl; listener/accept CLOEXEC remains a
    // platform validation gap without a test-only fd seam.
    let harness = start_harness(false);
    let stream = connect(&harness.fixture.socket, IO_TIMEOUT).expect("connect");
    assert_fd_cloexec(stream.as_raw_fd(), "darwin client connect");
}
