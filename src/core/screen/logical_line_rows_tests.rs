// SPDX-License-Identifier: GPL-3.0-only
//! [`Screen::logical_line_rows`] finds the same first and last rows as a walk
//! over the wrap flags of every row, for every row of terminals fed generated
//! streams: long and short soft-wrapped lines, hard breaks, wide characters,
//! a reflowing resize, a history cap that evicts line heads, and the
//! alternate screen.

use super::*;
use crate::core::Terminal;

/// The wrap flag of every absolute row, history first, as the window walk
/// it replaced read them.
fn wrap_flags(screen: &Screen) -> Vec<bool> {
    let columns = screen.dimensions.columns;
    let history = screen.scrollback.physical_len(columns);
    let mut flags: Vec<bool> = screen
        .scrollback
        .physical_tail(columns, history)
        .iter()
        .map(|line| line.wrapped)
        .collect();
    flags.extend(screen.rows.iter().map(|line| line.wrapped));
    flags
}

fn reference(flags: &[bool], row: usize) -> (usize, usize) {
    let row = row.min(flags.len() - 1);
    let mut first = row;
    while first > 0 && flags[first - 1] {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < flags.len() && flags[last] {
        last += 1;
    }
    (first, last)
}

fn stream(seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let mut out = String::new();
    for _ in 0..40 {
        let length = match next() % 4 {
            0 => 0,
            1 => next() % 30,
            2 => next() % 200,
            _ => next() % 900,
        };
        for i in 0..length {
            if next() % 23 == 0 {
                out.push('\u{4e16}');
            } else {
                out.push(char::from(b'a' + (i % 26) as u8));
            }
        }
        out.push_str("\r\n");
    }
    out.into_bytes()
}

fn check(terminal: &Terminal, label: &str) {
    let screen = terminal.screen();
    let flags = wrap_flags(screen);
    for row in 0..flags.len() + 2 {
        assert_eq!(
            screen.logical_line_rows(row),
            reference(&flags, row),
            "{label}: row {row} of {}",
            flags.len()
        );
    }
}

#[test]
fn logical_line_rows_match_the_wrap_flag_walk() {
    for seed in 0..24u64 {
        let columns = 7 + (seed as usize % 5) * 9;
        let mut terminal = Terminal::new(columns, 6);
        if seed % 3 == 0 {
            terminal.set_scrollback_limit(40);
        }
        terminal.advance(&stream(seed));
        check(&terminal, &format!("seed {seed}"));
        // The open tail: the last line still wrapping at the bottom.
        terminal.advance(&b"q".repeat(columns * 3 + 1));
        check(&terminal, &format!("seed {seed} open tail"));
        terminal.resize(columns + 5, 6);
        check(&terminal, &format!("seed {seed} resized"));
        terminal.advance(b"\x1b[?1049h");
        terminal.advance(&stream(seed + 100)[..200]);
        check(&terminal, &format!("seed {seed} alternate"));
    }
}
