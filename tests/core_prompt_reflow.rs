// SPDX-License-Identifier: GPL-3.0-only
// Project-authored bounded protocol fixtures.
use odytty::core::{Terminal, verified_command_ranges};

#[test]
fn finished_output_excludes_the_next_prompt_after_padding_reflow() {
    let mut terminal = Terminal::new(3, 8);
    terminal.advance(
        "\x1b]133;A\x07$ x\r\n\x1b]133;C\x07AB\u{6f22}\x1b]133;D;0\x07\x1b]133;A\x07$ ".as_bytes(),
    );
    let before = verified_command_ranges(&terminal.prompt_marks(), 3, 7);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].output_end_column, Some(1));
    terminal.resize(6, 8);
    let after = verified_command_ranges(&terminal.prompt_marks(), 6, 7);
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].output_end_column,
        Some(3),
        "the next prompt starts after the two-cell owner"
    );
}

#[test]
fn malformed_prompt_token_does_not_change_prompt_state() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b]133;AX;click_events=1\x07");
    assert_eq!(
        (
            terminal.prompt_marks().is_empty(),
            terminal.click_events_enabled()
        ),
        (true, false)
    );
}

#[test]
fn one_column_live_print_uses_the_same_wide_owner_fallback_as_reflow() {
    let mut terminal = Terminal::new(1, 3);
    terminal.advance("\u{6f22}".as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, '\u{6f22}');
}

#[test]
fn completed_history_output_excludes_next_prompt_after_lazy_padding_reflow() {
    let mut terminal = Terminal::new(3, 4);
    terminal.advance(
        "\x1b]133;A\x07$ x\r\n\x1b]133;C\x07AB\u{6f22}\x1b]133;D;0\x07\x1b]133;A\x07$ ".as_bytes(),
    );
    terminal.advance(b"\r\n\r\n\r\n\r\n");
    terminal.resize(6, 4);
    let ranges = verified_command_ranges(
        &terminal.prompt_marks(),
        6,
        terminal.screen().scrollback_len() + 3,
    );
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].output_end_column, Some(3));
}

#[test]
fn malformed_output_token_does_not_stamp_an_output_start() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b]133;CX\x07");
    assert!(terminal.prompt_marks().is_empty());
}

#[test]
fn malformed_end_token_does_not_end_the_command() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b]133;DX;0\x07");
    assert!(terminal.prompt_marks().is_empty());
}

#[test]
fn repeated_one_column_wide_owners_do_not_insert_blank_rows() {
    let mut terminal = Terminal::new(1, 4);
    terminal.advance("\u{6f22}\u{6f22}".as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, '\u{6f22}');
    assert_eq!(terminal.screen().cell(1, 0).unwrap().ch, '\u{6f22}');
}
