//! Behavioral tests for the terminal core: printing, SGR, cursor movement,
//! erase/scroll, alternate screen, scrollback/reflow, OSC titles, mouse-mode
//! tracking, wide/combining Unicode. Drives the public `Terminal`/`Screen` API
//! plus the crate-internal `MAX_COMBINING` bound.

use super::types::MAX_COMBINING;
use super::*;

fn assert_blank_with_background(terminal: &Terminal, row: usize, column: usize, background: Color) {
    let cell = terminal.screen().cell(row, column).unwrap();
    assert_eq!(cell.ch, ' ');
    assert_eq!(
        cell.attrs,
        Attrs {
            background,
            ..Attrs::default()
        }
    );
    assert!(!cell.wide_continuation);
}

#[test]
fn prints_plain_text_into_owned_grid() {
    let mut terminal = Terminal::new(10, 3);

    terminal.advance(b"hello\r\nody");

    assert_eq!(terminal.screen().plain_text(), "hello\nody\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 3 });
}

#[test]
fn applies_basic_sgr_attributes() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[1;31mR\x1b[0mN");

    let red = terminal.screen().cell(0, 0).unwrap();
    let normal = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(red.ch, 'R');
    assert!(red.attrs.bold);
    assert_eq!(red.attrs.foreground, Color::Indexed(1));
    assert_eq!(normal.ch, 'N');
    assert_eq!(normal.attrs, Attrs::default());
}

#[test]
fn applies_extended_sgr_text_attributes() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[2;8;9mX\x1b[0mN");

    let styled = terminal.screen().cell(0, 0).unwrap();
    let normal = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(styled.ch, 'X');
    assert!(styled.attrs.dim);
    assert!(styled.attrs.hidden);
    assert!(styled.attrs.strikethrough);
    assert_eq!(normal.ch, 'N');
    assert_eq!(normal.attrs, Attrs::default());
}

#[test]
fn sgr_resets_text_attributes_independently() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[1;2;3;4;7;8;9mA\x1b[22;23;24;27;28;29mB");

    let all = terminal.screen().cell(0, 0).unwrap();
    assert!(all.attrs.bold);
    assert!(all.attrs.dim);
    assert!(all.attrs.italic);
    assert!(all.attrs.underline);
    assert!(all.attrs.inverse);
    assert!(all.attrs.hidden);
    assert!(all.attrs.strikethrough);

    let reset = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(reset.attrs, Attrs::default());
}

#[test]
fn sgr_22_clears_bold_and_dim_together() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[1;2mA\x1b[22mB");

    let styled = terminal.screen().cell(0, 0).unwrap();
    assert!(styled.attrs.bold);
    assert!(styled.attrs.dim);

    let reset = terminal.screen().cell(0, 1).unwrap();
    assert!(!reset.attrs.bold);
    assert!(!reset.attrs.dim);
}

#[test]
fn responds_to_primary_device_attributes() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[c");

    assert_eq!(terminal.take_host_output(), b"\x1b[?1;2c");
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn reports_cursor_position_for_dsr_6n() {
    let mut terminal = Terminal::new(20, 5);

    // Move the cursor to row 3, column 5 (1-based H), then request DSR 6n.
    terminal.advance(b"\x1b[3;5H\x1b[6n");

    // Reply is the cursor position report, 1-based: ESC [ row ; col R.
    assert_eq!(terminal.take_host_output(), b"\x1b[3;5R");
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn dsr_6n_tracks_cursor_after_printing() {
    let mut terminal = Terminal::new(20, 5);

    // Print four glyphs on the top row; cursor sits at column 5 (1-based).
    terminal.advance(b"less\x1b[6n");

    assert_eq!(terminal.take_host_output(), b"\x1b[1;5R");
}

#[test]
fn dsr_6n_honors_origin_mode_region() {
    let mut terminal = Terminal::new(20, 10);

    // DECSTBM rows 3..=8 (1-based), enable DECOM, home within the region,
    // then ask for the cursor position: row must be region-relative (1).
    terminal.advance(b"\x1b[3;8r\x1b[?6h\x1b[H\x1b[6n");

    assert_eq!(terminal.take_host_output(), b"\x1b[1;1R");
}

#[test]
fn responds_to_dsr_5n_status() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[5n");

    // 5n -> terminal OK (ESC [ 0 n).
    assert_eq!(terminal.take_host_output(), b"\x1b[0n");
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn ignores_private_dsr_request() {
    let mut terminal = Terminal::new(10, 2);

    // DECDSR (private marker) is not answered by the plain DSR path.
    terminal.advance(b"\x1b[?6n");

    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn bare_cursor_moves_default_to_one() {
    // ECMA-48: an omitted parameter on CUU/CUD/CUF/CUB means 1. `vte`
    // delivers the omitted parameter as an explicit `0`, so the encoder
    // must still treat it as 1 rather than as a zero-count no-op.
    let mut terminal = Terminal::new(10, 10);

    terminal.advance(b"\x1b[5;5H"); // row 5, col 5 (1-based) -> index (4,4)
    assert_eq!(terminal.screen().cursor(), Position { row: 4, column: 4 });

    terminal.advance(b"\x1b[A"); // bare CUU -> up 1
    assert_eq!(terminal.screen().cursor(), Position { row: 3, column: 4 });
    terminal.advance(b"\x1b[B"); // bare CUD -> down 1
    assert_eq!(terminal.screen().cursor(), Position { row: 4, column: 4 });
    terminal.advance(b"\x1b[C"); // bare CUF -> right 1
    assert_eq!(terminal.screen().cursor(), Position { row: 4, column: 5 });
    terminal.advance(b"\x1b[D"); // bare CUB -> left 1
    assert_eq!(terminal.screen().cursor(), Position { row: 4, column: 4 });
}

#[test]
fn zero_count_cursor_moves_are_treated_as_one() {
    // A literal `0` count is equivalent to an omitted one for these moves.
    let mut terminal = Terminal::new(10, 10);

    terminal.advance(b"\x1b[5;5H\x1b[0A");
    assert_eq!(terminal.screen().cursor(), Position { row: 3, column: 4 });
}

#[test]
fn completion_pager_redraw_clears_stale_rows() {
    // Distilled from a captured fish completion redraw: the shell prints the
    // command line, drops to the row below to list candidates, returns to
    // the command line with a bare CUU, then narrows the prefix and issues
    // ED-to-end-of-screen (ESC [ J) to wipe the old candidate rows. If the
    // bare CUU is a no-op the cursor never returns to the command line, so
    // ESC [ J clears from the wrong row and the candidates linger. This is
    // the operator-reported "stale completion text" regression.
    let mut terminal = Terminal::new(40, 6);

    // Command line on row 0, then candidates on row 1.
    terminal.advance(b"> less build\r\nBackups/ Bonnie build.sh busy.log");
    // Return to the command line: CR + bare CUU.
    terminal.advance(b"\r\x1b[A");
    assert_eq!(terminal.screen().cursor().row, 0);

    // Narrow the prefix: echo a char, then clear to end of screen.
    terminal.advance(b"\x1b[12C u\x1b[J");

    let text = terminal.screen().plain_text();
    assert!(
        !text.contains("Backups/") && !text.contains("busy.log"),
        "stale completion candidates remained:\n{text}"
    );
}

#[test]
fn saves_and_restores_cursor_with_escape_sequences() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"abc\x1b7XX\x1b8Z");

    assert_eq!(terminal.screen().plain_text(), "abcZX\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
}

#[test]
fn saves_and_restores_cursor_with_csi_sequences() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"abc\x1b[sXX\x1b[uZ");

    assert_eq!(terminal.screen().plain_text(), "abcZX\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
}

#[test]
fn isolates_alternate_screen_from_primary_screen() {
    let mut terminal = Terminal::new(8, 3);

    terminal.advance(b"PRI\x1b[?1049hALT\x1b[?1049lMARY");

    assert_eq!(terminal.screen().plain_text(), "PRIMARY\n\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 7 });
}

#[test]
fn scroll_region_scrolls_only_inside_margins() {
    let mut terminal = Terminal::new(8, 4);

    terminal.advance(b"top\r\none\r\ntwo\r\nbot");
    terminal.advance(b"\x1b[2;3r\x1b[3;1H\nX");

    assert_eq!(terminal.screen().plain_text(), "top\ntwo\nX\nbot");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn tracks_bracketed_paste_mode() {
    let mut terminal = Terminal::new(8, 2);

    assert!(!terminal.bracketed_paste_enabled());

    terminal.advance(b"\x1b[?2004h");
    assert!(terminal.bracketed_paste_enabled());

    terminal.advance(b"\x1b[?2004l");
    assert!(!terminal.bracketed_paste_enabled());
}

#[test]
fn tracks_keyboard_application_modes() {
    let mut terminal = Terminal::new(8, 2);

    assert_eq!(terminal.keyboard_modes(), KeyboardModes::default());

    terminal.advance(b"\x1b[?1h");
    assert_eq!(
        terminal.keyboard_modes(),
        KeyboardModes {
            application_cursor: true,
            application_keypad: false,
        }
    );

    terminal.advance(b"\x1b=");
    assert_eq!(
        terminal.keyboard_modes(),
        KeyboardModes {
            application_cursor: true,
            application_keypad: true,
        }
    );

    terminal.advance(b"\x1b[?1l");
    assert_eq!(
        terminal.keyboard_modes(),
        KeyboardModes {
            application_cursor: false,
            application_keypad: true,
        }
    );

    terminal.advance(b"\x1b>");
    assert_eq!(terminal.keyboard_modes(), KeyboardModes::default());
}

#[test]
fn applies_multiple_dec_private_modes_in_one_sequence() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"\x1b[?25;2004l");

    assert!(!terminal.snapshot().cursor_visible);
    assert!(!terminal.bracketed_paste_enabled());

    terminal.advance(b"\x1b[?25;2004h");

    assert!(terminal.snapshot().cursor_visible);
    assert!(terminal.bracketed_paste_enabled());
}

#[test]
fn line_feed_at_screen_bottom_outside_active_region_does_not_scroll_full_screen() {
    let mut terminal = Terminal::new(8, 4);

    terminal.advance(b"head\r\none\r\ntwo\r\nfoot");
    terminal.advance(b"\x1b[2;3r\x1b[4;1H\nZ");

    assert_eq!(terminal.screen().plain_text(), "head\none\ntwo\nZoot");
    assert_eq!(terminal.screen().scrollback_len(), 0);
}

#[test]
fn handles_cursor_movement_and_erase_line() {
    let mut terminal = Terminal::new(8, 2);

    terminal.advance(b"abcdef\x1b[3D\x1b[KZ");

    assert_eq!(terminal.screen().plain_text(), "abcZ\n");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 4 });
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

// ICH (CSI Ps @) / DCH (CSI Ps P): row-local insert/delete of cells. Baseline
// verified against xterm/Ghostty — cursor stays put, no wrap/scroll, shifted
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
    assert!(x.attrs.bold);
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
    // into col 0 and is cleaned to a blank in place — so a single leading
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
// line — it overwrites count cells with BCE blanks. Cursor stays put,
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
fn combining_mark_at_line_start_is_noop() {
    let mut terminal = Terminal::new(6, 1);
    // A combining mark with no preceding base char must not panic and must
    // leave the grid untouched.
    terminal.advance("\u{0301}".as_bytes());

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, ' ');
    assert!(cell.combining().is_empty());
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 0 });
}

#[test]
fn combining_marks_clamp_to_capacity_without_panicking() {
    let mut terminal = Terminal::new(6, 1);
    // Three combining marks on one base; only MAX_COMBINING are retained.
    terminal.advance("e\u{0301}\u{0302}\u{0303}".as_bytes());

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'e');
    assert_eq!(cell.combining().len(), MAX_COMBINING);
    assert_eq!(cell.combining(), &['\u{0301}', '\u{0302}']);
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
    assert!(original.attrs.bold);
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
    // vte splits on ';'; the title must be rejoined intact.
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
