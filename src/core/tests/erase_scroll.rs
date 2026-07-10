// SPDX-License-Identifier: GPL-3.0-only
//! Core behavioral tests (M4 mechanical split from core/tests.rs).

use super::*;

#[test]
fn full_screen_scroll_region_still_feeds_scrollback() {
    // A full-screen DECSTBM region (ESC[1;<rows>r) is equivalent to no region:
    // lines scrolled off the top leave the screen entirely, so they must enter
    // scrollback exactly as the no-region path does. Many TUIs set a full-screen
    // region and never reset it; without this, scrollback silently stops filling
    // and the user cannot scroll back at all.
    let mut terminal = Terminal::new(4, 3);
    // Set the scroll region to the entire screen (rows 1..=3 in 1-based DECSTBM).
    terminal.advance(b"\x1b[1;3r");
    // Print 8 lines into a 3-row screen: 5 rows must scroll off into scrollback.
    for i in 1..=8 {
        terminal.advance(format!("L{i}\r\n").as_bytes());
    }
    assert!(
        terminal.screen().scrollback_len() >= 5,
        "full-screen region must still feed scrollback, got {}",
        terminal.screen().scrollback_len()
    );
}

#[test]
fn partial_scroll_region_does_not_feed_scrollback() {
    // A PARTIAL region (top margin below the screen top) preserves content above
    // it, so scrolled-out lines are discarded, not saved — matching xterm.
    let mut terminal = Terminal::new(4, 4);
    // Region rows 2..=4 (1-based): top margin is row 1 (0-based), content above.
    terminal.advance(b"\x1b[2;4r");
    for i in 1..=10 {
        terminal.advance(format!("L{i}\r\n").as_bytes());
    }
    assert_eq!(
        terminal.screen().scrollback_len(),
        0,
        "a partial region must not feed scrollback"
    );
}

#[test]
fn top_anchored_partial_region_feeds_scrollback_and_preserves_footer() {
    // A TOP-ANCHORED partial region (top margin at row 0, a footer reserved
    // below the bottom margin) is what a full-screen TUI sets when it keeps a
    // bottom input composer, e.g. ratatui's `ESC[1;<rows-1>r`. The content
    // above the margin is real history: linefeed-at-region-bottom must feed it
    // into scrollback so wheel-up can reveal it, while the footer rows below
    // the margin stay fixed. This is the Codex/ratatui "scroll-up is dead"
    // regression: before the fix the scrolled-off rows were discarded and
    // scrollback stayed empty.
    let mut terminal = Terminal::new(6, 6);
    // Region rows 1..=4 (1-based) -> top row index 0, bottom row index 3.
    // Rows 4 and 5 (0-based) are the reserved footer.
    terminal.advance(b"\x1b[1;4r");
    // Paint the footer, then return the cursor into the region.
    terminal.advance(b"\x1b[5;1HFOOTA");
    terminal.advance(b"\x1b[6;1HFOOTB");
    terminal.advance(b"\x1b[1;1H");
    // Six lines into a four-row region: L0..L2 must scroll off into scrollback.
    for i in 0..6 {
        terminal.advance(format!("L{i}\r\n").as_bytes());
    }
    // Scrollback grew from 0 (reproduces the dead-scroll bug when it stays 0).
    assert_eq!(
        terminal.screen().scrollback_len(),
        3,
        "top-anchored region must feed scrollback, got {}",
        terminal.screen().scrollback_len()
    );
    // The footer rows below the margin are untouched.
    let footer_a: String = (0..5)
        .map(|c| terminal.screen().cell(4, c).unwrap().ch)
        .collect();
    let footer_b: String = (0..5)
        .map(|c| terminal.screen().cell(5, c).unwrap().ch)
        .collect();
    assert_eq!(footer_a, "FOOTA", "footer row preserved");
    assert_eq!(footer_b, "FOOTB", "footer row preserved");
    // The correct rows entered scrollback, oldest first, wrap chain intact.
    let paged = snapshot_rows(&terminal.snapshot_with_scrollback(3));
    assert_eq!(
        &paged[0..3],
        ["L0", "L1", "L2"],
        "scrollback holds the history"
    );
}

#[test]
fn explicit_su_at_top_zero_region_does_not_feed_scrollback() {
    // Contrast to the linefeed path: explicit SU (CSI S) is an application
    // scroll and keeps its documented no-pollution discard even when the region
    // is anchored at row 0. Only the natural linefeed/index path feeds history.
    let mut terminal = Terminal::new(4, 4);
    // Top-anchored partial region rows 1..=3 (top index 0, bottom index 2).
    terminal.advance(b"\x1b[1;3r");
    terminal.advance(b"\x1b[1;1Hr0\r\nr1\r\nr2");
    terminal.advance(b"\x1b[S");
    assert_eq!(
        terminal.screen().scrollback_len(),
        0,
        "explicit SU at top==0 must not feed scrollback"
    );
}

#[test]
fn alt_screen_top_anchored_region_never_feeds_scrollback() {
    // The scrollback-feeding path is primary-screen only. On the alternate
    // screen a top-anchored partial region still discards off the top; the alt
    // buffer never accumulates scrollback.
    let mut terminal = Terminal::new(4, 4);
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"\x1b[1;3r");
    terminal.advance(b"\x1b[1;1H");
    for i in 0..5 {
        terminal.advance(format!("a{i}\r\n").as_bytes());
    }
    assert_eq!(
        terminal.screen().scrollback_len(),
        0,
        "alternate screen never feeds scrollback"
    );
}

#[test]
fn background_color_erase_applies_to_ed_el_and_ech() {
    let mut terminal = Terminal::new(6, 3);
    let red = Color::Indexed(1);

    terminal.advance(b"abcdef\r\nghijkl\r\nmnopqr");
    terminal.advance(b"\x1b[1;34;41m\x1b[2;3H\x1b[J");

    assert_eq!(terminal.screen().cell(1, 1).unwrap().ch, 'h');
    assert_blank_with_background(&terminal, 1, 2, red);
    assert_blank_with_background(&terminal, 2, 5, red);

    terminal.advance(b"\x1b[1;1H\x1b[K");
    assert_blank_with_background(&terminal, 0, 0, red);
    assert_blank_with_background(&terminal, 0, 5, red);

    terminal.advance(b"\x1b[2;1Hzzzzzz\x1b[2;2H\x1b[2X");
    assert_eq!(terminal.screen().plain_text(), "\nz  zzz\n");
    assert_blank_with_background(&terminal, 1, 1, red);
    assert_blank_with_background(&terminal, 1, 2, red);
}

#[test]
fn background_color_erase_uses_default_after_sgr_49_and_reset() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance(b"\x1b[1;34;41mabcdef\x1b[49m\x1b[1;2H\x1b[K");
    assert_blank_with_background(&terminal, 0, 1, Color::Default);
    assert_blank_with_background(&terminal, 0, 5, Color::Default);

    terminal.advance(b"\x1b[41m\x1b[1;1H\x1b[0m\x1b[X");
    assert_blank_with_background(&terminal, 0, 0, Color::Default);
}

#[test]
fn wraps_after_right_edge_on_next_printable() {
    let mut terminal = Terminal::new(5, 2);

    terminal.advance(b"abcdeF");

    assert_eq!(terminal.screen().plain_text(), "abcde\nF");
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 1 });
}

#[test]
fn scrolls_at_bottom() {
    let mut terminal = Terminal::new(5, 2);

    terminal.advance(b"one\r\ntwo\r\nthree");

    assert_eq!(terminal.screen().plain_text(), "two\nthree");
    assert_eq!(terminal.screen().scrollback_len(), 1);
}

fn snapshot_rows(snapshot: &Snapshot) -> Vec<String> {
    let columns = snapshot.dimensions.columns;
    snapshot
        .cells
        .chunks(columns)
        .map(|row| {
            row.iter()
                .filter(|cell| !cell.wide_continuation)
                .map(|cell| cell.ch)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn scrollback_snapshot_offset_zero_matches_live() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"one\r\ntwo\r\nthree");

    // Offset 0 is byte-for-byte identical to the live snapshot, cursor and
    // visibility included.
    assert_eq!(
        terminal.snapshot_with_scrollback(0),
        terminal.snapshot(),
        "offset 0 must equal the live snapshot"
    );
}

#[test]
fn scrollback_snapshot_mixes_scrollback_and_visible_rows() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

    // Visible "three/four"; scrollback holds "one","two".
    assert_eq!(terminal.screen().scrollback_len(), 2);
    assert_eq!(
        snapshot_rows(&terminal.snapshot_with_scrollback(0)),
        ["three", "four"]
    );
    // Offset 1 pages up one row: scrollback "two" + visible "three".
    assert_eq!(
        snapshot_rows(&terminal.snapshot_with_scrollback(1)),
        ["two", "three"]
    );
    // Offset 2 reaches the oldest stored rows.
    assert_eq!(
        snapshot_rows(&terminal.snapshot_with_scrollback(2)),
        ["one", "two"]
    );
}

#[test]
fn scrollback_snapshot_clamps_beyond_history() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

    // Any offset past the available scrollback clamps to the oldest window.
    let clamped = terminal.snapshot_with_scrollback(999);
    assert_eq!(snapshot_rows(&clamped), ["one", "two"]);
    assert_eq!(clamped, terminal.snapshot_with_scrollback(2));
}

#[test]
fn scrollback_snapshot_hides_cursor_when_scrolled() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");

    // Offset 0 keeps the live cursor visible; any scrolled-back offset hides
    // it because the cursor does not belong to the historical viewport.
    assert!(terminal.snapshot_with_scrollback(0).cursor_visible);
    assert!(!terminal.snapshot_with_scrollback(1).cursor_visible);
    assert!(!terminal.snapshot_with_scrollback(999).cursor_visible);
}

#[test]
fn scrollback_snapshot_isolates_alternate_screen() {
    let mut terminal = Terminal::new(5, 2);
    // Build primary scrollback, then enter the alternate screen.
    terminal.advance(b"one\r\ntwo\r\nthree\r\nfour");
    assert_eq!(terminal.screen().scrollback_len(), 2);
    terminal.advance(b"\x1b[?1049h");

    // Alternate screen has no scrollback: every offset clamps to the live
    // alternate grid and primary history never leaks in.
    assert_eq!(terminal.screen().scrollback_len(), 0);
    let live = terminal.snapshot();
    assert_eq!(terminal.snapshot_with_scrollback(0), live);
    assert_eq!(terminal.snapshot_with_scrollback(5), live);
}

#[test]
fn background_color_erase_applies_to_scroll_and_line_fills() {
    let mut terminal = Terminal::new(4, 3);

    terminal.advance(b"r0\r\nr1\r\nr2");
    terminal.advance(b"\x1b[42m\x1b[3;1H\n");
    assert_blank_with_background(&terminal, 2, 0, Color::Indexed(2));
    assert_blank_with_background(&terminal, 2, 3, Color::Indexed(2));

    terminal.advance(b"\x1b[43m\x1b[2;1H\x1b[L");
    assert_blank_with_background(&terminal, 1, 0, Color::Indexed(3));
    assert_blank_with_background(&terminal, 1, 3, Color::Indexed(3));

    terminal.advance(b"\x1b[44m\x1b[2;1H\x1b[M");
    assert_blank_with_background(&terminal, 2, 0, Color::Indexed(4));
    assert_blank_with_background(&terminal, 2, 3, Color::Indexed(4));
}

#[test]
fn background_color_erase_applies_inside_scroll_regions() {
    let mut terminal = Terminal::new(4, 4);

    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
    terminal.advance(b"\x1b[45m\x1b[2;3r\x1b[3;1H\n");
    assert_blank_with_background(&terminal, 2, 0, Color::Indexed(5));
    assert_blank_with_background(&terminal, 2, 3, Color::Indexed(5));

    terminal.advance(b"\x1b[46m\x1b[2;1H\x1bM");
    assert_blank_with_background(&terminal, 1, 0, Color::Indexed(6));
    assert_blank_with_background(&terminal, 1, 3, Color::Indexed(6));
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn reverse_index_at_top_margin_scrolls_region_down() {
    let mut terminal = Terminal::new(8, 4);

    // Fill four rows, set region rows 2..=3 (1-based 2;3 -> top=1, bottom=2),
    // home into the region top, then RI to scroll the region down by one.
    terminal.advance(b"top\r\none\r\ntwo\r\nbot");
    terminal.advance(b"\x1b[2;3r"); // homes cursor to 1,1 (top-left)
    terminal.advance(b"\x1b[2;1H"); // move to region top (row index 1)
    terminal.advance(b"\x1bM"); // RI

    // Region (rows 1,2) scrolls down: blank inserted at top of region,
    // former bottom-of-region line discarded. Outside rows untouched.
    assert_eq!(terminal.screen().plain_text(), "top\n\none\nbot");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn reverse_index_below_top_moves_cursor_up() {
    let mut terminal = Terminal::new(8, 3);

    terminal.advance(b"\x1b[3;1H"); // row index 2
    terminal.advance(b"\x1bM"); // RI moves cursor up, no scroll

    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
    assert_eq!(terminal.screen().plain_text(), "\n\n");
}

#[test]
fn index_moves_cursor_down_preserving_column() {
    // NF5: ESC D (IND) mid-screen moves the cursor down one row, column
    // untouched — previously a silent no-op (missing dispatch_esc arm).
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"\x1b[1;3H"); // row 0, column 2
    terminal.advance(b"\x1bD");
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 2 });
    assert_eq!(terminal.screen().plain_text(), "\n\n");
}

#[test]
fn index_at_bottom_margin_scrolls_region() {
    // NF5: ESC D at the DECSTBM bottom margin scrolls the region up by one;
    // rows outside the region are untouched and nothing enters scrollback
    // (partial region discards, matching xterm).
    let mut terminal = Terminal::new(8, 4);
    terminal.advance(b"top\r\none\r\ntwo\r\nbot");
    terminal.advance(b"\x1b[2;3r"); // region rows index 1..=2; homes cursor
    terminal.advance(b"\x1b[3;1H"); // region bottom (row index 2)
    terminal.advance(b"\x1bD");
    assert_eq!(terminal.screen().plain_text(), "top\ntwo\n\nbot");
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
}

#[test]
fn index_at_screen_bottom_feeds_scrollback() {
    // NF5: with no region, ESC D at the last row scrolls the full screen and
    // the departing top line enters scrollback, exactly like LF.
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"r0\r\nr1\r\nr2");
    terminal.advance(b"\x1b[3;2H"); // last row, column 1
    terminal.advance(b"\x1bD");
    assert_eq!(terminal.screen().plain_text(), "r1\nr2\n");
    assert_eq!(terminal.screen().scrollback_len(), 1);
    // Column preserved: IND never touches the column.
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 1 });
}

#[test]
fn nel_moves_to_next_row_column_zero() {
    // NF5: ESC E (NEL) = IND + CR — next row, column 0.
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"abc"); // row 0, cursor at column 3
    terminal.advance(b"\x1bE");
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
    assert_eq!(terminal.screen().plain_text(), "abc\n\n");
}

#[test]
fn nel_at_bottom_margin_scrolls_region_and_returns_carriage() {
    // NF5: NEL at the DECSTBM bottom margin scrolls the region and lands the
    // cursor at column 0 of the (still-bottom) row.
    let mut terminal = Terminal::new(8, 4);
    terminal.advance(b"top\r\none\r\ntwo\r\nbot");
    terminal.advance(b"\x1b[2;3r");
    terminal.advance(b"\x1b[3;4H"); // region bottom, column 3
    terminal.advance(b"\x1bE");
    assert_eq!(terminal.screen().plain_text(), "top\ntwo\n\nbot");
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
}

#[test]
fn scroll_up_default_count_moves_content_up_one_line() {
    let mut terminal = Terminal::new(4, 4);
    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
    // SU with no param defaults to 1: every line shifts up one, blank at
    // the bottom, top line discarded. No scrollback pollution.
    terminal.advance(b"\x1b[S");
    assert_eq!(terminal.screen().plain_text(), "r1\nr2\nr3\n");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn scroll_up_explicit_count_and_clamp() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"r0\r\nr1\r\nr2");
    // SU 2 shifts content up two lines.
    terminal.advance(b"\x1b[2S");
    assert_eq!(terminal.screen().plain_text(), "r2\n\n");
    // SU with a count past the screen height clamps to a full clear.
    terminal.advance(b"\x1b[99S");
    assert_eq!(terminal.screen().plain_text(), "\n\n");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn scroll_down_default_count_moves_content_down_one_line() {
    let mut terminal = Terminal::new(4, 4);
    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
    // SD with no param defaults to 1: blank inserted at the top, bottom
    // line discarded.
    terminal.advance(b"\x1b[T");
    assert_eq!(terminal.screen().plain_text(), "\nr0\nr1\nr2");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn scroll_up_and_down_respect_scroll_region() {
    let mut terminal = Terminal::new(4, 4);
    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
    // Region = rows index 1..=2 (1-based 2;3). SU 1 scrolls only inside it.
    terminal.advance(b"\x1b[2;3r");
    terminal.advance(b"\x1b[S");
    assert_eq!(terminal.screen().plain_text(), "r0\nr2\n\nr3");
    // SD 1 inside the same region pushes the region content back down.
    terminal.advance(b"\x1b[T");
    assert_eq!(terminal.screen().plain_text(), "r0\n\nr2\nr3");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn scroll_up_fill_rows_use_background_color() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"r0\r\nr1\r\nr2");
    // BCE: SU's blank bottom row carries the active background color.
    terminal.advance(b"\x1b[41m\x1b[S");
    assert_blank_with_background(&terminal, 2, 0, Color::Indexed(1));
    assert_blank_with_background(&terminal, 2, 3, Color::Indexed(1));
}

#[test]
fn scroll_down_fill_rows_use_background_color() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"r0\r\nr1\r\nr2");
    // BCE: SD's blank top row carries the active background color.
    terminal.advance(b"\x1b[42m\x1b[T");
    assert_blank_with_background(&terminal, 0, 0, Color::Indexed(2));
    assert_blank_with_background(&terminal, 0, 3, Color::Indexed(2));
}

#[test]
fn cuu_stops_at_top_margin_when_cursor_inside_region() {
    // C7: CUU from inside a DECSTBM region stops AT the top margin, never
    // crossing into rows above the region (xterm / DEC STD 070).
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r"); // region rows index 2..=4; homes cursor
    terminal.advance(b"\x1b[4;2H"); // inside region (row index 3), column 1
    terminal.advance(b"\x1b[10A"); // CUU far past the margin
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 1 });
}

#[test]
fn cuu_above_region_travels_to_screen_top() {
    // C7: a cursor already ABOVE the region is outside the margins, so CUU
    // clamps to the screen top as before.
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r");
    terminal.advance(b"\x1b[2;1H"); // row index 1, above region top (2)
    terminal.advance(b"\x1b[10A");
    assert_eq!(terminal.screen().cursor().row, 0);
}

#[test]
fn cud_stops_at_bottom_margin_when_cursor_inside_region() {
    // C7 mirror: CUD from inside the region stops AT the bottom margin.
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r");
    terminal.advance(b"\x1b[4;3H"); // inside region (row index 3), column 2
    terminal.advance(b"\x1b[10B");
    assert_eq!(terminal.screen().cursor(), Position { row: 4, column: 2 });
}

#[test]
fn cud_below_region_travels_to_screen_bottom() {
    // C7: a cursor already BELOW the region clamps to the last screen row.
    let mut terminal = Terminal::new(8, 8);
    terminal.advance(b"\x1b[3;5r"); // region rows index 2..=4
    terminal.advance(b"\x1b[6;1H"); // row index 5, below region bottom (4)
    terminal.advance(b"\x1b[10B");
    assert_eq!(terminal.screen().cursor().row, 7);
}

#[test]
fn origin_mode_makes_cup_relative_to_region_top() {
    let mut terminal = Terminal::new(8, 6);
    // Region rows index 2..=4 (1-based 3;5), enable DECOM.
    terminal.advance(b"\x1b[3;5r\x1b[?6h");
    // After DECOM enable the cursor homes to the region top (row index 2).
    assert_eq!(terminal.screen().cursor().row, 2);
    // CUP row 1 addresses the region top, not the screen top.
    terminal.advance(b"\x1b[1;1H");
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
    // CUP row 2 is the second row of the region (screen index 3).
    terminal.advance(b"\x1b[2;1H");
    assert_eq!(terminal.screen().cursor(), Position { row: 3, column: 0 });
}

#[test]
fn origin_mode_clamps_cup_to_region_bottom() {
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r\x1b[?6h");
    // CUP far past the region bottom clamps to the region bottom (index 4),
    // never escaping into rows below the region.
    terminal.advance(b"\x1b[99;1H");
    assert_eq!(terminal.screen().cursor().row, 4);
}

#[test]
fn origin_mode_off_addresses_full_screen() {
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r"); // region set, DECOM off (default)
    // With DECOM off, CUP row 1 is the screen top regardless of the region.
    terminal.advance(b"\x1b[1;1H");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
    // And the cursor can address rows outside the region.
    terminal.advance(b"\x1b[6;1H");
    assert_eq!(terminal.screen().cursor().row, 5);
}

#[test]
fn origin_mode_disable_homes_to_screen_top() {
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r\x1b[?6h");
    assert_eq!(terminal.screen().cursor().row, 2);
    // Disabling DECOM homes back to the screen top-left.
    terminal.advance(b"\x1b[?6l");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
}

#[test]
fn origin_mode_applies_to_vpa() {
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r\x1b[?6h");
    // VPA (CSI Ps d) row 2 is region-relative under DECOM (screen index 3).
    terminal.advance(b"\x1b[2d");
    assert_eq!(terminal.screen().cursor().row, 3);
}

#[test]
fn origin_mode_reset_by_ris_and_decstr() {
    // RIS clears DECOM.
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r\x1b[?6h\x1bc");
    terminal.advance(b"\x1b[1;1H");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

    // DECSTR (soft reset) also clears DECOM.
    let mut terminal = Terminal::new(8, 6);
    terminal.advance(b"\x1b[3;5r\x1b[?6h\x1b[!p");
    terminal.advance(b"\x1b[1;1H");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
}

#[test]
fn decstbm_homes_to_region_top_under_origin_mode() {
    let mut terminal = Terminal::new(8, 6);
    // Enable DECOM first, then set a new region: DECSTBM homes to the new
    // region's top (index 1) rather than the screen top.
    terminal.advance(b"\x1b[?6h\x1b[2;4r");
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
}

#[test]
fn insert_lines_within_region_preserves_outside_rows() {
    let mut terminal = Terminal::new(8, 5);

    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3\r\nr4");
    terminal.advance(b"\x1b[2;4r"); // region rows index 1..=3
    terminal.advance(b"\x1b[3;1H"); // cursor at row index 2 (inside region)
    terminal.advance(b"\x1b[L"); // IL 1

    // Blank inserted at row 2; rows 2..3 shift down; region bottom (r3) lost.
    // Rows 0 and 4 (outside region) untouched. No scrollback pollution.
    assert_eq!(terminal.screen().plain_text(), "r0\nr1\n\nr2\nr4");
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
}

#[test]
fn delete_lines_within_region_preserves_outside_rows() {
    let mut terminal = Terminal::new(8, 5);

    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3\r\nr4");
    terminal.advance(b"\x1b[2;4r"); // region rows index 1..=3
    terminal.advance(b"\x1b[2;1H"); // cursor at row index 1 (region top)
    terminal.advance(b"\x1b[M"); // DL 1

    // r1 deleted; r2,r3 shift up; blank fills region bottom (row 3).
    // Rows 0 and 4 (outside region) untouched.
    assert_eq!(terminal.screen().plain_text(), "r0\nr2\nr3\n\nr4");
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
}

#[test]
fn insert_and_delete_lines_outside_region_are_noops() {
    let mut terminal = Terminal::new(8, 4);

    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
    terminal.advance(b"\x1b[2;3r"); // region rows index 1..=2
    terminal.advance(b"\x1b[4;1H"); // cursor at row index 3 (outside region)
    terminal.advance(b"\x1b[L"); // IL -> no-op
    terminal.advance(b"\x1b[M"); // DL -> no-op

    assert_eq!(terminal.screen().plain_text(), "r0\nr1\nr2\nr3");
}
