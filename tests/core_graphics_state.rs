// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored terminal ownership and invalidation regressions.
use odytty::core::Terminal;

#[test]
fn malformed_sixel_does_not_clear_pending_wrap() {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"ABCD\x1bPq\x1b\\E");
    assert_eq!(terminal.screen().cell(0, 3).unwrap().ch, 'D');
    assert_eq!(terminal.screen().cell(1, 0).unwrap().ch, 'E');
}

#[test]
fn heuristic_input_edge_keeps_the_complete_wide_owner() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance("\x1b]133;A\x07$ \x1b]133;B\x07\u{6f22}\x1b[1;3H".as_bytes());
    let region = terminal.input_region().unwrap();
    assert_eq!(region.end_col, 4);
}
