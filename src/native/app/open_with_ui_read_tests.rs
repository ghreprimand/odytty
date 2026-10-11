// SPDX-License-Identifier: GPL-3.0-only
//! The capped desktop-file reader keeps complete lines only: a command line cut
//! by the cap is never offered, and a cut inside a multi-byte scalar keeps the
//! lines before it.

use std::io::Cursor;

use super::read_capped_lines;

const ENTRY: &str = "[Desktop Entry]\nName=Viewer\nType=Application\n";

#[test]
fn a_source_within_the_cap_is_returned_whole_even_without_a_final_newline() {
    let text = format!("{ENTRY}Exec=viewer %f");
    let read = read_capped_lines(Cursor::new(text.as_bytes()), text.len() as u64);
    assert_eq!(read.as_deref(), Some(text.as_str()));
}

#[test]
fn an_exec_line_cut_by_the_cap_is_dropped_not_offered_as_complete() {
    let text = format!("{ENTRY}Exec=viewer --flag-that-is-cut-in-the-middle %f\n");
    // Cut inside the argument, after `Exec=viewer --fl`.
    let cut = ENTRY.len() + "Exec=viewer --fl".len();
    let read = read_capped_lines(Cursor::new(text.as_bytes()), cut as u64).expect("kept lines");
    assert_eq!(read, ENTRY, "the partial Exec line must not survive");
    assert!(!read.contains("Exec"));
}

#[test]
fn a_cut_inside_a_multibyte_scalar_keeps_the_complete_lines_before_it() {
    // U+00E9 is two bytes; cap the read between them.
    let text = format!("{ENTRY}Comment=caf\u{e9} au lait\n");
    let split = ENTRY.len() + "Comment=caf".len() + 1;
    assert!(
        !text.is_char_boundary(split),
        "the cap must split the scalar"
    );
    let read = read_capped_lines(Cursor::new(text.as_bytes()), split as u64)
        .expect("a scalar cut must not lose the whole entry");
    assert_eq!(read, ENTRY);
}

#[test]
fn a_long_source_with_no_complete_line_in_the_cap_is_refused() {
    let text = "x".repeat(64);
    assert_eq!(read_capped_lines(Cursor::new(text.as_bytes()), 16), None);
}

#[test]
fn a_source_of_exactly_the_cap_is_not_treated_as_cut() {
    let text = format!("{ENTRY}Exec=viewer %f\n");
    let read = read_capped_lines(Cursor::new(text.as_bytes()), text.len() as u64);
    assert_eq!(read.as_deref(), Some(text.as_str()));
}

#[test]
fn text_that_is_not_utf8_is_refused() {
    assert_eq!(read_capped_lines(Cursor::new(&[0xff, b'\n'][..]), 16), None);
}
