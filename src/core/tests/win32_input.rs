// SPDX-License-Identifier: GPL-3.0-only
use super::*;

#[test]
fn win32_input_mode_sets_resets_and_reports() {
    let mut terminal = Terminal::new(8, 2);
    assert!(!terminal.keyboard_modes().win32_input);

    terminal.advance(b"\x1b[?9001h");
    assert!(terminal.keyboard_modes().win32_input);
    terminal.advance(b"\x1b[?9001$p");
    assert_eq!(terminal.take_host_output(), b"\x1b[?9001;1$y");

    terminal.advance(b"\x1b[?9001l");
    assert!(!terminal.keyboard_modes().win32_input);
    terminal.advance(b"\x1b[?9001$p");
    assert_eq!(terminal.take_host_output(), b"\x1b[?9001;2$y");
}

#[test]
fn decstr_preserves_win32_input_but_ris_and_session_reset_clear_it() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"\x1b[?9001h\x1b[!p");
    assert!(terminal.keyboard_modes().win32_input);

    terminal.advance(b"\x1bc");
    assert!(!terminal.keyboard_modes().win32_input);

    terminal.advance(b"\x1b[?9001h");
    terminal.reset_input_reporting_modes();
    assert!(!terminal.keyboard_modes().win32_input);
}

#[test]
fn win32_input_is_session_global_across_alternate_screen() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"\x1b[?9001h\x1b[?1049h");
    assert!(terminal.keyboard_modes().win32_input);

    terminal.advance(b"\x1b[?9001l\x1b[?1049l");
    assert!(!terminal.keyboard_modes().win32_input);
}
