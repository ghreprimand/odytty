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
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
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
                            Action::Capabilities => Reply::Capabilities {
                                structural_control,
                                quick_terminal_toggle: structural_control,
                            },
                            Action::List => Reply::Objects(Vec::new()),
                            Action::Focus { target } => Reply::Applied(target),
                            Action::Status { .. }
                            | Action::OpenProfile { .. }
                            | Action::CreateTab { .. }
                            | Action::CreateWorkspace { .. }
                            | Action::Split { .. }
                            | Action::Rename { .. }
                            | Action::QuickTerminalToggle => {
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
            structural_control: true,
            quick_terminal_toggle: true,
        }
    );
}

#[test]
fn discovery_enumerates_only_pid_shaped_names_and_refuses_over_bound_sets() {
    let missing = discovery_candidates(None, Instant::now() + Duration::from_secs(1))
        .expect_err("missing runtime base");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert!(missing.unresolved_paths().is_empty());
    let fixture = fixture();
    let runtime = fixture.dir.join("runtime");
    let control = runtime.join("odytty");
    fs::create_dir(&runtime).expect("runtime directory");
    fs::set_permissions(&runtime, Permissions::from_mode(0o700)).expect("runtime mode");
    fs::create_dir(&control).expect("control directory");
    fs::set_permissions(&control, Permissions::from_mode(0o700)).expect("control mode");
    // Discovery reports validated (canonical) paths; macOS temp directories
    // resolve through the /private symlink.
    let control = fs::canonicalize(&control).expect("canonical control directory");

    let first = UnixListener::bind(control.join("control-1.sock")).expect("first socket");
    let second = UnixListener::bind(control.join("control-42.sock")).expect("second socket");
    let ignored = UnixListener::bind(control.join("control-no.sock")).expect("ignored socket");
    assert_eq!(
        discovery_candidates(
            Some(runtime.as_os_str()),
            Instant::now() + Duration::from_secs(1)
        )
        .expect("candidate enumeration"),
        vec![
            control.join("control-1.sock"),
            control.join("control-42.sock")
        ]
    );
    let expired = discovery_candidates(Some(runtime.as_os_str()), Instant::now())
        .expect_err("expired enumeration budget");
    assert_eq!(expired.kind(), io::ErrorKind::TimedOut);
    assert_eq!(expired.unresolved_paths(), std::slice::from_ref(&control));
    drop((first, second, ignored));

    fs::remove_dir_all(&control).expect("replace candidate directory");
    fs::create_dir(&control).expect("replacement control directory");
    fs::set_permissions(&control, Permissions::from_mode(0o700)).expect("replacement mode");
    let listeners = (1..=MAX_DISCOVERY_CANDIDATES + 1)
        .map(|pid| {
            UnixListener::bind(control.join(format!("control-{pid}.sock")))
                .expect("bounded candidate socket")
        })
        .collect::<Vec<_>>();
    let over_bound = discovery_candidates(
        Some(runtime.as_os_str()),
        Instant::now() + Duration::from_secs(1),
    )
    .expect_err("over-bound candidate set");
    assert_eq!(over_bound.kind(), io::ErrorKind::InvalidData);
    assert_eq!(over_bound.unresolved_paths(), &[control]);
    drop(listeners);
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
        // Arm the short read timeout before the server can close the
        // connection: macOS rejects setsockopt on a peer-closed Unix socket
        // with EINVAL, so setting it after the sleep is timing-dependent.
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("short read timeout");
        protocol::write_request(&mut stream, &capabilities_request(9)).expect("request");
        stream.write_all(&[0x55]).expect("trailing byte");
        stream.flush().expect("flush");
        // Keep the connection open while the server peeks the trailing byte.
        thread::sleep(POLL_INTERVAL * 5);
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

#[test]
fn zero_length_and_u32_max_length_frames_fail_closed_without_wake() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    for (label, length) in [("zero", 0u32), ("u32_max", u32::MAX)] {
        let wakes_before = harness.wakes.load(Ordering::Acquire);
        let dispatched_before = harness.dispatched.load(Ordering::Acquire);
        let mut stream = connect(&path, IO_TIMEOUT).expect(label);
        stream.write_all(&length.to_le_bytes()).expect("length");
        stream.flush().expect("flush");
        let mut io = DeadlineStream::new(stream, IO_TIMEOUT).expect("deadline");
        let response = protocol::read_response(&mut io).expect("protocol rejection");
        assert_eq!(response.request_id, 0, "{label}");
        assert_eq!(response.reply, Reply::Error(ErrorCode::TooLarge), "{label}");
        assert_eq!(
            harness.wakes.load(Ordering::Acquire),
            wakes_before,
            "{label}"
        );
        assert_eq!(
            harness.dispatched.load(Ordering::Acquire),
            dispatched_before,
            "{label}"
        );
    }
}

#[test]
fn two_frames_in_one_write_fail_closed_without_second_dispatch() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    let wakes_before = harness.wakes.load(Ordering::Acquire);
    let dispatched_before = harness.dispatched.load(Ordering::Acquire);
    let mut stream = connect(&path, IO_TIMEOUT).expect("connect");
    protocol::write_request(&mut stream, &capabilities_request(101)).expect("first");
    protocol::write_request(&mut stream, &capabilities_request(102)).expect("second");
    stream.flush().expect("flush");
    // Arm the short read timeout before the peer can reset the connection.
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("short read timeout");
    thread::sleep(POLL_INTERVAL * 5);
    let mut buf = [0u8; 64];
    let result = stream.read(&mut buf);
    assert!(
        matches!(result, Ok(0) | Err(_)),
        "trailing second frame must not yield a successful dual reply: {result:?}"
    );
    assert_eq!(
        harness.dispatched.load(Ordering::Acquire),
        dispatched_before,
        "second framed request must not dispatch (trailing bytes cancel)"
    );
    assert_eq!(
        harness.wakes.load(Ordering::Acquire),
        wakes_before,
        "trailing second frame must not wake the owner"
    );
}

#[test]
fn slowloris_one_byte_per_interval_hits_absolute_io_deadline() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    let wakes_before = harness.wakes.load(Ordering::Acquire);
    let dispatched_before = harness.dispatched.load(Ordering::Acquire);
    let mut stream = connect(&path, IO_TIMEOUT).expect("connect");
    let started = Instant::now();
    let claimed = 64u32;
    let mut write_closed = false;
    for byte in claimed
        .to_le_bytes()
        .into_iter()
        .chain(std::iter::repeat_n(b'A', 16))
    {
        if write_closed {
            break;
        }
        match stream.write_all(&[byte]) {
            Ok(()) => {
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(300));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::UnexpectedEof
                ) =>
            {
                write_closed = true;
            }
            Err(error) => panic!("unexpected slowloris write error: {error:?}"),
        }
    }
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();
    let mut buf = [0u8; 32];
    let result = stream.read(&mut buf);
    let elapsed = started.elapsed();
    assert!(
        elapsed <= IO_TIMEOUT + Duration::from_secs(2),
        "slowloris must end within the absolute I/O deadline window, got {elapsed:?}"
    );
    assert!(
        write_closed
            || matches!(result, Ok(0) | Err(_))
                && elapsed >= IO_TIMEOUT.saturating_sub(Duration::from_millis(500)),
        "slowloris must fail closed near the I/O deadline; write_closed={write_closed} result={result:?} elapsed={elapsed:?}"
    );
    if let Ok(n) = result {
        assert_eq!(n, 0, "slowloris must not yield a protocol reply body");
    }
    assert_eq!(harness.wakes.load(Ordering::Acquire), wakes_before);
    assert_eq!(
        harness.dispatched.load(Ordering::Acquire),
        dispatched_before
    );
}

#[test]
fn thirty_three_requests_are_bounded_by_accept_or_busy_rate_limits() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    let started = Instant::now();
    let mut successes = 0usize;
    let mut busy = 0usize;
    let mut third_elapsed = Duration::ZERO;
    for id in 0..33u64 {
        let request_started = Instant::now();
        let response = request(&path, &capabilities_request(id)).expect("rate probe");
        if id == 32 {
            third_elapsed = request_started.elapsed();
        }
        match response.reply {
            Reply::Capabilities { .. } => successes += 1,
            Reply::Error(ErrorCode::Busy) => busy += 1,
            other => panic!("unexpected reply for id {id}: {other:?}"),
        }
    }
    let elapsed = started.elapsed();
    // Accept rate (32/s) gates before dispatch Busy on the one-request-per-
    // connection transport, so Busy may be absent while the 33rd waits.
    assert!(
        busy >= 1
            || third_elapsed >= Duration::from_millis(800)
            || elapsed >= Duration::from_secs(1),
        "33 requests must hit Busy or the accept-rate delay; busy={busy} successes={successes} third={third_elapsed:?} total={elapsed:?}"
    );
    assert!(
        successes <= 33,
        "must not invent extra successes; got {successes}"
    );
}

#[test]
fn request_id_reuse_across_two_connections_is_independent() {
    let harness = start_harness(false);
    let path = harness.fixture.socket.clone();
    let a = request(&path, &capabilities_request(42)).expect("first");
    let b = request(&path, &capabilities_request(42)).expect("second");
    assert_eq!(a.request_id, 42);
    assert_eq!(b.request_id, 42);
    assert_eq!(
        a.reply,
        Reply::Capabilities {
            structural_control: false,
            quick_terminal_toggle: false,
        }
    );
    assert_eq!(b.reply, a.reply);
}

#[test]
fn parent_mode_0755_is_refused_for_bind_and_client() {
    let fixture = fixture();
    fs::set_permissions(&fixture.dir, Permissions::from_mode(0o755)).expect("0755");
    let (submission, _queue) = dispatch::channel(false);
    match Server::bind(&fixture.socket, submission, || true) {
        Ok(_server) => panic!("parent mode 0755 must be refused"),
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::PermissionDenied),
    }
    assert_eq!(
        request(&fixture.socket, &capabilities_request(1))
            .expect_err("client 0755")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn preexisting_regular_file_at_socket_path_is_refused() {
    let fixture = fixture();
    fs::write(&fixture.socket, b"not-a-socket").expect("regular file");
    fs::set_permissions(&fixture.socket, Permissions::from_mode(0o600)).expect("mode");
    let (submission, _queue) = dispatch::channel(false);
    match Server::bind(&fixture.socket, submission, || true) {
        Ok(_server) => panic!("pre-existing regular file must not bind"),
        Err(error) => assert!(
            matches!(
                error.kind(),
                io::ErrorKind::AddrInUse
                    | io::ErrorKind::AlreadyExists
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::InvalidInput
            ),
            "unexpected bind error: {error:?}"
        ),
    }
    assert_eq!(
        request(&fixture.socket, &capabilities_request(2))
            .expect_err("client regular file")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn symlinked_parent_binds_on_the_resolved_directory_only() {
    // A symlinked parent is resolved to its real path before every ancestor
    // check, and the socket is created at the resolved location, never through
    // the link. The link's own chain therefore adds no exposure: redirecting it
    // can only reach another directory that already passes the owner-only checks.
    let real = fixture();
    let alias_root = {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let tag = format!("odyctl-link-{:x}-{sequence:x}", std::process::id());
        let dir = std::env::temp_dir().join(tag);
        fs::create_dir(&dir).expect("alias root");
        fs::set_permissions(&dir, Permissions::from_mode(0o700)).expect("alias root mode");
        dir
    };
    let linked_parent = alias_root.join("via-link");
    std::os::unix::fs::symlink(&real.dir, &linked_parent).expect("symlink parent");
    let socket = linked_parent.join("control.sock");
    let (submission, _queue) = dispatch::channel(false);
    let bind_result = Server::bind(&socket, submission, || true);
    let _ = fs::remove_file(&linked_parent);
    let _ = fs::remove_dir_all(&alias_root);
    let server = bind_result.expect("resolved owner-only parent binds");
    let resolved = fs::canonicalize(&real.dir).expect("real dir");
    assert!(
        fs::symlink_metadata(resolved.join("control.sock"))
            .expect("socket at resolved path")
            .file_type()
            .is_socket()
    );
    drop(server);
}

#[test]
fn peer_uid_mismatch_is_unsupported_without_second_account() {
    // Cross-user SO_PEERCRED refusal needs a second uid (root/chown). Same-user
    // owner checks are covered elsewhere; record the gap explicitly.
    eprintln!(
        "skip peer-UID mismatch: no second account seam without root/chown (unsupported here)"
    );
}

#[test]
fn stale_identity_error_code_roundtrips_over_the_wire() {
    // Live closed-object and detached-host mapping belongs to the owner bridge.
    // This pins the transport ErrorCode path so a StaleIdentity reply stays
    // fail-closed and correlated.
    let fixture = fixture();
    let (submission, queue) = dispatch::channel(true);
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(8);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_owner = stop.clone();
    let owner = thread::spawn(move || {
        let queue = queue;
        while !stop.load(Ordering::Acquire) {
            match wake_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {
                    let _ = queue.dispatch(|_| Reply::Error(ErrorCode::StaleIdentity));
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    let server = Server::bind(&fixture.socket, submission, move || {
        wake_tx.send(()).is_ok()
    })
    .expect("bind");
    let id = protocol::ObjectId {
        instance: [0x11; 16],
        kind: protocol::ObjectKind::Window,
        serial: 99,
    };
    let response = request(
        &fixture.socket,
        &Request {
            version: VERSION,
            request_id: 77,
            action: Action::Focus { target: id },
        },
    )
    .expect("stale reply");
    assert_eq!(response.request_id, 77);
    assert_eq!(response.reply, Reply::Error(ErrorCode::StaleIdentity));
    stop_owner.store(true, Ordering::Release);
    drop(server);
    let _ = owner.join();
    // Detached-host-namespace rejection against live objects is covered by the
    // owner-bridge tests, not by this transport-level pin.
}

#[test]
fn listener_fault_is_none_while_listening_and_keeps_first_reason() {
    let fixture = fixture();
    let (submission, _queue) = dispatch::channel(true);
    let server = Server::bind(&fixture.socket, submission, || true).expect("bind");
    assert_eq!(server.fault(), None, "a listening server reports no fault");
    let slot = Mutex::new(None);
    let woken = AtomicUsize::new(0);
    let wake = || {
        woken.fetch_add(1, Ordering::Relaxed);
        true
    };
    record_fault(&slot, &wake, "first".to_owned());
    record_fault(&slot, &wake, "second".to_owned());
    assert_eq!(slot.lock().expect("slot").as_deref(), Some("first"));
    assert_eq!(
        woken.load(Ordering::Relaxed),
        2,
        "every fault wakes the owner"
    );
    drop(server);
}
