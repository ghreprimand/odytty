// SPDX-License-Identifier: GPL-3.0-only
//! The conformance replay fixtures under `compat/vttest/replay` describe the
//! screen their comments promise, but the vttest harness cannot read a grid
//! back, so their outcome is capped at `ignore`. This suite replays the two
//! fixtures whose comments make checkable promises through the owned terminal
//! model and asserts those promises, so a fixture that destroys its own check
//! (or leans on a control the terminal does not implement) fails here.
//!
//! The fixture grammar is the one the file headers state: `#` comment lines,
//! blank lines skipped, `\e \r \n \t \\ \xNN` escapes, no implicit newline.

use std::path::Path;

use odytty::core::Terminal;

fn assemble(line: &str) -> Vec<u8> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let escape = bytes[index + 1];
        index += 2;
        match escape {
            b'e' => out.push(0x1b),
            b'r' => out.push(b'\r'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'\\' => out.push(b'\\'),
            b'x' => {
                let digits = std::str::from_utf8(&bytes[index..index + 2]).expect("hex digits");
                out.push(u8::from_str_radix(digits, 16).expect("hex escape"));
                index += 2;
            }
            other => panic!("unknown escape \\{}", other as char),
        }
    }
    out
}

/// Replay `compat/vttest/replay/<name>.vtseq` on the baseline 24x80 grid.
fn replay(name: &str) -> Terminal {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("compat/vttest/replay")
        .join(format!("{name}.vtseq"));
    let text = std::fs::read_to_string(&path).expect("read replay fixture");
    let mut terminal = Terminal::new(80, 24);
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        terminal.advance(&assemble(line));
    }
    terminal
}

fn rows(terminal: &Terminal) -> Vec<String> {
    terminal
        .screen()
        .plain_text()
        .split('\n')
        .map(str::to_owned)
        .collect()
}

#[test]
fn cursor_addressing_clamps_without_wrapping_or_scrolling() {
    let terminal = replay("cursor-addressing");
    assert_eq!(
        terminal.screen().scrollback_len(),
        0,
        "the clamp check must not scroll the screen"
    );
    let rows = rows(&terminal);
    // The clamped print completes the bottom-right marker on the last row, and
    // every later check lands on the documented baseline.
    assert_eq!(rows[23], format!("BL{}BR", " ".repeat(76)));
    assert_eq!(rows[0], format!("clamp-min{}TR", " ".repeat(69)));
    assert_eq!(rows[9], format!("{}origin", " ".repeat(9)));
    assert_eq!(rows[11], format!("{}restored", " ".repeat(39)));
    let cursor = terminal.screen().cursor();
    assert_eq!((cursor.row, cursor.column), (23, 0));
}

#[test]
fn tab_stops_set_clear_and_return_to_the_eight_column_rhythm() {
    let terminal = replay("tab-stops");
    let rows = rows(&terminal);
    // Default stops: four tabs from column 1 land on columns 9, 17, 25, 33.
    assert_eq!(
        rows[1],
        format!(
            "{}a{}b{}c{}d",
            " ".repeat(8),
            " ".repeat(7),
            " ".repeat(7),
            " ".repeat(7)
        )
    );
    // Two explicit stops at columns 20 and 50.
    assert_eq!(
        rows[4],
        format!("{}first{}second", " ".repeat(19), " ".repeat(25))
    );
    // One stop cleared: the next tab from column 1 skips column 20.
    assert_eq!(rows[7], format!("{}remaining", " ".repeat(49)));
    // The explicit stops that replace the default ones restore the rhythm.
    assert_eq!(
        rows[9],
        format!(
            "{}w{}x{}y{}z",
            " ".repeat(8),
            " ".repeat(7),
            " ".repeat(7),
            " ".repeat(7)
        )
    );
    // A tab over existing content does not overwrite the cells it skips.
    assert_eq!(rows[11], "abcdefghTABlmnop");
}
