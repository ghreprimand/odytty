// SPDX-License-Identifier: GPL-3.0-only
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
fn osc_with_exactly_max_params_keeps_every_field_distinct() {
    // 16 fields fill the parameter table exactly; each stays its own param.
    let payload: Vec<String> = (0..16).map(|n| n.to_string()).collect();
    let bytes = format!("\x1b]{}\x07", payload.join(";"));
    let expected: Vec<Vec<u8>> = payload.iter().map(|f| f.as_bytes().to_vec()).collect();
    assert_eq!(
        drive(bytes.as_bytes()),
        vec![Action::Osc {
            params: expected,
            bell: true,
        }]
    );
}

#[test]
fn osc_overflow_fields_are_absorbed_into_the_final_param() {
    // More fields than parameter slots: the final slot absorbs the rest of
    // the payload verbatim (separators included) instead of silently
    // dropping the tail, so rejoining the params with `;` reconstructs the
    // exact original payload.
    let payload: Vec<String> = (0..20).map(|n| n.to_string()).collect();
    let bytes = format!("\x1b]{}\x07", payload.join(";"));
    let actions = drive(bytes.as_bytes());
    let Action::Osc { params, bell: true } = &actions[0] else {
        panic!("expected an OSC dispatch, got {actions:?}");
    };
    assert_eq!(params.len(), 16);
    for (index, param) in params.iter().take(15).enumerate() {
        assert_eq!(param, index.to_string().as_bytes());
    }
    assert_eq!(params[15], b"15;16;17;18;19".to_vec());
    let rejoined = params
        .iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect::<Vec<_>>()
        .join(";");
    assert_eq!(rejoined, payload.join(";"), "exact payload reconstruction");
}

#[test]
fn semicolon_rich_osc8_hyperlink_payload_survives_intact() {
    // An OSC 8 URI with many literal semicolons pushes the field count past
    // the table; the tail must arrive complete for the URI to be usable.
    let payload = "8;;http://example.test/a;b;c;d;e;f;g;h;i;j;k;l;m;n;o;p;q;r";
    let bytes = format!("\x1b]{payload}\x07");
    let actions = drive(bytes.as_bytes());
    let Action::Osc { params, bell: true } = &actions[0] else {
        panic!("expected an OSC dispatch, got {actions:?}");
    };
    assert_eq!(params.len(), 16);
    let rejoined = params
        .iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect::<Vec<_>>()
        .join(";");
    assert_eq!(rejoined, payload, "the URI tail is not truncated");
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
    input.extend(std::iter::repeat_n(b'x', 4096));
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
    input.extend(std::iter::repeat_n(b'y', (1 << 20) + 16));
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

/// I-6: the OSC dispatch path must not heap-allocate per terminated OSC. Shell
/// integration emits several OSC 133/7 per prompt; the previous code built a
/// `Vec<&[u8]>` on every dispatch. A process-wide counting allocator (active
/// only while a per-thread guard is set) proves a warmed parser dispatches a
/// realistic prompt corpus with zero allocations.
mod alloc_probe {
    use super::super::VtDispatch;
    use super::super::driver::OdyParser;
    use super::super::params::Params;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static RECORDING: Cell<bool> = const { Cell::new(false) };
        static ALLOC_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    // SAFETY: every method forwards to the System allocator; the only added work
    // is reading/incrementing const-initialized thread-locals, which never
    // allocate, so the allocator stays re-entrancy-safe.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if RECORDING.with(Cell::get) {
                ALLOC_COUNT.with(|c| c.set(c.get() + 1));
            }
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if RECORDING.with(Cell::get) {
                ALLOC_COUNT.with(|c| c.set(c.get() + 1));
            }
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    fn count_allocs(f: impl FnOnce()) -> usize {
        ALLOC_COUNT.with(|c| c.set(0));
        RECORDING.with(|c| c.set(true));
        f();
        RECORDING.with(|c| c.set(false));
        ALLOC_COUNT.with(Cell::get)
    }

    /// A sink that records nothing, so the only allocations the probe can observe
    /// are the parser's own.
    struct NullSink;

    impl VtDispatch for NullSink {
        fn print(&mut self, _c: char) {}
        fn execute(&mut self, _byte: u8) {}
        fn csi_dispatch(&mut self, _p: &Params, _i: &[u8], _ig: bool, _a: char) {}
        fn esc_dispatch(&mut self, _i: &[u8], _ig: bool, _b: u8) {}
        fn osc_dispatch(&mut self, _params: &[&[u8]], _bell: bool) {}
        fn hook(&mut self, _p: &Params, _i: &[u8], _ig: bool, _a: char) {}
        fn put(&mut self, _byte: u8) {}
        fn unhook(&mut self) {}
        fn apc_dispatch(&mut self, _data: &[u8]) {}
    }

    #[test]
    fn osc_dispatch_is_allocation_free_after_warmup() {
        // A full integrated prompt: OSC 133;A/B, OSC 7 cwd, OSC 133;C, OSC 133;D.
        const PROMPT: &[u8] = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]7;file://host/home/user/project\x07\x1b]133;C\x07output\r\n\x1b]133;D;0\x07";

        let mut parser = OdyParser::new();
        let mut sink = NullSink;
        // Warm up: grow the parser's reused OSC buffer to its steady capacity so
        // later dispatches never realloc it.
        for _ in 0..8 {
            parser.advance(&mut sink, PROMPT);
        }

        let allocs = count_allocs(|| {
            for _ in 0..256 {
                parser.advance(&mut sink, PROMPT);
            }
        });

        assert_eq!(
            allocs, 0,
            "warmed OSC dispatch must not heap-allocate per prompt (got {allocs} allocations over 256 prompts)"
        );
    }
}
