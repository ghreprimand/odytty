// SPDX-License-Identifier: GPL-3.0-only
//! Cancelled control strings and private CSI forms have no side effects.
//!
//! CAN (0x18) and SUB (0x1A) abandon an OSC or DCS string: nothing the string
//! would have done happens, while the text after it still prints. A CSI
//! sequence with a private marker or intermediate byte is a different command
//! from the plain form with the same final byte; unimplemented forms are
//! ignored instead of running the plain command. Each case is checked against
//! the terminated or plain form, which must keep working exactly as before.

use super::*;

const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// A 2x1 red Sixel image (one sixel column, colour register 0).
const SIXEL_BODY: &[u8] = b"\x1bPq#0;2;100;0;0#0~~";

fn with_end(body: &[u8], end: &[u8]) -> Vec<u8> {
    let mut bytes = body.to_vec();
    bytes.extend_from_slice(end);
    bytes.extend_from_slice(b"TAIL");
    bytes
}

fn ends() -> [(&'static str, Vec<u8>); 2] {
    [("CAN", vec![CAN]), ("SUB", vec![SUB])]
}

fn row_cells(terminal: &Terminal, row: usize) -> Vec<Cell> {
    let snapshot = terminal.snapshot();
    let columns = snapshot.dimensions.columns;
    snapshot.cells[row * columns..(row + 1) * columns].to_vec()
}

fn row_text(terminal: &Terminal, row: usize) -> String {
    row_cells(terminal, row)
        .iter()
        .map(|cell| cell.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn terminated_sixel_is_placed_but_a_cancelled_one_is_not() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(&with_end(SIXEL_BODY, b"\x1b\\"));
    assert_eq!(terminal.graphics().placements().len(), 1, "ST places it");

    for (name, end) in ends() {
        let mut terminal = Terminal::new(20, 4);
        terminal.advance(&with_end(SIXEL_BODY, &end));
        assert!(
            terminal.graphics().placements().is_empty(),
            "{name} must discard the Sixel"
        );
        assert!(terminal.graphics().store().is_empty(), "{name}: no image");
        assert_eq!(row_text(&terminal, 0), "TAIL", "{name}: text resumes");
    }
}

#[test]
fn cancelled_dcs_queries_send_no_reply() {
    for body in [&b"\x1bP$qm"[..], b"\x1bP+q544e"] {
        let mut terminal = Terminal::new(20, 4);
        terminal.advance(&with_end(body, b"\x1b\\"));
        assert!(
            !terminal.take_host_output().is_empty(),
            "{:?} terminated by ST replies",
            String::from_utf8_lossy(body)
        );
        for (name, end) in ends() {
            let mut terminal = Terminal::new(20, 4);
            terminal.advance(&with_end(body, &end));
            assert!(
                terminal.take_host_output().is_empty(),
                "{:?} cancelled by {name} must not reply",
                String::from_utf8_lossy(body)
            );
            assert_eq!(row_text(&terminal, 0), "TAIL");
        }
    }
}

#[test]
fn cancelled_osc_title_and_cwd_are_not_applied() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(&with_end(b"\x1b]0;hi", b"\x1b\\"));
    assert_eq!(terminal.title(), Some("hi"));
    terminal.advance(&with_end(b"\x1b]7;file:///tmp", b"\x07"));
    assert_eq!(terminal.current_working_directory(), Some("/tmp"));

    for (name, end) in ends() {
        let mut terminal = Terminal::new(20, 4);
        terminal.advance(&with_end(b"\x1b]0;hi", &end));
        assert_eq!(terminal.title(), None, "{name}: title unchanged");
        assert!(!terminal.take_title_changed(), "{name}: no title event");
        terminal.advance(&with_end(b"\x1b]7;file:///tmp", &end));
        assert_eq!(
            terminal.current_working_directory(),
            None,
            "{name}: cwd unchanged"
        );
        assert!(!terminal.take_working_directory_changed());
        assert_eq!(row_text(&terminal, 0), "TAILTAIL", "{name}: text resumes");
    }
}

#[test]
fn cancelled_osc52_write_makes_no_clipboard_request() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(&with_end(b"\x1b]52;c;aGVsbG8=", b"\x1b\\"));
    assert_eq!(terminal.take_clipboard_requests().len(), 1);

    for (name, end) in ends() {
        let mut terminal = Terminal::new(20, 4);
        terminal.advance(&with_end(b"\x1b]52;c;aGVsbG8=", &end));
        assert!(
            terminal.take_clipboard_requests().is_empty(),
            "{name}: a cancelled OSC 52 must not write the clipboard"
        );
    }
}

#[test]
fn cancelled_osc8_does_not_open_a_link() {
    for (name, end) in ends() {
        let mut terminal = Terminal::new(20, 4);
        terminal.advance(&with_end(b"\x1b]8;;https://example.invalid/", &end));
        let cells = row_cells(&terminal, 0);
        assert!(
            cells.iter().all(|cell| cell.attrs.hyperlink.is_none()),
            "{name}: text after a cancelled OSC 8 carries no link"
        );
    }
}

#[test]
fn a_cancelled_string_does_not_disturb_the_next_one() {
    for (name, end) in ends() {
        let mut terminal = Terminal::new(20, 4);
        let mut bytes = with_end(b"\x1b]0;stale", &end);
        bytes.extend_from_slice(b"\x1b]0;fresh\x07");
        terminal.advance(&bytes);
        assert_eq!(terminal.title(), Some("fresh"), "{name}");
    }
}

fn cursor(terminal: &Terminal) -> (usize, usize) {
    let position = terminal.screen().cursor();
    (position.row, position.column)
}

/// A terminal with text on every row and the cursor inside it at (2, 3).
fn populated() -> Terminal {
    let mut terminal = Terminal::new(10, 6);
    terminal.advance(b"abcdefghij\r\nklmnopqrst\r\nuvwxyzABCD\r\nEFGHIJKLMN\x1b[3;4H");
    assert_eq!(cursor(&terminal), (2, 3));
    terminal
}

#[test]
fn private_save_cursor_form_keeps_the_saved_position() {
    // XTSAVE (`CSI ? Pm s`) saves private modes in xterm; it is not DECSC.
    let mut terminal = populated();
    terminal.advance(b"\x1b7\x1b[H\x1b[?25s\x1b8");
    assert_eq!(
        cursor(&terminal),
        (2, 3),
        "ESC 8 restores the ESC 7 position"
    );

    // The plain form still saves the cursor.
    let mut terminal = populated();
    terminal.advance(b"\x1b7\x1b[H\x1b[s\x1b8");
    assert_eq!(cursor(&terminal), (0, 0), "plain CSI s saves the cursor");
}

#[test]
fn private_margin_and_movement_forms_leave_the_cursor_alone() {
    for sequence in [&b"\x1b[>2;4r"[..], b"\x1b[?1r", b"\x1b[>2A", b"\x1b[=2B"] {
        let mut terminal = populated();
        terminal.advance(sequence);
        assert_eq!(
            cursor(&terminal),
            (2, 3),
            "{:?} must be ignored",
            String::from_utf8_lossy(sequence)
        );
    }

    let mut terminal = populated();
    terminal.advance(b"\x1b[2;4r");
    assert_eq!(cursor(&terminal), (0, 0), "plain DECSTBM homes the cursor");
    let mut terminal = populated();
    terminal.advance(b"\x1b[2A");
    assert_eq!(cursor(&terminal), (0, 3), "plain CUU moves up");
}

/// Every final byte whose plain CSI form is implemented only in that form.
const PLAIN_ONLY_FINALS: &str = "ABCDEFGHf@bLMPXdgrsSTJKt";

#[test]
fn unimplemented_forms_of_plain_only_commands_change_nothing() {
    for prefix in [">", "=", "<", " ", "!"] {
        for final_byte in PLAIN_ONLY_FINALS.chars() {
            // `CSI ! p` is DECSTR and `CSI ? J/K` is DECSED/DECSEL; neither is
            // in this set. Skip the `<`/`=`/`>` + `u` Kitty keyboard forms by
            // leaving `u` out of the finals.
            let sequence = if prefix == " " || prefix == "!" {
                format!("\x1b[2{prefix}{final_byte}")
            } else {
                format!("\x1b[{prefix}2{final_byte}")
            };
            let mut terminal = populated();
            let before = terminal.snapshot();
            terminal.advance(b"\x1b7");
            terminal.advance(sequence.as_bytes());
            assert_eq!(cursor(&terminal), (2, 3), "{sequence:?} moved the cursor");
            assert_eq!(terminal.snapshot().cells, before.cells, "{sequence:?}");
            assert!(terminal.take_host_output().is_empty(), "{sequence:?}");
            terminal.advance(b"\x1b[H\x1b8");
            assert_eq!(cursor(&terminal), (2, 3), "{sequence:?} saved state");
        }
    }
}

#[test]
fn plain_forms_still_run() {
    let cases: [(&[u8], (usize, usize)); 10] = [
        (b"\x1b[A", (1, 3)),
        (b"\x1b[B", (3, 3)),
        (b"\x1b[C", (2, 4)),
        (b"\x1b[D", (2, 2)),
        (b"\x1b[E", (3, 0)),
        (b"\x1b[F", (1, 0)),
        (b"\x1b[6G", (2, 5)),
        (b"\x1b[5;6H", (4, 5)),
        (b"\x1b[5;6f", (4, 5)),
        (b"\x1b[2d", (1, 3)),
    ];
    for (sequence, expected) in cases {
        let mut terminal = populated();
        terminal.advance(sequence);
        assert_eq!(
            cursor(&terminal),
            expected,
            "{:?}",
            String::from_utf8_lossy(sequence)
        );
    }

    let mut terminal = populated();
    terminal.advance(b"\x1b[2P");
    assert_eq!(row_text(&terminal, 2), "uvwzABCD", "DCH deletes");
    let mut terminal = populated();
    terminal.advance(b"\x1b[2@");
    assert_eq!(row_text(&terminal, 2), "uvw  xyzAB", "ICH inserts");
    let mut terminal = populated();
    terminal.advance(b"\x1b[2X");
    assert_eq!(row_text(&terminal, 2), "uvw  zABCD", "ECH erases");
    let mut terminal = populated();
    terminal.advance(b"\x1b[K");
    assert_eq!(row_text(&terminal, 2), "uvw", "EL erases to end");
    let mut terminal = populated();
    terminal.advance(b"\x1b[M");
    assert_eq!(row_text(&terminal, 2), "EFGHIJKLMN", "DL deletes the line");
    let mut terminal = populated();
    terminal.advance(b"\x1b[L");
    assert_eq!(row_text(&terminal, 3), "uvwxyzABCD", "IL inserts a line");
    let mut terminal = populated();
    terminal.advance(b"Q\x1b[3b");
    assert_eq!(row_text(&terminal, 2), "uvwQQQQBCD", "REP repeats");
    let mut terminal = populated();
    terminal.advance(b"\x1b[S");
    assert_eq!(row_text(&terminal, 0), "klmnopqrst", "SU scrolls up");
    let mut terminal = populated();
    terminal.advance(b"\x1b[T");
    assert_eq!(row_text(&terminal, 1), "abcdefghij", "SD scrolls down");
    let mut terminal = populated();
    terminal.advance(b"\x1b[5;6H\x1b[s\x1b[H\x1b[u");
    assert_eq!(cursor(&terminal), (4, 5), "SCOSC/SCORC round-trip");
}
