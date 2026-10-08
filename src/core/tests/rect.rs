// SPDX-License-Identifier: GPL-3.0-only
use super::*;

fn row_text(terminal: &Terminal, row: usize) -> String {
    let columns = terminal.screen().dimensions().columns;
    (0..columns)
        .map(|column| terminal.screen().cell(row, column).unwrap().ch)
        .collect()
}

fn all_rows(terminal: &Terminal) -> Vec<String> {
    let rows = terminal.screen().dimensions().rows;
    (0..rows).map(|row| row_text(terminal, row)).collect()
}

fn cell_attrs(terminal: &Terminal, row: usize, column: usize) -> Attrs {
    terminal.screen().cell(row, column).unwrap().attrs
}

fn all_attrs(terminal: &Terminal) -> Vec<Attrs> {
    let dimensions = terminal.screen().dimensions();
    let mut attrs = Vec::with_capacity(dimensions.rows * dimensions.columns);
    for row in 0..dimensions.rows {
        for column in 0..dimensions.columns {
            attrs.push(cell_attrs(terminal, row, column));
        }
    }
    attrs
}

fn assert_bold_mask(terminal: &Terminal, expected: impl Fn(usize, usize) -> bool) {
    for row in 0..terminal.screen().dimensions().rows {
        for column in 0..terminal.screen().dimensions().columns {
            assert_eq!(
                terminal.screen().cell(row, column).unwrap().attrs.bold(),
                expected(row, column),
                "row {row}, column {column}"
            );
        }
    }
}

#[test]
fn decsca_marks_printed_cells_and_regular_erase_ignores_protection() {
    let mut terminal = Terminal::new(6, 2);

    terminal.advance(b"a\x1b[1\"qbc\x1b[0\"qdef");
    terminal.advance(b"\x1b[1;1H\x1b[2K");

    assert_eq!(row_text(&terminal, 0), "      ");
    assert!((0..6).all(|column| !terminal.screen().cell(0, column).unwrap().protected));
}

#[test]
fn decsel_and_decsed_preserve_protected_cells() {
    let mut terminal = Terminal::new(6, 3);

    terminal.advance(b"a\x1b[1\"qbc\x1b[0\"qdef");
    terminal.advance(b"\x1b[1;2H\x1b[?K");

    assert_eq!(row_text(&terminal, 0), "abc   ");
    assert!(terminal.screen().cell(0, 1).unwrap().protected);
    assert!(terminal.screen().cell(0, 2).unwrap().protected);

    terminal.advance(b"\x1b[2;1H123456");
    terminal.advance(b"\x1b[1\"q\x1b[3;1HUV\x1b[0\"qWX");
    terminal.advance(b"\x1b[1;1H\x1b[?2J");

    assert_eq!(all_rows(&terminal), vec![" bc   ", "      ", "UV    "]);
}

#[test]
fn decfra_fills_with_current_attrs_and_protection() {
    let mut terminal = Terminal::new(6, 4);

    terminal.advance(b"\x1b[31m\x1b[1\"q\x1b[65;2;2;3;4$x");

    for row in 1..=2 {
        for column in 1..=3 {
            let cell = terminal.screen().cell(row, column).unwrap();
            assert_eq!(cell.ch, 'A');
            assert_eq!(cell.attrs.foreground, Color::Indexed(1));
            assert!(cell.protected);
        }
    }

    terminal.advance(b"\x1b[0\"q\x1b[2;2;3;4${");

    assert_eq!(terminal.screen().cell(1, 1).unwrap().ch, 'A');
    assert!(terminal.screen().cell(1, 1).unwrap().protected);
}

#[test]
fn decera_and_decsera_respect_protection_matrix() {
    let mut terminal = Terminal::new(6, 3);

    terminal.advance(b"\x1b[1;1Haa");
    terminal.advance(b"\x1b[1\"qBB\x1b[0\"qcc");
    terminal.advance(b"\x1b[1;1;1;6${");

    assert_eq!(row_text(&terminal, 0), "  BB  ");

    terminal.advance(b"\x1b[1;1;1;6$z");

    assert_eq!(row_text(&terminal, 0), "      ");
}

#[test]
fn deccra_copy_overlap_left_right_up_and_down() {
    let mut right = Terminal::new(6, 1);
    right.advance(b"abcdef\x1b[1;1;1;4;1;1;3$v");
    assert_eq!(row_text(&right, 0), "ababcd");

    let mut left = Terminal::new(6, 1);
    left.advance(b"abcdef\x1b[1;3;1;6;1;1;1$v");
    assert_eq!(row_text(&left, 0), "cdefef");

    let mut down = Terminal::new(4, 4);
    down.advance(b"1111222233334444\x1b[1;1;3;4;1;2;1$v");
    assert_eq!(all_rows(&down), vec!["1111", "1111", "2222", "3333"]);

    let mut up = Terminal::new(4, 4);
    up.advance(b"1111222233334444\x1b[2;1;4;4;1;1;1$v");
    assert_eq!(all_rows(&up), vec!["2222", "3333", "4444", "4444"]);
}

#[test]
fn rectangle_ops_clamp_to_bounds_and_origin_region() {
    let mut terminal = Terminal::new(6, 5);

    terminal.advance(b"aaaaaabbbbbbccccccddddddeeeeee");
    terminal.advance(b"\x1b[2;4r\x1b[?6h");
    terminal.advance(b"\x1b[88;1;1;99;99$x");

    assert_eq!(
        all_rows(&terminal),
        vec!["aaaaaa", "XXXXXX", "XXXXXX", "XXXXXX", "eeeeee"]
    );
}

#[test]
fn degenerate_rectangles_are_noops() {
    let mut terminal = Terminal::new(4, 2);

    terminal.advance(b"abcdEFGH\x1b[2;4;1;1$z\x1b[70;2;4;1;1$x\x1b[2;4;1;1$v");

    assert_eq!(all_rows(&terminal), vec!["abcd", "EFGH"]);
}

#[test]
fn rectangle_edge_splitting_wide_pair_clears_the_pair() {
    let mut terminal = Terminal::new(5, 1);

    terminal.advance("a世b ".as_bytes());
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, '世');
    assert!(terminal.screen().cell(0, 2).unwrap().wide_continuation);

    terminal.advance(b"\x1b[1;3;1;3$z");

    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, ' ');
    assert_eq!(terminal.screen().cell(0, 2).unwrap().ch, ' ');
    assert!(!terminal.screen().cell(0, 1).unwrap().wide_continuation);
    assert!(!terminal.screen().cell(0, 2).unwrap().wide_continuation);
}

#[test]
fn attribute_rect_ops_respect_extent_and_origin_matrix() {
    for final_byte in *b"rt" {
        for exact in [false, true] {
            for origin in [false, true] {
                let mut terminal = Terminal::new(5, 5);
                if origin {
                    terminal.advance(b"\x1b[2;4r\x1b[?6h");
                }
                terminal.advance(if exact { b"\x1b[2*x" } else { b"\x1b[0*x" });
                terminal.advance(&format!("\x1b[1;3;3;4;1${}", final_byte as char).into_bytes());

                let top = if origin { 1 } else { 0 };
                let bottom = if origin { 3 } else { 2 };
                let expected = |row: usize, column: usize| {
                    if exact {
                        (top..=bottom).contains(&row) && (2..=3).contains(&column)
                    } else if row == top {
                        column >= 2
                    } else if row == bottom {
                        column <= 3
                    } else {
                        (top..bottom).contains(&row)
                    }
                };
                assert_bold_mask(&terminal, expected);
            }
        }
    }
}

#[test]
fn deccara_changes_supported_attrs_and_resets_them_individually() {
    let mut terminal = Terminal::new(4, 2);

    terminal.advance(b"\x1b[2*x\x1b[1;1;2;4;1;4;5;7$r");

    for row in 0..2 {
        for column in 0..4 {
            let attrs = cell_attrs(&terminal, row, column);
            assert!(attrs.bold());
            assert_eq!(attrs.effective_underline_style(), UnderlineStyle::Straight);
            assert!(attrs.blink());
            assert!(attrs.inverse());
        }
    }

    terminal.advance(b"\x1b[1;1;2;4;22;24;25;27$r");

    for row in 0..2 {
        for column in 0..4 {
            assert_eq!(cell_attrs(&terminal, row, column), Attrs::default());
        }
    }
}

#[test]
fn decrara_double_application_restores_original_attrs() {
    let mut terminal = Terminal::new(4, 2);

    terminal.advance(b"\x1b[2;3;8;9;38;2;1;2;3;48;5;27;58;5;42mAB\x1b[2*x\x1b[1;1;2;4;1;4;5;7$r");
    let before = all_attrs(&terminal);

    terminal.advance(b"\x1b[1;1;2;4;0$t");
    let middle = all_attrs(&terminal);
    for (original, reversed) in before.iter().zip(&middle) {
        let mut expected = *original;
        expected.set_bold(false);
        expected.set_underline_style(UnderlineStyle::None);
        expected.set_blink(false);
        expected.set_inverse(false);
        assert_eq!(*reversed, expected);
    }
    assert_ne!(middle, before);
    terminal.advance(b"\x1b[1;1;2;4;0$t");

    let after = all_attrs(&terminal);
    assert_eq!(after, before);
}

#[test]
fn deccara_and_decrara_ignore_decsca_protection() {
    let mut terminal = Terminal::new(4, 1);

    terminal.advance(b"\x1b[1\"qAB\x1b[0\"q");
    terminal.advance(b"\x1b[1;1;1;2;1$r");

    for column in 0..2 {
        let cell = terminal.screen().cell(0, column).unwrap();
        assert!(cell.protected);
        assert!(cell.attrs.bold());
    }

    terminal.advance(b"\x1b[1;1;1;2;1$t");

    for column in 0..2 {
        let cell = terminal.screen().cell(0, column).unwrap();
        assert!(cell.protected);
        assert!(!cell.attrs.bold());
    }
}

#[test]
fn deccara_uses_plain_underline_only() {
    let mut terminal = Terminal::new(2, 1);

    terminal.advance(b"\x1b[1;1;1;1;4:3$r\x1b[1;2;1;2;4$r");

    assert_eq!(
        cell_attrs(&terminal, 0, 0).effective_underline_style(),
        UnderlineStyle::None
    );
    assert_eq!(
        cell_attrs(&terminal, 0, 1).effective_underline_style(),
        UnderlineStyle::Straight
    );
}

#[test]
fn deccara_changes_wide_pair_attrs_per_cell_without_splitting() {
    let mut terminal = Terminal::new(5, 1);

    terminal.advance("a世b ".as_bytes());
    terminal.advance(b"\x1b[2*x\x1b[1;3;1;3;1$r");

    assert_eq!(row_text(&terminal, 0), "a世 b ");
    assert!(!terminal.screen().cell(0, 1).unwrap().attrs.bold());
    assert!(terminal.screen().cell(0, 2).unwrap().wide_continuation);
    assert!(terminal.screen().cell(0, 2).unwrap().attrs.bold());
}

#[test]
fn decsace_resets_to_stream_on_decstr_and_ris() {
    for reset in [b"\x1b[!p".as_slice(), b"\x1bc".as_slice()] {
        let mut terminal = Terminal::new(5, 3);

        terminal.advance(b"\x1b[2*x");
        terminal.advance(reset);
        terminal.advance(b"\x1b[1;3;3;4;1$r");

        assert_bold_mask(&terminal, |row, column| {
            if row == 0 {
                column >= 2
            } else if row == 2 {
                column <= 3
            } else {
                row == 1
            }
        });
    }
}

// Project-authored ASCII fixtures exercise parser writes and subsequent reflow.
fn assert_edge_write_severs(sequence: &[u8], expected: &str) {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"abcdef");
    assert!(terminal.visible_search_rows(0)[0].wrapped);
    terminal.advance(sequence);
    assert_eq!(row_text(&terminal, 0), expected);
    assert!(!terminal.visible_search_rows(0)[0].wrapped);
    terminal.resize(8, 3);
    assert_eq!(row_text(&terminal, 0).trim_end(), expected.trim_end());
    assert_eq!(row_text(&terminal, 1).trim_end(), "ef");
}

#[test]
fn decsel_edge_erase_severs_before_reflow() {
    for sequence in [b"\x1b[1;3H\x1b[?K".as_slice(), b"\x1b[1;3H\x1b[?2K"] {
        assert_edge_write_severs(
            sequence,
            if sequence.ends_with(b"?2K") {
                "    "
            } else {
                "ab  "
            },
        );
    }
}

#[test]
fn decsed_edge_erase_severs_before_reflow() {
    // Protect the continuation so DECSED 0 retains it while erasing row 0.
    assert_edge_write_severs(b"\x1b[1\"q\x1b[2;1Hef\x1b[0\"q\x1b[1;3H\x1b[?J", "ab  ");
}

#[test]
fn decera_edge_erase_severs_before_reflow() {
    assert_edge_write_severs(b"\x1b[1;3;1;4$z", "ab  ");
}

#[test]
fn decsera_edge_erase_severs_before_reflow() {
    assert_edge_write_severs(b"\x1b[1;3;1;4${", "ab  ");
}

#[test]
fn decfra_edge_fill_severs_before_reflow() {
    assert_edge_write_severs(b"\x1b[88;1;3;1;4$x", "abXX");
}

#[test]
fn deccra_destination_edge_severs_before_reflow() {
    assert_edge_write_severs(b"\x1b[2;1;2;2;1;1;3$v", "abef");
}

#[test]
fn interior_writes_and_attribute_changes_preserve_wrap() {
    for sequence in [
        b"\x1b[1;2H\x1b[?1K".as_slice(),
        b"\x1b[1;2;1;3$z",
        b"\x1b[1;2;1;3${",
        b"\x1b[88;1;2;1;3$x",
        b"\x1b[2;1;2;2;1;1;2$v",
        b"\x1b[1;1;1;4;1$r",
        b"\x1b[1;1;1;4;1$t",
    ] {
        let mut terminal = Terminal::new(4, 3);
        terminal.advance(b"abcdef");
        terminal.advance(sequence);
        assert!(terminal.visible_search_rows(0)[0].wrapped, "{sequence:?}");
    }
}

#[test]
fn selective_erase_preserves_protected_edge_but_severs_unprotected_edge() {
    for sequence in [
        b"\x1b[1;1H\x1b[?2K".as_slice(),
        b"\x1b[1;1;1;4${",
        b"\x1b[1;1H\x1b[?J",
    ] {
        let mut terminal = Terminal::new(4, 3);
        terminal.advance(b"ab\x1b[1\"qcd\x1b[0\"qef");
        terminal.advance(sequence);
        assert_eq!(row_text(&terminal, 0), "  cd");
        assert!(terminal.visible_search_rows(0)[0].wrapped);
        let mut terminal = Terminal::new(4, 3);
        terminal.advance(b"\x1b[1\"qab\x1b[0\"qcdef");
        terminal.advance(sequence);
        assert_eq!(row_text(&terminal, 0), "ab  ");
        assert!(!terminal.visible_search_rows(0)[0].wrapped);
    }
}

#[test]
fn wide_pair_repair_at_edge_severs_wrap() {
    for sequence in [
        b"\x1b[1;3;1;3$z".as_slice(),
        b"\x1b[1;3;1;3${",
        b"\x1b[88;1;3;1;3$x",
        b"\x1b[2;1;2;1;1;1;3$v",
        b"\x1b[1;3H\x1b[?1K",
    ] {
        let mut terminal = Terminal::new(4, 3);
        terminal.advance("ab\u{4e16}ef".as_bytes());
        terminal.advance(sequence);
        assert!(!terminal.screen().cell(0, 3).unwrap().wide_continuation);
        assert!(!terminal.visible_search_rows(0)[0].wrapped, "{sequence:?}");
        terminal.resize(8, 3);
        assert_eq!(row_text(&terminal, 1).trim_end(), "ef");
    }
}
