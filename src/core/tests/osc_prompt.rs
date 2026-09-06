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

fn stamped(kind: PromptKind, logical_offset: u32) -> PromptKind {
    match kind {
        PromptKind::CommandEnd { exit } => PromptKind::CommandEndAt {
            exit,
            logical_offset,
        },
        PromptKind::PromptStartAfterEnd { prev_exit } => PromptKind::PromptStartAfterEndAt {
            prev_exit,
            end_logical_offset: logical_offset,
        },
        kind => kind,
    }
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
        Some(stamped(PromptKind::CommandEnd { exit: Some(0) }, 0))
    );
}

#[test]
fn command_end_parses_exit_code() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(&osc133("D;130"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(stamped(PromptKind::CommandEnd { exit: Some(130) }, 0))
    );
}

#[test]
fn command_end_without_code_is_none_exit() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(&osc133("D"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(stamped(PromptKind::CommandEnd { exit: None }, 0))
    );
}

#[test]
fn malformed_exit_code_does_not_panic_and_yields_none() {
    let mut terminal = Terminal::new(8, 2);
    // Non-numeric exit payload: the variant still lands, exit is None.
    terminal.advance(&osc133("D;xx"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(stamped(PromptKind::CommandEnd { exit: None }, 0))
    );
    // Overflowing payload: still no panic, exit None.
    terminal.advance(&osc133("D;99999999999999999999"));
    assert_eq!(
        terminal.prompt_mark_at(0),
        Some(stamped(PromptKind::CommandEnd { exit: None }, 0))
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
    marked.advance(b"prompt$ ");
    marked.advance(&osc133("B"));
    marked.advance(b"echo hi\r\n");
    marked.advance(&osc133("C"));
    marked.advance(b"hi\r\n");
    marked.advance(&osc133("D;0"));

    let mut plain = Terminal::new(20, 4);
    plain.advance(b"prompt$ ");
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
fn command_end_offset_reflows_with_unterminated_output() {
    let mut terminal = Terminal::new(6, 8);
    terminal.advance(&osc133("A"));
    terminal.advance(b"$ printf demo\r\n");
    terminal.advance(&osc133("C"));
    terminal.advance(b"alpha\r\nabcdefghij");
    terminal.advance(&osc133("D;0"));
    terminal.advance(&osc133("A"));
    terminal.advance(b"$ ");

    let last_row = terminal.screen().scrollback_len().saturating_add(7);
    let marks = terminal.prompt_marks();
    let before = verified_command_ranges(&marks, 6, last_row);
    assert!(!before.is_empty(), "missing range from {marks:?}");
    assert_eq!(before[0].output_end_column, Some(3));

    terminal.resize(4, 8);
    let last_row = terminal.screen().scrollback_len().saturating_add(7);
    let marks = terminal.prompt_marks();
    let after = verified_command_ranges(&marks, 4, last_row);
    assert!(!after.is_empty(), "missing reflowed range from {marks:?}");
    assert_eq!(after[0].output_end_column, Some(1));
    assert_eq!(after[0].exit, Some(0));
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

/// The consumer gate for prompt-aware select+Delete
/// (`editable_input_selection_for_context_menu`) requires the cached input-start
/// absolute row to equal the LIVE `scrollback_len + cursor.row`. A width-change
/// resize rewraps scrollback to a different `physical_len`, so the cached row
/// must be re-anchored through the resize or the gate silently fails (the
/// after-a-side-by-side-split select+Delete regression). These pin that
/// re-anchoring, the width-unchanged fast path, and the no-mark case.
#[test]
fn input_start_is_reanchored_through_a_width_change_resize() {
    let mut terminal = Terminal::new(20, 3);
    // Push enough history that scrollback has physical rows that will rewrap on
    // a width shrink (so physical_len actually changes).
    for _ in 0..6 {
        terminal.advance(b"abcdefghijklmnop\r\n");
    }
    // A fresh prompt with an input mark on the live (single) input row.
    terminal.advance(&osc133("A"));
    terminal.advance(b"> ");
    terminal.advance(&osc133("B"));

    // Precondition: the gate would pass — input_row == scrollback_len + cursor.row.
    let (input_row, _col) = terminal
        .active_prompt_input_start()
        .expect("input mark set after B");
    let cursor_row = terminal.screen().scrollback_len() + terminal.screen().cursor().row;
    assert_eq!(
        input_row, cursor_row,
        "precondition: input anchor equals the live cursor row before resize"
    );

    // Shrink the width: scrollback rewraps (16-col lines wrap at width 10), so
    // physical_len changes and the cached absolute row goes stale.
    terminal.resize(10, 3);

    let (new_input_row, _) = terminal
        .active_prompt_input_start()
        .expect("input mark survives the resize");
    let new_cursor_row = terminal.screen().scrollback_len() + terminal.screen().cursor().row;
    assert_eq!(
        new_input_row, new_cursor_row,
        "input anchor must be re-anchored to the live cursor row after a width change"
    );
}

#[test]
fn input_start_survives_a_height_only_resize_unchanged() {
    // Guard (width-unchanged fast path): a height-only resize does not rewrap
    // scrollback, so the anchor stays valid and the gate keeps passing.
    let mut terminal = Terminal::new(20, 4);
    for _ in 0..3 {
        terminal.advance(b"some output line\r\n");
    }
    terminal.advance(&osc133("A"));
    terminal.advance(b"> ");
    terminal.advance(&osc133("B"));

    terminal.resize(20, 6); // same width, taller

    let (input_row, _) = terminal
        .active_prompt_input_start()
        .expect("input mark survives a height-only resize");
    let cursor_row = terminal.screen().scrollback_len() + terminal.screen().cursor().row;
    assert_eq!(
        input_row, cursor_row,
        "width-unchanged resize keeps the anchor valid"
    );
}

#[test]
fn resize_leaves_no_input_anchor_when_none_was_set() {
    // Guard (no-mark): with no active input mark (B never sent), a width-change
    // resize must not fabricate an anchor.
    let mut terminal = Terminal::new(20, 3);
    for _ in 0..4 {
        terminal.advance(b"abcdefghijklmnop\r\n");
    }
    terminal.advance(&osc133("A")); // prompt start only, no B
    assert_eq!(terminal.active_prompt_input_start(), None);
    terminal.resize(10, 3);
    assert_eq!(
        terminal.active_prompt_input_start(),
        None,
        "a width change must not invent an input anchor when none was set"
    );
}

#[test]
fn input_anchor_fails_closed_across_scrollback_eviction() {
    // A scrollback front-eviction between the `B` stamp and a read shifts every
    // absolute-row address. Without an eviction-immune anchor the cached row
    // resolves to different, more recent content, and synthesized cursor keys
    // would aim at the wrong row. The anchor witnesses the scrollback trim epoch
    // and fails closed (returns None) once eviction bumps it.
    let mut terminal = Terminal::new(20, 4);
    // Tiny scrollback so a trim is cheap to trigger.
    terminal.set_scrollback_limit(8);
    for _ in 0..4 {
        terminal.advance(b"history line here\r\n");
    }
    terminal.advance(&osc133("A"));
    terminal.advance(b"> ");
    terminal.advance(&osc133("B"));
    assert!(
        terminal.active_prompt_input_start().is_some(),
        "precondition: the input anchor is set after B"
    );

    // A burst of output overflows the scrollback cap and evicts the front,
    // WITHOUT a resize (which would otherwise re-anchor the mark).
    for _ in 0..40 {
        terminal.advance(b"more output line\r\n");
    }

    assert_eq!(
        terminal.active_prompt_input_start(),
        None,
        "the input anchor must fail closed once scrollback is evicted"
    );
    assert!(
        terminal.screen().input_region().is_none(),
        "the derived input region must also fail closed after eviction"
    );
}

#[test]
fn input_anchor_cleared_after_output_start_then_resize() {
    // Guard: after C (OutputStart) the input anchor is None and a later width
    // change must keep it None (the recompute only re-anchors a live input mark).
    let mut terminal = Terminal::new(20, 3);
    for _ in 0..4 {
        terminal.advance(b"abcdefghijklmnop\r\n");
    }
    terminal.advance(&osc133("A"));
    terminal.advance(b"> ");
    terminal.advance(&osc133("B"));
    terminal.advance(&osc133("C")); // command runs → input anchor cleared
    assert_eq!(terminal.active_prompt_input_start(), None);
    terminal.resize(10, 3);
    assert_eq!(terminal.active_prompt_input_start(), None);
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

// --- SH2 core: prompt-mark enumeration accessor ---

#[test]
fn prompt_marks_empty_buffer_is_empty() {
    let terminal = Terminal::new(8, 3);
    assert!(terminal.prompt_marks().is_empty());
}

#[test]
fn prompt_marks_enumerates_in_ascending_row_order() {
    let mut terminal = Terminal::new(8, 5);
    terminal.advance(&osc133("A")); // row 0
    terminal.advance(b"prompt\r\n");
    terminal.advance(&osc133("C")); // row 1
    terminal.advance(b"out\r\n");
    terminal.advance(&osc133("D;0")); // row 2

    let marks = terminal.prompt_marks();
    assert_eq!(
        marks,
        vec![
            (0, PromptKind::PromptStart),
            (1, PromptKind::OutputStart),
            (2, stamped(PromptKind::CommandEnd { exit: Some(0) }, 0)),
        ]
    );
    // The enumeration agrees with the point query at every marked row.
    for &(row, kind) in &marks {
        assert_eq!(terminal.prompt_mark_at(row), Some(kind));
    }
}

#[test]
fn prompt_marks_coordinate_matches_scrollback() {
    // A marked row scrolled into scrollback keeps absolute row 0, exactly like
    // prompt_mark_at.
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(&osc133("A"));
    terminal.advance(b"ab\r\ncd\r\nef\r\ngh");
    assert!(terminal.screen().scrollback_len() >= 1);
    assert_eq!(terminal.prompt_marks(), vec![(0, PromptKind::PromptStart)]);
}

#[test]
fn prompt_marks_empty_on_alt_screen_consistent_with_point_query() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc133("A"));
    assert_eq!(terminal.prompt_marks(), vec![(0, PromptKind::PromptStart)]);

    // Entering the alt screen hides the primary's marks; enumeration returns
    // empty, matching prompt_mark_at(0) == None.
    terminal.advance(b"\x1b[?1049h");
    assert!(terminal.prompt_marks().is_empty());
    assert_eq!(terminal.prompt_mark_at(0), None);

    // Leaving restores them.
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.prompt_marks(), vec![(0, PromptKind::PromptStart)]);
}

#[test]
fn command_blocks_derive_from_a_real_transcript() {
    // End-to-end: a full prompt cycle through the parser, enumerated, then
    // derived into a command block. Anchors the core derivation to live marks.
    //
    // Real shells emit `D` and the next prompt's `A` back to back in the same
    // prompt hook with NO newline between, so both land on the same row: the
    // prompt stamp merges over the exit instead of destroying it, and the
    // finished block keeps its verdict after the next prompt appears.
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&osc133("A"));
    terminal.advance(b"prompt$ ");
    terminal.advance(&osc133("B"));
    terminal.advance(b"echo hi\r\n");
    terminal.advance(&osc133("C"));
    terminal.advance(b"hi\r\n");
    terminal.advance(&osc133("D;0"));
    terminal.advance(&osc133("A")); // same row as D: the merge case
    terminal.advance(b"prompt$ ");
    terminal.advance(&osc133("B")); // second same-row prompt stamp keeps the exit

    assert_eq!(
        terminal.prompt_mark_at(2),
        Some(stamped(
            PromptKind::PromptStartAfterEnd { prev_exit: Some(0) },
            0,
        ))
    );

    let blocks = command_blocks(&terminal.prompt_marks());
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].prompt_row, 0);
    assert_eq!(blocks[0].output_start, Some(1));
    assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 1 });
    assert_eq!(blocks[0].exit, Some(0));
    assert_eq!(command_status(&blocks[0]), CommandStatus::Success);
    // The merged row also opens a fresh block awaiting input.
    assert_eq!(blocks[1].prompt_row, 2);
    assert_eq!(blocks[1].output, CommandOutput::Empty);
    assert_eq!(blocks[1].exit, None);
}

#[test]
fn supported_shell_shaped_streams_share_one_command_range_contract() {
    use crate::core::v013_fixtures::SHELL_OSC133_FIXTURES;

    for fixture in SHELL_OSC133_FIXTURES {
        let mut terminal = Terminal::new(96, 5);
        terminal.advance(&fixture.stream());

        let blocks = command_blocks(&terminal.prompt_marks());
        assert_eq!(blocks.len(), 2, "{} block count", fixture.shell);
        assert_eq!(blocks[0].prompt_row, 0, "{} prompt row", fixture.shell);
        assert_eq!(
            blocks[0].output,
            CommandOutput::Rows { start: 1, end: 1 },
            "{} output rows",
            fixture.shell
        );
        assert_eq!(blocks[0].exit, Some(fixture.exit), "{} exit", fixture.shell);
        assert_eq!(
            command_status(&blocks[0]),
            if fixture.exit == 0 {
                CommandStatus::Success
            } else {
                CommandStatus::Fail
            },
            "{} status",
            fixture.shell
        );
        assert!(
            terminal.screen().plain_text().contains(fixture.output),
            "{} visible output",
            fixture.shell
        );
        assert!(
            terminal.take_host_output().is_empty(),
            "{} OSC 133 must not emit a reply",
            fixture.shell
        );
    }
}

#[test]
fn missing_and_partial_shell_marks_fail_closed_without_guessing() {
    let mut open = Terminal::new(32, 4);
    open.advance(&osc133("A"));
    open.advance(b"prompt$ ");
    open.advance(&osc133("B"));
    open.advance(b"run\r\n");
    open.advance(&osc133("C"));
    open.advance(b"still-running");
    let blocks = command_blocks(&open.prompt_marks());
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].output, CommandOutput::Open { start: 1 });
    assert_eq!(command_status(&blocks[0]), CommandStatus::Running);

    let mut no_output_start = Terminal::new(32, 4);
    no_output_start.advance(&osc133("A"));
    no_output_start.advance(b"prompt$ ");
    no_output_start.advance(&osc133("B"));
    no_output_start.advance(b"silent\r\n");
    no_output_start.advance(&osc133("D;2"));
    no_output_start.advance(&osc133("A"));
    let blocks = command_blocks(&no_output_start.prompt_marks());
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].output, CommandOutput::Empty);
    assert_eq!(blocks[0].exit, Some(2));
    assert_eq!(command_status(&blocks[0]), CommandStatus::Fail);

    let mut stray = Terminal::new(32, 4);
    stray.advance(&osc133("C"));
    stray.advance(b"orphan\r\n");
    stray.advance(&osc133("D;9"));
    stray.advance(&osc133("A"));
    let blocks = command_blocks(&stray.prompt_marks());
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].output, CommandOutput::Empty);
    assert_eq!(blocks[0].exit, None);
}

#[test]
fn hostile_and_unterminated_marks_are_inert_or_conservative() {
    use crate::core::v013_fixtures::{HOSTILE_OSC133_FIXTURES, OscTerminator};

    for fixture in HOSTILE_OSC133_FIXTURES {
        let mut terminal = Terminal::new(16, 2);
        terminal.advance(&crate::core::v013_fixtures::osc133(
            fixture.payload,
            OscTerminator::Bell,
        ));

        assert_eq!(
            terminal.prompt_mark_at(0),
            fixture.expected.map(|kind| stamped(kind, 0)),
            "hostile OSC 133 fixture: {}",
            fixture.label
        );
        assert!(terminal.screen().plain_text().trim().is_empty());
        assert!(terminal.take_host_output().is_empty());
    }

    let mut unterminated = Terminal::new(16, 2);
    unterminated.advance(b"\x1b]133;D;0");
    assert!(unterminated.prompt_marks().is_empty());
    assert!(unterminated.screen().plain_text().trim().is_empty());
    assert!(unterminated.take_host_output().is_empty());
}

#[test]
fn command_blocks_derive_when_the_next_prompt_sits_on_its_own_row() {
    // The separated variant: a shell (or prompt theme) that advances a line
    // before printing the next prompt keeps a plain CommandEnd row, and the
    // derivation is unchanged.
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&osc133("A"));
    terminal.advance(b"$ ");
    terminal.advance(&osc133("B"));
    terminal.advance(b"echo hi\r\n");
    terminal.advance(&osc133("C"));
    terminal.advance(b"hi\r\n");
    terminal.advance(&osc133("D;0"));
    terminal.advance(b"\r\n");
    terminal.advance(&osc133("A")); // next prompt on its own row

    let blocks = command_blocks(&terminal.prompt_marks());
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 1 });
    assert_eq!(blocks[0].exit, Some(0));
    assert_eq!(command_status(&blocks[0]), CommandStatus::Success);
}

#[test]
fn silent_command_keeps_its_exit_through_the_single_row_collapse() {
    // A command that prints nothing lands C, D, and the next prompt's A all
    // on one row: C is collapsed by D (no output — as ever), and the prompt
    // stamp preserves the exit, so the verdict survives.
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&osc133("A"));
    terminal.advance(b"$ ");
    terminal.advance(&osc133("B"));
    terminal.advance(b"false\r\n");
    terminal.advance(&osc133("C"));
    terminal.advance(&osc133("D;1"));
    terminal.advance(&osc133("A"));
    terminal.advance(b"$ ");
    terminal.advance(&osc133("B"));

    let blocks = command_blocks(&terminal.prompt_marks());
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].output, CommandOutput::Empty);
    assert_eq!(blocks[0].exit, Some(1));
    assert_eq!(command_status(&blocks[0]), CommandStatus::Fail);
    assert_eq!(blocks[1].output, CommandOutput::Empty);
    assert_eq!(blocks[1].exit, None);
}

// --- SH-CLICK: click-events enable through the OSC 133 dispatch ---

#[test]
fn click_events_default_off_and_byte_identical_without_directive() {
    // Default-off: a fresh terminal and a plain prompt cycle never enable
    // click-events, so the SH-CLICK emit path stays inert.
    let mut terminal = Terminal::new(20, 6);
    assert!(!terminal.click_events_enabled());
    terminal.advance(&osc133("A"));
    terminal.advance(&osc133("B"));
    terminal.advance(&osc133("C"));
    terminal.advance(&osc133("D;0"));
    assert!(!terminal.click_events_enabled());
}

#[test]
fn click_events_enable_and_withdraw_through_dispatch() {
    let mut terminal = Terminal::new(20, 6);
    // A prompt announcing click_events=1 enables it.
    terminal.advance(&osc133("A;click_events=1"));
    assert!(terminal.click_events_enabled());
    // A plain prompt leaves the flag unchanged (absent attribute = no change).
    terminal.advance(b"\r\n");
    terminal.advance(&osc133("A"));
    assert!(terminal.click_events_enabled());
    // An explicit click_events=0 withdraws it.
    terminal.advance(b"\r\n");
    terminal.advance(&osc133("A;click_events=0"));
    assert!(!terminal.click_events_enabled());
}

#[test]
fn click_events_reset_by_ris() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&osc133("A;click_events=1"));
    assert!(terminal.click_events_enabled());
    // RIS returns click-events to its power-on (off) state.
    terminal.advance(b"\x1bc");
    assert!(!terminal.click_events_enabled());
}

#[test]
fn click_events_directive_does_not_leak_into_grid() {
    // The click_events attribute is consumed by the OSC handler, never printed.
    let mut plain = Terminal::new(20, 6);
    let mut marked = Terminal::new(20, 6);
    plain.advance(b"X");
    marked.advance(&osc133("A;click_events=1"));
    marked.advance(b"X");
    // The directive is consumed by the OSC handler; the render surface matches.
    assert_eq!(plain.snapshot(), marked.snapshot());
}

/// Extract the prompt-start (OSC 133 `A`) escape the given shell-integration
/// snippet emits, translated from the snippet's shell-`printf` form (`\e`, `\a`)
/// into the real bytes the shell writes to the PTY. Binds these tests to the
/// ACTUAL bundled snippet text rather than a hand-written sequence, so a snippet
/// that stops advertising click-events fails here.
fn snippet_prompt_start_bytes(snippet: &str) -> Vec<u8> {
    let start = snippet
        .find(r"\e]133;A")
        .expect("snippet emits an OSC 133 prompt-start");
    let rest = &snippet[start..];
    let end = rest.find(r"\a").expect("prompt-start is BEL-terminated") + r"\a".len();
    rest[..end]
        .replace(r"\e", "\x1b")
        .replace(r"\a", "\x07")
        .into_bytes()
}

/// The bundled shell-integration snippets must advertise `click_events=1` on
/// their prompt-start, so a terminal that parses the real snippet output through
/// the production OSC 133 dispatch ends up with click-to-position enabled. Without
/// the attribute the consumer-side feature (gated additionally on the `sh_click`
/// setting) can never turn on — the producer gap this closes. Bound to all three
/// snippets so each one's emission is checked.
#[test]
fn bundled_snippets_enable_click_events_through_dispatch() {
    use crate::shell_integration::{ShellKind, snippet};

    for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
        let prompt_start = snippet_prompt_start_bytes(snippet(kind));
        let mut terminal = Terminal::new(20, 6);
        assert!(!terminal.click_events_enabled());
        terminal.advance(&prompt_start);
        assert!(
            terminal.click_events_enabled(),
            "{kind:?} snippet prompt-start must enable click-events through the OSC 133 dispatch"
        );
    }
}
