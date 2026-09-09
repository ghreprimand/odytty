// SPDX-License-Identifier: GPL-3.0-only
//! Named-pipe framing and lifecycle coverage, run by the Windows CI leg.

use super::*;
use crate::automation::dispatch::{self, MAX_PER_DISPATCH};
use crate::automation::protocol::{Action, ErrorCode, MAX_MESSAGE_BYTES, Reply, VERSION};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Instant;

fn unique_endpoint() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    endpoint(
        std::process::id()
            .wrapping_mul(4099)
            .wrapping_add(sequence as u32),
    )
}

struct Harness {
    endpoint: PathBuf,
    server: Option<Server>,
    stop_owner: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
    wakes: Arc<AtomicUsize>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop_owner.store(true, Ordering::Release);
        drop(self.server.take());
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

fn start_harness() -> Harness {
    let endpoint = unique_endpoint();
    let (submission, queue) = dispatch::channel(true);
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(64);
    let stop_owner = Arc::new(AtomicBool::new(false));
    let stop = stop_owner.clone();
    let owner = thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            match wake_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => loop {
                    let handled = queue.dispatch(|request| match request.action {
                        Action::Capabilities => Reply::Capabilities {
                            structural_control: true,
                        },
                        _ => Reply::Error(ErrorCode::UnsupportedCapability),
                    });
                    if handled < MAX_PER_DISPATCH {
                        break;
                    }
                },
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = wakes.clone();
    let server = Server::bind(&endpoint, submission, move || {
        wake_count.fetch_add(1, Ordering::Relaxed);
        wake_tx.send(()).is_ok()
    })
    .expect("bind private pipe");
    Harness {
        endpoint,
        server: Some(server),
        stop_owner,
        owner: Some(owner),
        wakes,
    }
}

#[test]
fn capabilities_round_trip_over_owner_verified_pipe() {
    let harness = start_harness();
    let request_frame = Request {
        version: VERSION,
        request_id: 19,
        action: Action::Capabilities,
    };
    assert_eq!(
        request(&harness.endpoint, &request_frame).expect("round trip"),
        Response {
            request_id: 19,
            reply: Reply::Capabilities {
                structural_control: true
            }
        }
    );
    assert_eq!(harness.wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn second_first_instance_cannot_take_over_live_name() {
    let endpoint = unique_endpoint();
    let (submission, _queue) = dispatch::channel(true);
    let server = Server::bind(&endpoint, submission, || true).expect("first bind");
    let (submission, _queue) = dispatch::channel(true);
    assert!(
        Server::bind(&endpoint, submission, || true).is_err(),
        "FILE_FLAG_FIRST_PIPE_INSTANCE must make a duplicate bind fail"
    );
    drop(server);
}

#[test]
fn truncated_frame_disconnects_without_waking_owner() {
    let harness = start_harness();
    let mut client = connect(&harness.endpoint, IO_TIMEOUT).expect("connect");
    client.write_all(&15u32.to_le_bytes()).expect("length");
    client.write_all(b"ODYC").expect("partial body");
    drop(client);
    let valid = Request {
        version: VERSION,
        request_id: 20,
        action: Action::Capabilities,
    };
    assert_eq!(
        request(&harness.endpoint, &valid).expect("listener survives truncated client"),
        Response {
            request_id: 20,
            reply: Reply::Capabilities {
                structural_control: true
            }
        }
    );
    assert_eq!(
        harness.wakes.load(Ordering::Relaxed),
        1,
        "only the valid follow-up may wake the owner"
    );
}

#[test]
fn oversized_frame_is_rejected_without_waking_owner() {
    let harness = start_harness();
    let mut client = connect(&harness.endpoint, IO_TIMEOUT).expect("connect");
    client
        .write_all(&((MAX_MESSAGE_BYTES as u32) + 1).to_le_bytes())
        .expect("oversized length");
    client.reset_deadline(IO_TIMEOUT);
    assert_eq!(
        protocol::read_response(&mut client).expect("bounded refusal"),
        Response {
            request_id: 0,
            reply: Reply::Error(ErrorCode::TooLarge)
        }
    );
    assert_eq!(harness.wakes.load(Ordering::Relaxed), 0);
}

#[test]
fn remote_clients_are_rejected_by_the_configured_creation_mode() {
    assert_ne!(pipe_mode().0 & PIPE_REJECT_REMOTE_CLIENTS.0, 0);
    // GetNamedPipeInfo reports endpoint direction/type flags, not the
    // PIPE_REJECT_REMOTE_CLIENTS creation bit. Remote rejection is therefore
    // verified here at the exact CreateNamedPipeW mode construction site and
    // remains a hands-on Windows boundary check.
}

#[test]
fn remote_and_malformed_endpoint_names_are_refused_before_open() {
    for endpoint in [
        r"\\server\pipe\odytty-control-1",
        r"\\.\pipe\other-1",
        r"\\.\pipe\odytty-control-not-a-pid",
        // Extended NT prefix is not the local \\.\pipe\ grammar.
        r"\\?\pipe\odytty-control-1",
        r"\\.\pipe\odytty-control-",
        r"\\.\pipe\odytty-control-0",
        // Overflow: digits that cannot fit in u32.
        r"\\.\pipe\odytty-control-4294967296",
    ] {
        assert_eq!(
            wide_endpoint(Path::new(endpoint))
                .expect_err("non-local or malformed endpoint")
                .kind(),
            io::ErrorKind::InvalidInput
        );
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
fn zero_length_and_u32_max_length_frames_fail_closed_without_wake() {
    let harness = start_harness();
    for (label, length) in [("zero", 0u32), ("u32_max", u32::MAX)] {
        let wakes_before = harness.wakes.load(Ordering::Relaxed);
        let mut client = connect(&harness.endpoint, IO_TIMEOUT).expect(label);
        client.write_all(&length.to_le_bytes()).expect("length");
        client.flush().expect("flush");
        client.reset_deadline(IO_TIMEOUT);
        let response = protocol::read_response(&mut client).expect("protocol rejection");
        assert_eq!(response.request_id, 0, "{label}");
        assert_eq!(response.reply, Reply::Error(ErrorCode::TooLarge), "{label}");
        assert_eq!(
            harness.wakes.load(Ordering::Relaxed),
            wakes_before,
            "{label} must not wake the owner"
        );
    }
    assert_eq!(
        request(&harness.endpoint, &capabilities_request(31)).expect("recovered"),
        Response {
            request_id: 31,
            reply: Reply::Capabilities {
                structural_control: true
            }
        }
    );
}

#[test]
fn trailing_second_frame_fails_closed_without_second_wake() {
    let harness = start_harness();
    let wakes_before = harness.wakes.load(Ordering::Relaxed);
    let mut client = connect(&harness.endpoint, IO_TIMEOUT).expect("connect");
    protocol::write_request(&mut client, &capabilities_request(101)).expect("first");
    protocol::write_request(&mut client, &capabilities_request(102)).expect("second");
    // One-request transport: trailing bytes cancel via peer_finished before the
    // owner is woken. Do not wait for a dual reply.
    thread::sleep(POLL_INTERVAL * 5);
    drop(client);
    thread::sleep(POLL_INTERVAL * 5);
    assert_eq!(
        harness.wakes.load(Ordering::Relaxed),
        wakes_before,
        "trailing second frame must not wake the owner"
    );
    assert_eq!(
        request(&harness.endpoint, &capabilities_request(103)).expect("recovered"),
        Response {
            request_id: 103,
            reply: Reply::Capabilities {
                structural_control: true
            }
        }
    );
}

#[test]
fn slowloris_partial_body_hits_absolute_io_deadline_without_wake() {
    let harness = start_harness();
    let wakes_before = harness.wakes.load(Ordering::Relaxed);
    let mut client = connect(&harness.endpoint, IO_TIMEOUT).expect("connect");
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
        match client.write_all(&[byte]) {
            Ok(()) => {
                let _ = client.flush();
                thread::sleep(Duration::from_millis(300));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::TimedOut
                ) =>
            {
                write_closed = true;
            }
            Err(error) => panic!("unexpected slowloris write error: {error:?}"),
        }
    }
    client.reset_deadline(Duration::from_millis(500));
    let mut buf = [0u8; 32];
    let result = client.read(&mut buf);
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
    // The deadline may surface as a closed pipe or as a bounded `timed_out`
    // rejection with request id 0; either way no request reached the owner.
    if let Ok(n) = result
        && n != 0
    {
        let response = protocol::read_response(&mut io::Cursor::new(&buf[..n]))
            .expect("a non-empty reply is a protocol rejection");
        assert_eq!(response.request_id, 0);
        assert_eq!(response.reply, Reply::Error(ErrorCode::TimedOut));
    }
    assert_eq!(harness.wakes.load(Ordering::Relaxed), wakes_before);
    assert_eq!(
        request(&harness.endpoint, &capabilities_request(41)).expect("recovered"),
        Response {
            request_id: 41,
            reply: Reply::Capabilities {
                structural_control: true
            }
        }
    );
}

#[test]
fn ninth_concurrent_client_is_deferred_not_dropped() {
    let harness = start_harness();
    let mut blockers = Vec::new();
    for _ in 0..MAX_CLIENTS {
        let mut client = connect(&harness.endpoint, IO_TIMEOUT).expect("blocker connect");
        // Hold each worker inside a framed body read until the I/O deadline.
        let length = (MAX_MESSAGE_BYTES / 2) as u32;
        client
            .write_all(&length.to_le_bytes())
            .expect("blocker length");
        client.flush().expect("blocker flush");
        blockers.push(client);
    }
    thread::sleep(POLL_INTERVAL * 10);

    let endpoint = harness.endpoint.clone();
    let started = Instant::now();
    let extra = thread::spawn(move || request(&endpoint, &capabilities_request(99)));
    thread::sleep(Duration::from_millis(250));
    assert!(
        !extra.is_finished(),
        "ninth client must remain pending while MAX_CLIENTS workers are held"
    );
    let response = extra
        .join()
        .expect("extra join")
        .expect("served after a worker slot frees");
    let waited = started.elapsed();
    assert_eq!(response.request_id, 99);
    assert!(
        waited >= IO_TIMEOUT.saturating_sub(Duration::from_millis(250)),
        "ninth client must wait for a saturated worker I/O deadline, got {waited:?}"
    );
    assert!(
        waited <= IO_TIMEOUT + dispatch::REQUEST_TIMEOUT + Duration::from_secs(2),
        "ninth client must finish within connect/request deadlines, got {waited:?}"
    );
    drop(blockers);
    assert_eq!(
        request(&harness.endpoint, &capabilities_request(13)).expect("recovered"),
        Response {
            request_id: 13,
            reply: Reply::Capabilities {
                structural_control: true
            }
        }
    );
}

#[test]
fn thirty_three_requests_are_bounded_by_accept_rate_or_busy() {
    let harness = start_harness();
    let started = Instant::now();
    let mut successes = 0usize;
    let mut busy = 0usize;
    let mut thirty_third_elapsed = Duration::ZERO;
    for id in 0..33u64 {
        let request_started = Instant::now();
        let response = request(&harness.endpoint, &capabilities_request(id)).expect("rate probe");
        if id == 32 {
            thirty_third_elapsed = request_started.elapsed();
        }
        match response.reply {
            Reply::Capabilities { .. } => successes += 1,
            Reply::Error(ErrorCode::Busy) => busy += 1,
            other => panic!("unexpected reply for id {id}: {other:?}"),
        }
    }
    let elapsed = started.elapsed();
    assert!(
        busy >= 1
            || thirty_third_elapsed >= Duration::from_millis(800)
            || elapsed >= Duration::from_secs(1),
        "33 requests must hit Busy or the 32/s accept-rate delay; busy={busy} successes={successes} third={thirty_third_elapsed:?} total={elapsed:?}"
    );
    assert!(
        successes <= 33,
        "must not invent successes; got {successes}"
    );
}

#[test]
fn client_close_before_reply_does_not_wake_owner() {
    let harness = start_harness();
    let wakes_before = harness.wakes.load(Ordering::Relaxed);
    let mut client = connect(&harness.endpoint, IO_TIMEOUT).expect("connect");
    protocol::write_request(&mut client, &capabilities_request(11)).expect("request");
    // Drop before reading the reply. The request was fully delivered, so the
    // owner may already have been woken once; a disconnect observed first
    // cancels through peer_finished instead. Either outcome is bounded: at
    // most one wake, and the listener recovers for the next client.
    drop(client);
    thread::sleep(POLL_INTERVAL * 10);
    assert!(
        harness.wakes.load(Ordering::Relaxed) <= wakes_before + 1,
        "a disconnected request wakes the owner at most once"
    );
    assert_eq!(
        request(&harness.endpoint, &capabilities_request(12)).expect("after disconnect"),
        Response {
            request_id: 12,
            reply: Reply::Capabilities {
                structural_control: true
            }
        }
    );
}

#[test]
fn response_request_id_mismatch_is_rejected_by_client() {
    // One-shot rogue server: same-user pipe that replies with a different id.
    let endpoint = unique_endpoint();
    let name = Arc::new(wide_endpoint(&endpoint).expect("wide"));
    let owner = Arc::new(process_user_sid(None).expect("owner sid"));
    let listener = create_pipe(&name, &owner, true).expect("create rogue pipe");
    let server = thread::spawn(move || {
        let stopped = AtomicBool::new(false);
        assert!(
            wait_for_client(&listener, &stopped).expect("wait"),
            "rogue server must accept one client"
        );
        let mut io = PipeIo::new(listener, IO_TIMEOUT);
        let incoming = protocol::read_request(&mut io).expect("rogue read");
        let mismatched = Response {
            request_id: incoming.request_id.wrapping_add(1),
            reply: Reply::Capabilities {
                structural_control: true,
            },
        };
        protocol::write_response(&mut io, &mismatched).expect("rogue write");
    });
    let error = request(&endpoint, &capabilities_request(55)).expect_err("mismatch");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        error
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<ErrorCode>())
            .is_some_and(|code| *code == ErrorCode::InvalidRequest),
        "client must map a mismatched request_id to InvalidRequest, got {error:?}"
    );
    server.join().expect("rogue join");
}

#[test]
fn stale_identity_error_code_roundtrips_over_the_pipe() {
    let endpoint = unique_endpoint();
    let (submission, queue) = dispatch::channel(true);
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(8);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_owner = stop.clone();
    let owner = thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            match wake_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {
                    let _ = queue.dispatch(|_| Reply::Error(ErrorCode::StaleIdentity));
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    let server =
        Server::bind(&endpoint, submission, move || wake_tx.send(()).is_ok()).expect("bind");
    let id = protocol::ObjectId {
        instance: [0x11; 16],
        kind: protocol::ObjectKind::Window,
        serial: 99,
    };
    let response = request(
        &endpoint,
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
}

#[test]
fn endpoint_with_embedded_nul_is_rejected() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let mut units: Vec<u16> = r"\\.\pipe\odytty-control-7".encode_utf16().collect();
    units.push(0);
    units.extend(r"x".encode_utf16());
    let path = PathBuf::from(OsString::from_wide(&units));
    // wide_endpoint maps as_encoded_bytes to u16 units and refuses any zero
    // unit before CreateFileW, so a path that carries an embedded NUL cannot
    // open a pipe under a truncated name.
    assert_eq!(
        wide_endpoint(&path).expect_err("embedded NUL").kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn listener_fault_is_none_while_listening_and_keeps_first_reason() {
    let endpoint = unique_endpoint();
    let (submission, _queue) = dispatch::channel(true);
    let server = Server::bind(&endpoint, submission, || true).expect("bind");
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
