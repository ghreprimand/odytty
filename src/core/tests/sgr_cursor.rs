// SPDX-License-Identifier: GPL-3.0-only
//! Core behavioral tests (M4 mechanical split from core/tests.rs).

use super::*;

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
    assert!(red.attrs.bold());
    assert_eq!(red.attrs.foreground, Color::Indexed(1));
    assert_eq!(normal.ch, 'N');
    assert_eq!(normal.attrs, Attrs::default());
}

#[test]
fn private_prefixed_m_is_not_sgr() {
    // `CSI > 4 ; 2 m` is XTMODKEYS (set modifyOtherKeys), which apps emit at
    // startup to enable enhanced keyboard input. It must NOT be parsed as SGR
    // `4;2` (underline + dim) — doing so set those attributes globally and
    // smeared them across all subsequent text. The `>` private prefix arrives
    // in `intermediates`, so the SGR path is gated on empty intermediates.
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b[>4;2mX");
    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.ch, 'X');
    assert!(
        !cell.attrs.underline(),
        "XTMODKEYS must not enable underline"
    );
    assert!(!cell.attrs.dim(), "XTMODKEYS must not enable dim");
    assert_eq!(cell.attrs, Attrs::default());

    // A bare `CSI 4 m` is still a real SGR underline.
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b[4mU");
    assert!(terminal.screen().cell(0, 0).unwrap().attrs.underline());
}

#[test]
fn applies_extended_sgr_text_attributes() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[2;5;8;9mX\x1b[0mN");

    let styled = terminal.screen().cell(0, 0).unwrap();
    let normal = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(styled.ch, 'X');
    assert!(styled.attrs.dim());
    assert!(styled.attrs.blink());
    assert!(styled.attrs.hidden());
    assert!(styled.attrs.strikethrough());
    assert_eq!(normal.ch, 'N');
    assert_eq!(normal.attrs, Attrs::default());
}

#[test]
fn sgr_resets_text_attributes_independently() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[1;2;3;4;5;7;8;9mA\x1b[22;23;24;25;27;28;29mB");

    let all = terminal.screen().cell(0, 0).unwrap();
    assert!(all.attrs.bold());
    assert!(all.attrs.dim());
    assert!(all.attrs.italic());
    assert!(all.attrs.underline());
    assert_eq!(
        all.attrs.effective_underline_style(),
        UnderlineStyle::Straight
    );
    assert!(all.attrs.blink());
    assert!(all.attrs.inverse());
    assert!(all.attrs.hidden());
    assert!(all.attrs.strikethrough());

    let reset = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(reset.attrs, Attrs::default());
}

#[test]
fn sgr_22_clears_bold_and_dim_together() {
    let mut terminal = Terminal::new(10, 2);

    terminal.advance(b"\x1b[1;2mA\x1b[22mB");

    let styled = terminal.screen().cell(0, 0).unwrap();
    assert!(styled.attrs.bold());
    assert!(styled.attrs.dim());

    let reset = terminal.screen().cell(0, 1).unwrap();
    assert!(!reset.attrs.bold());
    assert!(!reset.attrs.dim());
}

#[test]
fn sgr_21_selects_double_underline() {
    let mut terminal = Terminal::new(1, 1);

    terminal.advance(b"\x1b[21mL");

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert!(cell.attrs.underline());
    assert_eq!(
        cell.attrs.effective_underline_style(),
        UnderlineStyle::Double
    );
}

#[test]
fn sgr_underline_subparams_select_styles() {
    let cases = [
        (b"\x1b[4mS".as_slice(), UnderlineStyle::Straight, true),
        (b"\x1b[4:0mN".as_slice(), UnderlineStyle::None, false),
        (b"\x1b[4:1mS".as_slice(), UnderlineStyle::Straight, true),
        (b"\x1b[4:2mD".as_slice(), UnderlineStyle::Double, true),
        (b"\x1b[4:3mC".as_slice(), UnderlineStyle::Curly, true),
        (b"\x1b[4:4mO".as_slice(), UnderlineStyle::Dotted, true),
        (b"\x1b[4:5mA".as_slice(), UnderlineStyle::Dashed, true),
    ];

    for (input, style, underlined) in cases {
        let mut terminal = Terminal::new(1, 1);
        terminal.advance(input);
        let cell = terminal.screen().cell(0, 0).unwrap();
        assert_eq!(cell.attrs.underline(), underlined, "{input:?}");
        assert_eq!(cell.attrs.effective_underline_style(), style, "{input:?}");
    }
}

#[test]
fn sgr_underline_subparams_reject_malformed_styles() {
    let mut terminal = Terminal::new(2, 1);

    terminal.advance(b"\x1b[4:9mA\x1b[4:1:2mB");

    let bad_value = terminal.screen().cell(0, 0).unwrap();
    let too_many = terminal.screen().cell(0, 1).unwrap();
    assert!(!bad_value.attrs.underline());
    assert_eq!(
        bad_value.attrs.effective_underline_style(),
        UnderlineStyle::None
    );
    assert!(!too_many.attrs.underline());
    assert_eq!(
        too_many.attrs.effective_underline_style(),
        UnderlineStyle::None
    );
}

#[test]
fn sgr_24_turns_underline_off_without_clearing_color() {
    let mut terminal = Terminal::new(2, 1);

    terminal.advance(b"\x1b[58;5;42;4mA\x1b[24mB");

    let underlined = terminal.screen().cell(0, 0).unwrap();
    let off = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(underlined.attrs.underline_color, Some(Color::Indexed(42)));
    assert_eq!(off.attrs.underline_color, Some(Color::Indexed(42)));
    assert_eq!(off.attrs.effective_underline_style(), UnderlineStyle::None);
}

#[test]
fn sgr_underline_color_supports_semicolon_and_colon_forms() {
    let mut terminal = Terminal::new(4, 1);

    terminal.advance(
        b"\x1b[58;5;123;4mA\
          \x1b[58;2;10;20;30mB\
          \x1b[58:2::1:2:3mC\
          \x1b[59mD",
    );

    let indexed = terminal.screen().cell(0, 0).unwrap();
    let semicolon_rgb = terminal.screen().cell(0, 1).unwrap();
    let colon_rgb = terminal.screen().cell(0, 2).unwrap();
    let reset = terminal.screen().cell(0, 3).unwrap();
    assert_eq!(indexed.attrs.underline_color, Some(Color::Indexed(123)));
    assert_eq!(
        semicolon_rgb.attrs.underline_color,
        Some(Color::Rgb(10, 20, 30))
    );
    assert_eq!(colon_rgb.attrs.underline_color, Some(Color::Rgb(1, 2, 3)));
    assert_eq!(reset.attrs.underline_color, None);
}

#[test]
fn sgr_underline_color_rejects_malformed_subparams() {
    let mut terminal = Terminal::new(2, 1);

    terminal.advance(b"\x1b[58:9:77;4mA\x1b[58:2:1:2;4mB");

    let bad_mode = terminal.screen().cell(0, 0).unwrap();
    let short_rgb = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(bad_mode.attrs.underline_color, None);
    assert_eq!(short_rgb.attrs.underline_color, None);
    assert_eq!(
        bad_mode.attrs.effective_underline_style(),
        UnderlineStyle::Straight
    );
    assert_eq!(
        short_rgb.attrs.effective_underline_style(),
        UnderlineStyle::Straight
    );
}

#[test]
fn sgr_reset_clears_underline_style_and_color() {
    let mut terminal = Terminal::new(2, 1);

    terminal.advance(b"\x1b[58:5:77;4:5mA\x1b[0mB");

    let styled = terminal.screen().cell(0, 0).unwrap();
    let reset = terminal.screen().cell(0, 1).unwrap();
    assert_eq!(
        styled.attrs.effective_underline_style(),
        UnderlineStyle::Dashed
    );
    assert_eq!(styled.attrs.underline_color, Some(Color::Indexed(77)));
    assert_eq!(reset.attrs, Attrs::default());
}

/// Regression for the silent drop of 24-bit SGR color channels valued 60–63.
///
/// 60/61/62/63 are the ASCII codes of the CSI private-marker bytes `<=>?`. A
/// former value-equality filter in `sgr_params` treated a *parameter value* of
/// 60–63 as a marker and discarded the group, so any truecolor channel of
/// 60/61/62/63 was silently lost (the color fell back to `Default`) and the
/// param decode desynced. The marker is tracked structurally (as a sequence
/// intermediate), so a real channel value must now round-trip exactly across
/// foreground (38), background (48), and underline (58).
#[test]
fn truecolor_channels_60_to_63_are_not_dropped() {
    let mut terminal = Terminal::new(8, 1);

    terminal.advance(
        b"\x1b[38;2;60;61;62mA\
          \x1b[0m\x1b[48;2;63;60;61mB\
          \x1b[0m\x1b[58;2;62;63;60mC",
    );

    let fg = terminal.screen().cell(0, 0).unwrap();
    let bg = terminal.screen().cell(0, 1).unwrap();
    let ul = terminal.screen().cell(0, 2).unwrap();
    assert_eq!(fg.attrs.foreground, Color::Rgb(60, 61, 62));
    assert_eq!(bg.attrs.background, Color::Rgb(63, 60, 61));
    assert_eq!(ul.attrs.underline_color, Some(Color::Rgb(62, 63, 60)));
}

/// Every single channel value 0..=255 in every channel position round-trips for
/// foreground truecolor — the exhaustive guard that no value (not just the
/// historical 60–63) is misclassified as a marker.
#[test]
fn truecolor_every_channel_value_round_trips() {
    for v in 0u16..=255 {
        // Place the value in each of R, G, B in turn (other channels fixed).
        let seqs = [
            format!("\x1b[38;2;{v};7;9mX"),
            format!("\x1b[38;2;7;{v};9mX"),
            format!("\x1b[38;2;7;9;{v}mX"),
        ];
        let expected = [
            Color::Rgb(v as u8, 7, 9),
            Color::Rgb(7, v as u8, 9),
            Color::Rgb(7, 9, v as u8),
        ];
        for (seq, want) in seqs.iter().zip(expected) {
            let mut t = Terminal::new(2, 1);
            t.advance(seq.as_bytes());
            assert_eq!(
                t.screen().cell(0, 0).unwrap().attrs.foreground,
                want,
                "channel value {v} dropped in seq {seq:?}"
            );
        }
    }
}

/// A truecolor underline color whose channel is 60 must not consume the trailing
/// underline-style attribute. Before the fix, dropping the `60` group desynced
/// the decode so the following `4` (underline on) was mis-consumed.
#[test]
fn truecolor_underline_channel_60_keeps_trailing_underline_style() {
    let mut terminal = Terminal::new(2, 1);

    terminal.advance(b"\x1b[58;2;10;20;60;4mU");

    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.attrs.underline_color, Some(Color::Rgb(10, 20, 60)));
    assert_eq!(
        cell.attrs.effective_underline_style(),
        UnderlineStyle::Straight,
        "trailing underline style must survive a 60-valued channel"
    );
}

/// The fix must not regress genuine CSI private-marker routing: the marker is an
/// intermediate, so DEC private modes (`CSI ? … h/l`) and other marker-prefixed
/// sequences still dispatch correctly — they do not collide with parameter
/// values that happen to equal 60–63.
#[test]
fn private_marker_sequences_still_route_after_fix() {
    let mut terminal = Terminal::new(4, 2);

    // DECSET 25 (show cursor) then DECRST (hide): the `?` marker must still be
    // recognized as a private-mode introducer, not treated as SGR/param data.
    terminal.advance(b"\x1b[?25h");
    assert!(terminal.snapshot().cursor_visible);
    terminal.advance(b"\x1b[?25l");
    assert!(!terminal.snapshot().cursor_visible);

    // A bare SGR with no marker still applies normally afterward.
    terminal.advance(b"\x1b[1mB");
    assert!(terminal.screen().cell(0, 0).unwrap().attrs.bold());

    // `CSI > c` (secondary device attributes) — the `>` marker must still route
    // to DA2, producing its host reply (not be misread as a parameter value).
    let _ = terminal.take_host_output();
    terminal.advance(b"\x1b[>c");
    assert_eq!(terminal.take_host_output(), b"\x1b[>65;1;0c");

    // `CSI = u` (kitty keyboard set-flags, the `=` marker) must still route:
    // it consumes the sequence as a private-marker control, leaving no host
    // reply and not corrupting subsequent printable text.
    terminal.advance(b"\x1b[=1u");
    assert!(terminal.take_host_output().is_empty());
    terminal.advance(b"Z");
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, 'Z');
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
    // ECMA-48: an omitted parameter on CUU/CUD/CUF/CUB means 1. The parser
    // materializes the omitted parameter as an explicit `0`, so the encoder must
    // still treat it as 1 rather than as a zero-count no-op.
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
    // the reported "stale completion text" regression.
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
            ..KeyboardModes::default()
        }
    );

    terminal.advance(b"\x1b=");
    assert_eq!(
        terminal.keyboard_modes(),
        KeyboardModes {
            application_cursor: true,
            application_keypad: true,
            ..KeyboardModes::default()
        }
    );

    terminal.advance(b"\x1b[?1l");
    assert_eq!(
        terminal.keyboard_modes(),
        KeyboardModes {
            application_cursor: false,
            application_keypad: true,
            ..KeyboardModes::default()
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
fn irm_off_overwrites_in_place() {
    // Replace mode (the default): printing over existing text overwrites it.
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"abc\r"); // "abc", cursor back to column 0
    terminal.advance(b"X");
    assert_eq!(terminal.screen().plain_text(), "Xbc");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
}

#[test]
fn irm_on_shifts_existing_cells_right() {
    // CSI 4 h enables insert mode: a printed glyph pushes the cells at and right
    // of the cursor toward the right edge instead of overwriting them.
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"abc\r");
    terminal.advance(b"\x1b[4h"); // IRM on
    terminal.advance(b"X");
    assert_eq!(terminal.screen().plain_text(), "Xabc");
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
    // Continuing to type keeps inserting at the cursor.
    terminal.advance(b"Y");
    assert_eq!(terminal.screen().plain_text(), "XYabc");
}

#[test]
fn irm_drops_cells_past_the_right_edge() {
    // Inserting near the right edge pushes content off the line; it is dropped,
    // never wrapped, matching xterm IRM semantics.
    let mut terminal = Terminal::new(4, 1);
    terminal.advance(b"abcd\r"); // fills the row; cursor returns to column 0
    terminal.advance(b"\x1b[4h");
    terminal.advance(b"X");
    // "abcd" shifts right, 'd' falls off the 4-wide row: "Xabc".
    assert_eq!(terminal.screen().plain_text(), "Xabc");
}

#[test]
fn irm_reset_restores_replace_mode() {
    // CSI 4 l turns insert mode back off.
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"abc\r\x1b[4h"); // IRM on
    terminal.advance(b"\x1b[4l"); // IRM off
    terminal.advance(b"X");
    assert_eq!(terminal.screen().plain_text(), "Xbc");
}

#[test]
fn irm_is_reset_by_ris_and_decstr() {
    // RIS (ESC c) clears insert mode.
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"\x1b[4h\x1bc");
    terminal.advance(b"abc\r");
    terminal.advance(b"X");
    assert_eq!(terminal.screen().plain_text(), "Xbc", "RIS must clear IRM");

    // DECSTR (CSI ! p) clears insert mode.
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"\x1b[4h\x1b[!p");
    terminal.advance(b"abc\r");
    terminal.advance(b"X");
    assert_eq!(
        terminal.screen().plain_text(),
        "Xbc",
        "DECSTR must clear IRM"
    );
}

#[test]
fn irm_decrqm_reports_live_state() {
    // DECRQM for an ANSI mode: CSI 4 $ p → CSI 4 ; <status> $ y, status 1=set,
    // 2=reset.
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"\x1b[4$p");
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[4;2$y",
        "IRM reset by default"
    );

    terminal.advance(b"\x1b[4h\x1b[4$p");
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[4;1$y",
        "IRM set after CSI 4 h"
    );
}
