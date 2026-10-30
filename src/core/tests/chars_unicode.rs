// SPDX-License-Identifier: GPL-3.0-only
//! ICH, DCH, ECH and wide-cell editing behavior.

use super::*;

// ICH (CSI Ps @) / DCH (CSI Ps P): row-local insert/delete of cells. Baseline
// verified against xterm/Ghostty - cursor stays put, no wrap/scroll, shifted
// cells keep their attrs, fill blanks use the current background color and
// otherwise default attributes.
#[test]
fn insert_chars_shifts_right_and_keeps_cursor() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance(b"abcdef");
    terminal.advance(b"\x1b[1;3H"); // cursor at column index 2 (the 'c')
    terminal.advance(b"\x1b[2@"); // ICH 2

    // "ab" + 2 blanks + "cd"; "ef" pushed off the right edge are discarded.
    assert_eq!(terminal.screen().plain_text(), "ab  cd");
    // Cursor unchanged.
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
}

#[test]
fn delete_chars_shifts_left_and_keeps_cursor() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance(b"abcdef");
    terminal.advance(b"\x1b[1;2H"); // cursor at column index 1 (the 'b')
    terminal.advance(b"\x1b[2P"); // DCH 2

    // "a" + "def" shifted left + 2 blanks at the right edge.
    assert_eq!(terminal.screen().plain_text(), "adef");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
}

#[test]
fn insert_and_delete_chars_default_and_zero_count_is_one() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance(b"abcdef");
    terminal.advance(b"\x1b[1;1H");
    terminal.advance(b"\x1b[@"); // ICH, omitted count -> 1
    assert_eq!(terminal.screen().plain_text(), " abcde");

    terminal.advance(b"\x1b[1;1H");
    terminal.advance(b"\x1b[0P"); // DCH, zero count -> 1
    assert_eq!(terminal.screen().plain_text(), "abcde");
}

#[test]
fn insert_chars_count_clamps_to_remaining_columns() {
    let mut terminal = Terminal::new(5, 1);

    terminal.advance(b"abcde");
    terminal.advance(b"\x1b[1;3H"); // column index 2
    terminal.advance(b"\x1b[99@"); // ICH count far exceeds remaining 3 columns

    // Everything from the cursor is blanked; "ab" preserved.
    assert_eq!(terminal.screen().plain_text(), "ab");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
}

#[test]
fn insert_and_delete_chars_fill_with_current_background() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance(b"abcdef\x1b[42m\x1b[1;3H\x1b[2@");
    assert_eq!(terminal.screen().plain_text(), "ab  cd");
    assert_blank_with_background(&terminal, 0, 2, Color::Indexed(2));
    assert_blank_with_background(&terminal, 0, 3, Color::Indexed(2));

    terminal.advance(b"\x1b[43m\x1b[1;2H\x1b[2P");
    assert_blank_with_background(&terminal, 0, 4, Color::Indexed(3));
    assert_blank_with_background(&terminal, 0, 5, Color::Indexed(3));
}

#[test]
fn delete_chars_preserves_attrs_of_shifted_cells() {
    let mut terminal = Terminal::new(6, 1);

    // 'a' plain, then bold-red "XY", then plain 'z'.
    terminal.advance(b"a\x1b[1;31mXY\x1b[0mz");
    terminal.advance(b"\x1b[1;1H"); // cursor at 'a'
    terminal.advance(b"\x1b[1P"); // DCH 1 -> delete 'a', shift left

    assert_eq!(terminal.screen().plain_text(), "XYz");
    // Shifted X/Y keep their bold-red attrs.
    let x = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(x.ch, 'X');
    assert!(x.attrs.bold());
    assert_eq!(x.attrs.foreground, Color::Indexed(1));
}

#[test]
fn delete_chars_cleans_up_orphaned_wide_continuation() {
    let mut terminal = Terminal::new(6, 1);

    // Wide glyph occupies cols 0-1 (lead + continuation), then "ab".
    terminal.advance("世ab".as_bytes());
    // Sanity: continuation spacer present at col 1.
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);

    terminal.advance(b"\x1b[1;1H"); // cursor at the wide lead
    terminal.advance(b"\x1b[1P"); // DCH 1 -> remove the lead

    // DCH removes ONE cell (the wide lead). Its continuation spacer shifts
    // into col 0 and is cleaned to a blank in place - so a single leading
    // blank remains, then "ab". The orphaned continuation must NOT survive
    // as a dangling spacer. plain_text only trims trailing space, so the
    // leading blank is retained.
    let plain = terminal.screen().plain_text();
    assert_eq!(plain, " ab");
    // No cell still flagged as a wide continuation.
    assert!(
        (0..6).all(|c| !terminal.screen().cell(0, c).unwrap().wide_continuation),
        "no orphaned wide-continuation cells should remain"
    );
}

// ECH (CSI Ps X): row-local erase-in-place. Unlike DCH it does NOT shift the
// line - it overwrites count cells with BCE blanks. Cursor stays put,
// pending_wrap clears, count clamps to the row tail.
#[test]
fn erase_chars_blanks_in_place_without_shifting() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance(b"abcdef");
    terminal.advance(b"\x1b[1;2H"); // cursor at column index 1 (the 'b')
    terminal.advance(b"\x1b[2X"); // ECH 2 -> erase 'b','c' in place

    // No shift: "a" + 2 blanks + "def".
    assert_eq!(terminal.screen().plain_text(), "a  def");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
}

#[test]
fn erase_chars_default_and_zero_count_is_one() {
    let mut terminal = Terminal::new(6, 1);

    terminal.advance(b"abcdef");
    terminal.advance(b"\x1b[1;1H");
    terminal.advance(b"\x1b[X"); // omitted count -> 1
    assert_eq!(terminal.screen().plain_text(), " bcdef");

    terminal.advance(b"\x1b[1;3H");
    terminal.advance(b"\x1b[0X"); // zero count -> 1
    assert_eq!(terminal.screen().plain_text(), " b def");
}

#[test]
fn erase_chars_count_clamps_to_row_tail() {
    let mut terminal = Terminal::new(5, 1);

    terminal.advance(b"abcde");
    terminal.advance(b"\x1b[1;3H"); // column index 2
    terminal.advance(b"\x1b[99X"); // far exceeds remaining 3 columns

    // Erases from cursor to end of row; "ab" preserved, cursor unchanged.
    assert_eq!(terminal.screen().plain_text(), "ab");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
}

#[test]
fn erase_chars_is_row_local_and_resets_attrs() {
    let mut terminal = Terminal::new(6, 2);

    // Row 0: plain 'a' then bold-red "bc". Row 1: "xyz" (must stay intact).
    terminal.advance(b"a\x1b[1;31mbc\x1b[0m\r\nxyz");
    terminal.advance(b"\x1b[1;2H"); // back to row 0, column index 1 ('b')
    terminal.advance(b"\x1b[2X"); // erase the bold-red 'b','c'

    // Row 1 untouched (row-local).
    assert_eq!(terminal.screen().plain_text(), "a\nxyz");
    // Erased cells carry DEFAULT attrs, not the prior bold-red.
    let erased = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(erased.ch, ' ');
    assert_eq!(erased.attrs, Attrs::default());
}

#[test]
fn erase_chars_clears_pending_wrap() {
    let mut terminal = Terminal::new(4, 2);

    // Fill the row to arm pending_wrap at the right edge.
    terminal.advance(b"abcd");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 3 });

    terminal.advance(b"\x1b[1X"); // ECH clears pending_wrap

    // Because pending_wrap was cleared, the next printable overwrites the
    // last column on THIS row instead of wrapping to row 1. Z lands at
    // column 3 (the cursor re-caps at columns-1 and re-arms pending_wrap);
    // crucially it stays on row 0.
    terminal.advance(b"Z");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 3 });
    // Row 1 is empty (Z did not wrap there); plain_text joins both rows.
    assert_eq!(terminal.screen().plain_text(), "abcZ\n");
}

#[test]
fn erase_chars_cleans_up_orphaned_wide_continuation() {
    let mut terminal = Terminal::new(6, 1);

    // Wide glyph at cols 0-1 (lead + continuation), then "ab".
    terminal.advance("世ab".as_bytes());
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);

    terminal.advance(b"\x1b[1;1H"); // cursor at the wide lead
    terminal.advance(b"\x1b[1X"); // ECH 1 -> erase the lead in place

    // Erasing only the lead orphans the continuation spacer at col 1; it
    // must be cleaned to a blank, not left dangling. "ab" stays in place.
    assert_eq!(terminal.screen().plain_text(), "  ab");
    assert!(
        (0..6).all(|c| !terminal.screen().cell(0, c).unwrap().wide_continuation),
        "no orphaned wide-continuation cells should remain"
    );
}

// --- Wide-cell write/erase coherence (C2) ---

#[test]
fn overwrite_wide_lead_with_narrow_clears_continuation() {
    let mut terminal = Terminal::new(6, 1);
    terminal.advance("世ab".as_bytes());
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);

    // Overwrite the wide lead (col 0) with a narrow char.
    terminal.advance(b"\x1b[1;1HX");

    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, 'X');
    // The orphaned continuation spacer at col 1 must be cleared.
    assert!(!terminal.screen().cell(0, 1).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, ' ');
    assert_eq!(terminal.screen().plain_text(), "X ab");
}

#[test]
fn overwrite_wide_continuation_with_narrow_clears_lead() {
    let mut terminal = Terminal::new(6, 1);
    terminal.advance("世ab".as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, '世');

    // Overwrite the continuation half (col 1) with a narrow char.
    terminal.advance(b"\x1b[1;2HX");

    // The orphaned wide lead at col 0 must be cleared to a blank.
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, ' ');
    assert!(!terminal.screen().cell(0, 0).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, 'X');
    assert_eq!(terminal.screen().plain_text(), " Xab");
    assert!(
        (0..6).all(|c| !terminal.screen().cell(0, c).unwrap().wide_continuation),
        "no orphaned wide-continuation cells should remain"
    );
}

#[test]
fn new_wide_over_two_wide_pairs_clears_far_orphan() {
    let mut terminal = Terminal::new(6, 1);
    // 世 at 0-1, 界 at 2-3, 'a' at 4.
    terminal.advance("世界a".as_bytes());

    // Write a fullwidth 'Ａ' (width 2) starting at col 1 (continuation of 世).
    terminal.advance("\x1b[1;2HＡ".as_bytes());

    // Left orphan: 世's lead at col 0 cleared. Far orphan: 界's continuation
    // at col 3 cleared. New pair sits at cols 1-2.
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, ' ');
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, 'Ａ');
    assert!(terminal.screen().cell(0, 2).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(0, 3).unwrap().ch, ' ');
    assert!(!terminal.screen().cell(0, 3).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(0, 4).unwrap().ch, 'a');
}

#[test]
fn wide_char_at_line_end_wraps_without_splitting() {
    let mut terminal = Terminal::new(3, 2);
    // Fill cols 0,1 with narrow chars; cursor lands at col 2 (last column).
    terminal.advance(b"ab");
    // A wide glyph cannot fit in the single remaining column: it must wrap.
    terminal.advance("世".as_bytes());

    // Row 0 keeps "ab"; the trailing cell stays blank (no half-wide split).
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, 'a');
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, 'b');
    assert_eq!(terminal.screen().cell(0, 2).unwrap().ch, ' ');
    assert!(!terminal.screen().cell(0, 2).unwrap().wide_continuation);
    // The wide glyph lands whole on row 1.
    assert_eq!(terminal.screen().cell(1, 0).unwrap().ch, '世');
    assert!(terminal.screen().cell(1, 1).unwrap().wide_continuation);
}

#[test]
fn erase_line_from_cursor_clears_orphaned_wide_lead() {
    let mut terminal = Terminal::new(6, 1);
    terminal.advance("ab世".as_bytes()); // 世 at cols 2-3
    assert!(terminal.screen().cell(0, 3).unwrap().wide_continuation);

    // Cursor onto the continuation half (col 3); erase from there to EOL.
    terminal.advance(b"\x1b[1;4H\x1b[0K");

    // The wide lead at col 2 is orphaned by the erase and must be cleared.
    assert_eq!(terminal.screen().cell(0, 2).unwrap().ch, ' ');
    assert!(
        (0..6).all(|c| !terminal.screen().cell(0, c).unwrap().wide_continuation),
        "no orphaned wide-continuation cells should remain"
    );
    assert_eq!(terminal.screen().plain_text(), "ab");
}

#[test]
fn erase_line_to_cursor_clears_orphaned_wide_continuation() {
    let mut terminal = Terminal::new(6, 1);
    terminal.advance("世ab".as_bytes()); // 世 at cols 0-1
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);

    // Cursor onto the wide lead (col 0); erase from start to cursor.
    terminal.advance(b"\x1b[1;1H\x1b[1K");

    // The continuation at col 1 is orphaned by the erase and must be cleared.
    assert!(!terminal.screen().cell(0, 1).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, ' ');
    assert_eq!(terminal.screen().plain_text(), "  ab");
}

#[test]
fn wide_coherence_holds_on_alternate_screen() {
    let mut terminal = Terminal::new(6, 1);
    terminal.advance(b"\x1b[?1049h"); // enter alternate screen
    terminal.advance("世ab".as_bytes());
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);

    // Overwrite-half coherence works identically on the alternate screen.
    terminal.advance(b"\x1b[1;1HX");
    assert!(!terminal.screen().cell(0, 1).unwrap().wide_continuation);

    // Alternate screen never feeds scrollback.
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

// --- Combining marks (C2b) ---

#[test]
fn combining_mark_attaches_to_preceding_cell() {
    let mut terminal = Terminal::new(6, 1);
    // 'e' then COMBINING ACUTE ACCENT (U+0301), zero width.
    terminal.advance("e\u{0301}".as_bytes());

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'e');
    assert_eq!(cell.combining(), &['\u{0301}']);
    assert_eq!(cell.grapheme(), "e\u{0301}");
    // The mark does not consume a column; the cursor advanced only by 'e'.
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
    assert_eq!(terminal.screen().plain_text(), "e\u{0301}");
}

#[test]
fn combining_mark_attaches_to_wide_lead_not_spacer() {
    let mut terminal = Terminal::new(6, 1);
    // Wide '世' (cols 0-1) then a combining mark: it must attach to the lead
    // at col 0, stepping back over the continuation spacer at col 1.
    terminal.advance("世\u{0301}".as_bytes());

    assert_eq!(
        terminal.screen().cell(0, 0).unwrap().combining(),
        &['\u{0301}']
    );
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);
    assert!(terminal.screen().cell(0, 1).unwrap().combining().is_empty());
}

#[test]
fn combining_mark_at_line_start_has_its_own_source_owner() {
    let mut terminal = Terminal::new(6, 1);
    // A leading mark survives without inventing a space in the source.
    terminal.advance("\u{0301}".as_bytes());

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, '\u{0301}');
    assert!(cell.combining().is_empty());
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
}

#[test]
fn combining_marks_preserve_bounded_spill_without_panicking() {
    let mut terminal = Terminal::new(6, 1);
    // Three retained marks survive text and snapshot-cell conversion.
    terminal.advance("e\u{0301}\u{0302}\u{0303}".as_bytes());

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'e');
    assert_eq!(cell.combining(), &['\u{0301}', '\u{0302}', '\u{0303}']);
    assert_eq!(cell.grapheme(), "e\u{0301}\u{0302}\u{0303}");
    assert_eq!(terminal.screen().plain_text(), "e\u{0301}\u{0302}\u{0303}");
    assert_eq!(
        terminal
            .search(
                "e\u{0301}\u{0302}\u{0303}",
                crate::core::SearchOptions::case_sensitive()
            )
            .len(),
        1
    );

    let copied = cell;
    assert_eq!(copied.grapheme(), "e\u{0301}\u{0302}\u{0303}");
    let restored = crate::core::SnapshotCell::from(copied).to_cell();
    assert_eq!(restored.grapheme(), "e\u{0301}\u{0302}\u{0303}");
}

#[test]
fn overwriting_a_cell_clears_its_combining_marks() {
    let mut terminal = Terminal::new(6, 1);
    terminal.advance("e\u{0301}".as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().combining().len(), 1);

    // Overwrite col 0 with a fresh char: combining state must reset.
    terminal.advance(b"\x1b[1;1Hx");
    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'x');
    assert!(cell.combining().is_empty());
}

#[test]
fn combining_mark_in_pending_wrap_attaches_to_last_column() {
    let mut terminal = Terminal::new(3, 2);
    // Fill the row so the last char sets pending-wrap; a following combining
    // mark must attach to that last-column cell, not wrap to a new row.
    terminal.advance("abc".as_bytes());
    terminal.advance("\u{0301}".as_bytes());

    assert_eq!(terminal.screen().cell(0, 2).unwrap().ch, 'c');
    assert_eq!(
        terminal.screen().cell(0, 2).unwrap().combining(),
        &['\u{0301}']
    );
    // No premature wrap onto row 1.
    assert_eq!(terminal.screen().cell(1, 0).unwrap().ch, ' ');
}

#[test]
fn del_in_ground_writes_no_cell_and_does_not_move_the_cursor() {
    // xterm and the VT100 ignore DEL (0x7F) in Ground: it is not padding that
    // occupies a column. A stray DEL must neither write a cell nor advance
    // the cursor, and the text around it stays contiguous.
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"a\x7fb");
    let row: String = (0..4)
        .map(|col| terminal.screen().cell(0, col).unwrap().ch)
        .collect();
    assert_eq!(row, "ab  ");
    assert_eq!(terminal.screen().cursor().column, 2);

    // A DEL alone, at column 0, and one in pending wrap, change nothing.
    let mut terminal = Terminal::new(3, 2);
    terminal.advance(b"\x7f");
    assert_eq!(terminal.screen().cursor().column, 0);
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, ' ');
    terminal.advance(b"abc\x7f");
    assert_eq!(
        terminal.screen().cursor().row,
        0,
        "DEL must not trigger the pending wrap"
    );
    terminal.advance(b"d");
    assert_eq!(terminal.screen().cell(1, 0).unwrap().ch, 'd');
}

#[test]
fn an_unchanged_ambiguous_width_keeps_the_open_owner() {
    // Session initialization and the settings sweep repeat the width already
    // in force between reads. That changes nothing, so a mark in the next
    // read still joins its base, under either policy.
    for wide in [false, true] {
        let mut terminal = Terminal::new(8, 2);
        terminal.set_ambiguous_wide(wide);
        terminal.advance(b"e");
        terminal.set_ambiguous_wide(wide);
        terminal.advance("\u{0301}".as_bytes());
        let cell = terminal.screen().cell(0, 0).unwrap();
        assert_eq!(cell.combining(), &['\u{0301}'], "wide={wide}");
        assert_eq!(terminal.screen().cursor().column, 1, "wide={wide}");
    }
}

#[test]
fn a_changed_ambiguous_width_still_ends_the_open_owner() {
    // A real policy change reflows the grid and, like a resize, ends the
    // open owner: a following mark starts its own cell.
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"e");
    terminal.set_ambiguous_wide(true);
    terminal.advance("\u{0301}".as_bytes());
    assert!(terminal.screen().cell(0, 0).unwrap().combining().is_empty());
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, '\u{0301}');
    assert_eq!(terminal.screen().cursor().column, 2);
}

#[test]
fn a_combining_mark_after_del_joins_the_base_before_it() {
    // xterm drops DEL entirely, so it does not end the cluster: the mark joins
    // the "e" instead of starting a cell of its own.
    let mut terminal = Terminal::new(8, 2);
    terminal.advance("e\x7f\u{0301}".as_bytes());
    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'e');
    assert_eq!(cell.combining(), &['\u{0301}']);
    assert_eq!(terminal.screen().cursor().column, 1);
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, ' ');
}

#[test]
fn a_real_control_still_ends_the_cluster_before_a_combining_mark() {
    // Control for the DEL case: BEL is a real control, so it keeps ending the
    // cluster and the mark gets a cell of its own.
    let mut terminal = Terminal::new(8, 2);
    terminal.advance("e\x07\u{0301}".as_bytes());
    assert!(terminal.screen().cell(0, 0).unwrap().combining().is_empty());
}

#[test]
fn del_does_not_become_the_repeat_character() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"a\x7f\x1b[2b");
    let row: String = (0..4)
        .map(|col| terminal.screen().cell(0, col).unwrap().ch)
        .collect();
    assert_eq!(row, "aaa ");
}

#[test]
fn khun_and_malayalam_three_cell_owner_copy_wrap_and_reflow() {
    use crate::core::text_owner_spans;
    use crate::selection::{CellPoint, SelectionRange, selected_text};

    for (text, leading, trailing) in [
        (
            "\u{1a63}\u{1a60}\u{1a32}\u{1a71}",
            "\u{1a63}\u{1a60}",
            "\u{1a32}\u{1a71}",
        ),
        (
            "\u{0d7b}\u{0d4d}\u{0d31}\u{0d46}",
            "\u{0d7b}\u{0d4d}",
            "\u{0d31}\u{0d46}",
        ),
    ] {
        for ambiguous in [false, true] {
            let owners = text_owner_spans(text, ambiguous);
            assert_eq!(owners.len(), 2, "{text:?}");
            assert_eq!(owners.iter().map(|o| o.width).collect::<Vec<_>>(), [1, 2]);
            assert_eq!(
                owners.iter().map(|o| o.chars.clone()).collect::<Vec<_>>(),
                [0..2, 2..4]
            );
            assert_eq!(owners[0].cell.grapheme(), leading);
            assert_eq!(owners[1].cell.grapheme(), trailing);

            let mut terminal = Terminal::new(8, 3);
            terminal.set_ambiguous_wide(ambiguous);
            terminal.advance(text.as_bytes());
            assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 3 });
            assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), leading);
            assert_eq!(terminal.screen().cell(0, 1).unwrap().grapheme(), trailing);
            assert!(terminal.screen().cell(0, 2).unwrap().wide_continuation);
            let copied = selected_text(
                &terminal.snapshot(),
                SelectionRange {
                    start: CellPoint { row: 0, column: 0 },
                    end: CellPoint { row: 0, column: 2 },
                },
            );
            assert_eq!(copied, text);
            terminal.resize(3, 3);
            assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), leading);
            assert_eq!(terminal.screen().cell(0, 1).unwrap().grapheme(), trailing);
            assert!(terminal.screen().cell(0, 2).unwrap().wide_continuation);
            terminal.resize(8, 3);
            assert_eq!(
                selected_text(
                    &terminal.snapshot(),
                    SelectionRange {
                        start: CellPoint { row: 0, column: 0 },
                        end: CellPoint { row: 0, column: 2 },
                    }
                ),
                text
            );

            let mut wrapped = Terminal::new(3, 3);
            wrapped.set_ambiguous_wide(ambiguous);
            wrapped.advance(text.as_bytes());
            wrapped.advance(b"X");
            assert_eq!(wrapped.screen().cell(1, 0).unwrap().ch, 'X');
            assert_eq!(wrapped.screen().cursor(), Position { row: 1, column: 1 });
            wrapped.resize(8, 3);
            assert_eq!(wrapped.screen().cell(0, 0).unwrap().grapheme(), leading);
            assert_eq!(wrapped.screen().cell(0, 1).unwrap().grapheme(), trailing);
            assert!(wrapped.screen().cell(0, 2).unwrap().wide_continuation);
            assert_eq!(wrapped.screen().cell(0, 3).unwrap().ch, 'X');
            assert_eq!(
                selected_text(
                    &wrapped.snapshot(),
                    SelectionRange {
                        start: CellPoint { row: 0, column: 0 },
                        end: CellPoint { row: 0, column: 3 },
                    }
                ),
                format!("{text}X")
            );
        }
    }
}
