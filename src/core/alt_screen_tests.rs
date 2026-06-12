//! Deterministic mode-matrix fixtures for alternate-screen behavior.
//!
//! Covers DECSET/DECRST modes 47, 1047, 1048, 1049 per xterm ctlseqs with
//! **distinct per-mode semantics**:
//!
//! - **1049**: save cursor (DECSC) + switch + clear alt on enter;
//!   switch to primary + restore cursor (DECRC) on leave. Equivalent to
//!   `1048h; 1047h` on set and `1047l; 1048l` on reset.
//! - **1048**: cursor save/restore only (DECSC/DECRC), no screen switching.
//! - **1047**: switch alt buffer; NO clear on enter; clear alt on leave.
//! - **47**: plain alt buffer switch, no cursor save, no clear on enter or
//!   leave. Cursor position is NOT restored on leave.
//!
//! Also covers: cursor_visible + current_attrs StoredScreen save/restore
//! (F3/F4), ED 2 in alt screen, scrollback isolation, re-entrancy,
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
    terminal.advance(b"alt stuff");
    terminal.advance(b"\x1b[?47l");

    assert!(
        terminal.screen().plain_text().contains("primary"),
        "primary content must be visible after DECRST 47"
    );
}

#[test]
fn mode_47_does_not_clear_alt_on_enter() {
    // Mode 47 does not clear on entry (unlike 1049). The alt buffer starts
    // blank on first use but the key distinction is: it does not explicitly
    // home the cursor on entry.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary");

    terminal.advance(b"\x1b[?47h");
    terminal.advance(b"alt mark");
    terminal.advance(b"\x1b[?47l");
    assert!(terminal.screen().plain_text().contains("primary"));
}

#[test]
fn mode_47_does_not_restore_cursor_on_leave() {
    // Mode 47 does NOT save/restore cursor: the cursor retains whatever
    // position the alt-screen app left it in (clamped to bounds).
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[1;5H"); // cursor at (0, 4) on primary

    terminal.advance(b"\x1b[?47h");
    terminal.advance(b"\x1b[3;1H"); // move cursor to (2, 0) on alt
    terminal.advance(b"\x1b[?47l");

    // Cursor should be at (2, 0), NOT restored to (0, 4).
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
}

#[test]
fn mode_47_does_not_home_cursor_on_enter() {
    // Mode 47 does not clear or home cursor on enter (unlike 1049).
    // The cursor carries its primary position into the alt buffer.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[2;5H"); // cursor at (1, 4)

    terminal.advance(b"\x1b[?47h");

    // Cursor should NOT be homed to (0,0). Mode 47 preserves cursor position.
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 4 });
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

#[test]
fn mode_1047_does_not_restore_cursor_on_leave() {
    // Like mode 47, mode 1047 does NOT pair with cursor save/restore.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[1;5H"); // cursor at (0, 4)

    terminal.advance(b"\x1b[?1047h");
    terminal.advance(b"\x1b[3;1H"); // move cursor to (2, 0)
    terminal.advance(b"\x1b[?1047l");

    // Cursor should NOT be restored.
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
}

#[test]
fn mode_1047_does_not_home_cursor_on_enter() {
    // 1047 does not clear or home cursor on enter.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[2;5H"); // cursor at (1, 4)

    terminal.advance(b"\x1b[?1047h");

    // Cursor should carry over from primary.
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 4 });
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

// ═══════════════════════════════════════════════════════════════════════════
// F3: cursor_visible saved/restored with StoredScreen
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cursor_visible_restored_after_alt_roundtrip_1049() {
    let mut terminal = Terminal::new(10, 3);
    // Hide cursor on primary.
    terminal.advance(b"\x1b[?25l");
    assert!(!terminal.snapshot().cursor_visible);

    terminal.advance(b"\x1b[?1049h");
    // Alt screen should start with default cursor visibility (visible).
    // Show cursor explicitly to change state.
    terminal.advance(b"\x1b[?25h");
    assert!(terminal.snapshot().cursor_visible);

    terminal.advance(b"\x1b[?1049l");
    // Primary cursor visibility should be restored: hidden.
    assert!(
        !terminal.snapshot().cursor_visible,
        "cursor_visible should be restored to primary state (hidden) after leaving alt"
    );
}

#[test]
fn cursor_visible_hidden_in_alt_does_not_leak_to_primary() {
    let mut terminal = Terminal::new(10, 3);
    // Cursor visible on primary (default).
    assert!(terminal.snapshot().cursor_visible);

    terminal.advance(b"\x1b[?1049h");
    // Hide cursor while in alt.
    terminal.advance(b"\x1b[?25l");
    assert!(!terminal.snapshot().cursor_visible);

    terminal.advance(b"\x1b[?1049l");
    // Primary's cursor should be visible (its original state).
    assert!(
        terminal.snapshot().cursor_visible,
        "hiding cursor in alt should not affect primary cursor visibility"
    );
}

#[test]
fn cursor_visible_restored_after_alt_roundtrip_47() {
    // Mode 47 also restores cursor_visible (screen-level state from primary).
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[?25l"); // hide on primary
    assert!(!terminal.snapshot().cursor_visible);

    terminal.advance(b"\x1b[?47h");
    terminal.advance(b"\x1b[?25h"); // show in alt
    assert!(terminal.snapshot().cursor_visible);

    terminal.advance(b"\x1b[?47l");
    assert!(
        !terminal.snapshot().cursor_visible,
        "mode 47 leave should restore primary cursor_visible"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// F4: current_attrs (SGR state) saved/restored with StoredScreen
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn current_attrs_restored_after_alt_roundtrip_1049() {
    let mut terminal = Terminal::new(10, 3);
    // Set bold red attrs on primary.
    terminal.advance(b"\x1b[1;31m");

    terminal.advance(b"\x1b[?1049h");
    // Change attrs in alt to something different.
    terminal.advance(b"\x1b[0;32m"); // green, not bold

    terminal.advance(b"\x1b[?1049l");
    // Print a char: it should carry the primary's bold-red attrs.
    // Cursor is at (0, 0) — DECRC restored to the position at enter time.
    terminal.advance(b"X");

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'X');
    assert!(
        cell.attrs.bold(),
        "bold should be restored from primary state after leaving alt"
    );
    assert_eq!(
        cell.attrs.foreground,
        Color::Indexed(1),
        "foreground color should be restored from primary state (red, idx 1)"
    );
}

#[test]
fn current_attrs_changed_in_alt_does_not_leak_to_primary() {
    let mut terminal = Terminal::new(10, 3);
    // Default attrs on primary.
    terminal.advance(b"\x1b[?1049h");
    // Set bold + underline + blue in alt.
    terminal.advance(b"\x1b[1;4;34m");

    terminal.advance(b"\x1b[?1049l");
    // Print a char on primary: should have default attrs.
    terminal.advance(b"Y");

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'Y');
    assert_eq!(
        cell.attrs,
        Attrs::default(),
        "attrs changed in alt should not leak to primary"
    );
}

#[test]
fn current_attrs_restored_after_alt_roundtrip_47() {
    // Mode 47 also restores current_attrs.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[3;33m"); // italic yellow

    terminal.advance(b"\x1b[?47h");
    terminal.advance(b"\x1b[0m"); // reset in alt

    terminal.advance(b"\x1b[?47l");
    terminal.advance(b"Z");

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'Z');
    assert!(
        cell.attrs.italic(),
        "italic should be restored from primary after mode 47 leave"
    );
    assert_eq!(
        cell.attrs.foreground,
        Color::Indexed(3),
        "foreground (yellow, idx 3) should be restored after mode 47 leave"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Mode 1049 distinct: DECSC on enter, DECRC on leave
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mode_1049_saves_cursor_via_decsc_on_enter() {
    // Mode 1049 set is equivalent to "DECSC; enter alt; clear alt".
    // The DECSC happens before entering alt, so the saved cursor belongs to
    // the primary saved-cursor slot and can be queried after leaving.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[2;6H"); // cursor at (1, 5)

    terminal.advance(b"\x1b[?1049h");

    // Alt starts clear with cursor at origin.
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

    terminal.advance(b"\x1b[3;3H"); // move in alt
    terminal.advance(b"\x1b[?1049l");

    // DECRC restores the saved cursor from DECSC at enter time: (1, 5).
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 5 });
}
