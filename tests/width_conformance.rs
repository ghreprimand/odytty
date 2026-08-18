// SPDX-License-Identifier: GPL-3.0-only
//! Unicode width occupancy: a measured surface, not an assumed one.
//!
//! `print_char` asks `UnicodeWidthChar::width` once per scalar, with no
//! lookahead. That is the whole width algorithm. This file records what that
//! produces for a representative sample, including cases that disagree with
//! Unicode grapheme-cluster width (VS15/VS16, ZWJ emoji, Khmer table outliers).
//! Known-divergent rows assert the *current* occupancy so a future change
//! cannot silently retcon the number; they also assert it is not the Unicode
//! expected value, so a real fix has to promote the row rather than leave a
//! green-but-wrong pass.
//!
//! Flag pairs (RI+RI) occupy 2 columns by arithmetic coincidence (1+1) with
//! the same no-clustering root cause. They are not a conforming pass.
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

/// U+17A4 / U+17D8: unicode-width 0.2.2 table values, not combining absorption.
///
/// `print_char` only attaches when `width == 0`. These scalars are width 2 and
/// 3 in the crate OdyTTY uses, so they consume columns and leave combining
/// empty. A following ASCII letter is not swallowed into the Khmer cell.
///
/// The ucs-detect report listed measured 3 for U+17A4 and 2 for U+17D8; the
/// crate's own docs and tests are the other way around (QAA=2, BEYYAL=3). This
/// test pins occupancy against the crate, which is what `print_char` reads.
#[test]
fn khmer_qaa_and_beyyal_are_unicode_width_table_values_not_absorption() {
    let qaa = '\u{17A4}'; // KHMER INDEPENDENT VOWEL QAA
    let beyyal = '\u{17D8}'; // KHMER SIGN BEYYAL

    // QAA: one lead cell, one wide spacer. Following 'X' is its own cell,
    // not a combining mark on the Khmer.
    let mut qaa_term = Terminal::new(80, 1);
    qaa_term.advance("\u{17A4}X".as_bytes());
    let qaa_lead = qaa_term.screen().cell(0, 0).unwrap();
    let qaa_next = qaa_term.screen().cell(0, 1).unwrap();
    let qaa_x = qaa_term.screen().cell(0, 2).unwrap();
    assert_eq!(qaa_lead.ch, qaa);
    assert!(qaa_lead.combining().is_empty());
    assert_eq!(qaa_lead.grapheme(), "\u{17A4}");
    assert!(
        qaa_next.wide_continuation,
        "width 2 writes a continuation spacer; not a swallowed follower"
    );
    assert_eq!(qaa_x.ch, 'X');
    assert!(qaa_x.combining().is_empty());
    assert_eq!(qaa_term.screen().cursor(), Position { row: 0, column: 3 });

    // BEYYAL: width 3 advances the cursor by 3 but only width==2 writes a
    // spacer, so columns 1 and 2 stay blank. Following 'X' lands at column 3.
    let mut bey_term = Terminal::new(80, 1);
    bey_term.advance("\u{17D8}X".as_bytes());
    let bey_lead = bey_term.screen().cell(0, 0).unwrap();
    let bey_c1 = bey_term.screen().cell(0, 1).unwrap();
    let bey_c2 = bey_term.screen().cell(0, 2).unwrap();
    let bey_x = bey_term.screen().cell(0, 3).unwrap();
    assert_eq!(bey_lead.ch, beyyal);
    assert!(bey_lead.combining().is_empty());
    assert_eq!(bey_lead.grapheme(), "\u{17D8}");
    assert_eq!(bey_c1.ch, ' ');
    assert!(!bey_c1.wide_continuation);
    assert!(bey_c1.combining().is_empty());
    assert_eq!(bey_c2.ch, ' ');
    assert!(!bey_c2.wide_continuation);
    assert_eq!(bey_x.ch, 'X');
    assert_eq!(bey_term.screen().cursor(), Position { row: 0, column: 4 });
}

#[derive(Clone, Copy, Debug)]
enum WidthExpect {
    /// Occupancy matches Unicode / ucs-detect expected width.
    Conforming { width: usize },
    /// Occupancy is recorded and is *not* the Unicode expected width.
    KnownDivergent { unicode: usize, odytty: usize },
    /// Occupancy matches the Unicode number by adding independent widths, not
    /// by clustering. Not a conforming pass.
    Coincident { width: usize },
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
            expect: WidthExpect::KnownDivergent {
                unicode: 1,
                odytty: 2,
            },
        },
        Case {
            name: "khmer_beyyal_u17d8",
            input: "\u{17D8}",
            expect: WidthExpect::KnownDivergent {
                unicode: 1,
                odytty: 3,
            },
        },
        // WHITE SMILING FACE is width 1; VS16 requests emoji presentation
        // (width 2). Independent lookup: 1 + 0 = 1.
        Case {
            name: "vs16_on_text_default_smiley",
            input: "\u{263A}\u{FE0F}",
            expect: WidthExpect::KnownDivergent {
                unicode: 2,
                odytty: 1,
            },
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
        // Family ZWJ sequence: each emoji is width 2, ZWJ is 0; sum 6, cluster 2.
        Case {
            name: "zwj_family_man_woman_girl",
            input: "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            expect: WidthExpect::KnownDivergent {
                unicode: 2,
                odytty: 6,
            },
        },
        // US flag: two regional indicators. 1+1=2 matches the clustered width
        // without ever pairing them.
        Case {
            name: "ri_flag_us_coincident",
            input: "\u{1F1FA}\u{1F1F8}",
            expect: WidthExpect::Coincident { width: 2 },
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
            WidthExpect::Coincident { width } => {
                assert_eq!(
                    got, width,
                    "{}: coincident occupancy drifted (got {got}, want {width})",
                    case.name
                );
            }
        }
    }
}

/// Flag pairs occupy 2 columns as two independent width-1 cells, not a cluster.
#[test]
fn ri_flag_pair_is_two_independent_cells_not_a_cluster() {
    let input = "\u{1F1FA}\u{1F1F8}";
    assert_eq!(occupancy(input), 2);
    let left = cell_at(input, 0);
    let right = cell_at(input, 1);
    assert_eq!(left.ch, '\u{1F1FA}');
    assert_eq!(right.ch, '\u{1F1F8}');
    assert!(left.combining().is_empty());
    assert!(right.combining().is_empty());
    assert!(!left.wide_continuation);
    assert!(!right.wide_continuation);
    assert_eq!(left.grapheme(), "\u{1F1FA}");
    assert_eq!(right.grapheme(), "\u{1F1F8}");
}

/// VS16 on a text-default scalar attaches (width 0) but does not promote
/// occupancy from 1 to 2.
#[test]
fn vs16_does_not_promote_a_text_default_scalar_to_emoji_width() {
    let input = "\u{263A}\u{FE0F}";
    assert_eq!(occupancy(input), 1);
    let cell = cell_at(input, 0);
    // VS16 is width 0, so it *does* attach as combining — that is still not
    // emoji-width promotion. Occupancy stays 1.
    assert_eq!(cell.ch, '\u{263A}');
    assert_eq!(cell.combining(), &['\u{FE0F}']);
    assert_eq!(occupancy("\u{263A}"), 1);
}
