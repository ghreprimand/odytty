// SPDX-License-Identifier: GPL-3.0-only
use super::*;

#[test]
fn kitty_keyboard_query_reports_active_flags() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[?u");
    assert_eq!(terminal.take_host_output(), b"\x1b[?0u");

    terminal.advance(b"\x1b[=1;1u\x1b[?u");
    assert_eq!(terminal.take_host_output(), b"\x1b[?1u");
}

#[test]
fn kitty_keyboard_set_add_and_remove_modes() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[=1;1u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 1);

    terminal.advance(b"\x1b[=8;2u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 9);

    terminal.advance(b"\x1b[=1;3u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 8);

    terminal.advance(b"\x1b[=4;9u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 8);
}

#[test]
fn kitty_keyboard_accepts_completion_flags() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[=22;1u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 22);

    terminal.advance(b"\x1b[=9;2u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 31);

    terminal.advance(b"\x1b[=18;3u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 13);
}

#[test]
fn kitty_keyboard_push_and_pop_restore_flags() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[=1;1u\x1b[>8u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 8);

    terminal.advance(b"\x1b[>9u\x1b[<u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 8);

    terminal.advance(b"\x1b[<1u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 1);

    terminal.advance(b"\x1b[<1u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 0);
}

#[test]
fn kitty_keyboard_pop_count_unwinds_multiple_states() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[=1;1u\x1b[>2u\x1b[>4u\x1b[>8u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 8);

    terminal.advance(b"\x1b[<2u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 2);
}

#[test]
fn kitty_keyboard_stack_overflow_evicts_oldest_state() {
    let mut terminal = Terminal::new(8, 2);

    for flags in 1..=18 {
        terminal.advance(format!("\x1b[>{flags}u").as_bytes());
    }
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 18);

    terminal.advance(b"\x1b[<16u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 2);

    terminal.advance(b"\x1b[<1u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 0);
}

#[test]
fn kitty_keyboard_flags_are_isolated_between_primary_and_alt_screen() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[=1;1u\x1b[?1049h");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 0);

    terminal.advance(b"\x1b[=8;1u\x1b[?1049l");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 1);

    terminal.advance(b"\x1b[?1049h");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 0);
}

#[test]
fn kitty_keyboard_flags_reset_on_decstr_and_ris() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[=1;1u\x1b[>8u\x1b[!p");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 0);

    terminal.advance(b"\x1b[=1;1u\x1b[>8u\x1bc");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 0);

    terminal.advance(b"\x1b[<1u");
    assert_eq!(terminal.keyboard_modes().kitty_keyboard_flags, 0);
}
