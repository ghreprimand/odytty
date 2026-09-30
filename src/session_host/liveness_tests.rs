// SPDX-License-Identifier: GPL-3.0-only
//! Bounded registry and control-operation liveness regressions.

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::protocol::{HostHello, read_client_hello, write_host_hello};
use super::{kill_session, list_live_sessions, prepare_runtime_dir, session_socket_path};

struct TestRuntime {
    base: PathBuf,
    dir: PathBuf,
}

impl TestRuntime {
    fn new() -> Self {
        let base = crate::test_dirs::fresh_socket_dir("olv");
        let dir = prepare_runtime_dir(&base).expect("prepare private runtime directory");
        Self { base, dir }
    }

    fn socket(&self, id: &str) -> PathBuf {
        session_socket_path(&self.dir, id).expect("valid synthetic session id")
    }
}

impl Drop for TestRuntime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn navigator_listing_does_not_wait_for_a_detached_host_handshake() {
    let runtime = TestRuntime::new();
    let socket = runtime.socket("slow");
    let _listener = UnixListener::bind(&socket).expect("bind wedged host surface");

    let start = Instant::now();
    let sessions = list_live_sessions(Some(&runtime.base)).expect("list sessions");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "navigator listing blocked for {elapsed:?} on a host that never accepts"
    );
    assert_eq!(sessions.len(), 1, "a listening host remains represented");
    assert_eq!(sessions[0].id, "slow");
    assert_eq!(sessions[0].state, "running");
}

#[test]
fn navigator_liveness_probe_sends_no_attach_hello_or_snapshot_request() {
    let runtime = TestRuntime::new();
    let socket = runtime.socket("observe");
    let listener = UnixListener::bind(&socket).expect("bind observing host");
    listener
        .set_nonblocking(true)
        .expect("nonblocking fake listener");
    let (tx, rx) = std::sync::mpsc::channel();
    let host = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break Some(stream),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fake host accept failed: {error}"),
            }
        };
        let Some(ref mut stream) = stream else {
            let _ = tx.send(None);
            return;
        };
        // Watch for 120 ms without socket timeouts: macOS rejects
        // SO_RCVTIMEO with EINVAL on an accepted connection whose peer has
        // already closed, which is exactly what a handshake-free probe does.
        // End of stream or silence both mean the probe sent nothing.
        stream
            .set_nonblocking(true)
            .expect("nonblocking fake host read");
        let watch_end = Instant::now() + Duration::from_millis(120);
        let mut bytes = [0_u8; 1];
        let observed = loop {
            match std::io::Read::read(stream, &mut bytes) {
                Ok(count) => break Some(count),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= watch_end {
                        break Some(0);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => break None,
            }
        };
        let _ = tx.send(observed);
    });

    let _ = list_live_sessions(Some(&runtime.base)).expect("list sessions");
    let observed = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("fake host observation completes");
    host.join().expect("fake host thread");
    assert_eq!(
        observed,
        Some(0),
        "listing must not send an attach hello that makes the host capture a snapshot"
    );
}

#[test]
fn kill_distinguishes_missing_sessions_from_a_host_that_does_not_respond() {
    let runtime = TestRuntime::new();

    assert!(
        kill_session(Some(&runtime.base), "missing").is_ok(),
        "an absent session is already gone"
    );

    let socket = runtime.socket("wedged");
    let _listener = UnixListener::bind(&socket).expect("bind nonaccepting host");
    let start = Instant::now();
    let result = kill_session(Some(&runtime.base), "wedged");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "kill operation exceeded its bounded handshake deadline: {elapsed:?}"
    );
    assert!(
        result.is_err(),
        "a live but unresponsive session must not be reported as already gone"
    );
    assert!(
        socket.exists(),
        "failure must leave the live session registered"
    );
}

#[test]
fn kill_treats_a_stale_socket_as_an_already_absent_session() {
    let runtime = TestRuntime::new();
    let socket = runtime.socket("stale");
    drop(UnixListener::bind(&socket).expect("bind stale socket"));

    assert!(
        kill_session(Some(&runtime.base), "stale").is_ok(),
        "a socket whose listener has gone is already absent"
    );
}

#[test]
fn rejecting_host_is_reported_as_a_failed_kill_operation() {
    let runtime = TestRuntime::new();
    let socket = runtime.socket("reject");
    let listener = UnixListener::bind(&socket).expect("bind rejecting host");
    let host = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        let hello = read_client_hello(&mut stream).expect("read client hello");
        assert_eq!(hello.session_id, "reject");
        write_host_hello(&mut stream, &HostHello::rejected("test failure"))
            .expect("send rejection");
    });

    let result = kill_session(Some(&runtime.base), "reject");
    host.join().expect("rejecting host thread");
    assert!(
        result.is_err(),
        "a host rejection must remain distinguishable from an absent session"
    );
}
