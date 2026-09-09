// SPDX-License-Identifier: GPL-3.0-only
//! Named-pipe framing and lifecycle coverage, run by the Windows CI leg.

use super::*;
use crate::automation::dispatch::{self, MAX_PER_DISPATCH};
use crate::automation::protocol::{Action, MAX_MESSAGE_BYTES, VERSION};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::mpsc::{self, RecvTimeoutError};

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
    ] {
        assert_eq!(
            wide_endpoint(Path::new(endpoint))
                .expect_err("non-local or malformed endpoint")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
