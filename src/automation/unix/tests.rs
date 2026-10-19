// SPDX-License-Identifier: GPL-3.0-only
//! Hostile-client and endpoint-boundary regressions for the Unix transport.
//!
//! Cross-user credential refusal (peer uid != euid) needs a second account and
//! is recorded as untestable here without root or chown. Same-user owner
//! checks, path permissions, framing, disconnect cancel, and saturation are
//! exercised with synthetic fixture directories only.

use super::connection::{CONNECT_RETRY_INTERVAL, retry_connect};
use super::*;
use crate::automation::dispatch;
use crate::automation::protocol::{
    self, Action, ErrorCode, MAX_MESSAGE_BYTES, Reply, Request, VERSION,
};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
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
    socket_fd_from_links(
        inode,
        fs::read_dir("/proc/self/fd")
            .ok()?
            .flatten()
            .map(|entry| (entry.file_name(), fs::read_link(entry.path()))),
    )
}

#[cfg(target_os = "linux")]
fn socket_fd_from_links(
    inode: u64,
    entries: impl Iterator<Item = (std::ffi::OsString, std::io::Result<PathBuf>)>,
) -> Option<i32> {
    let needle = format!("socket:[{inode}]");
    for (name, target) in entries {
        let Some(fd) = name.to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        // Other test threads close descriptors between read_dir and read_link.
        let Ok(target) = target else {
            continue;
        };
        if target.to_string_lossy() == needle {
            return Some(fd);
        }
    }
    None
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_lookup_skips_closed_and_unrelated_entries() {
    for (name, target) in [
        ("5", Err(std::io::Error::from(std::io::ErrorKind::NotFound))),
        ("not-a-descriptor", Ok(PathBuf::from("socket:[123]"))),
    ] {
        assert_eq!(
            socket_fd_from_links(
                123,
                [
                    (std::ffi::OsString::from(name), target),
                    (
                        std::ffi::OsString::from("6"),
                        Ok(PathBuf::from("socket:[456]"))
                    ),
                    (
                        std::ffi::OsString::from("7"),
                        Ok(PathBuf::from("socket:[123]"))
                    ),
                ]
                .into_iter()
            ),
            Some(7)
        );
    }
    assert_eq!(socket_fd_from_links(123, std::iter::empty()), None);
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
    let dir = crate::test_dirs::fresh_socket_dir("odc");
    fs::set_permissions(&dir, Permissions::from_mode(0o700)).expect("dir mode 0700");
    let socket = dir.join("control.sock");
    Fixture { dir, socket }
}

struct Harness {
    fixture: Fixture,
    server: Option<Server>,
    stop_owner: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
    /// While set, the owner thread dispatches nothing, so a test can queue a
    /// request and act before it is handled.
    paused: Arc<AtomicBool>,
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
    start_harness_observing_accept(structural_control, None)
}

fn start_harness_observing_accept(
    structural_control: bool,
    observer: Option<AcceptObserver>,
) -> Harness {
    let fixture = fixture();
    let (submission, queue) = dispatch::channel(structural_control);
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(64);
    let stop_owner = Arc::new(AtomicBool::new(false));
    let stop = stop_owner.clone();
    let paused = Arc::new(AtomicBool::new(false));
    let pause = paused.clone();
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = wakes.clone();
    let dispatched = Arc::new(AtomicUsize::new(0));
    let dispatch_count = dispatched.clone();
    let owner = thread::spawn(move || {
        let queue = queue;
        while !stop.load(Ordering::Acquire) {
            if pause.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
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
                },
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    let server = Server::bind_inner(
        &fixture.socket,
        submission,
        Arc::new(move || {
            wake_count.fetch_add(1, Ordering::Relaxed);
            wake_tx.send(()).is_ok()
        }),
        observer,
    )
    .expect("bind owner-private endpoint");
    Harness {
        fixture,
        server: Some(server),
        stop_owner,
        owner: Some(owner),
        paused,
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
fn listener_rejects_prequeued_trailing_data_and_serves_a_clean_successor() {
    for second_frame in [false, true] {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let ready_rx = std::sync::Mutex::new(ready_rx);
        let accepts = AtomicUsize::new(0);
        let observer = Arc::new(move |_: &UnixStream| {
            if accepts.fetch_add(1, Ordering::Relaxed) == 0 {
                ready_rx
                    .lock()
                    .expect("ready lock")
                    .recv_timeout(IO_TIMEOUT)
                    .expect("hostile bytes queued");
            }
        });
        let harness = start_harness_observing_accept(false, Some(observer));
        let mut client = connect(&harness.fixture.socket, IO_TIMEOUT).expect("connect");
        let mut bytes = Vec::new();
        protocol::write_request(&mut bytes, &capabilities_request(71)).expect("request");
        if second_frame {
            protocol::write_request(&mut bytes, &capabilities_request(72)).expect("second");
        } else {
            bytes.push(0x55);
        }
        client.write_all(&bytes).expect("queue hostile bytes");
        client
            .set_read_timeout(Some(IO_TIMEOUT))
            .expect("read deadline");
        ready_tx.send(()).expect("release accepted stream");
        let mut response = [0; 64];
        let result = loop {
            match client.read(&mut response) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result,
            }
        };
        assert!(
            matches!(result, Ok(0))
                || result
                    .as_ref()
                    .is_err_and(|error| { error.kind() == io::ErrorKind::ConnectionReset }),
            "hostile client must close without a reply: {result:?}"
        );
        assert_eq!(harness.wakes.load(Ordering::Acquire), 0);
        assert_eq!(harness.dispatched.load(Ordering::Acquire), 0);
        let reply = request(&harness.fixture.socket, &capabilities_request(73))
            .expect("listener serves a clean successor");
        assert_eq!(reply.request_id, 73);
        assert!(matches!(reply.reply, Reply::Capabilities { .. }));
        assert_eq!(harness.wakes.load(Ordering::Acquire), 1);
        assert_eq!(harness.dispatched.load(Ordering::Acquire), 1);
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
fn a_bound_socket_that_cannot_be_identified_leaves_whatever_occupies_the_path() {
    let fixture = fixture();
    let parent = fs::symlink_metadata(&fixture.dir).expect("parent metadata");
    let socket = fixture.socket.clone();
    let failed = bind_guarded(socket.clone(), parent.clone(), |bound| {
        // The path is swapped for a project-owned ordinary file before the
        // identification fails, so the object there was never bound by us.
        fs::remove_file(bound).expect("unlink the bound name");
        fs::write(bound, b"replacement").expect("plant a replacement");
        Err(io::Error::other("injected identification failure"))
    });
    assert!(failed.is_err());
    assert_eq!(
        fs::read(&socket).expect("the replacement survives"),
        b"replacement"
    );
    fs::remove_file(&socket).expect("clean up the replacement");
}

#[test]
fn an_identified_socket_is_still_removed_by_its_guard() {
    let fixture = fixture();
    let parent = fs::symlink_metadata(&fixture.dir).expect("parent metadata");
    let (listener, guard) = bind_guarded(fixture.socket.clone(), parent, |bound| {
        fs::symlink_metadata(bound)
    })
    .expect("bind");
    assert!(
        fs::symlink_metadata(&fixture.socket).is_ok_and(|m| m.file_type().is_socket()),
        "the healthy bind keeps its socket file until the guard drops"
    );
    drop(listener);
    drop(guard);
    assert!(fs::symlink_metadata(&fixture.socket).is_err());
}

// Queue the complete hostile buffer before the real connection handler reads.
// This tests already-present trailing data, rather than racing later writes
// against a valid first request that the server may have dispatched already.
fn assert_prequeued_trailing_data_is_rejected(bytes: &[u8]) {
    let (mut client, server) = UnixStream::pair().expect("socketpair");
    // Arm before serve can close the peer; macOS rejects a timeout on a closed peer.
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("short read timeout");
    client
        .write_all(bytes)
        .expect("queue complete hostile buffer");
    let (submission, queue) = dispatch::channel(false);
    serve(
        server,
        submission,
        Arc::new(|| panic!("queued trailing data must not wake the owner")),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("reject queued trailing data");
    assert_eq!(
        queue.dispatch(|_| panic!("queued trailing data must not dispatch")),
        0
    );
    let mut response = [0u8; 64];
    let result = client.read(&mut response);
    assert!(
        matches!(result, Ok(0))
            || result.as_ref().is_err_and(|error| {
                matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::ConnectionReset
                )
            }),
        "queued trailing data must receive no structural reply: {result:?}"
    );
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
        // A signal delivered to the test process (another test's child exiting,
        // for example) can interrupt the read; that says nothing about the
        // server, so retry it.
        let result = loop {
            match stream.read(&mut buf) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                other => break other,
            }
        };
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

    // Trailing bytes present when the request is read cancel work (no half-close).
    let mut bytes = Vec::new();
    protocol::write_request(&mut bytes, &capabilities_request(9)).expect("request");
    bytes.push(0x55);
    assert_prequeued_trailing_data_is_rejected(&bytes);

    // Endpoint recovers for a clean request after oversized and truncated frames.
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
    // Hold the owner so request 11 is still queued when its client goes away.
    harness.paused.store(true, Ordering::Release);
    let mut stream = connect(&path, IO_TIMEOUT).expect("connect");
    protocol::write_request(&mut stream, &capabilities_request(11)).expect("request");
    // Dropping the stream is EOF and must cancel via peer_finished rather than
    // waiting for the owner to apply it.
    drop(stream);
    thread::sleep(POLL_INTERVAL * 10);
    harness.paused.store(false, Ordering::Release);
    // A subsequent clean request still works: the worker joined and did not wedge.
    let response = request(&path, &capabilities_request(12)).expect("after disconnect");
    assert_eq!(response.request_id, 12);
    assert_eq!(
        harness.dispatched.load(Ordering::Acquire),
        1,
        "only the clean request is dispatched; the abandoned one was cancelled"
    );
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
fn accepted_and_client_connections_set_fd_cloexec_on_darwin() {
    // Darwin has no /proc fd/inode table for listener discovery, so the
    // listening descriptor itself stays unchecked here. The accepted
    // connection is checked through the accept observer, and the client side
    // directly.
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
    let stream = connect(&path, IO_TIMEOUT).expect("held client");
    assert_fd_cloexec(stream.as_raw_fd(), "darwin client connect");
    assert!(
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("accept observer result"),
        "accepted connection must have FD_CLOEXEC"
    );
    drop(stream);
    drop(server);
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
fn two_queued_frames_fail_closed_without_dispatch() {
    let mut bytes = Vec::new();
    protocol::write_request(&mut bytes, &capabilities_request(101)).expect("first");
    protocol::write_request(&mut bytes, &capabilities_request(102)).expect("second");
    assert_prequeued_trailing_data_is_rejected(&bytes);
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
    let mut last_elapsed = Duration::ZERO;
    for id in 0..33u64 {
        let request_started = Instant::now();
        let response = request(&path, &capabilities_request(id)).expect("rate probe");
        if id == 32 {
            last_elapsed = request_started.elapsed();
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
            || last_elapsed >= Duration::from_millis(800)
            || elapsed >= Duration::from_secs(1),
        "33 requests must hit Busy or the accept-rate delay; busy={busy} successes={successes} last={last_elapsed:?} total={elapsed:?}"
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
    let alias = fixture();
    let alias_root = &alias.dir;
    let linked_parent = alias_root.join("via-link");
    std::os::unix::fs::symlink(&real.dir, &linked_parent).expect("symlink parent");
    let socket = linked_parent.join("control.sock");
    let (submission, _queue) = dispatch::channel(false);
    let bind_result = Server::bind(&socket, submission, || true);
    let _ = fs::remove_file(&linked_parent);
    let _ = fs::remove_dir_all(alias_root);
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
#[ignore = "cross-user SO_PEERCRED refusal needs a second account (root or chown); same-user owner checks are covered elsewhere"]
fn peer_uid_mismatch_is_unsupported_without_second_account() {}

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

/// Descriptor and memory exhaustion and an aborted peer retry instead of
/// stopping the endpoint, up to a bounded streak; other errors still fault.
#[test]
fn transient_accept_errors_retry_and_other_errors_fault() {
    for errno in [
        libc::EMFILE,
        libc::ENFILE,
        libc::ENOBUFS,
        libc::ENOMEM,
        libc::ECONNABORTED,
    ] {
        let mut streak = 0;
        let error = io::Error::from_raw_os_error(errno);
        for _ in 0..MAX_TRANSIENT_ACCEPT_ERRORS {
            assert_eq!(
                accept_error_step(&error, &mut streak),
                AcceptStep::Retry,
                "{error}"
            );
        }
        assert_eq!(
            accept_error_step(&error, &mut streak),
            AcceptStep::Fault,
            "a transient error that never clears still faults: {error}"
        );
    }
    let mut streak = 0;
    for errno in [libc::EBADF, libc::EINVAL, libc::ENOTSOCK] {
        assert_eq!(
            accept_error_step(&io::Error::from_raw_os_error(errno), &mut streak),
            AcceptStep::Fault
        );
    }
}

/// Fill a listener's accept queue: lower its backlog to zero and connect
/// nonblocking clients until the kernel refuses with `EAGAIN`.
#[cfg(target_os = "linux")]
fn saturated_listener(socket: &Path) -> (UnixListener, Vec<UnixStream>) {
    let listener = UnixListener::bind(socket).expect("bind fixture listener");
    // SAFETY: a valid listening socket; a second listen only lowers the backlog.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
    let mut queued = Vec::new();
    for _ in 0..64 {
        match connect(socket, Duration::ZERO) {
            Ok(stream) => queued.push(stream),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => return (listener, queued),
            Err(error) => panic!("unexpected connect error while filling: {error}"),
        }
    }
    panic!("the accept queue never filled");
}

/// A full accept queue is waited for until the connect deadline, then reported
/// as a timeout, rather than failing at once with `WouldBlock`.
#[cfg(target_os = "linux")]
#[test]
fn a_full_accept_queue_is_waited_for_until_the_deadline() {
    let fixture = fixture();
    let (listener, _queued) = saturated_listener(&fixture.socket);
    let started = Instant::now();
    let error = connect(&fixture.socket, Duration::from_millis(150))
        .expect_err("nobody accepts, so the connect times out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "the connect waited for the deadline instead of failing at once"
    );
    // Once the owner accepts, a waiting connect gets the freed slot.
    let accepter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        let accepted = listener.accept().expect("accept one queued client");
        (listener, accepted)
    });
    connect(&fixture.socket, Duration::from_secs(5)).expect("a freed slot is taken");
    drop(accepter.join().expect("accepter"));
}

/// A fake clock for [`retry_connect`]: time moves only when the attempt or the
/// sleep says so, so no assertion depends on scheduling.
struct FakeClock {
    base: Instant,
    elapsed: std::cell::Cell<Duration>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            elapsed: std::cell::Cell::new(Duration::ZERO),
        }
    }
    fn now(&self) -> Instant {
        self.base + self.elapsed.get()
    }
    fn advance(&self, by: Duration) {
        self.elapsed.set(self.elapsed.get() + by);
    }
}

fn would_block() -> io::Error {
    io::Error::from(io::ErrorKind::WouldBlock)
}

/// An attempt that is interrupted every time stops at the deadline instead of
/// retrying without end.
#[test]
fn interrupted_connect_attempts_stop_at_the_deadline() {
    let clock = FakeClock::new();
    let deadline = clock.now() + Duration::from_millis(20);
    let attempts = std::cell::Cell::new(0u32);
    let result = retry_connect(
        deadline,
        || clock.now(),
        |_| panic!("an interrupted attempt does not sleep"),
        || {
            attempts.set(attempts.get() + 1);
            assert!(attempts.get() < 1_000, "retries ran past the deadline");
            clock.advance(Duration::from_millis(1));
            Err(io::Error::from(io::ErrorKind::Interrupted))
        },
    );
    let error = result.expect_err("never connects");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    assert!(
        (20..=21).contains(&attempts.get()),
        "attempts stop once the deadline passes: {}",
        attempts.get()
    );
}

/// A queue that frees only after the deadline is not connected to late: no
/// attempt runs at or after the deadline, and each sleep is bounded by the
/// time left.
#[test]
fn a_connect_retry_never_runs_or_succeeds_after_the_deadline() {
    let clock = FakeClock::new();
    let timeout = Duration::from_millis(12);
    let deadline = clock.now() + timeout;
    let attempt_times = std::cell::RefCell::new(Vec::new());
    let sleeps = std::cell::RefCell::new(Vec::new());
    let result = retry_connect(
        deadline,
        || clock.now(),
        |duration| {
            sleeps.borrow_mut().push(duration);
            clock.advance(duration);
        },
        || {
            let at = clock.elapsed.get();
            attempt_times.borrow_mut().push(at);
            // The queue frees exactly at the deadline.
            if at >= timeout {
                Ok(())
            } else {
                Err(would_block())
            }
        },
    );
    let error = result.expect_err("the deadline passed before the queue freed");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    assert!(
        attempt_times.borrow().iter().all(|at| *at < timeout),
        "no attempt at or after the deadline: {:?}",
        attempt_times.borrow()
    );
    assert_eq!(
        *sleeps.borrow(),
        [
            CONNECT_RETRY_INTERVAL,
            CONNECT_RETRY_INTERVAL,
            timeout - CONNECT_RETRY_INTERVAL * 2
        ],
        "the last sleep is the time left, not a full interval"
    );
}

/// The first attempt is always made, so a zero timeout still connects to a
/// ready listener and reports a full queue as a timeout.
#[test]
fn the_first_connect_attempt_runs_even_with_no_time_left() {
    let clock = FakeClock::new();
    let attempts = std::cell::Cell::new(0u32);
    retry_connect(
        clock.now(),
        || clock.now(),
        |_| panic!("no time to sleep"),
        || {
            attempts.set(attempts.get() + 1);
            Ok(())
        },
    )
    .expect("a ready listener connects at once");
    assert_eq!(attempts.get(), 1);
    let error = retry_connect(
        clock.now(),
        || clock.now(),
        |_| panic!("no time to sleep"),
        || Err(would_block()),
    )
    .expect_err("a full queue with no time left");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
}

/// A queue that frees before the deadline is connected to, and any other error
/// is handed back untouched for the caller.
#[test]
fn a_connect_retry_succeeds_before_the_deadline_and_passes_other_errors_on() {
    let clock = FakeClock::new();
    let deadline = clock.now() + Duration::from_secs(1);
    let mut full = 3;
    retry_connect(
        deadline,
        || clock.now(),
        |duration| clock.advance(duration),
        || {
            if full > 0 {
                full -= 1;
                Err(would_block())
            } else {
                Ok(())
            }
        },
    )
    .expect("the queue freed in time");
    assert_eq!(clock.elapsed.get(), CONNECT_RETRY_INTERVAL * 3);
    let error = retry_connect(
        deadline,
        || clock.now(),
        |_| panic!("not retried"),
        || Err(io::Error::from_raw_os_error(libc::ECONNREFUSED)),
    )
    .expect_err("refused");
    assert_eq!(error.raw_os_error(), Some(libc::ECONNREFUSED));
}

/// A `WouldBlock` accept is an idle poll, not a failure: it ends the transient
/// streak, so errors spread over a long session never reach the limit.
#[test]
fn an_idle_accept_resets_the_transient_error_streak() {
    let aborted = io::Error::from_raw_os_error(libc::ECONNABORTED);
    let idle = io::Error::from(io::ErrorKind::WouldBlock);
    let mut streak = 0;
    for _ in 0..10 * MAX_TRANSIENT_ACCEPT_ERRORS {
        for _ in 0..MAX_TRANSIENT_ACCEPT_ERRORS {
            assert_eq!(accept_error_step(&aborted, &mut streak), AcceptStep::Retry);
        }
        assert_eq!(accept_error_step(&idle, &mut streak), AcceptStep::Idle);
        assert_eq!(streak, 0);
    }
    // Without an idle poll between them the same errors still fault.
    assert_eq!(accept_error_step(&aborted, &mut streak), AcceptStep::Retry);
    for _ in 0..MAX_TRANSIENT_ACCEPT_ERRORS {
        if accept_error_step(&aborted, &mut streak) == AcceptStep::Fault {
            return;
        }
    }
    panic!("consecutive transient errors still fault");
}

/// A failure before the first request byte is written is marked not delivered:
/// a missing or wrongly permitted endpoint (the path check) and a busy one (the
/// connect). The peer-credential and stream-setup marks sit on failures this
/// process cannot provoke as its own user, so they stay untested.
#[test]
fn failures_before_the_first_written_byte_are_marked_not_delivered() {
    let fixture = fixture();
    let request = capabilities_request(3);
    let timeout = Duration::from_millis(200);
    // No endpoint at all.
    let error = request_with_deadlines(&fixture.socket, &request, timeout, timeout)
        .expect_err("nothing is bound");
    assert!(protocol::was_not_delivered(&error), "{error}");
    // An endpoint with the wrong mode.
    let listener = UnixListener::bind(&fixture.socket).expect("bind");
    fs::set_permissions(&fixture.socket, Permissions::from_mode(0o644)).expect("chmod");
    let error = request_with_deadlines(&fixture.socket, &request, timeout, timeout)
        .expect_err("a group-readable endpoint is refused");
    assert!(protocol::was_not_delivered(&error), "{error}");
    drop(listener);
    // A busy endpoint: the connect times out against a full accept queue.
    #[cfg(target_os = "linux")]
    {
        fs::remove_file(&fixture.socket).expect("remove the first fixture socket");
        let (listener, _queued) = saturated_listener(&fixture.socket);
        fs::set_permissions(&fixture.socket, Permissions::from_mode(0o600)).expect("chmod");
        let error = request_with_deadlines(&fixture.socket, &request, timeout, timeout)
            .expect_err("nobody accepts");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        assert!(protocol::was_not_delivered(&error), "{error}");
        drop(listener);
    }
}

/// A failure after the request was written is not marked: the owner may have
/// acted on it. The peer here reads the request and closes without replying.
#[test]
fn a_failure_after_the_request_is_written_is_not_marked_not_delivered() {
    let fixture = fixture();
    let listener = UnixListener::bind(&fixture.socket).expect("bind");
    fs::set_permissions(&fixture.socket, Permissions::from_mode(0o600)).expect("chmod");
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        protocol::read_request(&mut stream).expect("the request arrives whole");
        // Dropped without a reply.
    });
    let timeout = Duration::from_secs(5);
    let error = request_with_deadlines(&fixture.socket, &capabilities_request(4), timeout, timeout)
        .expect_err("the peer closed without a reply");
    peer.join().expect("peer");
    assert!(!protocol::was_not_delivered(&error), "{error}");
}
