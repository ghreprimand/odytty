// SPDX-License-Identifier: GPL-3.0-only
//! Stable regression anchors for the parser and graphics fuzzing workspace.
//!
//! Fuzzing discoveries are minimized and promoted here before closure. These
//! initial synthetic fixtures pin chunk equivalence, reset behavior, disabled
//! named transports, and Sixel output bounds without depending on libFuzzer.

use odytty::core::Terminal;
use odytty::graphics::sixel::{SixelBackground, decode_sixel};
use odytty::parser::{OdyParser, Params, VtDispatch};

fn decode_hex(fixture: &str) -> Vec<u8> {
    let digits = fixture
        .bytes()
        .filter(|byte| byte.is_ascii_hexdigit())
        .collect::<Vec<_>>();
    assert_eq!(digits.len() % 2, 0, "hex fixture has an odd digit count");
    digits
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("hex fixture is ASCII");
            u8::from_str_radix(text, 16).expect("hex fixture contains valid byte pairs")
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParserEvent {
    Print(char),
    Execute(u8),
    Csi(Vec<Vec<u16>>, Vec<u8>, bool, char),
    Escape(Vec<u8>, bool, u8),
    Osc(Vec<Vec<u8>>, bool),
    Hook(Vec<Vec<u16>>, Vec<u8>, bool, char),
    Put(u8),
    Unhook,
    Apc(Vec<u8>),
}

#[derive(Default)]
struct ParserRecorder {
    events: Vec<ParserEvent>,
}

fn params_owned(params: &Params) -> Vec<Vec<u16>> {
    params.iter().map(<[u16]>::to_vec).collect()
}

impl VtDispatch for ParserRecorder {
    fn print(&mut self, value: char) {
        self.events.push(ParserEvent::Print(value));
    }

    fn execute(&mut self, byte: u8) {
        self.events.push(ParserEvent::Execute(byte));
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.events.push(ParserEvent::Csi(
            params_owned(params),
            intermediates.to_vec(),
            ignore,
            action,
        ));
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.events
            .push(ParserEvent::Escape(intermediates.to_vec(), ignore, byte));
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.events.push(ParserEvent::Osc(
            params.iter().map(|value| value.to_vec()).collect(),
            bell_terminated,
        ));
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.events.push(ParserEvent::Hook(
            params_owned(params),
            intermediates.to_vec(),
            ignore,
            action,
        ));
    }

    fn put(&mut self, byte: u8) {
        self.events.push(ParserEvent::Put(byte));
    }

    fn unhook(&mut self) {
        self.events.push(ParserEvent::Unhook);
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        self.events.push(ParserEvent::Apc(data.to_vec()));
    }
}

fn parser_events(chunks: &[&[u8]]) -> Vec<ParserEvent> {
    let mut parser = OdyParser::new();
    let mut recorder = ParserRecorder::default();
    for chunk in chunks {
        parser.advance(&mut recorder, chunk);
    }
    recorder.events
}

#[test]
fn parser_dispatch_fixture_is_chunk_invariant() {
    let input = decode_hex(include_str!(
        "fixtures/fuzz/parser_graphics/parser-dispatch.hex"
    ));
    let whole = parser_events(&[&input]);
    let bytes = input
        .iter()
        .map(std::slice::from_ref)
        .collect::<Vec<&[u8]>>();
    assert_eq!(whole, parser_events(&bytes));
}

fn terminal_state(input: &[u8], byte_chunks: bool) -> (String, Vec<u8>, bool) {
    let mut terminal = Terminal::new(80, 24);
    terminal.set_scrollback_limit(256);
    if byte_chunks {
        for byte in input {
            terminal.advance(std::slice::from_ref(byte));
        }
    } else {
        terminal.advance(input);
    }

    let before_reset = format!(
        "{:?}{:?}{:?}{:?}",
        terminal.snapshot(),
        terminal.snapshot_state(256),
        terminal.snapshot_layout_state(),
        terminal.title()
    );
    let host_output = terminal.take_host_output();
    terminal.advance(b"\x18\x1bcFUZZOK");
    let reset_ok = terminal
        .snapshot()
        .cells
        .iter()
        .take(6)
        .map(|cell| cell.ch)
        .eq("FUZZOK".chars());
    (before_reset, host_output, reset_ok)
}

#[test]
fn terminal_stream_fixture_is_chunk_invariant_and_resets() {
    let input = decode_hex(include_str!(
        "fixtures/fuzz/parser_graphics/terminal-stream.hex"
    ));
    let whole = terminal_state(&input, false);
    let bytes = terminal_state(&input, true);
    assert_eq!(whole, bytes);
    assert!(whole.2);
}

#[test]
fn kitty_named_transport_stays_disabled_for_windows_style_fixture() {
    let encoded_path =
        include_str!("fixtures/fuzz/parser_graphics/kitty-windows-path.base64").trim();
    let apc = format!("\x1b_Ga=T,t=f,f=100;{encoded_path}\x1b\\");
    let mut terminal = Terminal::new(80, 24);
    terminal.set_kitty_named_transports_enabled(false);
    terminal.advance(apc.as_bytes());

    assert!(terminal.graphics().store().is_empty());
    let response = terminal.take_host_output();
    assert!(
        response
            .windows(b"EPERM:named-transport-disabled".len())
            .any(|window| window == b"EPERM:named-transport-disabled")
    );
}

#[test]
fn sixel_fixture_has_checked_rgba_extent() {
    let input = decode_hex(include_str!(
        "fixtures/fuzz/parser_graphics/sixel-red-column.hex"
    ));
    let image = decode_sixel(&input, SixelBackground::Opaque).expect("synthetic Sixel decodes");
    let pixels = usize::try_from(image.width)
        .unwrap()
        .checked_mul(usize::try_from(image.height).unwrap())
        .expect("fixture dimensions fit usize");

    assert!(image.width <= 10_000);
    assert!(image.height <= 10_000);
    assert!(pixels <= 40_000_000);
    assert_eq!(image.rgba.len(), pixels * 4);
}
