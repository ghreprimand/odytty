// SPDX-License-Identifier: GPL-3.0-only
//! Attach admission and frame-send bounds: connections that never finish
//! their hello, or that read slowly, must not stall the host loop.

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

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

/// Send a shutdown frame and return how long the host took to exit.
fn shut_down(
    mut client: SessionHostClient,
    host: thread::JoinHandle<anyhow::Result<super::HostExit>>,
) -> Duration {
    let start = Instant::now();
    client.shutdown().expect("shutdown frame");
    drop(client);
    let exit = host.join().expect("host thread").expect("host exit");
    assert_eq!(exit.reason, HostExitReason::Killed);
    start.elapsed()
}

#[test]
fn connections_that_never_send_a_hello_do_not_delay_pty_output() {
    let runtime = Runtime::new();
    let config = echo_host(&runtime, "nohello");
    let socket = config.runtime_paths().expect("paths").socket;
    let host = thread::spawn(move || run_host(config));
    wait_for_socket(&socket);
    let mut client = attach(&socket, "nohello");
    assert!(echo_latency(&mut client, "warm") < WAIT);

    // Six silent connections: each used to hold the host loop for the whole
    // two-second hello deadline, one after another.
    let silent: Vec<UnixStream> = (0..6)
        .map(|_| UnixStream::connect(&socket).expect("silent connect"))
        .collect();
    thread::sleep(Duration::from_millis(50));
    let latency = echo_latency(&mut client, "during");
    assert!(
        latency < Duration::from_secs(1),
        "PTY output waited {latency:?} behind connections that sent no hello"
    );
    let shutdown = shut_down(client, host);
    assert!(
        shutdown < Duration::from_secs(1),
        "shutdown took {shutdown:?} with silent connections pending"
    );
    drop(silent);
}

#[test]
fn a_dribbled_hello_does_not_hold_the_host_loop() {
    let runtime = Runtime::new();
    let config = echo_host(&runtime, "dribble");
    let socket = config.runtime_paths().expect("paths").socket;
    let host = thread::spawn(move || run_host(config));
    wait_for_socket(&socket);
    let mut client = attach(&socket, "dribble");

    let mut slow = UnixStream::connect(&socket).expect("slow connect");
    let dribbler = thread::spawn(move || {
        for byte in HOST_PROTOCOL_MAGIC.iter().take(6) {
            if slow.write_all(&[*byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(300));
        }
        slow
    });
    thread::sleep(Duration::from_millis(100));
    let latency = echo_latency(&mut client, "dribbled");
    assert!(
        latency < Duration::from_secs(1),
        "PTY output waited {latency:?} behind a dribbled hello"
    );
    drop(dribbler.join().expect("dribbler"));
    shut_down(client, host);
}

#[test]
fn a_new_client_attaches_while_a_silent_connection_is_pending() {
    let runtime = Runtime::new();
    let config = echo_host(&runtime, "pending");
    let socket = config.runtime_paths().expect("paths").socket;
    let host = thread::spawn(move || run_host(config));
    wait_for_socket(&socket);

    let _silent = UnixStream::connect(&socket).expect("silent connect");
    thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    let mut client = attach(&socket, "pending");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "a real attach waited {elapsed:?} behind a silent connection"
    );
    assert!(echo_latency(&mut client, "ok") < WAIT);
    shut_down(client, host);
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
    let host = thread::spawn(move || run_host(config));
    wait_for_socket(&socket);
    let mut client = attach(&socket, "slowread");
    // Let the scrollback fill so the next snapshot is far larger than a
    // socket buffer.
    assert!(echo_latency(&mut client, "filled") < WAIT);

    let mut slow = UnixStream::connect(&socket).expect("slow connect");
    super::protocol::write_client_hello(
        &mut slow,
        &super::protocol::ClientHello::current("slowread"),
    )
    .expect("slow hello");
    let (snapshot_started_tx, snapshot_started_rx) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        use std::io::Read;
        slow.set_read_timeout(Some(WAIT))
            .expect("slow read timeout");
        super::protocol::read_host_hello(&mut slow).expect("slow host hello");
        // Snapshot capture and encoding precede the frame send deadline. Wait
        // for its header so instrumentation overhead in that setup
        // does not become part of the write-liveness measurement.
        let mut header = [0u8; 5];
        slow.read_exact(&mut header).expect("snapshot header");
        assert_eq!(header[0], 1, "the first host frame must be a snapshot");
        let payload_len = u32::from_be_bytes(header[1..].try_into().expect("length"));
        assert!(
            payload_len > 8 * 1024 * 1024,
            "snapshot must outlast the slow reader"
        );
        snapshot_started_tx.send(()).expect("snapshot started");
        let mut chunk = vec![0u8; 64 * 1024];
        let start = Instant::now();
        let end = start + Duration::from_secs(8);
        let mut total = 0;
        while Instant::now() < end {
            match slow.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    total += n;
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        assert!(
            total < payload_len as usize,
            "the slow reader must receive a truncated snapshot"
        );
    });
    snapshot_started_rx
        .recv_timeout(WAIT)
        .expect("snapshot write did not start");
    let latency = echo_latency(&mut client, "slow");
    assert!(
        latency < Duration::from_millis(3500),
        "PTY output waited {latency:?} behind a slowly read snapshot"
    );
    reader.join().expect("slow reader");
    shut_down(client, host);
}
