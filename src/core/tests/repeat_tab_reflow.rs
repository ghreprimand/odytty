// SPDX-License-Identifier: GPL-3.0-only
//! Core behavioral tests (M4 mechanical split from core/tests.rs).

use super::*;

// REP (CSI Ps b): repeat the last printed graphic char N times through normal
// print processing. Baseline: repeats carry the CURRENT SGR attrs and obey
// autowrap, exactly as if the char were typed again; omitted/zero count = 1;
// no-op when nothing graphic has been printed.
#[test]
fn repeat_char_repeats_last_graphic() {
    let mut terminal = Terminal::new(8, 1);

    terminal.advance(b"a\x1b[3b"); // print 'a', then REP 3

    // One original + three repeats = four 'a'.
    assert_eq!(terminal.screen().plain_text(), "aaaa");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
}

#[test]
fn repeat_char_default_and_zero_count_is_one() {
    let mut terminal = Terminal::new(8, 1);

    terminal.advance(b"x\x1bb"); // not a CSI; ensure only real REP counts
    // (ESC b is not REP; nothing should repeat from it.)
    assert_eq!(terminal.screen().plain_text(), "x");

    terminal.advance(b"\x1b[b"); // REP omitted -> 1
    assert_eq!(terminal.screen().plain_text(), "xx");

    terminal.advance(b"\x1b[0b"); // REP 0 -> 1
    assert_eq!(terminal.screen().plain_text(), "xxx");
}

#[test]
fn repeat_char_is_noop_without_preceding_graphic() {
    let mut terminal = Terminal::new(8, 1);

    terminal.advance(b"\x1b[5b"); // REP before any printable char

    assert_eq!(terminal.screen().plain_text(), "");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
}

#[test]
fn repeat_char_preserves_current_attrs() {
    let mut terminal = Terminal::new(8, 1);

    // REP reprints the previous graphic char through normal print handling,
    // so it uses CURRENT SGR attrs rather than the original cell attrs.
    terminal.advance(b"\x1b[1;31mr\x1b[0m\x1b[2b");

    let original = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(original.ch, 'r');
    assert!(original.attrs.bold());
    assert_eq!(original.attrs.foreground, Color::Indexed(1));

    for column in 1..3 {
        let repeated = terminal.screen().cell(0, column).unwrap();
        assert_eq!(repeated.ch, 'r');
        assert_eq!(repeated.attrs, Attrs::default());
    }
}

#[test]
fn repeat_char_is_reset_by_ris_and_decstr() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"a\x1bc\x1b[3b");
    assert_eq!(terminal.screen().plain_text(), "\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

    terminal.advance(b"b\x1b[!p\x1b[3b");
    assert_eq!(terminal.screen().plain_text(), "b\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
}

#[test]
fn repeat_char_obeys_autowrap() {
    let mut terminal = Terminal::new(3, 2);

    terminal.advance(b"a\x1b[3b"); // 'a' then REP 3 -> 4 'a' total across wrap

    // Row 0 fills to width 3; the 4th 'a' wraps onto row 1.
    assert_eq!(terminal.screen().plain_text(), "aaa\na");
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 1 });
}

#[test]
fn repeat_char_repeats_wide_glyph() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance("世".as_bytes()); // wide lead + continuation
    terminal.advance(b"\x1b[1b"); // REP 1 -> a second wide glyph

    // Policy (documented): REP replays a wide last char as a full wide glyph.
    assert_eq!(terminal.screen().plain_text(), "世世");
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);
    assert!(terminal.screen().cell(0, 3).unwrap().wide_continuation);
}

// Tab stops (HT / HTS / TBC): owned every-8 default model. HT advances to
// the next stop right of the cursor or clamps to the right edge; HTS (ESC H)
// sets a stop at the current column; TBC (CSI Ps g) clears current (0) or
// all (3). Reset policy: RIS restores defaults; DECSTR preserves stops
// (VT220 soft-reset definition). Resize preserves retained stops and
// default-fills newly exposed columns.

// Helper: column the cursor lands on after a single HT from `start`.
fn tab_to(terminal: &mut Terminal, start: usize) -> usize {
    terminal.advance(format!("\x1b[1;{}H", start + 1).as_bytes());
    terminal.advance(b"\t");
    terminal.screen().cursor().column
}

#[test]
fn default_tab_stops_advance_every_eight() {
    let mut terminal = Terminal::new(40, 1);

    assert_eq!(tab_to(&mut terminal, 0), 8);
    assert_eq!(tab_to(&mut terminal, 7), 8);
    assert_eq!(tab_to(&mut terminal, 8), 16);
    assert_eq!(tab_to(&mut terminal, 15), 16);
    assert_eq!(tab_to(&mut terminal, 23), 24);
}

#[test]
fn tab_clamps_to_right_edge_when_no_later_stop() {
    let mut terminal = Terminal::new(12, 1);

    // Width 12: default stop at col 8 only. From col 9 there is no later
    // stop, so HT clamps to the right edge (col 11).
    assert_eq!(tab_to(&mut terminal, 9), 11);
    // From the right edge, HT stays clamped.
    assert_eq!(tab_to(&mut terminal, 11), 11);
}

#[test]
fn hts_sets_custom_tab_stop() {
    let mut terminal = Terminal::new(20, 1);

    // Set a custom stop at column 3 via HTS.
    terminal.advance(b"\x1b[1;4H"); // move to column index 3
    terminal.advance(b"\x1bH"); // HTS at column 3

    // From column 0, HT now lands on the new stop at 3 (before the default 8).
    assert_eq!(tab_to(&mut terminal, 0), 3);
    // From column 3, HT advances to the default stop at 8.
    assert_eq!(tab_to(&mut terminal, 3), 8);
}

#[test]
fn tbc_clears_current_tab_stop() {
    let mut terminal = Terminal::new(20, 1);

    // Clear the default stop at column 8.
    terminal.advance(b"\x1b[1;9H"); // column index 8
    terminal.advance(b"\x1b[0g"); // TBC current column

    // From column 0, HT now skips the cleared 8 and lands on the next
    // default stop at 16.
    assert_eq!(tab_to(&mut terminal, 0), 16);
}

#[test]
fn tbc_clears_all_tab_stops() {
    let mut terminal = Terminal::new(20, 1);

    terminal.advance(b"\x1b[3g"); // TBC clear all

    // With no stops anywhere, HT from column 0 clamps to the right edge.
    assert_eq!(tab_to(&mut terminal, 0), 19);
}

#[test]
fn ris_restores_default_tab_stops_decstr_preserves() {
    let mut terminal = Terminal::new(20, 2);

    // Wipe all stops, then confirm HT clamps.
    terminal.advance(b"\x1b[3g");
    assert_eq!(tab_to(&mut terminal, 0), 19);

    // DECSTR (soft reset) PRESERVES the (now empty) tab-stop table.
    terminal.advance(b"\x1b[!p");
    assert_eq!(tab_to(&mut terminal, 0), 19);

    // RIS (hard reset) RESTORES the default every-8 stops.
    terminal.advance(b"\x1bc");
    assert_eq!(tab_to(&mut terminal, 0), 8);
}

#[test]
fn resize_preserves_stops_and_default_fills_growth() {
    let mut terminal = Terminal::new(10, 1);

    // Custom stop at column 3; default stop at 8 also present.
    terminal.advance(b"\x1b[1;4H\x1bH");

    // Grow to 24 columns: retained stops (3, 8) preserved; new columns get
    // default stops (16).
    terminal.resize(24, 1);
    assert_eq!(tab_to(&mut terminal, 0), 3); // custom stop retained
    assert_eq!(tab_to(&mut terminal, 3), 8); // default retained
    assert_eq!(tab_to(&mut terminal, 8), 16); // default-filled on growth

    // Shrink to 6 columns: stops beyond width are dropped; the custom 3
    // remains, and HT past it clamps to the new right edge (col 5).
    terminal.resize(6, 1);
    assert_eq!(tab_to(&mut terminal, 0), 3);
    assert_eq!(tab_to(&mut terminal, 3), 5);
}

// --- Resize reflow (shrink/grow content preservation) ---

/// Visible text with trailing blank rows (fixed-height grid padding) removed,
/// so reflow assertions focus on content rather than grid height.
fn visible_text(terminal: &Terminal) -> String {
    terminal
        .screen()
        .plain_text()
        .trim_end_matches('\n')
        .to_string()
}

#[test]
fn reflow_shrink_then_grow_recovers_wide_line() {
    // Operator bug: text that disappears into a narrowed window must
    // reappear when widened again. A 30-char line on a 20-wide grid wraps;
    // shrinking to 10 re-wraps it; widening to 40 must rejoin it intact.
    let mut terminal = Terminal::new(20, 3);
    let line = "abcdefghijklmnopqrstuvwxyz0123"; // 30 chars
    terminal.advance(line.as_bytes());

    // Width 20: soft-wrapped across two rows.
    assert_eq!(visible_text(&terminal), "abcdefghijklmnopqrst\nuvwxyz0123");

    // Shrink to 10: the logical line re-wraps to three full rows.
    terminal.resize(10, 3);
    assert_eq!(
        visible_text(&terminal),
        "abcdefghij\nklmnopqrst\nuvwxyz0123"
    );

    // Grow to 40: the soft-wrapped rows rejoin into the original line.
    terminal.resize(40, 3);
    assert_eq!(visible_text(&terminal), line);
}

#[test]
fn reflow_preserves_content_through_scrollback_roundtrip() {
    // When the reflowed line is taller than the visible window, the overflow
    // goes to scrollback and is still recovered on widening.
    let mut terminal = Terminal::new(20, 2);
    let line = "abcdefghijklmnopqrstuvwxyz0123"; // 30 chars
    terminal.advance(line.as_bytes());

    // Shrink to 10 (3 rows of content, only 2 visible): top row spills into
    // scrollback rather than being truncated.
    terminal.resize(10, 2);
    assert_eq!(terminal.screen().scrollback_len(), 1);
    assert_eq!(visible_text(&terminal), "klmnopqrst\nuvwxyz0123");

    // Grow to 40: scrollback + visible rejoin into the original line.
    terminal.resize(40, 2);
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(visible_text(&terminal), line);
}

#[test]
fn reflow_does_not_join_hard_newlines() {
    // Hard line breaks (explicit newlines) must never be merged by reflow,
    // even when both lines would fit on one row at the new width.
    let mut terminal = Terminal::new(20, 3);
    terminal.advance(b"foo\r\nbar");

    terminal.resize(3, 3);
    assert_eq!(visible_text(&terminal), "foo\nbar");

    terminal.resize(20, 3);
    // Stays two separate lines, not "foobar".
    assert_eq!(visible_text(&terminal), "foo\nbar");
}

#[test]
fn reflow_keeps_cursor_on_its_character() {
    // The cursor must follow its logical character through a re-wrap so an
    // active prompt stays put.
    let mut terminal = Terminal::new(20, 3);
    terminal.advance(b"$ hello"); // cursor at col 7, row 0
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 7 });

    // Shrink to 4: "$ hello" wraps to "$ he" / "llo"; the cursor sits just
    // past the last char on the second wrapped row.
    terminal.resize(4, 3);
    let cursor = terminal.screen().cursor();
    assert_eq!(cursor, Position { row: 1, column: 3 });

    // Typing continues the same logical line from the cursor; widening
    // rejoins it into the expected text.
    terminal.advance(b"!");
    terminal.resize(20, 3);
    assert_eq!(visible_text(&terminal), "$ hello!");
}

#[test]
fn reflow_grow_then_shrink_is_stable_for_short_lines() {
    // Lines that always fit are unaffected by reflow (no spurious joins or
    // blank bloat) across repeated resizes.
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"a\r\nb\r\nc");
    let before = visible_text(&terminal);
    assert_eq!(before, "a\nb\nc");

    terminal.resize(40, 3);
    assert_eq!(visible_text(&terminal), "a\nb\nc");
    terminal.resize(5, 3);
    assert_eq!(visible_text(&terminal), "a\nb\nc");
    terminal.resize(10, 3);
    assert_eq!(visible_text(&terminal), "a\nb\nc");
}

#[test]
fn reflow_does_not_touch_alternate_screen_but_isolates_it() {
    // The alternate screen does not reflow (apps repaint), keeps no
    // scrollback, and primary history never leaks into it. Leaving the
    // alternate screen after a resize shows the reflowed primary content.
    let mut terminal = Terminal::new(20, 3);
    let line = "abcdefghijklmnopqrstuvwxyz0123"; // 30 chars, wraps at 20
    terminal.advance(line.as_bytes());

    // Enter the alternate screen and draw app content.
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(b"TUI");
    assert_eq!(terminal.screen().scrollback_len(), 0);

    // Resize while in the alternate screen: alt grid is truncated/padded
    // (no scrollback growth), and its content is preserved within bounds.
    terminal.resize(10, 3);
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert!(terminal.screen().plain_text().contains("TUI"));

    // Leave the alternate screen: the reflowed primary line is intact at the
    // new width (re-wrapped to 10).
    terminal.advance(b"\x1b[?1049l");
    terminal.resize(40, 3);
    assert_eq!(visible_text(&terminal), line);
}

// Baseline: xterm, Ghostty, and xterm.js all specify that IL (CSI L) and
// DL (CSI M) move the cursor to the left margin (column 0) and unset the
// pending wrap state. These fixtures start the cursor at a NONZERO column
// to prove the column-reset policy (a column-preserving impl would fail
// them). RI (ESC M), by contrast, preserves the column — see
// reverse_index_preserves_cursor_column.
#[test]
fn insert_lines_resets_cursor_to_left_margin() {
    let mut terminal = Terminal::new(8, 4);

    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
    terminal.advance(b"\x1b[2;5H"); // row index 1, column index 4 (nonzero)
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 4 });

    terminal.advance(b"\x1b[L"); // IL 1

    // Cursor homed to the left margin of the current row.
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 0 });
}

#[test]
fn delete_lines_resets_cursor_to_left_margin() {
    let mut terminal = Terminal::new(8, 4);

    terminal.advance(b"r0\r\nr1\r\nr2\r\nr3");
    terminal.advance(b"\x1b[3;6H"); // row index 2, column index 5 (nonzero)
    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 5 });

    terminal.advance(b"\x1b[M"); // DL 1

    assert_eq!(terminal.screen().cursor(), Position { row: 2, column: 0 });
}

#[test]
fn insert_lines_at_right_edge_clears_pending_wrap() {
    let mut terminal = Terminal::new(4, 3);

    // Print to the last column to arm pending_wrap, then IL. The column
    // resets to 0 and pending_wrap is cleared, so the next printable lands
    // at column 1 (not wrapped to a new row).
    terminal.advance(b"abcd"); // fills row 0, cursor parked at col 3, pending_wrap set
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 3 });

    terminal.advance(b"\x1b[L"); // IL at row 0
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });

    terminal.advance(b"Z"); // lands at column 0 then advances to 1, no wrap
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
}

#[test]
fn reverse_index_preserves_cursor_column() {
    let mut terminal = Terminal::new(8, 3);

    // RI is NOT IL/DL: it preserves the cursor column (only the row/scroll
    // changes). Start at a nonzero column below the top margin.
    terminal.advance(b"\x1b[3;6H"); // row index 2, column index 5
    terminal.advance(b"\x1bM"); // RI moves cursor up one row, column intact

    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 5 });
}
