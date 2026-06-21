// SPDX-License-Identifier: GPL-3.0-only
//! Terminal reporting-surface fixtures: DECRQM/DECRPM, XTWINOPS report-only
//! queries, Secondary DA, and XTVERSION.

use super::*;

#[test]
fn decrqm_reports_default_dec_mode_inventory() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[?1$p");
    terminal.advance(b"\x1b[?6$p");
    terminal.advance(b"\x1b[?7$p");
    terminal.advance(b"\x1b[?12$p");
    terminal.advance(b"\x1b[?25$p");
    terminal.advance(b"\x1b[?1006$p");
    terminal.advance(b"\x1b[?1007$p");
    terminal.advance(b"\x1b[?2026$p");
    terminal.advance(b"\x1b[?4242$p");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[?1;2$y\
          \x1b[?6;2$y\
          \x1b[?7;1$y\
          \x1b[?12;1$y\
          \x1b[?25;1$y\
          \x1b[?1006;2$y\
          \x1b[?1007;1$y\
          \x1b[?2026;2$y\
          \x1b[?4242;0$y"
    );
}

#[test]
fn alternate_scroll_mode_defaults_on_and_toggles() {
    let mut terminal = Terminal::new(10, 4);

    // Default on (xterm parity), and not on the alternate screen yet.
    assert!(terminal.alternate_scroll_enabled());
    assert!(!terminal.on_alternate_screen());

    // DECRST 1007 disables it; DECSET 1007 re-enables.
    terminal.advance(b"\x1b[?1007l");
    assert!(!terminal.alternate_scroll_enabled());
    terminal.advance(b"\x1b[?1007h");
    assert!(terminal.alternate_scroll_enabled());

    // Entering/leaving the alternate screen is tracked, and the 1007 flag
    // survives the switch (it is a terminal mode, not buffer state).
    terminal.advance(b"\x1b[?1049h");
    assert!(terminal.on_alternate_screen());
    assert!(terminal.alternate_scroll_enabled());
    terminal.advance(b"\x1b[?1049l");
    assert!(!terminal.on_alternate_screen());
}

#[test]
fn decrqm_reports_live_dec_mode_state() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[?1h\x1b[?1$p");
    terminal.advance(b"\x1b[2;4r\x1b[?6h\x1b[?6$p");
    terminal.advance(b"\x1b[?7l\x1b[?7$p");
    terminal.advance(b"\x1b[?12l\x1b[?12$p");
    terminal.advance(b"\x1b[?25l\x1b[?25$p");
    terminal.advance(b"\x1b[?1002h\x1b[?1002$p");
    terminal.advance(b"\x1b[?1006h\x1b[?1006$p");
    terminal.advance(b"\x1b[?1004h\x1b[?1004$p");
    terminal.advance(b"\x1b[?2004h\x1b[?2004$p");
    terminal.advance(b"\x1b[?2026h\x1b[?2026$p");
    terminal.advance(b"\x1b[?80h\x1b[?80$p");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[?1;1$y\
          \x1b[?6;1$y\
          \x1b[?7;2$y\
          \x1b[?12;2$y\
          \x1b[?25;2$y\
          \x1b[?1002;1$y\
          \x1b[?1006;1$y\
          \x1b[?1004;1$y\
          \x1b[?2004;1$y\
          \x1b[?2026;1$y\
          \x1b[?80;1$y"
    );
}

#[test]
fn synchronized_output_mode_resets_on_decrst_ris_and_decstr() {
    let mut terminal = Terminal::new(10, 4);

    assert!(!terminal.synchronized_output_enabled());

    terminal.advance(b"\x1b[?2026h");
    assert!(terminal.synchronized_output_enabled());
    terminal.advance(b"\x1b[?2026l");
    assert!(!terminal.synchronized_output_enabled());

    terminal.advance(b"\x1b[?2026h\x1bc");
    assert!(!terminal.synchronized_output_enabled());

    terminal.advance(b"\x1b[?2026h\x1b[!p");
    assert!(!terminal.synchronized_output_enabled());
}

#[test]
fn decrqm_reports_ansi_known_and_unknown_modes() {
    let mut terminal = Terminal::new(10, 4);

    // IRM (mode 4) is implemented, so it reports its live reset state (2), not
    // the "permanently reset" (4) it used to claim. Mode 99 is unknown (0).
    terminal.advance(b"\x1b[4$p");
    terminal.advance(b"\x1b[99$p");
    assert_eq!(terminal.take_host_output(), b"\x1b[4;2$y\x1b[99;0$y");

    // After CSI 4 h, IRM reports set (1).
    terminal.advance(b"\x1b[4h\x1b[4$p");
    assert_eq!(terminal.take_host_output(), b"\x1b[4;1$y");
}

#[test]
fn decrqm_reports_alt_screen_and_saved_cursor_modes() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[?1048h\x1b[?1048$p");
    terminal.advance(b"\x1b[?1049h\x1b[?47$p\x1b[?1047$p\x1b[?1049$p\x1b[?1048$p");
    terminal.advance(b"\x1b[?1049l\x1b[?1049$p");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[?1048;1$y\
          \x1b[?47;1$y\
          \x1b[?1047;1$y\
          \x1b[?1049;1$y\
          \x1b[?1048;2$y\
          \x1b[?1049;2$y"
    );
}

#[test]
fn decawm_can_disable_right_edge_wrap() {
    let mut terminal = Terminal::new(3, 2);

    terminal.advance(b"\x1b[?7labcde");

    assert_eq!(terminal.screen().plain_text(), "abe\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
}

#[test]
fn xtwinops_reports_text_pixels_cell_pixels_and_character_size() {
    let mut terminal = Terminal::new(10, 4);
    terminal.set_cell_metrics(9, 17);

    terminal.advance(b"\x1b[14t");
    terminal.advance(b"\x1b[16t");
    terminal.advance(b"\x1b[18t");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[4;68;90t\x1b[6;17;9t\x1b[8;4;10t"
    );
}

#[test]
fn xtwinops_uses_headless_default_metrics_until_overridden() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[14t\x1b[16t");

    assert_eq!(terminal.take_host_output(), b"\x1b[4;64;80t\x1b[6;16;8t");
}

#[test]
fn xtwinops_manipulation_and_title_stack_ops_are_ignored() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b]2;kept\x07");
    terminal.advance(b"\x1b[1t\x1b[8;30;100t\x1b[22;0t\x1b[23;0t\x1b[24t");

    assert!(terminal.take_host_output().is_empty());
    assert_eq!(terminal.title(), Some("kept"));
}

#[test]
fn responds_to_secondary_device_attributes() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[>c");

    assert_eq!(terminal.take_host_output(), b"\x1b[>65;1;0c");
}

#[test]
fn responds_to_xtversion() {
    let mut terminal = Terminal::new(10, 4);
    let expected = format!("\x1bP>|OdyTTY {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes();

    terminal.advance(b"\x1b[>0q");
    terminal.advance(b"\x1b[>1q");

    assert_eq!(terminal.take_host_output(), expected);
}

#[test]
fn xtgettcap_reports_known_and_unknown_capabilities() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1bP+q544e;436f;524742;5858\x1b\\");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1bP1+r544e=787465726d2d323536636f6c6f72\x1b\\\
          \x1bP1+r436f=323536\x1b\\\
          \x1bP1+r524742=31\x1b\\\
          \x1bP0+r\x1b\\"
    );
}

#[test]
fn xtgettcap_ignores_malformed_hex_names() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1bP+q544;zzzz;544e\x1b\\");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1bP1+r544e=787465726d2d323536636f6c6f72\x1b\\"
    );
}

#[test]
fn decrqss_reports_sgr_with_extended_underline_color() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[1;3;4:5;5;7;9;38:2::10:20:30;48;5;42;58:5:77m");
    terminal.advance(b"\x1bP$qm\x1b\\");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1bP1$r1;3;4:5;5;7;9;38:2::10:20:30;48:5:42;58:5:77m\x1b\\"
    );
}

#[test]
fn decrqss_reports_cursor_style_and_scroll_region() {
    let mut terminal = Terminal::new(10, 6);

    terminal.advance(b"\x1b[5 q\x1bP$q q\x1b\\");
    terminal.advance(b"\x1b[2;5r\x1bP$qr\x1b\\");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1bP1$r5 q\x1b\\\x1bP1$r2;5r\x1b\\"
    );
}

#[test]
fn decrqss_reports_decsca() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1bP$q\"q\x1b\\");
    terminal.advance(b"\x1b[1\"q\x1bP$q\"q\x1b\\");

    assert_eq!(
        terminal.take_host_output(),
        b"\x1bP1$r0\"q\x1b\\\x1bP1$r1\"q\x1b\\"
    );
}

#[test]
fn decrqss_rejects_unknown_selectors() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1bP$qx\x1b\\");

    assert_eq!(terminal.take_host_output(), b"\x1bP0$r\x1b\\");
}
