// SPDX-License-Identifier: GPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use odytty::parser::{OdyParser, Params, VtDispatch};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const SENTINEL: &[u8] = b"\x18\x1bcZ";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Print(char),
    Execute(u8),
    Csi {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        ignore: bool,
        action: char,
    },
    Escape {
        intermediates: Vec<u8>,
        ignore: bool,
        byte: u8,
    },
    Osc {
        params: Vec<Vec<u8>>,
        bell_terminated: bool,
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
struct Recorder {
    events: Vec<Event>,
}

impl VtDispatch for Recorder {
    fn print(&mut self, value: char) {
        self.events.push(Event::Print(value));
    }

    fn execute(&mut self, byte: u8) {
        self.events.push(Event::Execute(byte));
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.events.push(Event::Csi {
            params: owned_params(params),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.events.push(Event::Escape {
            intermediates: intermediates.to_vec(),
            ignore,
            byte,
        });
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.events.push(Event::Osc {
            params: params.iter().map(|value| value.to_vec()).collect(),
            bell_terminated,
        });
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.events.push(Event::Hook {
            params: owned_params(params),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }

    fn put(&mut self, byte: u8) {
        self.events.push(Event::Put(byte));
    }

    fn unhook(&mut self) {
        self.events.push(Event::Unhook);
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        self.events.push(Event::Apc(data.to_vec()));
    }
}

fn owned_params(params: &Params) -> Vec<Vec<u16>> {
    params.iter().map(<[u16]>::to_vec).collect()
}

#[derive(Clone, Copy)]
enum Feed {
    Whole,
    Bytes,
    Chunks,
}

fn record(data: &[u8], feed: Feed) -> Vec<Event> {
    let mut parser = OdyParser::new();
    let mut recorder = Recorder::default();

    match feed {
        Feed::Whole => parser.advance(&mut recorder, data),
        Feed::Bytes => {
            for byte in data {
                parser.advance(&mut recorder, std::slice::from_ref(byte));
            }
        }
        Feed::Chunks => {
            const WIDTHS: [usize; 6] = [1, 2, 3, 5, 8, 13];
            let mut offset = 0;
            let mut width = 0;
            while offset < data.len() {
                let end = offset.saturating_add(WIDTHS[width % WIDTHS.len()]);
                let end = end.min(data.len());
                parser.advance(&mut recorder, &data[offset..end]);
                offset = end;
                width += 1;
            }
        }
    }

    recorder.events
}

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let mut data = Vec::with_capacity(input.len() + SENTINEL.len());
    data.extend_from_slice(input);
    data.extend_from_slice(SENTINEL);

    let whole = record(&data, Feed::Whole);
    let bytes = record(&data, Feed::Bytes);
    let chunks = record(&data, Feed::Chunks);

    assert_eq!(whole, bytes);
    assert_eq!(whole, chunks);
    assert!(matches!(whole.last(), Some(Event::Print('Z'))));
});
