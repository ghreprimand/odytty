// SPDX-License-Identifier: GPL-3.0-only
//! Bounded structural replies. No titles, paths, command lines or terminal data.

use super::*;

pub const MAX_OBJECTS: usize = 1024;
/// Smallest encoded object record: a 25-byte id, the parent flag and the
/// status flags.
const MIN_OBJECT_BYTES: usize = 25 + 1 + 1;
const RESPONSE_MAGIC: &[u8; 4] = b"ODYR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectStatus {
    pub id: ObjectId,
    pub parent: Option<ObjectId>,
    pub focused: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Structural control is explicitly enabled separately from read access.
    Capabilities {
        structural_control: bool,
        quick_terminal_toggle: bool,
    },
    Objects(Vec<ObjectStatus>),
    /// Creation returns its new stable identity. Focus/rename return the target.
    Applied(ObjectId),
    /// The action was queued for the next event-loop maintenance turn.
    Accepted,
    Error(ErrorCode),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub request_id: u64,
    pub reply: Reply,
}

pub fn write_response(writer: &mut impl Write, response: &Response) -> io::Result<()> {
    let body = encode_response(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    write_frame(writer, &body)
}

pub fn read_response(reader: &mut impl Read) -> io::Result<Response> {
    let bytes = read_frame(reader)?;
    decode_response(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn encode_response(response: &Response) -> Result<Vec<u8>, ErrorCode> {
    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(RESPONSE_MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&response.request_id.to_le_bytes());
    match &response.reply {
        Reply::Capabilities {
            structural_control,
            quick_terminal_toggle,
        } => {
            bytes.extend_from_slice(&[
                0,
                u8::from(*structural_control),
                u8::from(*quick_terminal_toggle),
            ]);
        }
        Reply::Objects(objects) => {
            if objects.len() > MAX_OBJECTS {
                return Err(ErrorCode::TooLarge);
            }
            bytes.push(1);
            bytes.extend_from_slice(&(objects.len() as u16).to_le_bytes());
            for object in objects {
                put_object(&mut bytes, &object.id);
                bytes.push(u8::from(object.parent.is_some()));
                if let Some(parent) = object.parent {
                    put_object(&mut bytes, &parent);
                }
                bytes.push(u8::from(object.focused) | (u8::from(object.hidden) << 1));
            }
        }
        Reply::Applied(id) => {
            bytes.push(2);
            put_object(&mut bytes, id);
        }
        Reply::Error(error) => {
            bytes.extend_from_slice(&[
                3,
                match error {
                    ErrorCode::InvalidRequest => 0,
                    ErrorCode::VersionMismatch => 1,
                    ErrorCode::UnsupportedCapability => 2,
                    ErrorCode::PermissionDenied => 3,
                    ErrorCode::StaleIdentity => 4,
                    ErrorCode::Busy => 5,
                    ErrorCode::TimedOut => 6,
                    ErrorCode::TooLarge => 7,
                    ErrorCode::OutcomeUnknown => 8,
                    ErrorCode::Unavailable => 9,
                    ErrorCode::Cancelled => 10,
                },
            ]);
        }
        Reply::Accepted => bytes.push(4),
    }
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ErrorCode::TooLarge);
    }
    Ok(bytes)
}

fn decode_response(bytes: &[u8]) -> Result<Response, ErrorCode> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ErrorCode::TooLarge);
    }
    let mut cursor = Cursor(bytes);
    if cursor.take(4)? != RESPONSE_MAGIC {
        return Err(ErrorCode::InvalidRequest);
    }
    if u16::from_le_bytes(cursor.array()?) != VERSION {
        return Err(ErrorCode::VersionMismatch);
    }
    let request_id = u64::from_le_bytes(cursor.array()?);
    let reply = match cursor.byte()? {
        0 => Reply::Capabilities {
            structural_control: boolean(&mut cursor)?,
            quick_terminal_toggle: boolean(&mut cursor)?,
        },
        1 => {
            let count = u16::from_le_bytes(cursor.array()?) as usize;
            if count > MAX_OBJECTS {
                return Err(ErrorCode::TooLarge);
            }
            // A count the remaining bytes cannot hold is a truncated reply,
            // refused before any allocation.
            if count.saturating_mul(MIN_OBJECT_BYTES) > cursor.0.len() {
                return Err(ErrorCode::InvalidRequest);
            }
            let mut objects = Vec::with_capacity(count);
            for _ in 0..count {
                let id = cursor.object()?;
                let parent = if boolean(&mut cursor)? {
                    Some(cursor.object()?)
                } else {
                    None
                };
                let flags = cursor.byte()?;
                if flags & !3 != 0 {
                    return Err(ErrorCode::InvalidRequest);
                }
                objects.push(ObjectStatus {
                    id,
                    parent,
                    focused: flags & 1 != 0,
                    hidden: flags & 2 != 0,
                });
            }
            Reply::Objects(objects)
        }
        2 => Reply::Applied(cursor.object()?),
        3 => Reply::Error(match cursor.byte()? {
            0 => ErrorCode::InvalidRequest,
            1 => ErrorCode::VersionMismatch,
            2 => ErrorCode::UnsupportedCapability,
            3 => ErrorCode::PermissionDenied,
            4 => ErrorCode::StaleIdentity,
            5 => ErrorCode::Busy,
            6 => ErrorCode::TimedOut,
            7 => ErrorCode::TooLarge,
            8 => ErrorCode::OutcomeUnknown,
            9 => ErrorCode::Unavailable,
            10 => ErrorCode::Cancelled,
            _ => return Err(ErrorCode::InvalidRequest),
        }),
        4 => Reply::Accepted,
        _ => return Err(ErrorCode::InvalidRequest),
    };
    if !cursor.0.is_empty() {
        return Err(ErrorCode::InvalidRequest);
    }
    Ok(Response { request_id, reply })
}

fn boolean(cursor: &mut Cursor<'_>) -> Result<bool, ErrorCode> {
    match cursor.byte()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ErrorCode::InvalidRequest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object() -> ObjectStatus {
        ObjectStatus {
            id: ObjectId {
                instance: [7; 16],
                kind: ObjectKind::Window,
                serial: u64::MAX,
            },
            parent: None,
            focused: true,
            hidden: false,
        }
    }

    #[test]
    fn replies_roundtrip_and_reject_truncation_and_trailing_bytes() {
        for reply in [
            Reply::Capabilities {
                structural_control: false,
                quick_terminal_toggle: false,
            },
            Reply::Capabilities {
                structural_control: true,
                quick_terminal_toggle: true,
            },
            Reply::Objects(vec![object()]),
            Reply::Applied(object().id),
            Reply::Accepted,
            Reply::Error(ErrorCode::OutcomeUnknown),
        ] {
            let value = Response {
                request_id: u64::MAX,
                reply,
            };
            let mut bytes = Vec::new();
            write_response(&mut bytes, &value).unwrap();
            assert_eq!(read_response(&mut bytes.as_slice()).unwrap(), value);
            let body = &bytes[4..];
            for end in 0..body.len() {
                assert!(decode_response(&body[..end]).is_err());
            }
            let mut extra = body.to_vec();
            extra.push(0);
            assert_eq!(decode_response(&extra), Err(ErrorCode::InvalidRequest));
        }
    }

    #[test]
    fn count_and_frame_bounds_are_enforced_before_object_allocation() {
        let oversized = Response {
            request_id: 0,
            reply: Reply::Objects(vec![object(); MAX_OBJECTS + 1]),
        };
        assert_eq!(encode_response(&oversized), Err(ErrorCode::TooLarge));
        let mut body = encode_response(&Response {
            request_id: 0,
            reply: Reply::Objects(vec![]),
        })
        .unwrap();
        body[15..17].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(decode_response(&body), Err(ErrorCode::TooLarge));
    }

    /// The two ways a reply can be too big or too short, pinned through the
    /// framed reader a client uses: a length prefix outside the window is
    /// `TooLarge`, an object count above the cap is `TooLarge`, and a count the
    /// body cannot hold is `InvalidRequest`.
    #[test]
    fn framed_replies_classify_prefix_count_and_truncation_separately() {
        fn code_of(error: std::io::Error) -> ErrorCode {
            *error
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<ErrorCode>())
                .expect("a protocol error code")
        }
        fn framed(body: &[u8]) -> Vec<u8> {
            let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
            bytes.extend_from_slice(body);
            bytes
        }
        for length in [0u32, 14, MAX_MESSAGE_BYTES as u32 + 1, u32::MAX] {
            let error = read_response(&mut length.to_le_bytes().as_slice()).unwrap_err();
            assert_eq!(code_of(error), ErrorCode::TooLarge, "prefix {length}");
        }
        let mut body = encode_response(&Response {
            request_id: 0,
            reply: Reply::Objects(vec![]),
        })
        .unwrap();
        body[15..17].copy_from_slice(&u16::MAX.to_le_bytes());
        let error = read_response(&mut framed(&body).as_slice()).unwrap_err();
        assert_eq!(code_of(error), ErrorCode::TooLarge, "count above the cap");
        let mut body = encode_response(&Response {
            request_id: 0,
            reply: Reply::Objects(vec![object()]),
        })
        .unwrap();
        body.truncate(body.len() - 1);
        let error = read_response(&mut framed(&body).as_slice()).unwrap_err();
        assert_eq!(code_of(error), ErrorCode::InvalidRequest, "truncated body");
    }

    #[test]
    fn a_count_the_body_cannot_hold_is_a_truncated_reply_not_an_oversized_one() {
        let mut body = encode_response(&Response {
            request_id: 0,
            reply: Reply::Objects(vec![object()]),
        })
        .unwrap();
        body.truncate(body.len() - 1);
        assert_eq!(decode_response(&body), Err(ErrorCode::InvalidRequest));
        let mut body = encode_response(&Response {
            request_id: 0,
            reply: Reply::Objects(vec![]),
        })
        .unwrap();
        let count = u16::try_from(MAX_OBJECTS).unwrap();
        body[15..17].copy_from_slice(&count.to_le_bytes());
        assert_eq!(
            decode_response(&body),
            Err(ErrorCode::InvalidRequest),
            "the largest allowed count with no objects is truncated"
        );
    }
}
