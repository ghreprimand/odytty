// SPDX-License-Identifier: GPL-3.0-only
//! Non-blocking attach handshakes and deadline-bounded frame writes for the
//! session host.
//!
//! The host runs one loop for PTY output, client input, accept, and shutdown,
//! so nothing a connecting peer does may block it. An accepted connection is
//! therefore not read inline: it becomes a [`PendingHandshake`] whose hello is
//! read without blocking, a little each loop turn, until it completes, fails,
//! or passes its deadline. Only the bytes of the hello are consumed, so frames
//! a client sends straight after its hello stay queued for its reader thread.
//!
//! Every frame the host writes goes through a [`DeadlineWriter`]: each write
//! is bounded by the per-write send timeout and the whole frame by one
//! absolute deadline, so a peer that reads slowly cannot stretch a large
//! snapshot across many successful writes. The zero-progress versus
//! partial-progress distinction of the frame writer is preserved: a deadline
//! hit before any byte is written is a plain timeout, and one hit mid-frame is
//! a `TruncatedWrite`. For these host-to-client frames the host evicts the
//! client in both cases, because a client that accepts nothing would
//! otherwise stall every later broadcast to the other attached clients. The
//! client-to-host direction keeps its own policy: a zero-progress timeout
//! drops that frame and keeps the stream, and only a `TruncatedWrite` tears
//! it down.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::protocol::{
    ClientHello, HOST_PROTOCOL_MAGIC, HostHello, MAX_HANDSHAKE_STRING, read_client_hello,
    write_host_hello,
};

/// Fixed prefix of a client hello: magic, three `u16` versions, and the `u32`
/// session-id length.
const HELLO_FIXED_LEN: usize = HOST_PROTOCOL_MAGIC.len() + 3 * 2 + 4;

/// Largest slice handed to one socket write, so the frame deadline is checked
/// between chunks of a large frame.
const DEADLINE_WRITE_CHUNK: usize = 64 * 1024;

/// Bytes read from one pending connection per poll attempt.
const HANDSHAKE_READ_CHUNK: usize = 512;

/// An accepted connection whose client hello has not arrived in full yet.
pub(super) struct PendingHandshake {
    stream: UnixStream,
    buffer: Vec<u8>,
    deadline: Instant,
}

/// Outcome of one [`PendingHandshake::poll`].
#[derive(Debug)]
pub(super) enum HandshakeProgress {
    /// The hello is incomplete and the deadline has not passed.
    Pending,
    /// The hello arrived and decoded.
    Hello(ClientHello),
    /// The connection closed, failed, sent an invalid hello, or ran out of
    /// time. The message is sent back in a rejected host hello.
    Failed(String),
}

impl PendingHandshake {
    /// Track `stream` until `deadline`. Fails when the socket cannot be made
    /// non-blocking; the caller then simply drops the connection.
    pub(super) fn new(stream: UnixStream, deadline: Instant) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            buffer: Vec::with_capacity(HELLO_FIXED_LEN),
            deadline,
        })
    }

    /// Read whatever part of the hello is available without blocking.
    pub(super) fn poll(&mut self, now: Instant) -> HandshakeProgress {
        loop {
            let target = self.target_len();
            if self.buffer.len() >= target {
                return match read_client_hello(&mut self.buffer.as_slice()) {
                    Ok(hello) => HandshakeProgress::Hello(hello),
                    Err(error) => HandshakeProgress::Failed(format!("invalid hello: {error}")),
                };
            }
            let mut chunk = [0u8; HANDSHAKE_READ_CHUNK];
            let want = (target - self.buffer.len()).min(chunk.len());
            match (&self.stream).read(&mut chunk[..want]) {
                Ok(0) => {
                    return HandshakeProgress::Failed(
                        "connection closed before its hello completed".to_owned(),
                    );
                }
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if now >= self.deadline {
                        return HandshakeProgress::Failed(
                            "attach handshake exceeded its deadline".to_owned(),
                        );
                    }
                    return HandshakeProgress::Pending;
                }
                Err(error) => return HandshakeProgress::Failed(format!("invalid hello: {error}")),
            }
        }
    }

    /// Total hello length once the session-id length is known. An oversized
    /// declared length stops at the fixed prefix so decoding reports it.
    fn target_len(&self) -> usize {
        if self.buffer.len() < HELLO_FIXED_LEN {
            return HELLO_FIXED_LEN;
        }
        let mut len = [0u8; 4];
        len.copy_from_slice(&self.buffer[HELLO_FIXED_LEN - 4..HELLO_FIXED_LEN]);
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_HANDSHAKE_STRING {
            HELLO_FIXED_LEN
        } else {
            HELLO_FIXED_LEN + len
        }
    }

    /// Hand the connection over for admission, restoring blocking mode so
    /// the reader thread and the bounded writes behave as on a fresh accept.
    pub(super) fn into_stream(self) -> io::Result<UnixStream> {
        self.stream.set_nonblocking(false)?;
        Ok(self.stream)
    }

    /// Best-effort rejection of a failed handshake, then drop.
    pub(super) fn reject(self, message: &str) {
        reject_nonblocking(&self.stream, message);
    }
}

/// Send a rejected host hello without ever blocking: the stream is left
/// non-blocking and any error (including a full buffer) is ignored because
/// the connection is dropped immediately afterwards.
pub(super) fn reject_nonblocking(stream: &UnixStream, message: &str) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let mut writer = stream;
    let _ = write_host_hello(&mut writer, &HostHello::rejected(message));
}

/// A [`Write`] adapter bounding one whole frame by an absolute deadline.
pub(super) struct DeadlineWriter<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
    per_write: Duration,
}

impl<'a> DeadlineWriter<'a> {
    pub(super) fn new(stream: &'a UnixStream, budget: Duration, per_write: Duration) -> Self {
        Self {
            stream,
            deadline: Instant::now() + budget,
            per_write,
        }
    }
}

impl Write for DeadlineWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let now = Instant::now();
        if now >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "session-host frame send exceeded its deadline",
            ));
        }
        // Best-effort, as on the handshake path: macOS can reject the option
        // on a peer-closed socket, and the write below then reports the close.
        let _ = self
            .stream
            .set_write_timeout(Some((self.deadline - now).min(self.per_write)));
        // One kernel send may keep going for as long as the peer keeps
        // draining (the send timeout restarts on progress), so hand it at most
        // one chunk and check the deadline between chunks.
        let mut stream = self.stream;
        stream.write(&buf[..buf.len().min(DEADLINE_WRITE_CHUNK)])
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_host::protocol::{
        ClientFrame, HostFrame, ProtocolError, read_client_frame, read_host_hello,
        write_client_frame, write_client_hello, write_host_frame,
    };

    fn hello_bytes(session: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_client_hello(&mut bytes, &ClientHello::current(session)).expect("encode hello");
        bytes
    }

    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn a_hello_split_across_polls_completes_and_leaves_following_frames_unread() {
        let (mut client, server) = UnixStream::pair().expect("socketpair");
        let mut pending = PendingHandshake::new(server, far_deadline()).expect("pending");
        let hello = hello_bytes("split");
        client.write_all(&hello[..7]).expect("first part");
        assert!(matches!(
            pending.poll(Instant::now()),
            HandshakeProgress::Pending
        ));
        client.write_all(&hello[7..]).expect("rest");
        write_client_frame(
            &mut client,
            &ClientFrame::Resize {
                columns: 90,
                rows: 30,
            },
        )
        .expect("pipelined frame");
        match pending.poll(Instant::now()) {
            HandshakeProgress::Hello(hello) => assert_eq!(hello.session_id, "split"),
            other => panic!("expected a hello, got {other:?}"),
        }
        let mut stream = pending.into_stream().expect("blocking stream");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        assert!(matches!(
            read_client_frame(&mut stream).expect("frame after hello"),
            ClientFrame::Resize {
                columns: 90,
                rows: 30
            }
        ));
    }

    #[test]
    fn an_oversized_session_id_fails_at_the_fixed_prefix() {
        let (mut client, server) = UnixStream::pair().expect("socketpair");
        let mut pending = PendingHandshake::new(server, far_deadline()).expect("pending");
        let mut prefix = hello_bytes("x");
        prefix.truncate(HELLO_FIXED_LEN - 4);
        prefix.extend_from_slice(&((MAX_HANDSHAKE_STRING as u32) + 1).to_be_bytes());
        client.write_all(&prefix).expect("prefix");
        match pending.poll(Instant::now()) {
            HandshakeProgress::Failed(message) => assert!(message.contains("invalid hello")),
            other => panic!("expected an oversized id to fail, got {other:?}"),
        }
    }

    #[test]
    fn bad_magic_and_early_close_fail_without_waiting() {
        let (mut client, server) = UnixStream::pair().expect("socketpair");
        let mut pending = PendingHandshake::new(server, far_deadline()).expect("pending");
        let mut bytes = hello_bytes("magic");
        bytes[0] ^= 0xff;
        client.write_all(&bytes).expect("bad magic hello");
        assert!(matches!(
            pending.poll(Instant::now()),
            HandshakeProgress::Failed(_)
        ));

        let (client, server) = UnixStream::pair().expect("socketpair");
        let mut pending = PendingHandshake::new(server, far_deadline()).expect("pending");
        // CLOEXEC does not prevent a concurrent fork from retaining this peer.
        // Shut down the shared write side so EOF does not depend on alias drops.
        let inherited_client = client.try_clone().expect("duplicate peer handle");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("close peer write side");
        drop(client);
        match pending.poll(Instant::now()) {
            HandshakeProgress::Failed(message) => assert!(message.contains("closed")),
            other => panic!("expected a closed connection to fail, got {other:?}"),
        }
        drop(inherited_client);
    }

    #[test]
    fn a_rejected_handshake_tells_the_client_why() {
        let (mut client, server) = UnixStream::pair().expect("socketpair");
        // Arm the timeout while the peer is open: macOS rejects `SO_RCVTIMEO`
        // with `EINVAL` once the rejecting side has closed.
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let pending = PendingHandshake::new(server, far_deadline()).expect("pending");
        pending.reject("attach queue is full");
        let hello = read_host_hello(&mut client).expect("rejection hello");
        assert!(hello.into_result().is_err());
    }

    #[test]
    fn a_frame_to_a_peer_that_stops_reading_ends_at_the_deadline_as_truncated() {
        let (_peer, host) = UnixStream::pair().expect("socketpair");
        let frame = HostFrame::Output(vec![b'x'; 8 * 1024 * 1024]);
        let start = Instant::now();
        let mut writer = DeadlineWriter::new(
            &host,
            Duration::from_millis(300),
            Duration::from_millis(100),
        );
        let result = write_host_frame(&mut writer, &frame);
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(ProtocolError::TruncatedWrite { .. })),
            "a mid-frame deadline must desync visibly, got {result:?}"
        );
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");

        // The buffer is now full: the next frame makes no progress, which stays
        // a plain timeout rather than a truncated write.
        let mut writer = DeadlineWriter::new(
            &host,
            Duration::from_millis(300),
            Duration::from_millis(100),
        );
        match write_host_frame(&mut writer, &HostFrame::Output(vec![b'y'; 16])) {
            Err(ProtocolError::Io(error)) => assert!(matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )),
            other => panic!("expected a zero-progress timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_slowly_read_frame_is_bounded_by_the_whole_frame_deadline() {
        let (peer, host) = UnixStream::pair().expect("socketpair");
        let reader = std::thread::spawn(move || {
            let mut peer = peer;
            let mut chunk = vec![0u8; 16 * 1024];
            let end = Instant::now() + Duration::from_secs(3);
            while Instant::now() < end {
                match peer.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        let frame = HostFrame::Output(vec![b'z'; 32 * 1024 * 1024]);
        let start = Instant::now();
        let mut writer = DeadlineWriter::new(
            &host,
            Duration::from_millis(400),
            Duration::from_millis(200),
        );
        let result = write_host_frame(&mut writer, &frame);
        let elapsed = start.elapsed();
        assert!(matches!(result, Err(ProtocolError::TruncatedWrite { .. })));
        assert!(
            elapsed < Duration::from_millis(1500),
            "progressing writes must still stop at the frame deadline: {elapsed:?}"
        );
        drop(host);
        reader.join().expect("reader");
    }
}
