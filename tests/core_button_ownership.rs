// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored terminal ownership and invalidation regressions.
use odytty::core::Terminal;

#[test]
fn point_chip_does_not_cover_a_marked_space() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    terminal.advance("    \u{0301}\r".as_bytes());
    terminal.advance(b"\x1b]1337;Button=type=custom;code=7\x07");
    let spans = terminal.visible_button_spans(0);
    assert_eq!(spans.len(), 1);
    assert!(spans[0].start_col > 3, "marked cell 3 is program output");
}

#[test]
fn point_chip_does_not_cover_an_inverse_space() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    terminal.advance(b"\x1b[7m \x1b[0m\r\x1b]1337;Button=type=custom;code=7\x07");
    let spans = terminal.visible_button_spans(0);
    assert_eq!(spans.len(), 1);
    assert!(
        spans[0].start_col > 0,
        "inverse cell 0 is visible program output"
    );
}

#[test]
fn invalidating_an_unfinished_run_does_not_consume_the_row_span_cap() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    for _ in 0..16 {
        terminal.advance(b"\x1b]133;P;odytty-button;code=1\x07\x1b]133;P;odytty-button;invalidate;code=1\x07x\x1b]133;P;odytty-button;end\x07");
    }
    assert_eq!(terminal.button_entry_count(), 0);
    terminal.advance(b"\x1b]133;P;odytty-button;code=7\x07go\x1b]133;P;odytty-button;end\x07");
    assert!(terminal.button_at(0, 0, 16).is_some());
}

#[test]
fn ordinary_print_releases_overwritten_button_label() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    terminal.advance(b"\x1b]133;P;odytty-button;code=7\x07Retry\x1b]133;P;odytty-button;end\x07");
    assert!(terminal.button_at(0, 0, 0).is_some());
    terminal.advance(b"\rXXXXX");
    assert!(terminal.button_at(0, 0, 0).is_none());
}

#[test]
fn alternate_resize_preserves_primary_button_refcounts() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    terminal.advance(
        b"\x1b]133;P;odytty-button;code=7;scope=sticky\x07Retry\x1b]133;P;odytty-button;end\x07",
    );
    assert!(terminal.button_at(0, 0, 0).is_some());
    terminal.advance(b"\x1b[?1049h");
    terminal.resize(21, 3);
    terminal.advance(b"\x1b[?1049l");
    assert!(terminal.button_at(0, 0, 0).is_some());
}

#[test]
fn point_chips_stay_after_visible_space_decorations() {
    for sgr in ["4", "9", "7", "4:3", "4:2"] {
        let mut terminal = Terminal::new(20, 3);
        terminal.set_buttons_enabled(true);
        terminal.advance(
            format!("\x1b[{sgr}m \x1b[0m\r\x1b]1337;Button=type=custom;code=7\x07").as_bytes(),
        );
        let spans = terminal.visible_button_spans(0);
        assert_eq!(spans.len(), 1, "sgr={sgr}");
        assert!(
            spans[0].start_col > 0,
            "sgr={sgr}: decorated space is existing output"
        );
        assert!(
            terminal.button_at(0, 0, 0).is_none(),
            "sgr={sgr}: existing output must not click"
        );
    }
}

#[test]
fn replacing_a_wide_continuation_releases_its_label_partner() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    terminal.advance(
        "\x1b]133;P;odytty-button;code=7\x07\u{754c}\x1b]133;P;odytty-button;end\x07".as_bytes(),
    );
    assert!(terminal.button_at(0, 0, 0).is_some());
    terminal.advance(b"\x1b[1;2HX");
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, ' ');
    assert!(
        terminal.button_at(0, 0, 0).is_none(),
        "erased wide partner must not retain a label"
    );
}

#[test]
fn alternate_resize_keeps_button_entries_in_primary_history() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    terminal.advance(b"\x1b]133;P;odytty-button;code=7;scope=sticky\x07Retry\x1b]133;P;odytty-button;end\x07\r\nb\r\nc\r\nd");
    assert_eq!(terminal.visible_button_spans(1).len(), 1);
    terminal.advance(b"\x1b[?1049h");
    terminal.resize(21, 3);
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(
        terminal.visible_button_spans(1).len(),
        1,
        "stored primary history still owns the button entry"
    );
}

#[test]
fn tier1_invalidate_all_ends_an_open_run_without_consuming_the_span_cap() {
    let mut terminal = Terminal::new(20, 3);
    terminal.set_buttons_enabled(true);
    for _ in 0..16 {
        terminal.advance(b"\x1b]133;P;odytty-button;code=1\x07\x1b]1337;Button=type=custom\x07x\x1b]133;P;odytty-button;end\x07");
    }
    assert_eq!(terminal.button_entry_count(), 0);
    terminal.advance(b"\x1b]133;P;odytty-button;code=7\x07go\x1b]133;P;odytty-button;end\x07");
    assert!(terminal.button_at(0, 0, 16).is_some());
}
