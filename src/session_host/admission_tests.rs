// SPDX-License-Identifier: GPL-3.0-only
//! Attach admission and frame-send bounds: connections that never finish
//! their hello, or that read slowly, must not stall the host loop.

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use super::host::{ATTACH_HANDSHAKE_TIMEOUT, FRAME_SEND_DEADLINE};
use super::protocol::{HOST_PROTOCOL_MAGIC, HostFrame};
use super::{HostCommand, HostConfig, HostExitReason, SessionHostClient, run_host};
use crate::core::Dimensions;

const WAIT: Duration = Duration::from_secs(15);

struct Runtime(PathBuf);

impl Runtime {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_socket_dir("oad"))
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn echo_host(runtime: &Runtime, id: &str) -> HostConfig {
    let mut config = HostConfig::new(id);
    config.runtime_base = Some(runtime.0.clone());
    config.command = HostCommand::Exec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "while read line; do printf 'got:%s\\n' \"$line\"; done".into(),
        ],
        working_directory: None,
    };
    config.detached_idle_timeout = Duration::from_secs(20);
    config.dimensions = Dimensions::new(80, 24);
    config
}

fn wait_for_socket(path: &std::path::Path) {
    let deadline = Instant::now() + WAIT;
    while UnixStream::connect(path).is_err() {
        assert!(Instant::now() < deadline, "host socket never appeared");
        thread::sleep(Duration::from_millis(10));
    }
}

fn attach(path: &std::path::Path, id: &str) -> SessionHostClient {
    let mut client = SessionHostClient::connect(path, id).expect("attach");
    let deadline = Instant::now() + WAIT;
    loop {
        match client.read_frame(Duration::from_millis(50)).expect("frame") {
            Some(HostFrame::Snapshot(_)) => return client,
            _ if Instant::now() < deadline => {}
            other => panic!("missing snapshot: {other:?}"),
        }
    }
}

/// Send `line` and return how long its echo took to arrive.
fn echo_latency(client: &mut SessionHostClient, line: &str) -> Duration {
    let start = Instant::now();
    client
        .send_input(format!("{line}\n").as_bytes())
        .expect("send input");
    let needle = format!("got:{line}");
    let mut seen = Vec::new();
    while start.elapsed() < WAIT {
        if let Some(HostFrame::Output(bytes)) =
            client.read_frame(Duration::from_millis(20)).expect("frame")
        {
            seen.extend_from_slice(&bytes);
            if String::from_utf8_lossy(&seen).contains(&needle) {
                return start.elapsed();
            }
        }
    }
    panic!(
        "echo of {line:?} never arrived; output={}",
        String::from_utf8_lossy(&seen)
    );
}

/// How long this machine takes to echo a line when nothing hostile is
/// attached: the larger of three samples after the first (warm-up) echo.
fn echo_baseline(client: &mut SessionHostClient) -> Duration {
    let _ = echo_latency(client, "warm");
    (0..3)
        .map(|n| echo_latency(client, &format!("base{n}")))
        .max()
        .unwrap_or_default()
}

/// The latency a hostile connection may add on top of `baseline`. The bug
/// these tests guard held the host loop for a whole host-side deadline per
/// connection, so half of the shortest such deadline separates "served
/// promptly" from "waited behind the peer" on any machine, while the
/// baseline term scales the allowance when the machine itself is slow.
fn allowed_over(baseline: Duration, deadline: Duration) -> Duration {
    baseline * 2 + deadline / 2
}

/// Run `attempt` up to [`ATTEMPTS`] times and pass on the first latency within
/// `bound`. A stalled host loop delays every attempt by its deadline, so it
/// fails all of them; a scheduling hiccup on a loaded runner has to repeat
/// to fail. Returns the passing latency, or panics with every observation.
const ATTEMPTS: usize = 3;
fn prompt_within(
    what: &str,
    bound: Duration,
    mut attempt: impl FnMut(usize) -> Duration,
) -> Duration {
    let mut seen = Vec::new();
    for n in 0..ATTEMPTS {
        let latency = attempt(n);
        if latency < bound {
            return latency;
        }
        seen.push(latency);
    }
    panic!("{what}: every attempt exceeded {bound:?}; observed {seen:?}");
}

/// Own the host before readiness or attach assertions can unwind.
struct OwnedHost {
    socket: PathBuf,
    id: String,
    worker: Option<thread::JoinHandle<anyhow::Result<super::HostExit>>>,
}
impl OwnedHost {
    fn start(config: HostConfig) -> Self {
        let socket = config.runtime_paths().expect("paths").socket;
        Self {
            socket,
            id: config.session_id.clone(),
            worker: Some(thread::spawn(move || run_host(config))),
        }
    }
    fn finish(mut self, mut client: SessionHostClient) -> Duration {
        let start = Instant::now();
        client.shutdown().expect("shutdown frame");
        drop(client);
        let exit = join_within(&mut self.worker, "host").expect("host exit");
        assert_eq!(exit.reason, HostExitReason::Killed);
        start.elapsed()
    }
}
impl Drop for OwnedHost {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            if !worker.is_finished()
                && let Ok(mut client) = SessionHostClient::connect(&self.socket, &self.id)
            {
                // The host installs its command reader only after the snapshot
                // is sent. Closing before draining it can discard Shutdown.
                let _ = client.read_frame(Duration::from_secs(2));
                let _ = client.shutdown();
            }
            // Cleanup must never turn the original assertion into an unbounded join.
            let end = Instant::now() + Duration::from_secs(5);
            while !worker.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                eprintln!("owned admission host exceeded cleanup deadline");
            }
        }
    }
}
fn join_within<T>(worker: &mut Option<thread::JoinHandle<T>>, label: &str) -> T {
    join_within_budget(worker, label, WAIT)
}
fn join_within_budget<T>(
    worker: &mut Option<thread::JoinHandle<T>>,
    label: &str,
    budget: Duration,
) -> T {
    let handle = worker.as_ref().expect("owned worker");
    let end = Instant::now() + budget;
    while !handle.is_finished() && Instant::now() < end {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(handle.is_finished(), "{label} exceeded join deadline");
    worker
        .take()
        .expect("owned worker")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
struct OwnedPeer<T> {
    socket: UnixStream,
    worker: Option<thread::JoinHandle<T>>,
}
impl<T> OwnedPeer<T> {
    fn start(socket: UnixStream, run: impl FnOnce() -> T + Send + 'static) -> Self
    where
        T: Send + 'static,
    {
        Self {
            socket,
            worker: Some(thread::spawn(run)),
        }
    }
    fn finish(mut self) -> T {
        join_within(&mut self.worker, "peer")
    }
}
impl<T> Drop for OwnedPeer<T> {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let end = Instant::now() + Duration::from_secs(5);
            while !worker.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                eprintln!("owned admission peer exceeded cleanup deadline");
            }
        }
    }
}

#[test]
fn connections_that_never_send_a_hello_do_not_delay_pty_output() {
    let runtime = Runtime::new();
    let config = echo_host(&runtime, "nohello");
    let socket = config.runtime_paths().expect("paths").socket;
    let host = OwnedHost::start(config);
    wait_for_socket(&socket);
    let mut client = attach(&socket, "nohello");
    let baseline = echo_baseline(&mut client);

    // Six silent connections: each used to hold the host loop for the whole
    // two-second hello deadline, one after another. Each attempt opens a
    // fresh set, because the host drops silent peers at its deadline.
    let bound = allowed_over(baseline, ATTACH_HANDSHAKE_TIMEOUT);
    let mut silent = Vec::new();
    prompt_within("PTY output behind silent connections", bound, |n| {
        silent = (0..6)
            .map(|_| UnixStream::connect(&socket).expect("silent connect"))
            .collect();
        thread::sleep(Duration::from_millis(50));
        echo_latency(&mut client, &format!("during{n}"))
    });
    // Shutdown must not wait for even one pending hello deadline, let alone
    // six; the whole deadline is the allowance because it is one-shot.
    let shutdown = host.finish(client);
    assert!(
        shutdown < ATTACH_HANDSHAKE_TIMEOUT + baseline * 2,
        "shutdown took {shutdown:?} with silent connections pending"
    );
    drop(silent);
}

#[test]
fn a_dribbled_hello_does_not_hold_the_host_loop() {
    let runtime = Runtime::new();
    let config = echo_host(&runtime, "dribble");
    let socket = config.runtime_paths().expect("paths").socket;
    let host = OwnedHost::start(config);
    wait_for_socket(&socket);
    let mut client = attach(&socket, "dribble");

    let baseline = echo_baseline(&mut client);

    let bound = allowed_over(baseline, ATTACH_HANDSHAKE_TIMEOUT);
    prompt_within("PTY output behind a dribbled hello", bound, |n| {
        let mut slow = UnixStream::connect(&socket).expect("slow connect");
        slow.set_write_timeout(Some(Duration::from_millis(100)))
            .expect("dribbler write bound");
        let dribbler =
            OwnedPeer::start(slow.try_clone().expect("peer cleanup socket"), move || {
                for byte in HOST_PROTOCOL_MAGIC.iter().take(6) {
                    if slow.write_all(&[*byte]).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(300));
                }
                slow
            });
        thread::sleep(Duration::from_millis(100));
        let latency = echo_latency(&mut client, &format!("dribbled{n}"));
        drop(dribbler.finish());
        latency
    });
    host.finish(client);
}

#[test]
fn a_new_client_attaches_while_a_silent_connection_is_pending() {
    let runtime = Runtime::new();
    let config = echo_host(&runtime, "pending");
    let socket = config.runtime_paths().expect("paths").socket;
    let host = OwnedHost::start(config);
    wait_for_socket(&socket);

    // Baseline: an attach, and an echo, with nothing hostile pending.
    let start = Instant::now();
    let mut clients = vec![attach(&socket, "pending")];
    let attach_baseline = start.elapsed();
    let baseline = echo_baseline(&mut clients[0]).max(attach_baseline);
    let bound = allowed_over(baseline, ATTACH_HANDSHAKE_TIMEOUT);

    // Each attempt holds a fresh silent connection (the host drops one at its
    // deadline) and times one more real attach behind it.
    let mut silent = Vec::new();
    prompt_within("a real attach behind a silent connection", bound, |_| {
        silent.push(UnixStream::connect(&socket).expect("silent connect"));
        thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        clients.push(attach(&socket, "pending"));
        start.elapsed()
    });
    let mut client = clients.pop().expect("attached client");
    assert!(echo_latency(&mut client, "ok") < WAIT);
    drop(silent);
    drop(clients);
    host.finish(client);
}

#[test]
fn a_client_that_reads_its_snapshot_slowly_is_bounded_by_the_frame_deadline() {
    let runtime = Runtime::new();
    let mut config = echo_host(&runtime, "slowread");
    config.command = HostCommand::Exec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "awk 'BEGIN { for (i = 0; i < 12000; i++) \
             printf \"%08d 0123456789abcdef0123456789abcdef0123456789abcdef\\n\", i }'; \
             while read line; do printf 'got:%s\\n' \"$line\"; done"
                .into(),
        ],
        working_directory: None,
    };
    let socket = config.runtime_paths().expect("paths").socket;
    let host = OwnedHost::start(config);
    wait_for_socket(&socket);
    let mut client = attach(&socket, "slowread");
    // Let the scrollback fill so the next snapshot is far larger than a
    // socket buffer.
    assert!(echo_latency(&mut client, "filled") < WAIT);
    let baseline = echo_baseline(&mut client);
    // Frame send deadline plus the reader's own pacing: the host may spend its
    // whole send deadline on the slow peer, so that, scaled by this machine's
    // baseline, is the most a prompt echo can wait.
    let bound = baseline * 2 + FRAME_SEND_DEADLINE + FRAME_SEND_DEADLINE * 3 / 4;
    prompt_within("PTY output behind a slowly read snapshot", bound, |n| {
        let mut slow = UnixStream::connect(&socket).expect("slow connect");
        super::protocol::write_client_hello(
            &mut slow,
            &super::protocol::ClientHello::current("slowread"),
        )
        .expect("slow hello");
        let (snapshot_started_tx, snapshot_started_rx) = std::sync::mpsc::sync_channel(1);
        let reader = OwnedPeer::start(slow.try_clone().expect("peer cleanup socket"), move || {
            use std::io::Read;
            let mut guarded = super::SocketReadDeadline::new(&slow, Instant::now() + WAIT);
            super::protocol::read_host_hello(&mut guarded).expect("slow host hello");
            // Snapshot capture and encoding precede the frame send deadline. Wait
            // for its header so instrumentation overhead in that setup
            // does not become part of the write-liveness measurement.
            let mut header = [0u8; 5];
            guarded.read_exact(&mut header).expect("snapshot header");
            assert_eq!(header[0], 1, "the first host frame must be a snapshot");
            let payload_len = u32::from_be_bytes(header[1..].try_into().expect("length"));
            assert!(
                payload_len > 8 * 1024 * 1024,
                "snapshot must outlast the slow reader"
            );
            snapshot_started_tx.send(()).expect("snapshot started");
            let mut chunk = vec![0u8; 64 * 1024];
            let start = Instant::now();
            let end = start + Duration::from_secs(5);
            let mut guarded = super::SocketReadDeadline::new(&slow, end);
            let mut reached_eof = false;
            let mut total = 0;
            while Instant::now() < end {
                match guarded.read(&mut chunk) {
                    Ok(0) => {
                        reached_eof = true;
                        break;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => panic!("snapshot read failed before EOF: {error}"),
                    Ok(n) => {
                        total += n;
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            assert!(
                reached_eof,
                "snapshot connection did not close within the frame deadline allowance"
            );
            assert!(
                total < payload_len as usize,
                "the slow reader must receive a truncated snapshot"
            );
        });
        snapshot_started_rx
            .recv_timeout(WAIT)
            .expect("snapshot write did not start");
        let latency = echo_latency(&mut client, &format!("slow{n}"));
        reader.finish();
        latency
    });
    host.finish(client);
}

#[test]
fn owned_host_is_shut_down_when_an_admission_assertion_unwinds() {
    let runtime = Runtime::new();
    let config = echo_host(&runtime, "unwind");
    let socket = config.runtime_paths().expect("paths").socket;
    let result = std::panic::catch_unwind(|| {
        let _host = OwnedHost::start(config);
        wait_for_socket(&socket);
        let _client = attach(&socket, "unwind");
        panic!("synthetic admission assertion");
    });
    let panic = result.expect_err("synthetic assertion unwinds");
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"synthetic admission assertion")
    );
    assert!(
        !socket.exists(),
        "unwind cleanup joined the host and removed its socket"
    );
}
#[test]
fn a_join_timeout_preserves_worker_ownership_for_unwind_cleanup() {
    let (socket, mut peer) = UnixStream::pair().expect("pair");
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&released);
    let mut owned = OwnedPeer::start(
        socket.try_clone().expect("peer cleanup socket"),
        move || {
            let mut byte = [0];
            let n = std::io::Read::read(&mut peer, &mut byte).expect("peer observes cleanup EOF");
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            n
        },
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        join_within_budget(
            &mut owned.worker,
            "synthetic waiting peer",
            Duration::from_millis(10),
        )
    }));
    assert!(result.is_err());
    assert!(
        owned.worker.is_some(),
        "timed-out join retains its cleanup owner"
    );
    drop(owned);
    assert!(
        released.load(std::sync::atomic::Ordering::SeqCst),
        "unwind cleanup releases the waiting peer"
    );
}
