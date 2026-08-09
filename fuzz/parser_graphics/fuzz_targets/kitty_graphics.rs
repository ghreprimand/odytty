// SPDX-License-Identifier: GPL-3.0-only
#![no_main]

#[allow(dead_code)]
mod support;

use libfuzzer_sys::fuzz_target;
use support::{assert_graphics_bounds, assert_recovery_sentinel, bounded_terminal};

const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 4096;

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let mut stream = Vec::with_capacity(input.len() + 5);
    stream.extend_from_slice(b"\x1b_G");
    stream.extend(input.iter().map(|byte| match *byte {
        0x18 | 0x1a | 0x1b | 0x9c => b'?',
        other => other,
    }));
    stream.extend_from_slice(b"\x1b\\");

    let mut terminal = bounded_terminal();
    terminal.advance(&stream);
    assert_graphics_bounds(&terminal);

    let response = terminal.take_host_output();
    assert!(response.len() <= MAX_RESPONSE_BYTES);

    assert_recovery_sentinel(&mut terminal);
});
