// SPDX-License-Identifier: GPL-3.0-only
//! Attach pump and client outcomes that must stay truthful: a later snapshot
//! that cannot be applied ends the attachment, the write-timeout setup accepts
//! only the documented peer-closed failure, and a failed detach keeps failing.

use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use super::*;
use crate::session_host::protocol::write_host_frame;

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
fn pump_events(script: impl FnOnce(&mut UnixStream)) -> (Vec<&'static str>, Terminal) {
    let (ours, mut theirs) = UnixStream::pair().expect("socketpair");
    script(&mut theirs);
    drop(theirs);
    let terminal = Arc::new(Mutex::new(Terminal::new(20, 4)));
    let (tx, rx) = mpsc::channel();
    let pump_terminal = terminal.clone();
    let pump = std::thread::spawn(move || {
        run_attach_pump(
            AttachReader { stream: ours },
            pump_terminal,
            Events(tx),
            SessionToken(5),
        );
    });
    pump.join().expect("the pump returns at EOF");
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
fn write_timeout_setup_accepts_only_the_peer_closed_failure() {
    assert!(accept_write_timeout_setup(Ok(())).is_ok());
    assert!(
        accept_write_timeout_setup(Err(io::Error::from(io::ErrorKind::InvalidInput))).is_ok(),
        "the macOS peer-closed EINVAL passes"
    );
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
        assert!(
            accept_write_timeout_setup(Err(io::Error::from(kind))).is_err(),
            "{kind:?} refuses the attach"
        );
    }
}

#[test]
fn a_detach_refused_on_a_desynced_stream_keeps_reporting_the_failure() {
    let (ours, _theirs) = UnixStream::pair().expect("socketpair");
    let mut client = AttachClient {
        stream: ours,
        detach: DetachState::Pending,
        poisoned: true,
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
