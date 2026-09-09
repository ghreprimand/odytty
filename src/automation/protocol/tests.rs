// SPDX-License-Identifier: GPL-3.0-only
use super::*;

fn object(kind: ObjectKind) -> ObjectId {
    ObjectId {
        instance: [0x42; 16],
        kind,
        serial: u64::MAX - 1,
    }
}

fn request(action: Action) -> Request {
    Request {
        version: VERSION,
        request_id: u64::MAX,
        action,
    }
}

fn actions() -> Vec<Action> {
    let window = object(ObjectKind::Window);
    let pane = object(ObjectKind::Pane);
    vec![
        Action::Capabilities,
        Action::List,
        Action::Status { target: pane },
        Action::Focus { target: pane },
        Action::OpenProfile {
            window,
            name: "Work profile".into(),
        },
        Action::CreateTab { window },
        Action::CreateWorkspace {
            window,
            name: "Workspace 雪".into(),
        },
        Action::Split {
            pane,
            direction: SplitDirection::Columns,
        },
        Action::Split {
            pane,
            direction: SplitDirection::Rows,
        },
        Action::Rename {
            target: object(ObjectKind::Tab),
            name: "Build; $(literal)".into(),
        },
    ]
}

#[test]
fn structural_actions_roundtrip_without_lossy_ids_or_names() {
    for action in actions() {
        let value = request(action);
        let encoded = encode(&value).unwrap();
        assert_eq!(decode(&encoded).unwrap(), value);
        let mut framed = Vec::new();
        write_request(&mut framed, &value).unwrap();
        assert_eq!(read_request(&mut framed.as_slice()).unwrap(), value);
    }
}

#[test]
fn every_truncation_and_appended_field_is_rejected() {
    for action in actions() {
        let encoded = encode(&request(action)).unwrap();
        for end in 0..encoded.len() {
            assert!(
                decode(&encoded[..end]).is_err(),
                "accepted truncated frame at {end}"
            );
        }
        let mut extra = encoded;
        extra.extend_from_slice(b"send_text=unsafe");
        assert_eq!(decode(&extra), Err(ErrorCode::InvalidRequest));
    }
}

#[test]
fn versions_and_unknown_capabilities_fail_closed() {
    let mut bytes = encode(&request(Action::Capabilities)).unwrap();
    bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(decode(&bytes), Err(ErrorCode::VersionMismatch));
    bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
    for opcode in 9..=255 {
        bytes[14] = opcode;
        assert_eq!(decode(&bytes), Err(ErrorCode::UnsupportedCapability));
    }
}

#[test]
fn privilege_and_target_kinds_are_explicit() {
    for action in actions() {
        assert_eq!(
            action.is_read_only(),
            matches!(
                action,
                Action::Capabilities | Action::List | Action::Status { .. }
            )
        );
    }
    assert_eq!(
        encode(&request(Action::CreateTab {
            window: object(ObjectKind::Pane)
        })),
        Err(ErrorCode::InvalidRequest)
    );
    assert_eq!(
        encode(&request(Action::Split {
            pane: object(ObjectKind::Tab),
            direction: SplitDirection::Rows
        })),
        Err(ErrorCode::InvalidRequest)
    );
}

#[test]
fn hostile_names_and_invalid_utf8_are_rejected() {
    for name in [
        String::new(),
        "x".repeat(MAX_NAME_BYTES + 1),
        "line\nline".into(),
        "a\0b".into(),
        "a\u{1b}b".into(),
    ] {
        assert_eq!(
            encode(&request(Action::Rename {
                target: object(ObjectKind::Workspace),
                name
            })),
            Err(ErrorCode::InvalidRequest)
        );
    }
    let mut bytes = encode(&request(Action::Rename {
        target: object(ObjectKind::Window),
        name: "a".into(),
    }))
    .unwrap();
    *bytes.last_mut().unwrap() = 0xff;
    assert_eq!(decode(&bytes), Err(ErrorCode::InvalidRequest));
}

#[test]
fn oversize_header_is_rejected_before_body_read() {
    struct HeaderOnly {
        bytes: &'static [u8],
    }
    impl Read for HeaderOnly {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            assert!(
                !self.bytes.is_empty(),
                "attempted to read an oversized body"
            );
            let count = out.len().min(self.bytes.len());
            out[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }
    assert_eq!(
        read_request(&mut HeaderOnly { bytes: &[255; 4] })
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn partial_frame_write_propagates_failure() {
    struct ShortWriter {
        remaining: usize,
    }
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            let count = bytes.len().min(self.remaining);
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        write_request(&mut ShortWriter { remaining: 7 }, &request(Action::List))
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
}
