// SPDX-License-Identifier: GPL-3.0-only
//! BEL (`0x07`) latch behavior: a bell control sets the drain-once flag without
//! touching the grid; the flag clears on drain.

use super::*;

#[test]
fn bel_sets_pending_and_drains_once() {
    let mut terminal = Terminal::new(8, 2);
    assert!(!terminal.take_bell(), "no bell before any input");

    terminal.advance(b"\x07");
    assert!(terminal.take_bell(), "BEL latches the pending flag");
    assert!(
        !terminal.take_bell(),
        "draining clears the flag (edge, not level)"
    );
}

#[test]
fn bel_does_not_print_or_move_cursor() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"ab\x07cd");

    // The bell must be transparent to the grid: "abcd" lands contiguously and
    // the cursor advances exactly four columns.
    let row: String = (0..4)
        .map(|col| terminal.screen().cell(0, col).unwrap().ch)
        .collect();
    assert_eq!(row, "abcd", "BEL leaves no glyph and does not shift text");
    assert!(terminal.take_bell(), "the embedded BEL still latched");
}

#[test]
fn repeated_bells_coalesce_into_one_drain() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"\x07\x07\x07");
    assert!(
        terminal.take_bell(),
        "multiple BELs coalesce to one pending"
    );
    assert!(!terminal.take_bell(), "and clear after a single drain");
}
