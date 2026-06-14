// SPDX-License-Identifier: GPL-3.0-only
//! OSC 133 semantic prompt marking (SH1).
//!
//! Covers parsing of the A/B/C/D sub-commands into per-row marks, exit-code
//! parsing and its malformed-payload defenses, the poll API + change flag, the
//! coordinate convention (marks anchored to absolute rows), survival through
//! scroll-out into scrollback and through a width-changing reflow, RIS reset
//! semantics, and the load-bearing invariant that the marking sequences are
//! byte-identical to the same text with the OSC 133 escapes stripped (no grid
//! write, no host reply, no Snapshot change).

use super::*;

/// Wrap an OSC 133 payload (the parts after `133`) in a BEL-terminated sequence.
fn osc133(payload: &str) -> Vec<u8> {
    let mut bytes = b"\x1b]133;".to_vec();
    bytes.extend_from_slice(payload.as_bytes());
    bytes.push(0x07);
    bytes
}

#[test]
fn marks_each_subcommand_on_the_cursor_row() {
    let mut terminal = Terminal::new(8, 4);
    assert_eq!(terminal.prompt_mark_at(0), None);
    assert!(!terminal.take_prompt_marks_changed());

    // Row 0: prompt start.
    terminal.advance(&osc133("A"));
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    assert!(terminal.take_prompt_marks_changed());
    assert!(!terminal.take_prompt_marks_changed());

    // Row 1: command/input start (B) also maps to PromptStart.
    terminal.advance(b"\r\n");
    terminal.advance(&osc133("B"));
    assert_eq!(terminal.prompt_mark_at(1), Some(PromptKind::PromptStart));

    // Row 2: output start.
    terminal.advance(b"\r\n");
    terminal.advance(&osc133("C"));
    assert_eq!(terminal.prompt_mark_at(2), Some(PromptKind::OutputStart));

    // Row 3: command finished, exit 0.
    terminal.advance(b"\r\n");
    terminal.advance(&osc133("D;0"));
    assert_eq!(
        terminal.prompt_mark_at(3),
        Some(PromptKind::CommandEnd { exit: Some(0) })
    );
}

#[test]
fn command_end_parses_exit_code() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(&osc133("D;130"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(PromptKind::CommandEnd { exit: Some(130) })
    );
}

#[test]
fn command_end_without_code_is_none_exit() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(&osc133("D"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(PromptKind::CommandEnd { exit: None })
    );
}

#[test]
fn malformed_exit_code_does_not_panic_and_yields_none() {
    let mut terminal = Terminal::new(8, 2);
    // Non-numeric exit payload: the variant still lands, exit is None.
    terminal.advance(&osc133("D;xx"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(PromptKind::CommandEnd { exit: None })
    );
    // Overflowing payload: still no panic, exit None.
    terminal.advance(&osc133("D;99999999999999999999"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(PromptKind::CommandEnd { exit: None })
    );
}

#[test]
fn unknown_subcommand_leaves_existing_mark_untouched() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(&osc133("A"));
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    // An unrecognized sub-command must not clobber the row's mark.
    terminal.advance(&osc133("Z"));
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    // An empty payload likewise.
    terminal.advance(b"\x1b]133;\x07");
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
}

#[test]
fn mark_survives_scroll_out_into_scrollback() {
    let mut terminal = Terminal::new(4, 2);
    // Mark and fill row 0, then scroll it off the top into scrollback.
    terminal.advance(&osc133("A"));
    terminal.advance(b"ab\r\ncd\r\nef\r\ngh");
    // Row 0 ("ab") now lives in scrollback; the absolute-row mark follows it.
    assert!(terminal.screen().scrollback_len() >= 1);
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
}

#[test]
fn mark_carries_through_width_changing_reflow() {
    let mut terminal = Terminal::new(8, 3);
    // Mark row 0 and fill it with content longer than the narrowed width so the
    // logical line re-wraps to multiple physical rows on resize.
    terminal.advance(&osc133("A"));
    terminal.advance(b"ABCDEFGH");
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));

    // Narrow to width 4: "ABCDEFGH" wraps to two physical rows; the mark must
    // ride the FIRST of them and continuation rows must carry none.
    terminal.resize(4, 3);
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    assert_eq!(terminal.prompt_mark_at(1), None);

    // Widen back to 8: the logical line rejoins; the mark rides the single row.
    terminal.resize(8, 3);
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
}

#[test]
fn ris_clears_prompt_marks() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    terminal.advance(b"text\r\n");
    terminal.advance(&osc133("C"));
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));

    // RIS rebuilds the grid as blank rows and clears scrollback, so the
    // row-anchored marks vanish with the rows that carried them. (Unlike the
    // OSC 7 cwd, a prompt mark is positional terminal state, not shell state.)
    terminal.advance(b"\x1bc");
    assert_eq!(terminal.prompt_mark_at(0), None);
    assert_eq!(terminal.prompt_mark_at(1), None);
}

#[test]
fn ed2_clears_visible_marks_via_fresh_rows() {
    // Full erase-display (ED 2) replaces every visible row with a fresh blank
    // row, so a row-anchored mark goes with the row it sat on. This pins the
    // invariant that row-replacement paths produce marks-free blank rows.
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    terminal.advance(b"text");
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    terminal.advance(b"\x1b[2J");
    assert_eq!(terminal.prompt_mark_at(0), None);
}

#[test]
fn alt_screen_neither_leaks_nor_loses_marks() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    terminal.advance(b"primary");
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));

    // Enter the alternate screen (DECSET 1049): its fresh blank rows carry no
    // mark, so the primary mark must not leak into the alt view.
    terminal.advance(b"\x1b[?1049h");
    assert_eq!(terminal.prompt_mark_at(0), None);
    // A mark stamped on the alt screen is local to it.
    terminal.advance(&osc133("C"));
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::OutputStart));

    // Leaving the alternate screen (DECRST 1049) restores the primary rows with
    // their mark intact; the alt-only mark is gone.
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
}

#[test]
fn osc133_emits_no_host_response() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    terminal.advance(&osc133("D;0"));
    // OSC 133 is a report from the shell; the terminal must never reply.
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn osc133_payload_does_not_leak_into_grid() {
    let mut terminal = Terminal::new(40, 3);
    terminal.advance(b"A");
    terminal.advance(&osc133("D;0"));
    terminal.advance(b"B");
    // Only the printed characters reach the grid; the sequence does not.
    assert_eq!(terminal.screen().plain_text().lines().next(), Some("AB"));
}

#[test]
fn osc133_stream_is_byte_identical_to_stripped_text() {
    // The load-bearing invariant: feeding a full A…B…C…D;0 prompt cycle produces
    // a grid byte-identical to feeding the same visible text with every OSC 133
    // escape stripped. Proven through the render Snapshot, which also confirms
    // the marks never reach the rendering surface.
    let mut marked = Terminal::new(20, 4);
    marked.advance(&osc133("A"));
    marked.advance(b"user@host:~$ ");
    marked.advance(&osc133("B"));
    marked.advance(b"echo hi\r\n");
    marked.advance(&osc133("C"));
    marked.advance(b"hi\r\n");
    marked.advance(&osc133("D;0"));

    let mut plain = Terminal::new(20, 4);
    plain.advance(b"user@host:~$ ");
    plain.advance(b"echo hi\r\n");
    plain.advance(b"hi\r\n");

    assert_eq!(marked.snapshot(), plain.snapshot());
    // And the marks really were captured on the marked terminal.
    assert_eq!(marked.prompt_mark_at(0), Some(PromptKind::PromptStart));
}

// --- Concern 1: marks are logical-line-anchored (continuation-row handling) ---

#[test]
fn mark_on_continuation_row_anchors_to_logical_first_row() {
    // Width 4: "ABCDEF" wraps to row 0 ("ABCD", soft) + row 1 ("EF",
    // continuation). The cursor ends on the continuation row, so an OSC 133 here
    // must anchor to the logical line's FIRST physical row (row 0), never the
    // continuation row.
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"ABCDEF");
    terminal.advance(&osc133("A"));
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    assert_eq!(terminal.prompt_mark_at(1), None);
}

#[test]
fn continuation_anchored_mark_survives_scroll_out() {
    // A mark stamped while the cursor sat on a wrapped continuation row must
    // ride the logical line's first physical row all the way into scrollback.
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"ABCDEF"); // row 0 "ABCD" (soft) + row 1 "EF"
    terminal.advance(&osc133("A")); // anchors to row 0
    // Hard-terminate the wrapped line, then push enough rows to scroll it off.
    terminal.advance(b"\r\nG\r\nH\r\nI");
    assert!(terminal.screen().scrollback_len() >= 1);
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
}

#[test]
fn continuation_anchored_mark_survives_width_reflow() {
    // Mark a wrapped logical line, then re-wrap to a different width: the mark
    // stays on the first physical row of the re-wrapped line, never a
    // continuation row.
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"ABCDEF"); // wraps at width 4
    terminal.advance(&osc133("A"));
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));

    // Widen to 8: "ABCDEF" fits on one row; mark rides it.
    terminal.resize(8, 3);
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));

    // Narrow to 3: "ABCDEF" wraps to rows 0/1; mark on the first only.
    terminal.resize(3, 3);
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    assert_eq!(terminal.prompt_mark_at(1), None);
}

#[test]
fn mark_on_offscreen_logical_line_is_adopted_on_scroll_out() {
    // When the logical line's true first row has already scrolled into
    // scrollback, the walk-back stops at the top of the live grid, so the mark
    // lands on a still-visible continuation row. The carry path must adopt it
    // onto the open scrollback logical line rather than drop it.
    let mut terminal = Terminal::new(4, 2);
    // 10 chars at width 4: the logical line spans scrollback + both live rows.
    terminal.advance(b"ABCDEFGHIJ");
    terminal.advance(&osc133("C")); // stamped on a visible continuation row
    // Scroll the remainder of the logical line off the bottom.
    terminal.advance(b"\r\nZ\r\nY");
    // The mark was adopted onto the logical line and rides its first row.
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::OutputStart));
}

// --- Concern 2: take_prompt_marks_changed signals clears/repositions too ---

#[test]
fn ris_sets_change_flag_when_marks_existed() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    assert!(terminal.take_prompt_marks_changed()); // the stamp itself
    // RIS clears the mark — a poller that already cleared the flag must still
    // learn the marks changed.
    terminal.advance(b"\x1bc");
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn ris_without_marks_does_not_set_change_flag() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"\x1bc");
    assert!(!terminal.take_prompt_marks_changed());
}

#[test]
fn ed2_sets_change_flag_when_marks_existed() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    assert!(terminal.take_prompt_marks_changed());
    terminal.advance(b"\x1b[2J"); // full erase replaces the marked row
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn ed2_without_marks_does_not_set_change_flag() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"text");
    terminal.advance(b"\x1b[2J");
    assert!(!terminal.take_prompt_marks_changed());
}

#[test]
fn resize_sets_change_flag_when_marks_existed() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    terminal.advance(b"text");
    assert!(terminal.take_prompt_marks_changed());
    // A resize re-anchors marks; the poll API must report the possible move.
    terminal.resize(4, 3);
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn resize_without_marks_does_not_set_change_flag() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"text");
    terminal.resize(4, 3);
    assert!(!terminal.take_prompt_marks_changed());
}

#[test]
fn alt_screen_enter_leave_sets_change_flag_when_primary_had_marks() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    assert!(terminal.take_prompt_marks_changed()); // the stamp itself

    // Entering swaps the marked primary out for a blank alt buffer:
    // prompt_mark_at(0) changes None, so a poller that cleared the flag must
    // still learn marks changed.
    terminal.advance(b"\x1b[?1049h");
    assert_eq!(terminal.prompt_mark_at(0), None);
    assert!(terminal.take_prompt_marks_changed());

    // Leaving restores the marked primary: the mark reappears, flag set again.
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.prompt_mark_at(0), Some(PromptKind::PromptStart));
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn alt_screen_switch_without_marks_does_not_set_change_flag() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"text");
    terminal.advance(b"\x1b[?1049h");
    assert!(!terminal.take_prompt_marks_changed());
    terminal.advance(b"\x1b[?1049l");
    assert!(!terminal.take_prompt_marks_changed());
}

#[test]
fn resize_while_alt_active_flags_stored_primary_marks() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    terminal.advance(b"text");
    terminal.advance(b"\x1b[?1049h"); // enter alt (flag set + consumed below)
    assert!(terminal.take_prompt_marks_changed());

    // Resize while the alt screen is active re-anchors the STORED primary's
    // marks; the poll API must report it even though the active alt has none.
    terminal.resize(4, 3);
    assert!(terminal.take_prompt_marks_changed());
}
