// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored owner regressions for scripts outside the tuned groups:
//! Unicode 17 extended grapheme clusters plus the generalized virama link.
//! Widths follow the frozen width oracle (wcwidth 0.9.1 `wcswidth`).
//! Escaped scalar sequences only; no corpus prose.
use odytty::core::{
    Position, SearchOptions, SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps,
    Terminal,
};
use odytty::selection::{CellPoint, SelectionRange, selected_text, selected_text_block};

/// Each newly formed class, as one two-cell owner.
const CLASSES: [(&str, &str); 8] = [
    ("balinese spacing vowel", "\u{1b13}\u{1b3e}"),
    ("balinese two spacing vowels", "\u{1b13}\u{1b3e}\u{1b3f}"),
    ("balinese adeg-adeg conjunct", "\u{1b13}\u{1b44}\u{1b13}"),
    ("newa conjunct", "\u{11412}\u{11442}\u{11412}"),
    (
        "newa three-consonant conjunct",
        "\u{11412}\u{11442}\u{11412}\u{11442}\u{11412}",
    ),
    ("sharada conjunct", "\u{11191}\u{111c0}\u{11191}"),
    ("prepended concatenation mark", "\u{0600}\u{0661}"),
    ("lepcha spacing vowel", "\u{1c00}\u{1c24}"),
];

fn restored(t: &Terminal) -> Terminal {
    let e = SnapshotEnvelope::from_terminal(t, SnapshotCaptureLimits::default());
    Terminal::from_snapshot_envelope(
        &SnapshotEnvelope::decode(&e.encode().unwrap(), SnapshotEnvelopeCaps::default()).unwrap(),
    )
    .unwrap()
}

fn owners(t: &Terminal, row: usize) -> Vec<String> {
    (0..t.screen().dimensions().columns)
        .filter_map(|column| t.screen().cell(row, column))
        .filter(|cell| !cell.wide_continuation && !cell.layout_padding && cell.ch != ' ')
        .map(|cell| cell.grapheme())
        .collect()
}

fn owner(t: &Terminal, row: usize, col: usize, text: &str) {
    let lead = t.screen().cell(row, col).unwrap();
    assert_eq!(lead.grapheme(), text);
    assert!(!lead.wide_continuation);
    let tail = t.screen().cell(row, col + 1).unwrap();
    assert!(tail.wide_continuation, "{text:?} is two cells");
    assert!(tail.combining().is_empty());
}

fn range(row: usize, start: usize, end: usize) -> SelectionRange {
    SelectionRange {
        start: CellPoint { row, column: start },
        end: CellPoint { row, column: end },
    }
}

#[test]
fn balinese_consonant_with_spacing_vowel_is_one_owner() {
    let mut t = Terminal::new(10, 2);
    t.advance("\u{1b13}\u{1b3e}".as_bytes());
    owner(&t, 0, 0, "\u{1b13}\u{1b3e}");
    assert_eq!(t.screen().cursor(), Position { row: 0, column: 2 });
}

#[test]
fn base_with_two_spacing_marks_caps_at_two_cells() {
    let mut t = Terminal::new(10, 2);
    t.advance("\u{1b13}\u{1b3e}\u{1b3f}|".as_bytes());
    owner(&t, 0, 0, "\u{1b13}\u{1b3e}\u{1b3f}");
    assert_eq!(t.screen().cell(0, 2).unwrap().ch, '|');
}

#[test]
fn balinese_virama_links_following_consonant() {
    // U+1B44 is a spacing virama: the link joins the next consonant, and the
    // conjunct takes two cells where three separate owners took three.
    let mut t = Terminal::new(10, 2);
    t.advance("\u{1b13}\u{1b44}\u{1b13}|".as_bytes());
    owner(&t, 0, 0, "\u{1b13}\u{1b44}\u{1b13}");
    assert_eq!(t.screen().cell(0, 2).unwrap().ch, '|');
}

#[test]
fn newa_conjunct_chain_is_one_two_cell_owner() {
    let text = "\u{11412}\u{11442}\u{11412}\u{11442}\u{11412}";
    let mut t = Terminal::new(10, 2);
    t.advance(text.as_bytes());
    owner(&t, 0, 0, text);
    assert_eq!(t.screen().cursor(), Position { row: 0, column: 2 });
}

#[test]
fn prepend_concatenation_mark_owns_following_digits() {
    let mut t = Terminal::new(10, 2);
    t.advance("\u{0600}\u{0661}|".as_bytes());
    owner(&t, 0, 0, "\u{0600}\u{0661}");
    assert_eq!(t.screen().cell(0, 2).unwrap().ch, '|');
    // An ASCII digit after a prepend joins it as well (UAX #29 GB9b).
    let mut t = Terminal::new(10, 2);
    t.advance("\u{0600}1|".as_bytes());
    owner(&t, 0, 0, "\u{0600}1");
}

#[test]
fn zwnj_breaks_generalized_virama_link() {
    let mut t = Terminal::new(10, 2);
    t.advance("\u{11412}\u{11442}\u{200c}\u{11412}".as_bytes());
    assert_eq!(owners(&t, 0), ["\u{11412}\u{11442}\u{200c}", "\u{11412}"]);
    // ZWJ keeps the link, as in the tuned groups.
    let mut t = Terminal::new(10, 2);
    t.advance("\u{11412}\u{11442}\u{200d}\u{11412}".as_bytes());
    owner(&t, 0, 0, "\u{11412}\u{11442}\u{200d}\u{11412}");
}

#[test]
fn ahom_excluded_spacing_marks_still_break() {
    // U+11720 is Mc but not Grapheme_Cluster_Break=SpacingMark.
    let mut t = Terminal::new(10, 2);
    t.advance("\u{11700}\u{11720}".as_bytes());
    assert_eq!(owners(&t, 0), ["\u{11700}", "\u{11720}"]);
}

#[test]
fn hangul_leading_jamo_pair_keeps_its_four_cells() {
    // One Unicode cluster, but the width oracle measures four cells and an
    // owner never exceeds two: the owner splits where the width overflows.
    let mut t = Terminal::new(10, 2);
    t.advance("\u{1100}\u{1100}\u{1161}|".as_bytes());
    assert_eq!(owners(&t, 0), ["\u{1100}", "\u{1100}\u{1161}", "|"]);
    assert_eq!(t.screen().cell(0, 4).unwrap().ch, '|');
}

#[test]
fn tuned_scripts_and_emoji_keep_their_own_ownership() {
    for (text, expected) in [
        ("a\u{903}", &["a", "\u{903}"][..]),
        ("a\u{1f3fb}", &["a", "\u{1f3fb}"][..]),
        ("\u{1b13}\u{915}", &["\u{1b13}", "\u{915}"][..]),
        (
            "\u{1b13}\u{1b44}\u{915}",
            &["\u{1b13}\u{1b44}", "\u{915}"][..],
        ),
    ] {
        let mut t = Terminal::new(10, 2);
        t.advance(text.as_bytes());
        assert_eq!(owners(&t, 0), expected, "{text:?}");
    }
}

#[test]
fn every_split_of_new_cluster_classes_matches_one_write() {
    for (name, text) in CLASSES {
        let mut whole = Terminal::new(20, 3);
        whole.advance(text.as_bytes());
        owner(&whole, 0, 0, text);
        for split in 0..=text.len() {
            let mut t = Terminal::new(20, 3);
            t.advance(&text.as_bytes()[..split]);
            t.advance(&text.as_bytes()[split..]);
            assert_eq!(t.snapshot().cells, whole.snapshot().cells, "{name} {split}");
            assert_eq!(t.screen().cursor(), whole.screen().cursor(), "{name}");
            if text.is_char_boundary(split) {
                let mut prefix = Terminal::new(20, 3);
                prefix.advance(&text.as_bytes()[..split]);
                let mut t = restored(&prefix);
                t.advance(&text.as_bytes()[split..]);
                owner(&t, 0, 0, text);
            }
        }
        let mut t = Terminal::new(20, 3);
        for byte in text.as_bytes() {
            t.advance(&[*byte]);
        }
        assert_eq!(t.snapshot().cells, whole.snapshot().cells, "{name}");
    }
}

#[test]
fn new_classes_report_their_cursor_in_both_width_policies() {
    for (name, text) in CLASSES {
        for wide in [false, true] {
            let mut t = Terminal::new(20, 3);
            t.set_ambiguous_wide(wide);
            t.advance(text.as_bytes());
            owner(&t, 0, 0, text);
            t.advance(b"X\x1b[6n");
            assert_eq!(t.screen().cell(0, 2).unwrap().ch, 'X', "{name}");
            assert_eq!(t.take_host_output(), b"\x1b[1;4R", "{name}");
        }
    }
}

#[test]
fn new_classes_wrap_at_the_right_edge_and_insert_whole() {
    for (name, text) in CLASSES {
        let mut t = Terminal::new(4, 3);
        t.advance(b"ABC");
        t.advance(text.as_bytes());
        assert!(t.screen().cell(0, 3).unwrap().layout_padding, "{name}");
        owner(&t, 1, 0, text);
        assert_eq!(t.screen().cursor(), Position { row: 1, column: 2 });
        let mut t = Terminal::new(10, 2);
        t.advance(b"LR\x1b[2G\x1b[4h");
        t.advance(text.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().ch, 'L');
        owner(&t, 0, 1, text);
        assert_eq!(t.screen().cell(0, 3).unwrap().ch, 'R', "{name}");
    }
}

#[test]
fn editing_either_half_clears_the_whole_owner() {
    for (name, text) in CLASSES {
        for cmd in [
            "\x1b[2GX",
            "\x1b[3GX",
            "\x1b[2G\x1b[X",
            "\x1b[3G\x1b[X",
            "\x1b[2G\x1b[P",
            "\x1b[3G\x1b[@",
            "\x1b[3G\x1b[1K",
        ] {
            let mut t = Terminal::new(10, 2);
            t.advance(format!("L{text}R").as_bytes());
            owner(&t, 0, 1, text);
            t.advance(cmd.as_bytes());
            assert_eq!(
                t.screen().cell(0, 0).unwrap().ch == 'L',
                !cmd.ends_with("1K")
            );
            for col in 0..10 {
                let cell = t.screen().cell(0, col).unwrap();
                assert_ne!(cell.grapheme(), text, "{name} {cmd:?}");
                assert!(
                    !(cell.wide_continuation
                        && col > 0
                        && t.screen().cell(0, col - 1).unwrap().combining().is_empty()
                        && t.screen().cell(0, col - 1).unwrap().ch == ' '),
                    "{name} {cmd:?}: orphaned continuation"
                );
            }
        }
    }
}

#[test]
fn copy_search_reflow_snapshot_and_export_keep_source_text() {
    for (name, text) in CLASSES {
        let mut t = Terminal::new(8, 4);
        t.advance(format!("L{text}R").as_bytes());
        owner(&t, 0, 1, text);
        for end in [1, 2] {
            assert_eq!(
                selected_text(&t.snapshot(), range(0, 1, end)),
                text,
                "{name}"
            );
            assert_eq!(
                selected_text_block(&t.snapshot(), range(0, 1, end)),
                text,
                "{name}"
            );
        }
        let exported: String = t
            .screen()
            .export_chunk(0, 1)
            .rows
            .iter()
            .flat_map(|row| row.cells.iter())
            .filter(|cell| !cell.wide_continuation && !cell.layout_padding)
            .map(|cell| cell.grapheme())
            .collect();
        assert!(exported.contains(text), "{name}: export");
        for _ in 0..10 {
            t.advance(b"\r\n");
        }
        for width in [4, 9, 3, 12] {
            t.resize(width, 4);
            assert_eq!(
                t.search(text, SearchOptions::case_sensitive()).len(),
                1,
                "{name} at {width}"
            );
        }
        assert_eq!(
            restored(&t)
                .search(text, SearchOptions::case_sensitive())
                .len(),
            1,
            "{name}"
        );
    }
}

#[test]
fn alternate_screen_redraw_keeps_owners() {
    for (name, text) in CLASSES {
        let mut t = Terminal::new(20, 4);
        t.advance(format!("old\r\x1b[2K> {text} X").as_bytes());
        owner(&t, 0, 2, text);
        assert_eq!(t.screen().cell(0, 5).unwrap().ch, 'X', "{name}");
        t.advance(b"\x1b[?1049h");
        t.advance(format!("|{text}|{text}|\r\n").as_bytes());
        owner(&t, 0, 1, text);
        owner(&t, 0, 4, text);
        t.advance(b"\x1b[?1049l");
        owner(&t, 0, 2, text);
    }
}
