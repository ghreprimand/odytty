// SPDX-License-Identifier: GPL-3.0-only
//! Unicode width occupancy: a measured surface, not an assumed one.
//!
//! Terminal ownership uses bounded script and recognized emoji extensions.
//! The default policy is the narrow table (`UnicodeWidthChar::width`). Wide
//! mode uses `UnicodeWidthChar::width_cjk` and is covered separately. This
//! file records what the default produces for a representative sample, including cases that disagree with
//! Unicode grapheme-cluster width (the preserved VS15 policy). Khmer scalar
//! compatibility cases now assert their frozen one-cell expected widths.
//! Known-divergent rows assert the *current* occupancy so a future change
//! cannot silently retcon the number; they also assert it is not the Unicode
//! expected value, so a real fix has to promote the row rather than leave a
//! green-but-wrong pass.
//!
//! Flag pairs (RI+RI) now occupy one two-cell source owner; lone RI remains
//! one cell as a deliberate compatibility policy.
//!
//! Windows: pure core `Terminal` storage. No PTY, no GPU, no platform branch.

use odytty::core::{Position, Terminal};

/// Occupancy of `input` on a fresh 80-column row: the cursor column after
/// printing, which is what a cursor-position-report width probe measures.
fn occupancy(input: &str) -> usize {
    let mut terminal = Terminal::new(80, 1);
    terminal.advance(input.as_bytes());
    terminal.screen().cursor().column
}

fn cell_at(input: &str, column: usize) -> odytty::core::Cell {
    let mut terminal = Terminal::new(80, 1);
    terminal.advance(input.as_bytes());
    terminal.screen().cell(0, column).expect("column in range")
}

/// U+17A4 / U+17D8 use the frozen one-cell compatibility widths.
/// Following ASCII remains a separate owner rather than a swallowed follower.
#[test]
fn khmer_qaa_and_beyyal_use_one_cell_without_absorbing_ascii() {
    for ch in ['\u{17a4}', '\u{17d8}'] {
        let mut terminal = Terminal::new(80, 1);
        terminal.advance(format!("{ch}X").as_bytes());
        let lead = terminal.screen().cell(0, 0).unwrap();
        let next = terminal.screen().cell(0, 1).unwrap();
        assert_eq!(lead.ch, ch);
        assert_eq!(lead.grapheme(), ch.to_string());
        assert!(lead.combining().is_empty());
        assert!(!lead.wide_continuation);
        assert_eq!(next.ch, 'X');
        assert!(next.combining().is_empty());
        assert!(!next.wide_continuation);
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
    }
}

#[derive(Clone, Copy, Debug)]
enum WidthExpect {
    /// Occupancy matches Unicode / ucs-detect expected width.
    Conforming { width: usize },
    /// Occupancy is recorded and is *not* the Unicode expected width.
    KnownDivergent { unicode: usize, odytty: usize },
}

struct Case {
    name: &'static str,
    input: &'static str,
    expect: WidthExpect,
}

fn cases() -> &'static [Case] {
    &[
        Case {
            name: "ascii",
            input: "A",
            expect: WidthExpect::Conforming { width: 1 },
        },
        Case {
            name: "cjk_wide",
            input: "世",
            expect: WidthExpect::Conforming { width: 2 },
        },
        Case {
            name: "combining_acute_on_e",
            input: "e\u{0301}",
            expect: WidthExpect::Conforming { width: 1 },
        },
        Case {
            name: "zwj_alone",
            input: "\u{200D}",
            expect: WidthExpect::Conforming { width: 0 },
        },
        Case {
            name: "vs16_alone",
            input: "\u{FE0F}",
            expect: WidthExpect::Conforming { width: 0 },
        },
        Case {
            name: "khmer_qaa_u17a4",
            input: "\u{17A4}",
            expect: WidthExpect::Conforming { width: 1 },
        },
        Case {
            name: "khmer_beyyal_u17d8",
            input: "\u{17D8}",
            expect: WidthExpect::Conforming { width: 1 },
        },
        // WHITE SMILING FACE is width 1; VS16 requests emoji presentation
        // (width 2). The retained source owner promotes to two cells.
        Case {
            name: "vs16_on_text_default_smiley",
            input: "\u{263A}\u{FE0F}",
            expect: WidthExpect::Conforming { width: 2 },
        },
        // GRINNING FACE is width 2; VS15 requests text presentation (width 1).
        // Independent lookup: 2 + 0 = 2.
        Case {
            name: "vs15_on_emoji_default_grin_text",
            input: "\u{1F600}\u{FE0E}",
            expect: WidthExpect::KnownDivergent {
                unicode: 1,
                odytty: 2,
            },
        },
        // Listed family ZWJ sequence shares one two-cell owner.
        Case {
            name: "zwj_family_man_woman_girl",
            input: "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            expect: WidthExpect::Conforming { width: 2 },
        },
        // US flag: paired regional indicators share one two-cell owner.
        Case {
            name: "ri_flag_us",
            input: "\u{1F1FA}\u{1F1F8}",
            expect: WidthExpect::Conforming { width: 2 },
        },
    ]
}

#[test]
fn width_conformance_sample_is_measured_not_assumed() {
    for case in cases() {
        let got = occupancy(case.input);
        match case.expect {
            WidthExpect::Conforming { width } => {
                assert_eq!(
                    got, width,
                    "{}: conforming occupancy drifted (got {got}, want {width})",
                    case.name
                );
            }
            WidthExpect::KnownDivergent { unicode, odytty } => {
                assert_ne!(
                    unicode, odytty,
                    "{}: known-divergent row must not record equal widths",
                    case.name
                );
                assert_eq!(
                    got, odytty,
                    "{}: recorded OdyTTY occupancy drifted (got {got}, recorded {odytty})",
                    case.name
                );
            }
        }
    }
}

/// Flag pairs retain both indicators in one two-cell source owner.
#[test]
fn ri_flag_pair_is_one_two_cell_source_owner() {
    let input = "\u{1F1FA}\u{1F1F8}";
    assert_eq!(occupancy(input), 2);
    let left = cell_at(input, 0);
    let right = cell_at(input, 1);
    assert_eq!(left.grapheme(), input);
    assert_eq!(left.combining(), &['\u{1F1F8}']);
    assert!(!left.wide_continuation);
    assert!(right.wide_continuation);
    assert!(right.combining().is_empty());
}

/// Listed VS16 promotes a text-default scalar to a two-cell source owner.
#[test]
fn vs16_promotes_a_listed_text_default_scalar_to_emoji_width() {
    let input = "\u{263A}\u{FE0F}";
    assert_eq!(occupancy(input), 2);
    let cell = cell_at(input, 0);
    assert_eq!(cell.ch, '\u{263A}');
    assert_eq!(cell.combining(), &['\u{FE0F}']);
    assert!(cell_at(input, 1).wide_continuation);
    assert_eq!(occupancy("\u{263A}"), 1);
}
