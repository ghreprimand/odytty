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
    FrameTooLarge { len: usize, max: usize },
    StringTooLarge { len: usize, max: usize },
    InvalidUtf8,
    InvalidPayload(&'static str),
    Rejected(String),
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
    Invalidate { render_revision: u64 },
    SessionExit { exit_code: Option<i32> },
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
    writer.write_all(&[kind])?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
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

    #[test]
    fn unknown_client_frame_kind_errors_without_panicking() {
        // Version-skew safety: an unknown kind decodes to InvalidFrameKind, never
        // a panic, so a reader thread can drop the client cleanly.
        let mut buffer = Vec::new();
        write_frame(&mut buffer, 200, &[]).expect("write frame");
        let err = read_client_frame(&mut buffer.as_slice()).expect_err("unknown kind must error");
        assert!(matches!(err, ProtocolError::InvalidFrameKind(200)));
    }
}
