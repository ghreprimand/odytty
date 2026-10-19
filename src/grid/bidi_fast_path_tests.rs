// SPDX-License-Identifier: GPL-3.0-only
//! The left-to-right identity skip in the display map: a paragraph that is
//! not right to left, is in implicit mode, and holds no scalar that can raise
//! a level is left as the identity layout without building its owners. The
//! map with the skip equals the full plan across directions, modes, wrapped
//! paragraphs opening above the viewport, marks, wide cells, and every class
//! that raises a level; plain text takes the skip, and right-to-left input
//! does not.

use super::bidi::fast_path;
use super::*;
use crate::core::{BidiBaseDirection, Terminal};

const COLUMNS: usize = 11;
const ROWS: usize = 5;

/// The map the live frame plans at scrollback `offset`.
fn plan(terminal: &Terminal, offset: usize, default: BidiBaseDirection) -> BidiDisplayMap {
    let snapshot = terminal.snapshot_with_scrollback(offset);
    let (wrapped, properties): (Vec<bool>, Vec<_>) = terminal
        .visible_search_rows(offset)
        .iter()
        .map(|row| (row.wrapped, row.paragraph_bidi))
        .unzip();
    let (rows, overflow) = terminal.paragraph_context_rows(offset, max_bidi_context_rows(COLUMNS));
    let context = BidiParagraphContext {
        properties: rows
            .first()
            .map(|row| row.paragraph_bidi)
            .unwrap_or_default(),
        rows: rows.into_iter().map(|row| row.cells).collect(),
        overflow,
    };
    BidiDisplayMap::plan_with_paragraphs(
        &snapshot,
        &wrapped,
        &context,
        BidiParagraphs {
            properties: &properties,
            default_direction: default,
        },
    )
}

/// Left-to-right text, neutrals, numbers, marks, and wide cells, then every
/// class that raises a level: Hebrew, Arabic, an Arabic-Indic digit, RLM,
/// and the embedding, override, and isolate controls.
const PLAIN: &[&str] = &[
    "a", "Z", "7", " ", ".", "(", ")", "-", "e\u{301}", "\u{4e2d}", "\u{e9}", "\u{200e}",
];
const RAISING: &[&str] = &[
    "\u{5d0}",
    "\u{5d0}\u{5b0}",
    "\u{628}",
    "\u{661}",
    "\u{200f}",
    "\u{202b}",
    "\u{202a}",
    "\u{202e}",
    "\u{202d}",
    "\u{202c}",
    "\u{2067}",
    "\u{2066}",
    "\u{2068}",
    "\u{2069}",
];
const ESCAPES: &[&str] = &[
    "\r\n", "\r\n", "\x1b[2 k", "\x1b[1 k", "\x1b[0 k", "\x1b[8l", "\x1b[8h",
];

/// A deterministic mix of plain text, raising scalars (`raising` in 1,000),
/// line breaks, and BiDi escape sequences.
fn corpus(seed: u64, raising: u64) -> String {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let mut next = move |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    let mut text = String::new();
    for _ in 0..400 {
        let pick = next(1000);
        if pick < raising {
            text.push_str(RAISING[next(RAISING.len() as u64) as usize]);
        } else if pick < raising + 25 {
            text.push_str(ESCAPES[next(ESCAPES.len() as u64) as usize]);
        } else {
            text.push_str(PLAIN[next(PLAIN.len() as u64) as usize]);
        }
    }
    text
}

#[test]
fn identity_skip_matches_the_full_plan() {
    for seed in 0..48 {
        // Half the corpora hold no raising scalar at all.
        let raising = if seed % 2 == 0 { 0 } else { 1 + seed % 7 * 4 };
        let mut terminal = Terminal::new(COLUMNS, ROWS);
        terminal.advance(corpus(seed, raising).as_bytes());
        let history = terminal.screen().scrollback_len();
        for offset in [0, 1, 3, history / 2, history] {
            for default in [
                BidiBaseDirection::LeftToRight,
                BidiBaseDirection::RightToLeft,
                BidiBaseDirection::Auto,
            ] {
                let skipped = plan(&terminal, offset, default);
                let full = fast_path::without(|| plan(&terminal, offset, default));
                assert_eq!(
                    skipped, full,
                    "seed {seed}, offset {offset}, default {default:?}"
                );
            }
        }
    }
}

#[test]
fn plain_text_takes_the_identity_skip() {
    let mut terminal = Terminal::new(COLUMNS, ROWS);
    terminal.advance(b"abc (abc) 123 abc xy abc (abc) 123\r\nplain\r\n");
    for default in [BidiBaseDirection::LeftToRight, BidiBaseDirection::Auto] {
        let before = fast_path::taken_count();
        let map = plan(&terminal, 0, default);
        assert!(map.is_identity());
        assert!(
            fast_path::taken_count() > before,
            "{default:?}: the plain paragraphs skip the full plan"
        );
    }
}

#[test]
fn right_to_left_input_takes_the_full_plan() {
    let cases: &[(&str, BidiBaseDirection)] = &[
        ("abc \u{5d0}\u{5d1}", BidiBaseDirection::LeftToRight),
        ("abc \u{661}\u{662}", BidiBaseDirection::Auto),
        ("abc \u{2067}x\u{2069}", BidiBaseDirection::LeftToRight),
        ("abc", BidiBaseDirection::RightToLeft),
        ("\x1b[2 kabc", BidiBaseDirection::LeftToRight),
        ("\x1b[8labc", BidiBaseDirection::LeftToRight),
    ];
    for (text, default) in cases {
        // One row, so the text's paragraph is the only one.
        let mut terminal = Terminal::new(COLUMNS, 1);
        terminal.advance(text.as_bytes());
        let before = fast_path::taken_count();
        let skipped = plan(&terminal, 0, *default);
        assert_eq!(
            fast_path::taken_count(),
            before,
            "{text:?} with {default:?}"
        );
        assert_eq!(
            skipped,
            fast_path::without(|| plan(&terminal, 0, *default)),
            "{text:?} with {default:?}"
        );
    }
}
