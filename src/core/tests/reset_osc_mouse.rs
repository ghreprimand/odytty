// SPDX-License-Identifier: GPL-3.0-only
//! Core behavioral tests (M4 mechanical split from core/tests.rs).

use super::*;

#[test]
fn hard_reset_restores_power_on_state() {
    let mut terminal = Terminal::new(8, 3);

    // Dirty as much state as possible: scrollback, alt screen, margins,
    // saved cursor, attrs, bracketed paste, hidden cursor, pending DA reply.
    terminal.advance(b"a\r\nb\r\nc\r\nd"); // forces a scrollback line
    terminal.advance(b"\x1b[?2004h"); // bracketed paste on
    terminal.advance(b"\x1b[?1h\x1b="); // keyboard application modes on
    terminal.advance(b"\x1b[?25l"); // cursor hidden
    terminal.advance(b"\x1b[2;3r"); // scroll region
    terminal.advance(b"\x1b7"); // save cursor
    terminal.advance(b"\x1b[1;31m"); // bold red attrs
    terminal.advance(b"\x1b[?1049h"); // enter alt screen
    terminal.advance(b"\x1b[c"); // queue a primary DA reply in host_output

    terminal.advance(b"\x1bc"); // RIS

    assert_eq!(terminal.screen().plain_text(), "\n\n");
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    assert!(!terminal.bracketed_paste_enabled());
    assert_eq!(terminal.keyboard_modes(), KeyboardModes::default());
    assert!(terminal.take_host_output().is_empty());

    // Power-on attrs: text printed after RIS carries default attributes.
    terminal.advance(b"Z");
    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'Z');
    assert_eq!(cell.attrs, Attrs::default());

    // Cursor visible again after reset (snapshot reflects it).
    assert!(terminal.snapshot().cursor_visible);

    // Scroll region cleared: a bottom-row newline now scrolls the whole
    // screen and feeds scrollback (region scroll would not).
    terminal.advance(b"\x1b[3;1H\n");
    assert_eq!(terminal.screen().scrollback_len(), 1);
}

#[test]
fn soft_reset_keeps_cells_but_resets_modes() {
    let mut terminal = Terminal::new(8, 3);

    terminal.advance(b"old\r\nkeep\r\ntwo\r\nthree"); // visible content + scrollback
    assert_eq!(terminal.screen().scrollback_len(), 1);
    terminal.advance(b"\x1b[?2004h"); // bracketed paste on
    terminal.advance(b"\x1b[?1h\x1b="); // keyboard application modes on
    terminal.advance(b"\x1b[?25l"); // cursor hidden
    terminal.advance(b"\x1b[2;3r"); // scroll region
    terminal.advance(b"\x1b7"); // save cursor
    terminal.advance(b"\x1b[c"); // queue a primary DA reply in host_output

    terminal.advance(b"\x1b[!p"); // DECSTR soft reset

    // Visible cells and scrollback preserved (NOT cleared).
    assert_eq!(terminal.screen().plain_text(), "keep\ntwo\nthree");
    assert_eq!(terminal.screen().scrollback_len(), 1);

    // Modes reset.
    assert!(!terminal.bracketed_paste_enabled());
    assert_eq!(terminal.keyboard_modes(), KeyboardModes::default());
    assert!(terminal.snapshot().cursor_visible);
    assert!(terminal.take_host_output().is_empty());

    // Cursor policy: DECSTR homes the cursor to top-left (documented).
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

    // Saved cursor dropped: a restore after soft reset is a no-op, so the
    // cursor stays where it was moved rather than jumping to a stale save.
    terminal.advance(b"\x1b[2;5H"); // move to row 1, col 4
    terminal.advance(b"\x1b8"); // restore -> no saved cursor, no movement
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 4 });

    // Scroll region cleared by the soft reset.
    terminal.advance(b"\x1b[3;1H\n");
    assert_eq!(terminal.screen().scrollback_len(), 2);
}

// === OSC title handling ===

#[test]
fn osc_sets_window_title() {
    let mut terminal = Terminal::new(20, 3);
    assert_eq!(terminal.title(), None);
    assert!(!terminal.take_title_changed());

    // OSC 2 (window title), BEL-terminated.
    terminal.advance(b"\x1b]2;hello\x07");
    assert_eq!(terminal.title(), Some("hello"));
    assert!(terminal.take_title_changed());
    // Flag clears after the poll.
    assert!(!terminal.take_title_changed());

    // OSC 0 (icon + window title), ST-terminated.
    terminal.advance(b"\x1b]0;second\x1b\\");
    assert_eq!(terminal.title(), Some("second"));
    assert!(terminal.take_title_changed());
}

#[test]
fn osc_title_payload_does_not_leak_into_grid() {
    let mut terminal = Terminal::new(20, 2);
    terminal.advance(b"A\x1b]2;NOTONSCREEN\x07B");
    // Only the printed A and B reach the grid; the title text does not.
    assert_eq!(terminal.screen().plain_text(), "AB\n");
    assert_eq!(terminal.title(), Some("NOTONSCREEN"));
}

#[test]
fn osc_empty_title_is_explicit_empty() {
    let mut terminal = Terminal::new(20, 2);
    terminal.advance(b"\x1b]2;\x07");
    // Empty payload is a real (set) empty title, distinct from never-set.
    assert_eq!(terminal.title(), Some(""));
    assert!(terminal.take_title_changed());
}

#[test]
fn osc_title_preserves_embedded_semicolons() {
    let mut terminal = Terminal::new(40, 2);
    // The parser splits on ';'; the title must be rejoined intact.
    terminal.advance(b"\x1b]2;a; b; c\x07");
    assert_eq!(terminal.title(), Some("a; b; c"));
}

#[test]
fn osc_title_handles_utf8_and_invalid_bytes() {
    let mut terminal = Terminal::new(40, 2);
    // Valid multi-byte UTF-8 round-trips.
    terminal.advance("\x1b]2;héllo 🚀\x07".as_bytes());
    assert_eq!(terminal.title(), Some("héllo 🚀"));

    // Invalid UTF-8 must not panic; lossy replacement is acceptable.
    terminal.advance(b"\x1b]2;\xff\xfe\x07");
    let title = terminal.title().expect("title set");
    assert!(title.contains('\u{FFFD}'));
}

#[test]
fn osc_icon_name_only_does_not_change_window_title() {
    let mut terminal = Terminal::new(20, 2);
    terminal.advance(b"\x1b]2;window\x07");
    assert!(terminal.take_title_changed());

    // OSC 1 sets the icon name only; the window title is untouched.
    terminal.advance(b"\x1b]1;iconname\x07");
    assert_eq!(terminal.title(), Some("window"));
    assert!(!terminal.take_title_changed());
}

#[test]
fn unknown_osc_sequences_are_consumed_without_corruption() {
    let mut terminal = Terminal::new(40, 2);
    // A spread of OSCs a real shell/editor emits: cwd (7), hyperlink (8),
    // colors (10/11), palette (4), clipboard (52), shell integration (133).
    terminal.advance(b"X");
    terminal.advance(b"\x1b]7;file://host/home/user\x07");
    terminal.advance(b"\x1b]8;;https://example.com\x07");
    terminal.advance(b"\x1b]10;rgb:ffff/ffff/ffff\x07");
    terminal.advance(b"\x1b]11;rgb:0000/0000/0000\x07");
    terminal.advance(b"\x1b]4;1;rgb:ff00/0000/0000\x07");
    terminal.advance(b"\x1b]52;c;SGVsbG8=\x07");
    terminal.advance(b"\x1b]133;A\x07");
    terminal.advance(b"Y");

    // Only the printed characters reach the grid; no payload leaks, no title.
    assert_eq!(terminal.screen().plain_text(), "XY\n");
    assert_eq!(terminal.title(), None);
}

#[test]
fn osc8_hyperlink_associates_printed_cells_until_close() {
    let mut terminal = Terminal::new(20, 2);
    terminal.advance(b"\x1b]8;;https://example.com\x07AB\x1b]8;;\x07C");

    let a = terminal.screen().cell(0, 0).unwrap();
    let b = terminal.screen().cell(0, 1).unwrap();
    let c = terminal.screen().cell(0, 2).unwrap();
    assert_eq!(a.ch, 'A');
    assert_eq!(a.attrs.hyperlink, b.attrs.hyperlink);
    assert!(c.attrs.hyperlink.is_none());

    let link = terminal
        .hyperlink(a.attrs.hyperlink.expect("A has OSC 8 link"))
        .expect("link table entry");
    assert_eq!(link.uri, "https://example.com");
}

#[test]
fn osc8_id_dedups_discontiguous_regions() {
    let mut terminal = Terminal::new(20, 2);
    terminal.advance(
        b"\x1b]8;id=docs;https://example.com\x07A\x1b]8;;\x07 \
          \x1b]8;id=docs;https://example.com\x07B\x1b]8;;\x07 \
          \x1b]8;id=docs;https://example.org\x07C",
    );

    let a = terminal.screen().cell(0, 0).unwrap().attrs.hyperlink;
    let b = terminal.screen().cell(0, 2).unwrap().attrs.hyperlink;
    let c = terminal.screen().cell(0, 4).unwrap().attrs.hyperlink;
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn osc8_anonymous_repaint_loop_keeps_one_registry_entry() {
    let mut terminal = Terminal::new(80, 24);

    for _ in 0..5000 {
        terminal.advance(b"\x1b]8;;https://example.com\x1b\\X\x1b]8;;\x1b\\");
    }

    assert_eq!(
        terminal.hyperlink_count_for_test(),
        1,
        "identical anonymous OSC 8 links should share one interned entry"
    );
}

#[test]
fn osc8_link_state_survives_sgr_reset_until_osc_close() {
    let mut terminal = Terminal::new(20, 2);
    terminal.advance(b"\x1b]8;;https://example.com\x07A\x1b[0mB\x1b]8;;\x07C");

    let a = terminal.screen().cell(0, 0).unwrap().attrs.hyperlink;
    let b = terminal.screen().cell(0, 1).unwrap().attrs.hyperlink;
    let c = terminal.screen().cell(0, 2).unwrap().attrs.hyperlink;
    assert_eq!(a, b);
    assert!(a.is_some());
    assert!(c.is_none());
}

#[test]
fn osc8_uri_cap_ignores_oversized_link() {
    let mut terminal = Terminal::new(20, 2);
    let uri = "a".repeat(MAX_URI_BYTES + 1);
    terminal.advance(format!("\x1b]8;;{uri}\x07A").as_bytes());

    assert!(
        terminal
            .screen()
            .cell(0, 0)
            .unwrap()
            .attrs
            .hyperlink
            .is_none()
    );
}

#[test]
fn osc8_link_refs_survive_resize_reflow() {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"\x1b]8;;https://example.com\x07abcdef");
    terminal.resize(3, 3);

    let linked = terminal
        .snapshot()
        .cells
        .iter()
        .filter(|cell| cell.ch != ' ')
        .map(|cell| cell.attrs.hyperlink)
        .collect::<Vec<_>>();
    assert!(!linked.is_empty());
    assert!(linked.iter().all(|id| id.is_some() && *id == linked[0]));
}

#[test]
fn osc8_primary_link_state_restores_after_alternate_screen() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b]8;;https://primary.example\x07P");
    let primary = terminal.screen().cell(0, 0).unwrap().attrs.hyperlink;

    terminal.advance(b"\x1b[?1049h\x1b]8;;https://alt.example\x07A");
    let alt = terminal.screen().cell(0, 0).unwrap().attrs.hyperlink;
    assert_ne!(primary, alt);

    terminal.advance(b"\x1b[?1049lQ");
    assert_eq!(
        terminal.screen().cell(0, 0).unwrap().attrs.hyperlink,
        primary
    );
    assert_eq!(
        terminal.screen().cell(0, 1).unwrap().attrs.hyperlink,
        primary
    );
}

#[test]
fn ris_clears_hyperlink_cells_and_table() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b]8;;https://example.com\x07A");
    let id = terminal
        .screen()
        .cell(0, 0)
        .unwrap()
        .attrs
        .hyperlink
        .unwrap();

    terminal.advance(b"\x1bcB");
    assert!(terminal.hyperlink(id).is_none());
    assert!(
        terminal
            .screen()
            .cell(0, 0)
            .unwrap()
            .attrs
            .hyperlink
            .is_none()
    );
}

// === Mouse mode tracking ===

#[test]
fn mouse_tracking_modes_set_and_reset() {
    let mut terminal = Terminal::new(10, 3);
    assert_eq!(terminal.mouse_protocol(), MouseProtocol::default());
    assert!(!terminal.mouse_protocol().is_enabled());

    terminal.advance(b"\x1b[?1000h");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Normal);
    assert!(terminal.mouse_protocol().is_enabled());

    terminal.advance(b"\x1b[?9h");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::X10);

    terminal.advance(b"\x1b[?1002h");
    assert_eq!(
        terminal.mouse_protocol().tracking,
        MouseTracking::ButtonEvent
    );

    terminal.advance(b"\x1b[?1003h");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::AnyEvent);

    // Any tracking DECRST returns to Off (xterm shared-variable semantics).
    terminal.advance(b"\x1b[?1003l");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Off);
}

#[test]
fn later_mouse_decset_wins() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1000h\x1b[?1002h");
    // The later DECSET (1002) is the active tracking mode.
    assert_eq!(
        terminal.mouse_protocol().tracking,
        MouseTracking::ButtonEvent
    );
    // A DECRST of any mouse mode turns reporting off.
    terminal.advance(b"\x1b[?1000l");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Off);
}

#[test]
fn mouse_encoding_modes_set_and_reset() {
    let mut terminal = Terminal::new(10, 3);
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Default);

    terminal.advance(b"\x1b[?1006h");
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Sgr);
    terminal.advance(b"\x1b[?1005h");
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Utf8);
    terminal.advance(b"\x1b[?1015h");
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Urxvt);

    // Encoding and tracking are independent axes.
    terminal.advance(b"\x1b[?1000h");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Normal);
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Urxvt);

    // DECRST of an encoding mode restores the default encoding only.
    terminal.advance(b"\x1b[?1015l");
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Default);
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Normal);
}

#[test]
fn ris_resets_mouse_modes_but_keeps_title() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b]2;keepme\x07\x1b[?1002h\x1b[?1006h");
    assert_eq!(
        terminal.mouse_protocol().tracking,
        MouseTracking::ButtonEvent
    );

    terminal.advance(b"\x1bc"); // RIS
    assert_eq!(terminal.mouse_protocol(), MouseProtocol::default());
    // Title persists across RIS (a window property, not power-on state).
    assert_eq!(terminal.title(), Some("keepme"));
}
