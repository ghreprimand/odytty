//! Driver-level integration tests for [`OdyParser`] — feed bytes and assert
//! the recorded [`VtDispatch`] action stream matches expectation. These
//! exercise the full Layer 1 → Layer 2 → adapter path; parser golden and
//! self-consistency tests cover the full corpus.

use super::VtDispatch;
use super::driver::OdyParser;
use super::params::Params;

/// Every dispatch action, recorded in order, for assertions.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Print(char),
    Execute(u8),
    Csi {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        ignore: bool,
        action: char,
    },
    Esc {
        intermediates: Vec<u8>,
        ignore: bool,
        byte: u8,
    },
    Osc {
        params: Vec<Vec<u8>>,
        bell: bool,
    },
    Hook {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        ignore: bool,
        action: char,
    },
    Put(u8),
    Unhook,
    Apc(Vec<u8>),
}

#[derive(Default)]
struct Recorder(Vec<Action>);

fn param_groups(params: &Params) -> Vec<Vec<u16>> {
    params.iter().map(<[u16]>::to_vec).collect()
}

impl VtDispatch for Recorder {
    fn print(&mut self, c: char) {
        self.0.push(Action::Print(c));
    }
    fn execute(&mut self, byte: u8) {
        self.0.push(Action::Execute(byte));
    }
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.0.push(Action::Csi {
            params: param_groups(params),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.0.push(Action::Esc {
            intermediates: intermediates.to_vec(),
            ignore,
            byte,
        });
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.0.push(Action::Osc {
            params: params.iter().map(|p| p.to_vec()).collect(),
            bell: bell_terminated,
        });
    }
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.0.push(Action::Hook {
            params: param_groups(params),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }
    fn put(&mut self, byte: u8) {
        self.0.push(Action::Put(byte));
    }
    fn unhook(&mut self) {
        self.0.push(Action::Unhook);
    }
    fn apc_dispatch(&mut self, data: &[u8]) {
        self.0.push(Action::Apc(data.to_vec()));
    }
}

/// Feed `bytes` to a fresh parser and return the recorded actions.
fn drive(bytes: &[u8]) -> Vec<Action> {
    let mut rec = Recorder::default();
    let mut parser = OdyParser::new();
    parser.advance(&mut rec, bytes);
    rec.0
}

#[test]
fn prints_plain_ascii() {
    assert_eq!(drive(b"Hi"), vec![Action::Print('H'), Action::Print('i')]);
}

#[test]
fn executes_c0_controls() {
    assert_eq!(
        drive(b"\r\n\t"),
        vec![
            Action::Execute(b'\r'),
            Action::Execute(b'\n'),
            Action::Execute(b'\t'),
        ]
    );
}

#[test]
fn decodes_utf8_scalars() {
    assert_eq!(
        drive("é→★".as_bytes()),
        vec![Action::Print('é'), Action::Print('→'), Action::Print('★')]
    );
}

#[test]
fn c1_from_utf8_whole_executes() {
    // U+0085 (NEL) decoded whole in Ground executes as 0x85.
    assert_eq!(drive("\u{0085}".as_bytes()), vec![Action::Execute(0x85)]);
}

#[test]
fn c1_from_utf8_split_executes_uniform() {
    // PA2-r uniform-execute policy: a C1 scalar arriving SPLIT across advance()
    // calls also executes (not prints), making chunking irrelevant.
    let mut rec = Recorder::default();
    let mut parser = OdyParser::new();
    parser.advance(&mut rec, &[0xC2]);
    parser.advance(&mut rec, &[0x85]);
    assert_eq!(rec.0, vec![Action::Execute(0x85)]);
}

#[test]
fn csi_cursor_up_carries_param() {
    assert_eq!(
        drive(b"\x1b[5A"),
        vec![Action::Csi {
            params: vec![vec![5]],
            intermediates: vec![],
            ignore: false,
            action: 'A',
        }]
    );
}

#[test]
fn bare_csi_yields_zero_param() {
    // `ESC [ A` has no digits; materialises a single `0`.
    assert_eq!(
        drive(b"\x1b[A"),
        vec![Action::Csi {
            params: vec![vec![0]],
            intermediates: vec![],
            ignore: false,
            action: 'A',
        }]
    );
}

#[test]
fn csi_private_marker_and_intermediate() {
    assert_eq!(
        drive(b"\x1b[?1049h"),
        vec![Action::Csi {
            params: vec![vec![1049]],
            intermediates: vec![b'?'],
            ignore: false,
            action: 'h',
        }]
    );
    assert_eq!(
        drive(b"\x1b[4 q"),
        vec![Action::Csi {
            params: vec![vec![4]],
            intermediates: vec![b' '],
            ignore: false,
            action: 'q',
        }]
    );
}

#[test]
fn csi_subparams_via_colon() {
    assert_eq!(
        drive(b"\x1b[38:2::1:2:3m"),
        vec![Action::Csi {
            params: vec![vec![38, 2, 0, 1, 2, 3]],
            intermediates: vec![],
            ignore: false,
            action: 'm',
        }]
    );
}

#[test]
fn esc_dispatch_simple() {
    assert_eq!(
        drive(b"\x1bM"),
        vec![Action::Esc {
            intermediates: vec![],
            ignore: false,
            byte: b'M',
        }]
    );
}

#[test]
fn osc_bel_and_st_terminators() {
    assert_eq!(
        drive(b"\x1b]0;hi\x07"),
        vec![Action::Osc {
            params: vec![b"0".to_vec(), b"hi".to_vec()],
            bell: true,
        }]
    );
    assert_eq!(
        drive(b"\x1b]2;t\x1b\\"),
        vec![
            Action::Osc {
                params: vec![b"2".to_vec(), b"t".to_vec()],
                bell: false,
            },
            Action::Esc {
                intermediates: vec![],
                ignore: false,
                byte: b'\\',
            },
        ]
    );
}

#[test]
fn dcs_hook_put_unhook() {
    assert_eq!(
        drive(b"\x1bP1;2|ab\x1b\\"),
        vec![
            Action::Hook {
                params: vec![vec![1], vec![2]],
                intermediates: vec![],
                ignore: false,
                action: '|',
            },
            Action::Put(b'a'),
            Action::Put(b'b'),
            Action::Unhook,
            Action::Esc {
                intermediates: vec![],
                ignore: false,
                byte: b'\\',
            },
        ]
    );
}

#[test]
fn apc_payload_is_surfaced() {
    assert_eq!(
        drive(b"\x1b_Gf=100;data\x1b\\"),
        vec![
            Action::Apc(b"Gf=100;data".to_vec()),
            Action::Esc {
                intermediates: vec![],
                ignore: false,
                byte: b'\\',
            },
        ]
    );
}

#[test]
fn apc_under_cap_is_surfaced_whole() {
    let mut input = b"\x1b_G".to_vec();
    input.extend(std::iter::repeat(b'x').take(4096));
    input.extend_from_slice(b"\x1b\\");
    let actions = drive(&input);
    match &actions[..] {
        [Action::Apc(data), Action::Esc { byte: b'\\', .. }] => {
            assert_eq!(data.len(), 1 + 4096, "payload = `G` + 4096 bytes");
            assert_eq!(data[0], b'G');
        }
        other => panic!("expected one surfaced APC + ST esc, got {other:?}"),
    }
}

#[test]
fn apc_over_cap_is_dropped_not_truncated() {
    let mut input = b"\x1b_G".to_vec();
    input.extend(std::iter::repeat(b'y').take((1 << 20) + 16));
    input.extend_from_slice(b"\x1b\\Z");
    let actions = drive(&input);
    assert!(
        !actions.iter().any(|a| matches!(a, Action::Apc(_))),
        "over-cap APC must not be dispatched: {actions:?}"
    );
    assert_eq!(
        actions.last(),
        Some(&Action::Print('Z')),
        "parser recovers to Ground after a dropped APC"
    );
}

#[test]
fn sos_and_pm_strings_are_discarded() {
    assert_eq!(
        drive(b"\x1bXsos\x1b\\"),
        vec![Action::Esc {
            intermediates: vec![],
            ignore: false,
            byte: b'\\',
        }]
    );
    assert_eq!(
        drive(b"\x1b^pm\x1b\\"),
        vec![Action::Esc {
            intermediates: vec![],
            ignore: false,
            byte: b'\\',
        }]
    );
}

#[test]
fn excess_intermediates_set_ignore() {
    let actions = drive(b"\x1b[1 !#p");
    match &actions[..] {
        [Action::Csi { ignore, action, .. }] => {
            assert!(*ignore, "excess intermediates must set ignore");
            assert_eq!(*action, 'p');
        }
        other => panic!("expected one ignored CSI, got {other:?}"),
    }
}

#[test]
fn param_overflow_sets_ignore() {
    let mut input = Vec::from(&b"\x1b["[..]);
    for i in 0..40 {
        if i > 0 {
            input.push(b';');
        }
        input.push(b'1');
    }
    input.push(b'm');
    let actions = drive(&input);
    match &actions[..] {
        [Action::Csi { ignore, .. }] => assert!(*ignore, "param overflow must set ignore"),
        other => panic!("expected one ignored CSI, got {other:?}"),
    }
}

#[test]
fn can_aborts_sequence_to_ground() {
    assert_eq!(
        drive(b"\x1b[31\x18m"),
        vec![Action::Execute(0x18), Action::Print('m')]
    );
}

#[test]
fn split_csi_across_advance_calls() {
    let mut rec = Recorder::default();
    let mut parser = OdyParser::new();
    parser.advance(&mut rec, b"\x1b[1");
    parser.advance(&mut rec, b";2H");
    assert_eq!(
        rec.0,
        vec![Action::Csi {
            params: vec![vec![1], vec![2]],
            intermediates: vec![],
            ignore: false,
            action: 'H',
        }]
    );
}

#[test]
fn lone_c1_byte_executes() {
    // 0x85 alone (invalid UTF-8 lead) executes as NEL, does NOT introduce.
    assert_eq!(drive(b"\x85"), vec![Action::Execute(0x85)]);
    // 0x9B alone (the would-be 8-bit CSI introducer) executes too.
    assert_eq!(
        drive(b"\x9bA"),
        vec![Action::Execute(0x9B), Action::Print('A')]
    );
}

#[test]
fn invalid_utf8_emits_fffd() {
    // 0xFE is never a valid UTF-8 lead → U+FFFD.
    assert_eq!(
        drive(b"\xfeA"),
        vec![Action::Print('\u{FFFD}'), Action::Print('A')]
    );
}
