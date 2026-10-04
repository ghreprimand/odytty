// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored emoji fixtures. Pure terminal-core checks on all platforms.
//! Ignored regressions specify deliberate G7 behavior without changing the
//! current scalar-width baseline. VS15 and unpaired RI remain compatibility
//! controls. These deliberately differ from the frozen wcwidth 0.9.1
//! reference: it gives U+231A U+FE0E width 1 and a standalone RI width 2.
//! OdyTTY preserves non-demotion (watch width 2) and standalone RI width 1.
//! No font, environment, GPU, or process-global state is involved.

use odytty::core::{
    Position, SearchOptions, SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps,
    Terminal,
};
use odytty::selection::{CellPoint, SelectionRange, selected_text};

fn range(start: usize, end: usize) -> SelectionRange {
    SelectionRange {
        start: CellPoint {
            row: 0,
            column: start,
        },
        end: CellPoint {
            row: 0,
            column: end,
        },
    }
}

fn owner(term: &Terminal, row: usize, column: usize, text: &str) {
    let head = term.screen().cell(row, column).unwrap();
    assert_eq!(head.grapheme(), text, "owner at {row}:{column}");
    assert!(!head.wide_continuation);
    assert!(!head.layout_padding);
    let tail = term.screen().cell(row, column + 1).unwrap();
    assert!(
        tail.wide_continuation,
        "second cell must belong to {text:?}"
    );
    assert!(tail.combining().is_empty());
    assert!(!tail.layout_padding);
}

fn ownership(text: &str) {
    let mut term = Terminal::new(16, 3);
    term.advance(format!("A{text}Z").as_bytes());
    owner(&term, 0, 1, text);
    assert_eq!(term.screen().cell(0, 3).unwrap().grapheme(), "Z");
    assert_eq!(term.screen().cursor(), Position { row: 0, column: 4 });
}

fn streaming(text: &str) {
    let input = format!("A{text}Z");
    for split in 0..=input.len() {
        let mut term = Terminal::new(16, 3);
        term.advance(&input.as_bytes()[..split]);
        term.advance(&input.as_bytes()[split..]);
        owner(&term, 0, 1, text);
        assert_eq!(
            term.screen().cell(0, 3).unwrap().grapheme(),
            "Z",
            "split {split}"
        );
        assert_eq!(
            term.screen().cursor(),
            Position { row: 0, column: 4 },
            "split {split}"
        );
    }
    let mut term = Terminal::new(16, 3);
    for byte in input.bytes() {
        term.advance(&[byte]);
    }
    owner(&term, 0, 1, text);
    assert_eq!(term.screen().cursor(), Position { row: 0, column: 4 });
}

fn right_edge(text: &str) {
    // Every split includes the boundary after a narrow VS16/keycap base that
    // provisionally fills the last column. Promotion must move its owner,
    // retain the original scalars, and leave non-text layout padding.
    let input = format!("abc{text}");
    for split in 0..=input.len() {
        let mut term = Terminal::new(4, 4);
        term.advance(&input.as_bytes()[..split]);
        term.advance(&input.as_bytes()[split..]);
        assert!(
            term.screen().cell(0, 3).unwrap().layout_padding,
            "split {split}"
        );
        assert!(term.visible_search_rows(0)[0].wrapped);
        owner(&term, 1, 0, text);
        assert_eq!(term.screen().cursor(), Position { row: 1, column: 2 });
        assert_eq!(
            term.search(&input, SearchOptions::case_sensitive()).len(),
            1
        );
    }
    let mut term = Terminal::new(4, 4);
    term.advance(format!("ab{text}").as_bytes());
    owner(&term, 0, 2, text);
    assert_eq!(term.screen().cursor(), Position { row: 0, column: 3 });
    term.advance(b"Z");
    assert!(term.visible_search_rows(0)[0].wrapped);
    assert_eq!(term.screen().cell(1, 0).unwrap().grapheme(), "Z");
    owner(&term, 0, 2, text);
}

fn overwrite_and_erase(text: &str) {
    // Both halves of the owner are edit boundaries, including ECH and EL.
    // Each edit must clear all retained scalars and preserve the suffix.
    for column in [1, 2] {
        for command in ["X", "\x1b[X", "\x1b[1K"] {
            let mut term = Terminal::new(16, 3);
            term.advance(format!("{text}Z").as_bytes());
            term.advance(format!("\x1b[1;{column}H{command}").as_bytes());
            for col in 0..2 {
                let cell = term.screen().cell(0, col).unwrap();
                assert!(!cell.wide_continuation);
                assert!(cell.combining().is_empty());
                assert_eq!(
                    cell.grapheme(),
                    if command == "X" && col == column - 1 {
                        "X"
                    } else {
                        " "
                    }
                );
            }
            assert_eq!(term.screen().cell(0, 2).unwrap().grapheme(), "Z");
            assert!(
                term.search(text, SearchOptions::case_sensitive())
                    .is_empty()
            );
        }
    }
}

fn logical_copy_and_search(text: &str) {
    let mut term = Terminal::new(16, 3);
    term.advance(format!("A{text}Z").as_bytes());
    assert_eq!(selected_text(&term.snapshot(), range(1, 1)), text);
    assert_eq!(selected_text(&term.snapshot(), range(1, 2)), text);
    assert_eq!(
        selected_text(&term.snapshot(), range(0, 3)),
        format!("A{text}Z")
    );
    let hits = term.search(text, SearchOptions::case_sensitive());
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].start.row, hits[0].start.column), (0, 1));
    assert_eq!((hits[0].end.row, hits[0].end.column), (0, 2));
    for columns in [4, 7, 3, 16] {
        term.resize(columns, 8);
        assert_eq!(
            term.search(&format!("A{text}Z"), SearchOptions::case_sensitive())
                .len(),
            1
        );
    }
}

fn roundtrip(text: &str) {
    let mut term = Terminal::new(16, 3);
    term.advance(format!("A{text}Z").as_bytes());
    let envelope = SnapshotEnvelope::from_terminal(&term, SnapshotCaptureLimits::default());
    let decoded =
        SnapshotEnvelope::decode(&envelope.encode().unwrap(), SnapshotEnvelopeCaps::default())
            .unwrap();
    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
    owner(&restored, 0, 1, text);
    assert_eq!(restored.snapshot().cells, term.snapshot().cells);
    assert_eq!(restored.screen().cursor(), Position { row: 0, column: 4 });
    assert_eq!(
        selected_text(&restored.snapshot(), range(0, 3)),
        format!("A{text}Z")
    );
}

macro_rules! emoji_case {
    ($name:ident, $text:literal) => {
        mod $name {
            use super::*;
            #[test]
            #[ignore = "G7: emoji cluster must own two cells"]
            fn cell_ownership_and_cursor() {
                ownership($text);
            }
            #[test]
            #[ignore = "G7: cluster extension across every UTF-8 split"]
            fn every_utf8_split() {
                streaming($text);
            }
            #[test]
            #[ignore = "G7: promotion and extension at pending wrap"]
            fn right_edge_pending_wrap() {
                right_edge($text);
            }
            #[test]
            #[ignore = "G7: edits clear the whole emoji owner"]
            fn overwrite_and_erase_cluster() {
                overwrite_and_erase($text);
            }
            #[test]
            #[ignore = "G7: logical copy and search map to the emoji owner"]
            fn logical_copy_search_and_reflow() {
                logical_copy_and_search($text);
            }
            #[test]
            #[ignore = "G7: snapshot retains emoji owner and scalars"]
            fn snapshot_roundtrip() {
                roundtrip($text);
            }
        }
    };
}

emoji_case!(vs16_heart, "\u{2764}\u{fe0f}");
emoji_case!(vs16_smiley, "\u{263a}\u{fe0f}");
emoji_case!(zwj_profession, "\u{1f469}\u{200d}\u{1f4bb}");
emoji_case!(zwj_family, "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}");
emoji_case!(skin_tone, "\u{1f44d}\u{1f3fd}");
emoji_case!(keycap, "1\u{fe0f}\u{20e3}");
emoji_case!(flag_pair, "\u{1f1fa}\u{1f1f8}");

#[test]
fn vs15_does_not_demote_existing_emoji_width() {
    for text in ["\u{231a}\u{fe0e}", "\u{1f600}\u{fe0e}"] {
        ownership(text);
        streaming(text);
        right_edge(text);
        overwrite_and_erase(text);
        logical_copy_and_search(text);
        roundtrip(text);
    }
}

#[test]
fn standalone_regional_indicator_keeps_one_cell() {
    let text = "\u{1f1fa}";
    let input = format!("A{text}Z");
    for split in 0..=input.len() {
        let mut term = Terminal::new(8, 3);
        term.advance(&input.as_bytes()[..split]);
        term.advance(&input.as_bytes()[split..]);
        assert_eq!(term.screen().cell(0, 1).unwrap().grapheme(), text);
        assert!(!term.screen().cell(0, 2).unwrap().wide_continuation);
        assert_eq!(term.screen().cell(0, 2).unwrap().grapheme(), "Z");
        assert_eq!(term.screen().cursor(), Position { row: 0, column: 3 });
        assert_eq!(selected_text(&term.snapshot(), range(1, 1)), text);
        assert_eq!(term.search(text, SearchOptions::case_sensitive()).len(), 1);
        let envelope = SnapshotEnvelope::from_terminal(&term, SnapshotCaptureLimits::default());
        let decoded =
            SnapshotEnvelope::decode(&envelope.encode().unwrap(), SnapshotEnvelopeCaps::default())
                .unwrap();
        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
        assert_eq!(restored.snapshot().cells, term.snapshot().cells);
        assert_eq!(restored.screen().cursor(), term.screen().cursor());
    }
}

#[test]
fn keycap_without_vs16_retains_one_cell_and_logical_text() {
    let text = "1\u{20e3}";
    let input = format!("A{text}Z");
    for split in 0..=input.len() {
        let mut term = Terminal::new(8, 3);
        term.advance(&input.as_bytes()[..split]);
        term.advance(&input.as_bytes()[split..]);
        let cell = term.screen().cell(0, 1).unwrap();
        assert_eq!(cell.grapheme(), text, "split {split}");
        assert_eq!(cell.combining(), &['\u{20e3}']);
        assert!(!cell.wide_continuation);
        assert!(!term.screen().cell(0, 2).unwrap().wide_continuation);
        assert_eq!(term.screen().cell(0, 2).unwrap().grapheme(), "Z");
        assert_eq!(term.screen().cursor(), Position { row: 0, column: 3 });
        assert_eq!(selected_text(&term.snapshot(), range(1, 1)), text);
        assert_eq!(selected_text(&term.snapshot(), range(0, 2)), input);
        let hits = term.search(text, SearchOptions::case_sensitive());
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].start.row, hits[0].start.column), (0, 1));
        assert_eq!((hits[0].end.row, hits[0].end.column), (0, 1));
        let envelope = SnapshotEnvelope::from_terminal(&term, SnapshotCaptureLimits::default());
        let decoded =
            SnapshotEnvelope::decode(&envelope.encode().unwrap(), SnapshotEnvelopeCaps::default())
                .unwrap();
        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
        assert_eq!(restored.snapshot().cells, term.snapshot().cells);
        assert_eq!(restored.screen().cursor(), term.screen().cursor());
        assert_eq!(selected_text(&restored.snapshot(), range(1, 1)), text);
        assert_eq!(
            restored.search(text, SearchOptions::case_sensitive()).len(),
            1
        );
    }
}
