// SPDX-License-Identifier: GPL-3.0-only
//! Version-one local structural-control frames, independent of terminal I/O.

use std::io::{self, Read, Write};

pub const VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_NAME_BYTES: usize = 256;
const MAGIC: &[u8; 4] = b"ODYC";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Window,
    Workspace,
    Tab,
    Pane,
}

/// Instance entropy prevents IDs from an earlier process lifetime resolving
/// after restart. The live owner allocates the serial; it is never a tree index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectId {
    pub instance: [u8; 16],
    pub kind: ObjectKind,
    pub serial: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    Columns,
    Rows,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Capabilities,
    List,
    Status {
        target: ObjectId,
    },
    Focus {
        target: ObjectId,
    },
    OpenProfile {
        window: ObjectId,
        name: String,
    },
    CreateTab {
        window: ObjectId,
    },
    CreateWorkspace {
        window: ObjectId,
        name: String,
    },
    Split {
        pane: ObjectId,
        direction: SplitDirection,
    },
    Rename {
        target: ObjectId,
        name: String,
    },
}

impl Action {
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::Capabilities | Self::List | Self::Status { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub version: u16,
    pub request_id: u64,
    pub action: Action,
}

/// Stable protocol errors contain no reflected untrusted payload or local path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidRequest,
    VersionMismatch,
    UnsupportedCapability,
    PermissionDenied,
    StaleIdentity,
    Busy,
    TimedOut,
    TooLarge,
    /// The owner already started a request when its caller stopped waiting.
    /// Retrying a structural mutation could duplicate it.
    OutcomeUnknown,
    Unavailable,
    Cancelled,
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "invalid_request",
            Self::VersionMismatch => "version_mismatch",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::PermissionDenied => "permission_denied",
            Self::StaleIdentity => "stale_identity",
            Self::Busy => "busy",
            Self::TimedOut => "timed_out",
            Self::TooLarge => "too_large",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
        })
    }
}

impl std::error::Error for ErrorCode {}

/// Read a single bounded frame. The transport sets its read deadline before
/// calling. Any read or parse error is fatal to that connection; no resync.
pub fn read_request(reader: &mut impl Read) -> io::Result<Request> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if !(15..=MAX_MESSAGE_BYTES).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            ErrorCode::TooLarge,
        ));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    decode(&body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// A partial write is fatal to the transport; it must never retry as a new frame.
pub fn write_request(writer: &mut impl Write, request: &Request) -> io::Result<()> {
    let body =
        encode(request).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    writer.write_all(&(body.len() as u32).to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()
}

pub fn decode(bytes: &[u8]) -> Result<Request, ErrorCode> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ErrorCode::TooLarge);
    }
    let mut cursor = Cursor(bytes);
    if cursor.take(4)? != MAGIC {
        return Err(ErrorCode::InvalidRequest);
    }
    let version = u16::from_le_bytes(cursor.array()?);
    if version != VERSION {
        return Err(ErrorCode::VersionMismatch);
    }
    let request_id = u64::from_le_bytes(cursor.array()?);
    let action = match cursor.byte()? {
        0 => Action::Capabilities,
        1 => Action::List,
        2 => Action::Status {
            target: cursor.object()?,
        },
        3 => Action::Focus {
            target: cursor.object()?,
        },
        4 => Action::OpenProfile {
            window: cursor.kind(ObjectKind::Window)?,
            name: cursor.name()?,
        },
        5 => Action::CreateTab {
            window: cursor.kind(ObjectKind::Window)?,
        },
        6 => Action::CreateWorkspace {
            window: cursor.kind(ObjectKind::Window)?,
            name: cursor.name()?,
        },
        7 => Action::Split {
            pane: cursor.kind(ObjectKind::Pane)?,
            direction: match cursor.byte()? {
                0 => SplitDirection::Columns,
                1 => SplitDirection::Rows,
                _ => return Err(ErrorCode::InvalidRequest),
            },
        },
        8 => Action::Rename {
            target: cursor.object()?,
            name: cursor.name()?,
        },
        _ => return Err(ErrorCode::UnsupportedCapability),
    };
    if !cursor.0.is_empty() {
        return Err(ErrorCode::InvalidRequest);
    }
    Ok(Request {
        version,
        request_id,
        action,
    })
}

pub fn encode(request: &Request) -> Result<Vec<u8>, ErrorCode> {
    if request.version != VERSION {
        return Err(ErrorCode::VersionMismatch);
    }
    let mut body = Vec::with_capacity(64);
    body.extend_from_slice(MAGIC);
    body.extend_from_slice(&request.version.to_le_bytes());
    body.extend_from_slice(&request.request_id.to_le_bytes());
    match &request.action {
        Action::Capabilities => body.push(0),
        Action::List => body.push(1),
        Action::Status { target } => {
            body.push(2);
            put_object(&mut body, target);
        }
        Action::Focus { target } => {
            body.push(3);
            put_object(&mut body, target);
        }
        Action::OpenProfile { window, name } => {
            body.push(4);
            put_object(&mut body, window);
            put_name(&mut body, name)?;
        }
        Action::CreateTab { window } => {
            body.push(5);
            put_object(&mut body, window);
        }
        Action::CreateWorkspace { window, name } => {
            body.push(6);
            put_object(&mut body, window);
            put_name(&mut body, name)?;
        }
        Action::Split { pane, direction } => {
            body.push(7);
            put_object(&mut body, pane);
            body.push(u8::from(*direction == SplitDirection::Rows));
        }
        Action::Rename { target, name } => {
            body.push(8);
            put_object(&mut body, target);
            put_name(&mut body, name)?;
        }
    }
    // The same validator applies on both sides, including object-kind constraints.
    decode(&body)?;
    Ok(body)
}

fn put_object(bytes: &mut Vec<u8>, object: &ObjectId) {
    bytes.extend_from_slice(&object.instance);
    bytes.push(match object.kind {
        ObjectKind::Window => 0,
        ObjectKind::Workspace => 1,
        ObjectKind::Tab => 2,
        ObjectKind::Pane => 3,
    });
    bytes.extend_from_slice(&object.serial.to_le_bytes());
}

fn put_name(bytes: &mut Vec<u8>, name: &str) -> Result<(), ErrorCode> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(ErrorCode::InvalidRequest);
    }
    bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
    bytes.extend_from_slice(name.as_bytes());
    Ok(())
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ErrorCode> {
        let (head, tail) = self
            .0
            .split_at_checked(count)
            .ok_or(ErrorCode::InvalidRequest)?;
        self.0 = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ErrorCode> {
        self.take(N)?
            .try_into()
            .map_err(|_| ErrorCode::InvalidRequest)
    }

    fn byte(&mut self) -> Result<u8, ErrorCode> {
        Ok(self.array::<1>()?[0])
    }

    fn object(&mut self) -> Result<ObjectId, ErrorCode> {
        let instance = self.array()?;
        let kind = match self.byte()? {
            0 => ObjectKind::Window,
            1 => ObjectKind::Workspace,
            2 => ObjectKind::Tab,
            3 => ObjectKind::Pane,
            _ => return Err(ErrorCode::InvalidRequest),
        };
        Ok(ObjectId {
            instance,
            kind,
            serial: u64::from_le_bytes(self.array()?),
        })
    }

    fn kind(&mut self, expected: ObjectKind) -> Result<ObjectId, ErrorCode> {
        let object = self.object()?;
        if object.kind != expected {
            return Err(ErrorCode::InvalidRequest);
        }
        Ok(object)
    }

    fn name(&mut self) -> Result<String, ErrorCode> {
        let length = u16::from_le_bytes(self.array()?) as usize;
        if !(1..=MAX_NAME_BYTES).contains(&length) {
            return Err(ErrorCode::InvalidRequest);
        }
        let name =
            std::str::from_utf8(self.take(length)?).map_err(|_| ErrorCode::InvalidRequest)?;
        if name.chars().any(char::is_control) {
            return Err(ErrorCode::InvalidRequest);
        }
        Ok(name.to_owned())
    }
}

#[cfg(test)]
#[path = "protocol/tests.rs"]
mod tests;

#[path = "protocol/response.rs"]
mod response;
pub use response::{ObjectStatus, Reply, Response, read_response, write_response};
