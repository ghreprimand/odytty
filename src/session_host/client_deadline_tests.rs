// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored socket fixtures, with no platform directories or fonts.
use super::super::protocol::{MAX_CLIENT_INPUT_LEN, write_host_frame};
use super::*;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::thread;

struct Peer {
    socket: UnixStream,
    worker: Option<thread::JoinHandle<()>>,
}
impl Peer {
    fn new(socket: UnixStream, run: impl FnOnce() + Send + 'static) -> Self {
        Self {
            socket,
            worker: Some(thread::spawn(run)),
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
        let end = Instant::now() + Duration::from_secs(5);
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(2));
            }
            if worker.is_finished() {
                let result = worker.join();
                if !thread::panicking() {
                    result.expect("peer thread");
                }
            } else if !thread::panicking() {
                panic!("peer thread exceeded cleanup deadline");
            }
        }
    }
}
fn pair() -> (SessionHostClient, UnixStream) {
    let (stream, peer) = UnixStream::pair().expect("pair");
    (
        SessionHostClient {
            stream,
            frame_reader: HostFrameReader::default(),
            poisoned: false,
        },
        peer,
    )
}
#[test]
fn zero_poll_timeout_is_rejected_before_consuming_a_frame() {
    let (mut client, mut peer) = pair();
    write_host_frame(&mut peer, &HostFrame::Output(b"kept".to_vec())).expect("frame");
    assert!(client.read_frame(Duration::ZERO).is_err());
    assert!(
        matches!(client.read_frame(Duration::from_secs(1)).expect("poll"), Some(HostFrame::Output(bytes)) if bytes == b"kept")
    );
}
#[test]
fn dribbled_frame_stops_at_one_call_deadline_and_resumes_exactly() {
    let (mut client, mut peer) = pair();
    let mut encoded = Vec::new();
    write_host_frame(&mut encoded, &HostFrame::Output(b"kept".to_vec())).expect("encode");
    let expected = encoded.len();
    let _worker = Peer::new(peer.try_clone().expect("cleanup clone"), move || {
        for byte in encoded {
            if peer.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(80));
        }
    });
    let start = Instant::now();
    let first = client.read_frame(Duration::from_millis(240)).expect("poll");
    assert!(
        first.is_none(),
        "one poll cannot consume a complete dribbled frame"
    );
    assert!(
        start.elapsed() < Duration::from_millis(600),
        "poll exceeded its total budget"
    );
    assert!(
        matches!(client.read_frame(Duration::from_secs(3)).expect("resume"), Some(HostFrame::Output(bytes)) if bytes == b"kept")
    );
    assert_eq!(expected, 9);
}
#[test]
fn buffered_final_frame_is_read_after_peer_close() {
    let (mut client, mut peer) = pair();
    write_host_frame(&mut peer, &HostFrame::SessionExit { exit_code: Some(7) }).expect("frame");
    drop(peer);
    assert!(matches!(
        client
            .read_frame(Duration::from_secs(1))
            .expect("final frame"),
        Some(HostFrame::SessionExit { exit_code: Some(7) })
    ));
    assert!(client.read_frame(Duration::from_secs(1)).is_err());
}
#[test]
fn input_to_a_host_that_stops_reading_is_bounded_and_partial_delivery_is_fatal() {
    let (mut client, peer) = pair();
    let _worker = Peer::new(peer.try_clone().expect("cleanup clone"), move || {
        // An outer backstop makes the failing-before test bounded too.
        thread::sleep(Duration::from_secs(4));
        let _ = peer.shutdown(Shutdown::Both);
        drop(peer);
    });
    let start = Instant::now();
    let error = client
        .send_input(&vec![b'x'; MAX_CLIENT_INPUT_LEN])
        .expect_err("stalled input");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "post-hello input exceeded frame budget"
    );
    assert!(
        matches!(error.downcast_ref::<ProtocolError>(), Some(ProtocolError::TruncatedWrite { written, .. }) if *written > 0),
        "partial delivery must report truncation: {error:?}"
    );
    assert!(
        client.resize(80, 24).is_err(),
        "a truncated stream cannot carry resize"
    );
    assert!(
        client.detach().is_err(),
        "a truncated stream cannot carry detach"
    );
    assert!(
        client.shutdown().is_err(),
        "a truncated stream cannot carry shutdown"
    );
}
#[test]
fn oversized_input_preserves_clean_stream() {
    let (mut client, mut peer) = pair();
    assert!(
        client
            .send_input(&vec![0; MAX_CLIENT_INPUT_LEN + 1])
            .is_err()
    );
    client.resize(80, 24).expect("clean resize");
    let frame = super::super::protocol::read_client_frame(&mut peer).expect("resize");
    assert!(matches!(
        frame,
        ClientFrame::Resize {
            columns: 80,
            rows: 24
        }
    ));
}

fn zero_progress_command_stays_clean(command: impl FnOnce(&mut SessionHostClient) -> Result<()>) {
    let (mut client, mut peer) = pair();
    client
        .stream
        .set_nonblocking(true)
        .expect("fill without blocking");
    loop {
        match client.stream.write(&[0; 65536]) {
            Ok(0) => panic!("socket closed while filling"),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill socket: {error}"),
        }
    }
    client
        .stream
        .set_nonblocking(false)
        .expect("restore blocking writes");
    let start = Instant::now();
    command(&mut client).expect("zero-progress frame is dropped");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "command exceeded send budget"
    );
    peer.set_nonblocking(true).expect("drain without blocking");
    let mut buffer = [0; 65536];
    loop {
        match peer.read(&mut buffer) {
            Ok(0) => panic!("zero-progress timeout closed the stream"),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("drain socket: {error}"),
        }
    }
    peer.set_nonblocking(false).expect("restore blocking reads");
    client.resize(90, 30).expect("later resize remains usable");
    assert!(matches!(
        super::super::protocol::read_client_frame(&mut peer).expect("clean next frame"),
        ClientFrame::Resize {
            columns: 90,
            rows: 30
        }
    ));
}
#[test]
fn resize_zero_progress_timeout_drops_only_the_frame() {
    zero_progress_command_stays_clean(|client| client.resize(80, 24));
}
#[test]
fn detach_zero_progress_timeout_drops_only_the_frame() {
    zero_progress_command_stays_clean(SessionHostClient::detach);
}
#[test]
fn shutdown_zero_progress_timeout_drops_only_the_frame() {
    zero_progress_command_stays_clean(SessionHostClient::shutdown);
}
#[test]
fn steadily_drained_input_still_hits_the_whole_frame_deadline() {
    let (mut client, peer) = pair();
    let _worker = Peer::new(peer.try_clone().expect("cleanup clone"), move || {
        let mut reader = SocketReadDeadline::new(&peer, Instant::now() + Duration::from_secs(5));
        let mut buffer = [0; 65536];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
    });
    let start = Instant::now();
    let error = client
        .send_input(&vec![b'x'; MAX_CLIENT_INPUT_LEN])
        .expect_err("dribbled input stops");
    assert!(matches!(
        error.downcast_ref::<ProtocolError>(),
        Some(ProtocolError::TruncatedWrite { .. })
    ));
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "successful writes extended the whole-frame budget"
    );
}
#[test]
fn live_socket_timeout_errors_are_propagated() {
    let (client, _peer) = pair();
    for code in [libc::EINVAL, libc::EPERM] {
        let error = super::super::socket::checked_socket_timeout(
            &client.stream,
            Err(io::Error::from_raw_os_error(code)),
        )
        .expect_err("live option error");
        assert_eq!(error.raw_os_error(), Some(code));
    }
}

#[test]
fn out_of_range_poll_timeout_is_rejected_without_consuming_data() {
    let (mut client, mut peer) = pair();
    write_host_frame(&mut peer, &HostFrame::Output(b"kept".to_vec())).expect("frame");
    assert!(client.read_frame(Duration::MAX).is_err());
    assert!(
        matches!(client.read_frame(Duration::from_secs(1)).expect("next poll"), Some(HostFrame::Output(bytes)) if bytes == b"kept")
    );
}

#[test]
fn only_macos_einval_is_tolerated_on_a_proven_closed_peer() {
    let (client, peer) = pair();
    drop(peer);
    let result = super::super::socket::checked_socket_timeout(
        &client.stream,
        Err(io::Error::from_raw_os_error(libc::EINVAL)),
    );
    #[cfg(target_os = "macos")]
    assert!(result.is_ok());
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        result.expect_err("non-macOS option error").raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(
        super::super::socket::checked_socket_timeout(
            &client.stream,
            Err(io::Error::from_raw_os_error(libc::EPERM))
        )
        .expect_err("other errors remain fatal")
        .raw_os_error(),
        Some(libc::EPERM)
    );
}
