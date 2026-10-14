// SPDX-License-Identifier: GPL-3.0-only
//! Attach pump and client outcomes that must stay truthful: a later snapshot
//! that cannot be applied ends the attachment, the write-timeout setup accepts
//! only the proven macOS peer-closed failure, and a failed detach keeps failing.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::session_host::protocol::write_host_frame;

/// Upper bound for one fixture pump to reach EOF.
const PUMP_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct Events(mpsc::Sender<&'static str>);

impl AttachEventSink for Events {
    fn redraw(&self, _session: SessionToken) {
        let _ = self.0.send("redraw");
    }
    fn exited(&self, _session: SessionToken) {
        let _ = self.0.send("exited");
    }
}

/// Run the pump over one end of a socket pair after `script` wrote host frames
/// to the other end and closed it; returns the sink events in order.
///
/// The host end is shut down, not only dropped, so EOF arrives even if a
/// concurrently forked child inherited the descriptor; a surviving alias pins
/// that independence. The pump gets a bounded window to finish, and on expiry
/// its own end is shut down before the join so the join cannot hang.
fn pump_events(script: impl FnOnce(&mut UnixStream)) -> (Vec<&'static str>, Terminal) {
    let (ours, mut theirs) = UnixStream::pair().expect("socketpair");
    script(&mut theirs);
    let _alias = theirs.try_clone().expect("host alias");
    theirs.shutdown(Shutdown::Both).expect("close host end");
    drop(theirs);
    let pump_end = ours.try_clone().expect("pump end clone");
    let terminal = Arc::new(Mutex::new(Terminal::new(20, 4)));
    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let pump_terminal = terminal.clone();
    let pump = std::thread::spawn(move || {
        run_attach_pump(
            AttachReader { stream: ours },
            pump_terminal,
            Events(tx),
            SessionToken(5),
        );
        let _ = done_tx.send(());
    });
    let finished = done_rx.recv_timeout(PUMP_DEADLINE).is_ok();
    if !finished {
        let _ = pump_end.shutdown(Shutdown::Both);
    }
    pump.join().expect("the pump thread does not panic");
    assert!(finished, "the pump returns at EOF within {PUMP_DEADLINE:?}");
    let terminal = Arc::try_unwrap(terminal)
        .ok()
        .expect("the pump released the mirror")
        .into_inner()
        .expect("mirror lock");
    (rx.try_iter().collect(), terminal)
}

#[test]
fn a_later_snapshot_that_does_not_decode_ends_the_attachment() {
    let (events, terminal) = pump_events(|host| {
        write_host_frame(host, &HostFrame::Output(b"kept".to_vec())).expect("output");
        write_host_frame(host, &HostFrame::Snapshot(b"not an envelope".to_vec()))
            .expect("snapshot");
        write_host_frame(host, &HostFrame::Output(b"after".to_vec())).expect("output");
    });
    assert_eq!(
        events,
        vec!["redraw", "exited"],
        "the bad snapshot ends the attachment without a redraw"
    );
    let text: String = terminal
        .snapshot()
        .cells
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(text.contains("kept") && !text.contains("after"), "{text:?}");
}

#[test]
fn a_later_snapshot_that_decodes_still_restores_and_redraws() {
    let mut source = Terminal::new(20, 4);
    source.advance(b"restored");
    let bytes = SnapshotEnvelope::from_terminal(&source, Default::default())
        .encode()
        .expect("encode");
    let (events, terminal) = pump_events(|host| {
        write_host_frame(host, &HostFrame::Snapshot(bytes)).expect("snapshot");
    });
    assert_eq!(events, vec!["redraw", "exited"]);
    let text: String = terminal
        .snapshot()
        .cells
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(text.starts_with("restored"), "{text:?}");
}

#[test]
fn a_mirror_lays_out_query_bytes_but_never_answers_them() {
    // The host already answered the query against its own terminal; the
    // mirror's reply would be a second answer, so the pump drops it.
    let (events, mut terminal) = pump_events(|host| {
        write_host_frame(host, &HostFrame::Output("x\x1b[6n".as_bytes().to_vec())).expect("output");
    });
    assert_eq!(events, vec!["redraw", "exited"]);
    assert_eq!(terminal.screen().cursor().column, 1);
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn write_timeout_setup_propagates_failures_on_a_live_socket() {
    let (ours, _theirs) = UnixStream::pair().expect("socketpair");
    assert!(settle_write_timeout(&ours, Ok(())).is_ok());
    // EINVAL on a socket whose peer is still open is not the documented
    // peer-closed case on any target, so the attach is refused.
    for code in [libc::EINVAL, libc::EPERM] {
        let error = settle_write_timeout(&ours, Err(io::Error::from_raw_os_error(code)))
            .expect_err("live socket option failure refuses the attach");
        assert_eq!(error.raw_os_error(), Some(code));
    }
    // A generic InvalidInput with no raw EINVAL is not the macOS report.
    assert!(
        settle_write_timeout(&ours, Err(io::Error::from(io::ErrorKind::InvalidInput))).is_err()
    );
}

#[test]
fn write_timeout_setup_tolerates_einval_only_on_a_closed_macos_peer() {
    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    // A forked child may hold the peer descriptor, so close the shared socket
    // explicitly while an alias survives.
    let _alias = theirs.try_clone().expect("alias");
    theirs.shutdown(Shutdown::Both).expect("close peer");
    drop(theirs);
    let result = settle_write_timeout(&ours, Err(io::Error::from_raw_os_error(libc::EINVAL)));
    #[cfg(target_os = "macos")]
    assert!(
        result.is_ok(),
        "the proven peer-closed EINVAL passes on macOS"
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        result
            .expect_err("EINVAL is fatal off macOS")
            .raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(
        settle_write_timeout(&ours, Err(io::Error::from_raw_os_error(libc::EPERM)))
            .expect_err("other errors stay fatal on a closed peer")
            .raw_os_error(),
        Some(libc::EPERM)
    );
}

#[test]
fn a_detach_refused_on_a_desynced_stream_keeps_reporting_the_failure() {
    let (ours, _theirs) = UnixStream::pair().expect("socketpair");
    let mut client = AttachClient {
        stream: ours,
        detach: DetachState::Pending,
        poisoned: true,
        write_budget: ATTACH_WRITE_TIMEOUT,
    };
    let first = client
        .detach()
        .expect_err("a desynced stream refuses detach");
    let second = client.detach().expect_err("the second call reports it too");
    assert_eq!(format!("{first:#}"), format!("{second:#}"));
    // Suppress the Drop retry for this fixture.
    client.detach = DetachState::Sent;
}

#[test]
fn a_detach_that_was_sent_is_not_sent_again() {
    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    let mut client = AttachClient {
        stream: ours,
        detach: DetachState::Pending,
        poisoned: false,
        write_budget: ATTACH_WRITE_TIMEOUT,
    };
    client.detach().expect("first detach");
    client.detach().expect("second detach");
    drop(client);
    let mut reader = &theirs;
    assert_eq!(
        crate::session_host::protocol::read_client_frame(&mut reader).expect("frame"),
        ClientFrame::Detach
    );
    theirs.set_nonblocking(true).expect("nonblocking");
    assert!(
        crate::session_host::protocol::read_client_frame(&mut reader).is_err(),
        "exactly one Detach frame"
    );
}
