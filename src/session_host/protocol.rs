// SPDX-License-Identifier: GPL-3.0-only
//! Versioned binary protocol for local session-host attach clients.

use std::fmt;
use std::io::{self, Read, Write};

use crate::core::{SNAPSHOT_FORMAT_VERSION, SNAPSHOT_PROTOCOL_VERSION};

pub const HOST_PROTOCOL_MAGIC: &[u8; 11] = b"ODYTTY-HOST";
pub const HOST_PROTOCOL_VERSION: u16 = 1;
pub const MAX_HANDSHAKE_STRING: usize = 4096;
pub const MAX_FRAME_LEN: usize = 64 * 1024 * 1024;

/// A live detached session as surfaced to the attach overlay and the `list`
/// CLI. This is a **pure data type** with no platform dependency, so it lives in
/// `protocol` (always compiled) rather than in `registry` (Unix-only): the
/// always-compiled attach overlay (`SessionAttachOverlay`) holds a
/// `Vec<ListedSession>`, which on Windows is simply always empty (the Unix-only
/// `registry::list_live_sessions` is the only producer). Keeping the type here
/// lets the overlay and the `list` formatter compile cross-platform while the
/// socket-backed data source stays `#[cfg(unix)]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSession {
    pub id: String,
    pub name: String,
    pub state: &'static str,
    pub age_ms: u128,
    pub pane_count: usize,
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    BadMagic,
    InvalidStatus(u8),
    InvalidFrameKind(u8),
    FrameTooLarge {
        len: usize,
        max: usize,
    },
    StringTooLarge {
        len: usize,
        max: usize,
    },
    InvalidUtf8,
    InvalidPayload(&'static str),
    Rejected(String),
    /// A frame write made partial progress and then hit the bounded send
    /// timeout: the peer has a truncated frame on its wire and the stream is
    /// permanently desynced. Distinct from a zero-progress timeout (plain
    /// [`Self::Io`] with `WouldBlock`/`TimedOut`), which callers may safely
    /// treat as "frame not sent, stream still clean".
    TruncatedWrite {
        written: usize,
        len: usize,
    },
}

impl ProtocolError {
    pub fn is_disconnect(&self) -> bool {
        match self {
            Self::Io(error) => matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
            ),
            _ => false,
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::BadMagic => write!(f, "bad session-host protocol magic"),
            Self::InvalidStatus(status) => write!(f, "invalid session-host status {status}"),
            Self::InvalidFrameKind(kind) => write!(f, "invalid session-host frame kind {kind}"),
            Self::FrameTooLarge { len, max } => {
                write!(f, "session-host frame too large: {len} > {max}")
            }
            Self::StringTooLarge { len, max } => {
                write!(f, "session-host string too large: {len} > {max}")
            }
            Self::InvalidUtf8 => write!(f, "session-host string is not valid UTF-8"),
            Self::InvalidPayload(name) => write!(f, "invalid {name} payload"),
            Self::Rejected(reason) => write!(f, "session-host rejected attach: {reason}"),
            Self::TruncatedWrite { written, len } => write!(
                f,
                "session-host frame write truncated by send timeout: {written} of {len} bytes sent, stream desynced"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHello {
    pub host_protocol_version: u16,
    pub snapshot_format_version: u16,
    pub snapshot_protocol_version: u16,
    pub session_id: String,
}

impl ClientHello {
    pub fn current(session_id: impl Into<String>) -> Self {
        Self {
            host_protocol_version: HOST_PROTOCOL_VERSION,
            snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
            snapshot_protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            session_id: session_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloStatus {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostHello {
    pub status: HelloStatus,
    pub host_protocol_version: u16,
    pub snapshot_format_version: u16,
    pub snapshot_protocol_version: u16,
    pub message: String,
}

impl HostHello {
    pub fn accepted() -> Self {
        Self {
            status: HelloStatus::Accepted,
            host_protocol_version: HOST_PROTOCOL_VERSION,
            snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
            snapshot_protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            message: String::new(),
        }
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            status: HelloStatus::Rejected,
            host_protocol_version: HOST_PROTOCOL_VERSION,
            snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
            snapshot_protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            message: message.into(),
        }
    }

    pub fn into_result(self) -> Result<Self, ProtocolError> {
        match self.status {
            HelloStatus::Accepted => Ok(self),
            HelloStatus::Rejected => Err(ProtocolError::Rejected(self.message)),
        }
    }
}

pub fn versions_compatible(hello: &ClientHello) -> bool {
    hello.host_protocol_version == HOST_PROTOCOL_VERSION
        && hello.snapshot_format_version == SNAPSHOT_FORMAT_VERSION
        && hello.snapshot_protocol_version == SNAPSHOT_PROTOCOL_VERSION
}

pub fn write_client_hello(
    writer: &mut impl Write,
    hello: &ClientHello,
) -> Result<(), ProtocolError> {
    writer.write_all(HOST_PROTOCOL_MAGIC)?;
    write_u16(writer, hello.host_protocol_version)?;
    write_u16(writer, hello.snapshot_format_version)?;
    write_u16(writer, hello.snapshot_protocol_version)?;
    write_string(writer, &hello.session_id, MAX_HANDSHAKE_STRING)?;
    writer.flush()?;
    Ok(())
}

pub fn read_client_hello(reader: &mut impl Read) -> Result<ClientHello, ProtocolError> {
    read_magic(reader)?;
    let host_protocol_version = read_u16(reader)?;
    let snapshot_format_version = read_u16(reader)?;
    let snapshot_protocol_version = read_u16(reader)?;
    let session_id = read_string(reader, MAX_HANDSHAKE_STRING)?;
    Ok(ClientHello {
        host_protocol_version,
        snapshot_format_version,
        snapshot_protocol_version,
        session_id,
    })
}

pub fn write_host_hello(writer: &mut impl Write, hello: &HostHello) -> Result<(), ProtocolError> {
    writer.write_all(HOST_PROTOCOL_MAGIC)?;
    writer.write_all(&[match hello.status {
        HelloStatus::Accepted => 0,
        HelloStatus::Rejected => 1,
    }])?;
    write_u16(writer, hello.host_protocol_version)?;
    write_u16(writer, hello.snapshot_format_version)?;
    write_u16(writer, hello.snapshot_protocol_version)?;
    write_string(writer, &hello.message, MAX_HANDSHAKE_STRING)?;
    writer.flush()?;
    Ok(())
}

pub fn read_host_hello(reader: &mut impl Read) -> Result<HostHello, ProtocolError> {
    read_magic(reader)?;
    let mut status = [0u8; 1];
    reader.read_exact(&mut status)?;
    let status = match status[0] {
        0 => HelloStatus::Accepted,
        1 => HelloStatus::Rejected,
        value => return Err(ProtocolError::InvalidStatus(value)),
    };
    let host_protocol_version = read_u16(reader)?;
    let snapshot_format_version = read_u16(reader)?;
    let snapshot_protocol_version = read_u16(reader)?;
    let message = read_string(reader, MAX_HANDSHAKE_STRING)?;
    Ok(HostHello {
        status,
        host_protocol_version,
        snapshot_format_version,
        snapshot_protocol_version,
        message,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFrame {
    Snapshot(Vec<u8>),
    Output(Vec<u8>),
    Invalidate {
        render_revision: u64,
    },
    /// The host applied a resize (its own TIOCSWINSZ + model reflow) and the new
    /// grid dimensions must propagate to EVERY attached client, not just the one
    /// that requested it (audit C-3). Without this a peer's resize silently
    /// garbles the other clients' mirrors, which keep advancing at their old
    /// width against output the host formatted for the new width. Carries the
    /// post-resize dimensions plus the same `render_revision` an `Invalidate`
    /// would, so a client both resizes its mirror and repaints from one frame.
    ///
    /// Wire kind 6. A host built before this frame never emits it, so an older
    /// client attached to an older host is unaffected; a newer client attached
    /// to an older host simply never receives it (the mirror stays as-is, the
    /// pre-fix behavior). The only skew that drops a client is a newer host
    /// emitting kind 6 to an older client (a binary downgrade with a live host),
    /// which decodes as `InvalidFrameKind` -- the same accepted degradation the
    /// `ClientFrame::Shutdown` frame documents.
    Resized {
        columns: u32,
        rows: u32,
        render_revision: u64,
    },
    SessionExit {
        exit_code: Option<i32>,
    },
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFrame {
    Input(Vec<u8>),
    Resize {
        columns: u32,
        rows: u32,
    },
    Detach,
    /// Ask the host to terminate the whole session: reap the shell, exit the
    /// run loop, and unlink the socket + lock so the session disappears from the
    /// registry. Emitted by "kill session" from the manager (kind 104, empty
    /// payload). A host from an OLDER binary that predates this frame decodes it
    /// as [`ProtocolError::InvalidFrameKind`] and drops the client without dying
    /// — acceptable degradation; the idle timeout still reaps it.
    Shutdown,
}

pub fn write_host_frame(writer: &mut impl Write, frame: &HostFrame) -> Result<(), ProtocolError> {
    match frame {
        HostFrame::Snapshot(bytes) => write_frame(writer, 1, bytes),
        HostFrame::Output(bytes) => write_frame(writer, 2, bytes),
        HostFrame::Invalidate { render_revision } => {
            write_frame(writer, 3, &render_revision.to_be_bytes())
        }
        HostFrame::Resized {
            columns,
            rows,
            render_revision,
        } => {
            let mut payload = Vec::with_capacity(16);
            payload.extend_from_slice(&columns.to_be_bytes());
            payload.extend_from_slice(&rows.to_be_bytes());
            payload.extend_from_slice(&render_revision.to_be_bytes());
            write_frame(writer, 6, &payload)
        }
        HostFrame::SessionExit { exit_code } => {
            let mut payload = Vec::with_capacity(5);
            match exit_code {
                Some(code) => {
                    payload.push(1);
                    payload.extend_from_slice(&code.to_be_bytes());
                }
                None => payload.push(0),
            }
            write_frame(writer, 4, &payload)
        }
        HostFrame::Error(message) => write_frame(writer, 5, message.as_bytes()),
    }
}

pub fn read_host_frame(reader: &mut impl Read) -> Result<HostFrame, ProtocolError> {
    let (kind, payload) = read_frame(reader)?;
    decode_host_frame(kind, payload)
}

fn decode_host_frame(kind: u8, payload: Vec<u8>) -> Result<HostFrame, ProtocolError> {
    match kind {
        1 => Ok(HostFrame::Snapshot(payload)),
        2 => Ok(HostFrame::Output(payload)),
        3 => {
            if payload.len() != 8 {
                return Err(ProtocolError::InvalidPayload("invalidate"));
            }
            Ok(HostFrame::Invalidate {
                render_revision: u64::from_be_bytes(payload.try_into().expect("len checked")),
            })
        }
        6 => {
            if payload.len() != 16 {
                return Err(ProtocolError::InvalidPayload("resized"));
            }
            let columns = u32::from_be_bytes(payload[0..4].try_into().expect("len checked"));
            let rows = u32::from_be_bytes(payload[4..8].try_into().expect("len checked"));
            let render_revision =
                u64::from_be_bytes(payload[8..16].try_into().expect("len checked"));
            Ok(HostFrame::Resized {
                columns,
                rows,
                render_revision,
            })
        }
        4 => match payload.as_slice() {
            [0] => Ok(HostFrame::SessionExit { exit_code: None }),
            [1, a, b, c, d] => Ok(HostFrame::SessionExit {
                exit_code: Some(i32::from_be_bytes([*a, *b, *c, *d])),
            }),
            _ => Err(ProtocolError::InvalidPayload("session-exit")),
        },
        5 => Ok(HostFrame::Error(
            String::from_utf8(payload).map_err(|_| ProtocolError::InvalidUtf8)?,
        )),
        value => Err(ProtocolError::InvalidFrameKind(value)),
    }
}

pub fn write_client_frame(
    writer: &mut impl Write,
    frame: &ClientFrame,
) -> Result<(), ProtocolError> {
    match frame {
        ClientFrame::Input(bytes) => write_frame(writer, 101, bytes),
        ClientFrame::Resize { columns, rows } => {
            let mut payload = Vec::with_capacity(8);
            payload.extend_from_slice(&columns.to_be_bytes());
            payload.extend_from_slice(&rows.to_be_bytes());
            write_frame(writer, 102, &payload)
        }
        ClientFrame::Detach => write_frame(writer, 103, &[]),
        ClientFrame::Shutdown => write_frame(writer, 104, &[]),
    }
}

pub fn read_client_frame(reader: &mut impl Read) -> Result<ClientFrame, ProtocolError> {
    let (kind, payload) = read_frame(reader)?;
    decode_client_frame(kind, payload)
}

/// Interpret a raw `(kind, payload)` pair as a [`ClientFrame`]. Shared by the
/// blocking [`read_client_frame`] and the poll-with-timeout [`ClientFrameReader`].
fn decode_client_frame(kind: u8, payload: Vec<u8>) -> Result<ClientFrame, ProtocolError> {
    match kind {
        101 => Ok(ClientFrame::Input(payload)),
        102 => {
            if payload.len() != 8 {
                return Err(ProtocolError::InvalidPayload("resize"));
            }
            let columns = u32::from_be_bytes(payload[0..4].try_into().expect("len checked"));
            let rows = u32::from_be_bytes(payload[4..8].try_into().expect("len checked"));
            Ok(ClientFrame::Resize { columns, rows })
        }
        103 if payload.is_empty() => Ok(ClientFrame::Detach),
        103 => Err(ProtocolError::InvalidPayload("detach")),
        104 if payload.is_empty() => Ok(ClientFrame::Shutdown),
        104 => Err(ProtocolError::InvalidPayload("shutdown")),
        value => Err(ProtocolError::InvalidFrameKind(value)),
    }
}

fn read_magic(reader: &mut impl Read) -> Result<(), ProtocolError> {
    let mut magic = [0u8; HOST_PROTOCOL_MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if &magic == HOST_PROTOCOL_MAGIC {
        Ok(())
    } else {
        Err(ProtocolError::BadMagic)
    }
}

fn write_frame(writer: &mut impl Write, kind: u8, payload: &[u8]) -> Result<(), ProtocolError> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge {
            len: payload.len(),
            max: MAX_FRAME_LEN,
        });
    }
    // One contiguous buffer: with a send timeout armed on the socket (both
    // attach directions), three separate header/payload writes could time out
    // between them and leave a truncated frame that desyncs the peer's
    // parser. A single buffered write narrows that window to the one
    // unavoidable partial-write case inside the kernel.
    let mut frame = Vec::with_capacity(1 + 4 + payload.len());
    frame.push(kind);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    // Manual write loop instead of `write_all`: `write_all` discards HOW MUCH
    // was written before an error, but that distinction is load-bearing here.
    // A send timeout with zero progress leaves the stream clean (nothing on
    // the wire; callers may drop the frame and retry later), while a timeout
    // AFTER partial progress leaves a truncated frame on the peer's wire —
    // the stream is permanently desynced and must be torn down, so it
    // surfaces as the distinct [`ProtocolError::TruncatedWrite`] that no
    // caller treats as transient.
    let mut written = 0;
    while written < frame.len() {
        match writer.write(&frame[written..]) {
            Ok(0) => {
                return Err(ProtocolError::Io(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "session-host stream accepted no bytes",
                )));
            }
            Ok(n) => written += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if written > 0
                    && matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
            {
                return Err(ProtocolError::TruncatedWrite {
                    written,
                    len: frame.len(),
                });
            }
            Err(error) => return Err(ProtocolError::Io(error)),
        }
    }
    writer.flush()?;
    Ok(())
}

fn read_frame(reader: &mut impl Read) -> Result<(u8, Vec<u8>), ProtocolError> {
    let mut kind = [0u8; 1];
    reader.read_exact(&mut kind)?;
    let len = read_u32(reader)? as usize;
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge {
            len,
            max: MAX_FRAME_LEN,
        });
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok((kind[0], payload))
}

/// Wire size of a frame header: 1-byte kind + 4-byte big-endian payload length.
const FRAME_HEADER_LEN: usize = 5;

/// Stateful, resumable host-frame reader for **poll-with-timeout** callers.
///
/// The stateless [`read_host_frame`] frames with sequential `read_exact` calls,
/// and `std::io::Read::read_exact` DISCARDS the bytes it already consumed when it
/// errors partway through a buffer — e.g. when `SO_RCVTIMEO` fires mid-frame. A
/// caller that treats the timeout as "no frame yet" and retries on the same
/// stream then parses leftover payload bytes as a fresh frame header, silently
/// and permanently desyncing the stream (audit P1). That is safe only for
/// blocking readers with no read timeout (`run_attach_pump`), where `read_exact`
/// waits for the whole payload.
///
/// This reader keeps partial progress across calls: a `WouldBlock`/`TimedOut`
/// error preserves every byte read so far, and the next call resumes exactly
/// where the previous one left off, so the poll-retry pattern is correct. After
/// any error **other** than `WouldBlock`/`TimedOut` the stream is undefined
/// mid-frame (exactly as with the stateless path) and the reader must be
/// discarded together with the stream.
#[derive(Debug, Default)]
pub struct HostFrameReader {
    header: [u8; FRAME_HEADER_LEN],
    header_filled: usize,
    payload: Vec<u8>,
    payload_filled: usize,
}

impl HostFrameReader {
    /// Read (or resume reading) one host frame. On `WouldBlock`/`TimedOut` the
    /// partial frame is retained and a later call resumes it.
    pub fn read(&mut self, reader: &mut impl Read) -> Result<HostFrame, ProtocolError> {
        let (kind, payload) = self.read_raw(reader)?;
        decode_host_frame(kind, payload)
    }

    fn read_raw(&mut self, reader: &mut impl Read) -> Result<(u8, Vec<u8>), ProtocolError> {
        while self.header_filled < FRAME_HEADER_LEN {
            let count = read_nonzero(reader, &mut self.header[self.header_filled..])?;
            self.header_filled += count;
            if self.header_filled == FRAME_HEADER_LEN {
                let len =
                    u32::from_be_bytes(self.header[1..5].try_into().expect("header is 5 bytes"))
                        as usize;
                if len > MAX_FRAME_LEN {
                    // Fatal for the stream; reset so the reader is not left with
                    // a header that has no matching payload buffer.
                    self.reset();
                    return Err(ProtocolError::FrameTooLarge {
                        len,
                        max: MAX_FRAME_LEN,
                    });
                }
                // Allocated in the same call that completes the header, so the
                // cross-call invariant holds: header complete ⇒ payload sized.
                self.payload = vec![0u8; len];
                self.payload_filled = 0;
            }
        }
        while self.payload_filled < self.payload.len() {
            let count = read_nonzero(reader, &mut self.payload[self.payload_filled..])?;
            self.payload_filled += count;
        }
        let kind = self.header[0];
        let payload = std::mem::take(&mut self.payload);
        self.reset();
        Ok((kind, payload))
    }

    fn reset(&mut self) {
        self.header_filled = 0;
        self.payload = Vec::new();
        self.payload_filled = 0;
    }
}

/// Largest payload slice grown per read by [`ClientFrameReader`]. The declared
/// frame length is honoured incrementally in chunks of this size, so a client
/// that announces a near-`MAX_FRAME_LEN` payload but withholds the body never
/// forces a large up-front allocation.
const CLIENT_PAYLOAD_GROWTH_CHUNK: usize = 64 * 1024;

/// Outcome of a single non-blocking client-frame read.
#[derive(Debug)]
pub enum ClientFramePoll {
    /// A whole frame was received and decoded.
    Frame(ClientFrame),
    /// The read timed out (or would block) while a frame was only partially
    /// received. The caller may bound how long a frame is allowed to stay in
    /// this state before reclaiming the connection.
    PartialTimeout,
    /// The read timed out (or would block) with no frame in progress — the peer
    /// is simply idle between frames, which is legitimate and must not detach.
    IdleTimeout,
}

/// Stateful, resumable **client**-frame reader for poll-with-timeout callers.
///
/// The per-client host reader must stay attached through arbitrary idle periods
/// (a user who is not typing) yet must not let a peer that starts a frame and
/// then withholds the rest wedge the reader thread and its slot forever. This
/// reader distinguishes the two: a timeout with no bytes buffered reports
/// [`ClientFramePoll::IdleTimeout`] (keep waiting), while a timeout mid-frame
/// reports [`ClientFramePoll::PartialTimeout`] (a stall the host can bound).
/// Partial progress is retained across calls exactly like [`HostFrameReader`],
/// so a `WouldBlock`/`TimedOut` never desyncs the stream.
#[derive(Debug, Default)]
pub struct ClientFrameReader {
    header: [u8; FRAME_HEADER_LEN],
    header_filled: usize,
    header_complete: bool,
    declared_len: usize,
    payload: Vec<u8>,
}

impl ClientFrameReader {
    /// Read (or resume reading) one client frame. Returns a decoded frame, or a
    /// timeout variant that tells the caller whether a frame is mid-flight.
    pub fn read(&mut self, reader: &mut impl Read) -> Result<ClientFramePoll, ProtocolError> {
        match self.read_raw(reader) {
            Ok((kind, payload)) => decode_client_frame(kind, payload).map(ClientFramePoll::Frame),
            Err(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if self.in_progress() {
                    Ok(ClientFramePoll::PartialTimeout)
                } else {
                    Ok(ClientFramePoll::IdleTimeout)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Whether any bytes of a frame are currently buffered.
    fn in_progress(&self) -> bool {
        self.header_filled > 0 || self.header_complete
    }

    fn read_raw(&mut self, reader: &mut impl Read) -> Result<(u8, Vec<u8>), ProtocolError> {
        while self.header_filled < FRAME_HEADER_LEN {
            let count = read_nonzero(reader, &mut self.header[self.header_filled..])?;
            self.header_filled += count;
            if self.header_filled == FRAME_HEADER_LEN {
                let len =
                    u32::from_be_bytes(self.header[1..5].try_into().expect("header is 5 bytes"))
                        as usize;
                if len > MAX_FRAME_LEN {
                    self.reset();
                    return Err(ProtocolError::FrameTooLarge {
                        len,
                        max: MAX_FRAME_LEN,
                    });
                }
                self.declared_len = len;
                self.header_complete = true;
                self.payload = Vec::new();
            }
        }
        // Grow the payload only as bytes actually arrive (capped per read), so a
        // withheld body never reserves the whole declared length up front.
        while self.payload.len() < self.declared_len {
            let start = self.payload.len();
            let want = (self.declared_len - start).min(CLIENT_PAYLOAD_GROWTH_CHUNK);
            self.payload.resize(start + want, 0);
            match read_nonzero(reader, &mut self.payload[start..start + want]) {
                Ok(count) => self.payload.truncate(start + count),
                Err(error) => {
                    // Keep only the bytes genuinely read; drop this chunk's
                    // speculative tail so resume state stays exact.
                    self.payload.truncate(start);
                    return Err(error);
                }
            }
        }
        let kind = self.header[0];
        let payload = std::mem::take(&mut self.payload);
        self.reset();
        Ok((kind, payload))
    }

    fn reset(&mut self) {
        self.header_filled = 0;
        self.header_complete = false;
        self.declared_len = 0;
        self.payload = Vec::new();
    }
}

/// One `read` into `buf`, retrying `Interrupted` (as `read_exact` does) and
/// mapping a zero-byte read to `UnexpectedEof`. `buf` is never empty here.
fn read_nonzero(reader: &mut impl Read, buf: &mut [u8]) -> Result<usize, ProtocolError> {
    loop {
        match reader.read(buf) {
            Ok(0) => {
                return Err(ProtocolError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                )));
            }
            Ok(count) => return Ok(count),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ProtocolError::Io(error)),
        }
    }
}

fn write_string(writer: &mut impl Write, value: &str, max: usize) -> Result<(), ProtocolError> {
    let bytes = value.as_bytes();
    if bytes.len() > max {
        return Err(ProtocolError::StringTooLarge {
            len: bytes.len(),
            max,
        });
    }
    write_u32(writer, bytes.len() as u32)?;
    writer.write_all(bytes)?;
    Ok(())
}

fn read_string(reader: &mut impl Read, max: usize) -> Result<String, ProtocolError> {
    let len = read_u32(reader)? as usize;
    if len > max {
        return Err(ProtocolError::StringTooLarge { len, max });
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8)
}

fn write_u16(writer: &mut impl Write, value: u16) -> Result<(), ProtocolError> {
    writer.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_u32(writer: &mut impl Write, value: u32) -> Result<(), ProtocolError> {
    writer.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn read_u16(reader: &mut impl Read) -> Result<u16, ProtocolError> {
    let mut bytes = [0u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(reader: &mut impl Read) -> Result<u32, ProtocolError> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_roundtrip(frame: &ClientFrame) -> ClientFrame {
        let mut buffer = Vec::new();
        write_client_frame(&mut buffer, frame).expect("write client frame");
        read_client_frame(&mut buffer.as_slice()).expect("read client frame")
    }

    #[test]
    fn shutdown_client_frame_round_trips() {
        assert_eq!(
            client_roundtrip(&ClientFrame::Shutdown),
            ClientFrame::Shutdown
        );
    }

    #[test]
    fn detach_client_frame_round_trips() {
        // Sibling of the Shutdown frame; the empty-payload kinds must not alias.
        assert_eq!(client_roundtrip(&ClientFrame::Detach), ClientFrame::Detach);
    }

    #[test]
    fn shutdown_kind_with_payload_is_invalid() {
        // Hand-craft a kind-104 frame carrying a byte; the empty-payload guard
        // (mirroring Detach) must reject it rather than mis-decode.
        let mut buffer = Vec::new();
        write_frame(&mut buffer, 104, b"x").expect("write frame");
        let err = read_client_frame(&mut buffer.as_slice()).expect_err("payload must be rejected");
        assert!(matches!(err, ProtocolError::InvalidPayload("shutdown")));
    }

    /// A scripted `Write`: accepts a fixed byte budget then reports the
    /// bounded send timeout, modeling `SO_SNDTIMEO` firing after a partial
    /// kernel write. Deterministic — no sockets, no sleeps.
    struct StallingWriter {
        accept: usize,
        written: Vec<u8>,
    }

    impl Write for StallingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.accept == 0 {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "send timeout"));
            }
            let n = buf.len().min(self.accept);
            self.accept -= n;
            self.written.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn zero_progress_send_timeout_stays_plain_io() {
        // Nothing accepted before the timeout: the stream is framing-clean,
        // so the error must remain the plain transient Io kind callers may
        // treat as "frame not sent, drop and continue".
        let mut writer = StallingWriter {
            accept: 0,
            written: Vec::new(),
        };
        let err = write_frame(&mut writer, 101, b"payload").expect_err("must time out");
        assert!(matches!(
            err,
            ProtocolError::Io(e) if e.kind() == io::ErrorKind::WouldBlock
        ));
        assert!(writer.written.is_empty(), "nothing may reach the wire");
    }

    #[test]
    fn partial_progress_send_timeout_is_truncated_write() {
        // Three bytes of the 12-byte frame (1 kind + 4 length + 7 payload)
        // land before the timeout: the peer's wire holds a truncated frame,
        // so the distinct fatal variant must surface with the progress count.
        let mut writer = StallingWriter {
            accept: 3,
            written: Vec::new(),
        };
        let err = write_frame(&mut writer, 101, b"payload").expect_err("must time out");
        assert!(matches!(
            err,
            ProtocolError::TruncatedWrite {
                written: 3,
                len: 12
            }
        ));
        assert_eq!(writer.written.len(), 3);
    }

    #[test]
    fn unknown_client_frame_kind_errors_without_panicking() {
        // Version-skew safety: an unknown kind decodes to InvalidFrameKind, never
        // a panic, so a reader thread can drop the client cleanly.
        let mut buffer = Vec::new();
        write_frame(&mut buffer, 200, &[]).expect("write frame");
        let err = read_client_frame(&mut buffer.as_slice()).expect_err("unknown kind must error");
        assert!(matches!(err, ProtocolError::InvalidFrameKind(200)));
    }

    /// A scripted `Read`: replays a fixed sequence of byte chunks and
    /// `WouldBlock` timeouts, modeling a socket with `SO_RCVTIMEO` whose peer
    /// stalls mid-frame. Deterministic — no sockets, no sleeps.
    enum Step {
        Bytes(Vec<u8>),
        Timeout,
    }

    struct ScriptedReader {
        steps: std::collections::VecDeque<Step>,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.steps.front_mut() {
                None => Ok(0), // EOF
                Some(Step::Timeout) => {
                    self.steps.pop_front();
                    Err(io::Error::from(io::ErrorKind::WouldBlock))
                }
                Some(Step::Bytes(bytes)) => {
                    let count = bytes.len().min(buf.len());
                    buf[..count].copy_from_slice(&bytes[..count]);
                    bytes.drain(..count);
                    if bytes.is_empty() {
                        self.steps.pop_front();
                    }
                    Ok(count)
                }
            }
        }
    }

    fn encoded_host_frame(frame: &HostFrame) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_host_frame(&mut bytes, frame).expect("encode host frame");
        bytes
    }

    fn is_would_block(err: &ProtocolError) -> bool {
        matches!(err, ProtocolError::Io(error) if error.kind() == io::ErrorKind::WouldBlock)
    }

    /// Regression: audit C-3 -- the `Resized` host frame round-trips its
    /// dimensions and render revision through the wire codec, and an older
    /// client (one that predates kind 6) rejects it as an unknown frame kind
    /// rather than misdecoding it as another frame.
    #[test]
    fn resized_host_frame_round_trips_and_is_rejected_by_older_decoders() {
        let frame = HostFrame::Resized {
            columns: 120,
            rows: 40,
            render_revision: 987_654_321,
        };
        let bytes = encoded_host_frame(&frame);
        let mut cursor = io::Cursor::new(bytes.clone());
        assert_eq!(read_host_frame(&mut cursor).expect("decode resized"), frame);
        // Wire kind 6, 16-byte payload (u32 columns + u32 rows + u64 revision).
        assert_eq!(bytes[0], 6, "resized frame is wire kind 6");
        // A wrong-length payload is a hard decode error, never a silent partial.
        assert!(matches!(
            decode_host_frame(6, vec![0u8; 15]),
            Err(ProtocolError::InvalidPayload("resized"))
        ));
        // An older decoder with no arm for kind 6 rejects it as an unknown kind
        // rather than misreading it (models a binary-downgrade skew).
        let mut unknown = bytes.clone();
        unknown[0] = 200;
        let mut cursor = io::Cursor::new(unknown);
        assert!(matches!(
            read_host_frame(&mut cursor),
            Err(ProtocolError::InvalidFrameKind(200))
        ));
    }

    /// Regression: audit P1 — a read timeout firing mid-frame must not desync
    /// the stream. The stateless reader loses the bytes `read_exact` already
    /// consumed and the retry decodes leftover payload as a frame header; the
    /// resumable reader keeps partial progress, so the poll-retry pattern
    /// yields both frames intact.
    #[test]
    fn host_frame_reader_resumes_after_mid_frame_timeout() {
        let first = HostFrame::Output(b"FIRSThALF!".to_vec());
        let second = HostFrame::Invalidate { render_revision: 7 };
        let first_bytes = encoded_host_frame(&first);
        // Stall points: mid-header (after 3 of 5 header bytes) and mid-payload
        // (after 5 of 10 payload bytes) — both read_exact loss sites.
        let mut reader = ScriptedReader {
            steps: [
                Step::Bytes(first_bytes[..3].to_vec()),
                Step::Timeout,
                Step::Bytes(first_bytes[3..10].to_vec()),
                Step::Timeout,
                Step::Bytes(first_bytes[10..].to_vec()),
                Step::Bytes(encoded_host_frame(&second)),
            ]
            .into(),
        };
        let mut frames = Vec::new();
        let mut timeouts = 0;
        let mut frame_reader = HostFrameReader::default();
        while frames.len() < 2 {
            match frame_reader.read(&mut reader) {
                Ok(frame) => frames.push(frame),
                Err(err) if is_would_block(&err) => timeouts += 1,
                Err(err) => panic!("desync: {err}"),
            }
        }
        assert_eq!(timeouts, 2, "both scripted mid-frame timeouts must fire");
        assert_eq!(frames, vec![first, second]);
    }

    /// The same script against the stateless `read_host_frame` poll pattern
    /// documents WHY the resumable reader exists: the retry after a mid-frame
    /// timeout mis-parses leftover bytes. Guards against someone "simplifying"
    /// a poll site back to the free function.
    #[test]
    fn stateless_read_host_frame_desyncs_on_mid_frame_timeout() {
        let first_bytes = encoded_host_frame(&HostFrame::Output(b"FIRSThALF!".to_vec()));
        let mut reader = ScriptedReader {
            steps: [
                Step::Bytes(first_bytes[..10].to_vec()),
                Step::Timeout,
                Step::Bytes(first_bytes[10..].to_vec()),
            ]
            .into(),
        };
        let mut outcome = Vec::new();
        for _ in 0..4 {
            match read_host_frame(&mut reader) {
                Ok(frame) => outcome.push(Ok(frame)),
                Err(err) if is_would_block(&err) => continue,
                Err(err) => {
                    outcome.push(Err(err));
                    break;
                }
            }
        }
        // The retry parsed payload bytes "hALF!" as a frame header: kind 0x68
        // ('h') with a bogus multi-GB length — never the real Output frame.
        assert!(
            matches!(
                outcome.as_slice(),
                [Err(ProtocolError::FrameTooLarge { .. })]
                    | [Err(ProtocolError::InvalidFrameKind(_))]
                    | [Err(ProtocolError::Io(_))]
            ),
            "stateless poll must desync (got {outcome:?})"
        );
    }

    /// An oversized frame length is fatal but must leave the reader reset, not
    /// holding a header with no payload buffer.
    #[test]
    fn host_frame_reader_rejects_oversized_frame_and_resets() {
        let mut bytes = vec![2u8];
        bytes.extend_from_slice(&((MAX_FRAME_LEN as u32) + 1).to_be_bytes());
        let follow_on = encoded_host_frame(&HostFrame::Output(b"ok".to_vec()));
        let mut reader = ScriptedReader {
            steps: [Step::Bytes(bytes), Step::Bytes(follow_on)].into(),
        };
        let mut frame_reader = HostFrameReader::default();
        let err = frame_reader
            .read(&mut reader)
            .expect_err("oversized frame must be rejected");
        assert!(matches!(err, ProtocolError::FrameTooLarge { .. }));
        // After the fatal error the reader starts a fresh frame (the stream is
        // undefined per the docs, but the reader's own state must be clean).
        let frame = frame_reader.read(&mut reader).expect("fresh frame decodes");
        assert_eq!(frame, HostFrame::Output(b"ok".to_vec()));
    }
}
