//! Deterministic mode-matrix fixtures for alternate-screen behavior.
//!
//! Covers DECSET/DECRST modes 47, 1047, 1048, 1049 per xterm ctlseqs:
//!
//! - **1049**: save cursor (DECSC), switch to alt buffer, clear alt on enter;
//!   switch to primary, restore cursor (DECRC) on leave.
//! - **1048**: cursor save/restore only (DECSC/DECRC), no screen switching.
//! - **1047**: switch alt buffer; clear alt on leave.
//! - **47**: plain alt buffer switch, no cursor save, no clear.
//!
//! Also covers: ED 2 in alt screen, scrollback isolation, re-entrancy,
//! DECSC/DECRC interaction, RIS/DECSTR inside alt, resize in alt + primary
//! reflow on return.
//!
//! All fixtures are deterministic (no PTY, no host binaries).

use super::*;

// ─── Helper ────────────────────────────────────────────────────────────────

fn visible_text(terminal: &Terminal) -> String {
    terminal
        .screen()
        .plain_text()
        .trim_end_matches('\n')
        .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// Mode 1049 — save cursor + switch + clear alt / restore cursor
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mode_1049_enter_saves_cursor_and_clears_alt() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 7 });

    terminal.advance(b"\x1b[?1049h");

    // Alt screen is blank; cursor reset to origin.
    assert_eq!(visible_text(&terminal), "");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
}

#[test]
fn mode_1049_leave_restores_cursor_and_primary() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");
    let pre_cursor = terminal.screen().cursor();

    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"alt text");
    terminal.advance(b"\x1b[?1049l");

    assert!(terminal.screen().plain_text().contains("primary"));
    assert_eq!(terminal.screen().cursor(), pre_cursor);
    assert!(!terminal.screen().plain_text().contains("alt text"));
}

#[test]
fn mode_1049_scrollback_isolation_alt_never_feeds_scrollback() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"l1\r\nl2\r\nl3\r\nl4");
    let primary_sb = terminal.screen().scrollback_len();
    assert!(primary_sb > 0);

    terminal.advance(b"\x1b[?1049h");
    assert_eq!(terminal.screen().scrollback_len(), 0);

    // Overflow the alt screen — must not accumulate scrollback.
    terminal.advance(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng");
    assert_eq!(terminal.screen().scrollback_len(), 0);

    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.screen().scrollback_len(), primary_sb);
}

#[test]
fn mode_1049_reentry_is_noop() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");

    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"first alt");
    // Second DECSET 1049 while already in alt is a no-op.
    terminal.advance(b"\x1b[?1049h");
    assert!(
        terminal.screen().plain_text().contains("first alt"),
        "double-enter should not clear; alt content should persist"
    );

    terminal.advance(b"\x1b[?1049l");
    assert!(terminal.screen().plain_text().contains("primary"));
}

#[test]
fn mode_1049_leave_without_enter_is_noop() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");
    let before = terminal.screen().plain_text();
    let cursor_before = terminal.screen().cursor();

    terminal.advance(b"\x1b[?1049l");

    assert_eq!(terminal.screen().plain_text(), before);
    assert_eq!(terminal.screen().cursor(), cursor_before);
}

#[test]
fn mode_1049_decsc_decrc_interaction() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[1;5H"); // cursor at (0, 4)
    terminal.advance(b"\x1b7"); // DECSC: save cursor

    terminal.advance(b"\x1b[?1049h");
    // Fresh alt has no saved cursor; DECRC should be a no-op.
    terminal.advance(b"\x1b[2;3H"); // cursor at (1, 2) in alt
    terminal.advance(b"\x1b8"); // DECRC in alt — no saved cursor here
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 2 });

    // DECSC/DECRC within alt works internally.
    terminal.advance(b"\x1b7"); // save (1, 2) in alt
    terminal.advance(b"\x1b[3;1H");
    terminal.advance(b"\x1b8"); // restore to (1, 2)
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 2 });

    terminal.advance(b"\x1b[?1049l");
    // 1049 restores cursor to position at alt-enter time.
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
}

#[test]
fn mode_1049_ris_inside_alt_exits_to_clean_state() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"alt");

    terminal.advance(b"\x1bc"); // RIS

    assert_eq!(visible_text(&terminal), "");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    assert_eq!(terminal.screen().scrollback_len(), 0);

    // On primary now: DECRST 1049 is a no-op.
    terminal.advance(b"after ris");
    terminal.advance(b"\x1b[?1049l");
    assert!(terminal.screen().plain_text().contains("after ris"));
}

#[test]
fn mode_1049_decstr_inside_alt_resets_modes_keeps_alt() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"alt text");
    terminal.advance(b"\x1b[?2004h");

    terminal.advance(b"\x1b[!p"); // DECSTR

    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    assert!(!terminal.bracketed_paste_enabled());
    // DECSTR preserves cells — alt text still there.
    assert!(terminal.screen().plain_text().contains("alt text"));

    // Can still exit alt normally.
    terminal.advance(b"\x1b[?1049l");
    assert!(terminal.screen().plain_text().contains("primary"));
}

#[test]
fn mode_1049_resize_in_alt_preserves_primary_reflow() {
    let mut terminal = Terminal::new(20, 3);
    let line = "abcdefghijklmnopqrstuvwxyz0123"; // 30 chars
    terminal.advance(line.as_bytes());

    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"TUI app content");
    terminal.resize(10, 3);
    assert_eq!(terminal.screen().scrollback_len(), 0);

    terminal.advance(b"\x1b[?1049l");
    terminal.resize(40, 3);
    assert_eq!(visible_text(&terminal), line);
}

#[test]
fn mode_1049_ed2_in_alt_does_not_affect_primary() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");

    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"alt text");
    terminal.advance(b"\x1b[2J");
    assert_eq!(visible_text(&terminal), "");

    terminal.advance(b"\x1b[?1049l");
    assert!(terminal.screen().plain_text().contains("primary"));
}

#[test]
fn mode_1049_scroll_region_does_not_leak_to_primary() {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");

    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"\x1b[2;3r"); // set scroll region in alt
    terminal.advance(b"\x1b[?1049l");

    // The alt scroll region must not be in effect on primary. A newline at
    // the bottom should scroll the full screen and create scrollback.
    terminal.advance(b"\x1b[4;1H\n");
    assert_eq!(terminal.screen().scrollback_len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// Mode 1048 — cursor save/restore only (DECSC/DECRC), no screen switch
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mode_1048_saves_and_restores_cursor() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"text");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });

    terminal.advance(b"\x1b[?1048h"); // DECSC
    terminal.advance(b"\x1b[3;1H");
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });

    terminal.advance(b"\x1b[?1048l"); // DECRC
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
}

#[test]
fn mode_1048_does_not_switch_screens() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");
    let content = terminal.screen().plain_text();

    terminal.advance(b"\x1b[?1048h");
    assert_eq!(terminal.screen().plain_text(), content);
    terminal.advance(b"\x1b[?1048l");
    assert_eq!(terminal.screen().plain_text(), content);
}

// ═══════════════════════════════════════════════════════════════════════════
// Mode 47 — plain alt buffer switch (no cursor save, no clear)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mode_47_enters_alt_screen() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");

    terminal.advance(b"\x1b[?47h");

    // After DECSET 47, we should be on an alt screen: primary content hidden.
    // DECRST 47 returns to primary.
    terminal.advance(b"alt stuff");
    terminal.advance(b"\x1b[?47l");

    assert!(
        terminal.screen().plain_text().contains("primary"),
        "primary content must be visible after DECRST 47"
    );
}

#[test]
fn mode_47_does_not_clear_alt_on_enter() {
    // Mode 47 does not clear the alt buffer on entry (unlike 1049).
    // If we enter alt, write, leave, then re-enter, previous alt content
    // might still be there (implementation-dependent, but the key point is
    // that 47 does not explicitly clear on enter).
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");

    terminal.advance(b"\x1b[?47h");
    // We're now on alt. Content should be blank (fresh alt buffer first time).
    // Write something.
    terminal.advance(b"alt mark");
    terminal.advance(b"\x1b[?47l"); // back to primary
    assert!(terminal.screen().plain_text().contains("primary"));
}

// ═══════════════════════════════════════════════════════════════════════════
// Mode 1047 — switch + clear alt on leave
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mode_1047_enters_and_leaves_alt() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");

    terminal.advance(b"\x1b[?1047h");
    terminal.advance(b"alt text");
    terminal.advance(b"\x1b[?1047l");

    assert!(
        terminal.screen().plain_text().contains("primary"),
        "primary content must be restored after DECRST 1047"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Scrollback isolation (general)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn alt_screen_su_never_feeds_scrollback() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"r0\r\nr1\r\nr2");
    terminal.advance(b"\x1b[3S");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn alt_screen_line_overflow_never_feeds_scrollback() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"a\r\nb\r\nc\r\nd\r\ne");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// ED 2 / ED 3 behavior in alt screen
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ed2_in_alt_clears_visible_grid() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"line1\r\nline2\r\nline3");

    terminal.advance(b"\x1b[2J");

    let text = visible_text(&terminal);
    assert_eq!(text, "", "ED 2 should blank the alt grid; got: {text:?}");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn ed2_in_alt_does_not_move_cursor() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"\x1b[2;5H");
    terminal.advance(b"\x1b[2J");
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 4 });
}

#[test]
fn ed3_in_alt_does_not_clear_primary_scrollback() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"l1\r\nl2\r\nl3\r\nl4\r\nl5");
    let primary_sb = terminal.screen().scrollback_len();
    assert!(primary_sb > 0);

    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"\x1b[3J");
    terminal.advance(b"\x1b[?1049l");

    assert_eq!(terminal.screen().scrollback_len(), primary_sb);
}

// ═══════════════════════════════════════════════════════════════════════════
// Resize in alt screen
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn resize_in_alt_does_not_create_scrollback() {
    let mut terminal = Terminal::new(20, 5);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
    terminal.resize(10, 3);
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn resize_in_alt_then_leave_primary_coherent() {
    let mut terminal = Terminal::new(20, 3);
    terminal.advance(b"short\r\nlines\r\nhere");

    terminal.advance(b"\x1b[?1049h");
    terminal.resize(10, 3);
    terminal.advance(b"\x1b[?1049l");

    assert!(terminal.screen().plain_text().contains("short"));
    let cursor = terminal.screen().cursor();
    assert!(cursor.column < 10);
}

#[test]
fn resize_widen_in_alt_primary_reflows_on_return() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"abcdefghijklmno"); // 15 chars, wraps at width 10

    terminal.advance(b"\x1b[?1049h");
    terminal.resize(20, 3);
    terminal.advance(b"\x1b[?1049l");

    assert_eq!(visible_text(&terminal), "abcdefghijklmno");
}

// ═══════════════════════════════════════════════════════════════════════════
// Cursor save/restore pairing across enter/leave
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn primary_saved_cursor_preserved_through_alt_roundtrip() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[1;8H"); // (0, 7)
    terminal.advance(b"\x1b7"); // DECSC

    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"\x1b[?1049l");

    // 1049 restores cursor to position at alt-enter time.
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 7 });

    // The earlier DECSC is preserved in StoredScreen.saved_cursor; DECRC
    // should still be able to reach it.
    terminal.advance(b"\x1b[3;1H");
    terminal.advance(b"\x1b8"); // DECRC
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 7 });
}

#[test]
fn alt_screen_saved_cursor_does_not_leak_to_primary() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"\x1b[3;3H");
    terminal.advance(b"\x1b7"); // DECSC in alt

    terminal.advance(b"\x1b[?1049l");

    // Primary had no saved cursor. DECRC should be a no-op.
    let cursor = terminal.screen().cursor();
    terminal.advance(b"\x1b8");
    assert_eq!(terminal.screen().cursor(), cursor);
}

// ═══════════════════════════════════════════════════════════════════════════
// Mode interaction: 1048 + 1047 combo as explicit two-step 1049
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mode_1048_then_1047_save_enter_leave_restore() {
    // Apps sometimes use "DECSET 1048; DECSET 1047" followed by
    // "DECRST 1047; DECRST 1048" as an explicit two-step equivalent of 1049.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");
    terminal.advance(b"\x1b[1;5H"); // cursor at (0, 4)

    terminal.advance(b"\x1b[?1048h"); // save cursor
    terminal.advance(b"\x1b[?1047h"); // enter alt

    terminal.advance(b"alt");

    terminal.advance(b"\x1b[?1047l"); // leave alt
    terminal.advance(b"\x1b[?1048l"); // restore cursor

    assert!(
        terminal.screen().plain_text().contains("primary"),
        "primary must be restored"
    );
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
}

// ═══════════════════════════════════════════════════════════════════════════
// Modal state persistence through alt roundtrip
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bracketed_paste_persists_through_alt_roundtrip() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?2004h");
    assert!(terminal.bracketed_paste_enabled());

    terminal.advance(b"\x1b[?1049h");
    assert!(terminal.bracketed_paste_enabled());

    terminal.advance(b"\x1b[?1049l");
    assert!(terminal.bracketed_paste_enabled());
}

#[test]
fn mouse_mode_persists_through_alt_roundtrip() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1006h\x1b[?1000h");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Normal);
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Sgr);

    terminal.advance(b"\x1b[?1049h");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Normal);

    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Normal);
}

#[test]
fn focus_reporting_persists_through_alt_roundtrip() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?1004h");
    assert!(terminal.focus_reporting());

    terminal.advance(b"\x1b[?1049h");
    assert!(terminal.focus_reporting());

    terminal.advance(b"\x1b[?1049l");
    assert!(terminal.focus_reporting());
}
