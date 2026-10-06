// SPDX-License-Identifier: GPL-3.0-only
// Project-authored OSC 133 fixtures: a sub-command letter followed by more
// bytes is not a sub-command.
use odytty::core::Terminal;

#[test]
fn longer_sub_command_letters_change_no_prompt_or_button_state() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b]133;BX;click_events=1\x07\x1b]133;PX;odytty-edit;len=1;cur=0\x07");
    assert!(terminal.prompt_marks().is_empty());
    assert!(!terminal.click_events_enabled());
}
