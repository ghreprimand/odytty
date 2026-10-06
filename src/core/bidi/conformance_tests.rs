// SPDX-License-Identifier: GPL-3.0-only
//! UAX #9 conformance for the bidi display plan, pinned to Unicode 17.0.0
//! BidiCharacterTest.txt and BidiTest.txt.
//!
//! The committed corpus is a deterministic strided subset of each file (see
//! `tests/fixtures/unicode-bidi/README.md`); the exact pass and total counts
//! below are asserted, so any change in either direction fails. Left-to-right
//! cases (paragraph direction 0) run through the public [`BidiLayout::plan`]
//! path, one owner per scalar on one row, including its identity shortcut.
//! Right-to-left and auto-direction cases exercise the same level resolution,
//! line rule, and reordering through the internal entry points, because the
//! plan forces paragraph level 0. Each test case is one line, so L1 and L2 run
//! on the whole paragraph; rules L3 and L4 are outside both files.
//!
//! The full files can be checked with the ignored test
//! `full_unicode_bidi_corpus`, given `ODYTTY_UCD_BIDI_DIR` naming a directory
//! that holds them.

use super::resolve::{apply_line_rules, resolve_paragraph, visual_order};
use super::{BidiIdentityReason, BidiLayout, BidiOwner};

const CHARACTER_SUBSET: &str =
    include_str!("../../../tests/fixtures/unicode-bidi/BidiCharacterTest-subset.txt");
const CLASS_SUBSET: &str = include_str!("../../../tests/fixtures/unicode-bidi/BidiTest-subset.txt");

/// Paragraph direction requested by a test case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Ltr,
    Rtl,
    Auto,
}

impl Direction {
    fn index(self) -> usize {
        match self {
            Direction::Ltr => 0,
            Direction::Rtl => 1,
            Direction::Auto => 2,
        }
    }
}

/// Passed and total cases, per direction: left to right, right to left, auto.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tally {
    passed: [usize; 3],
    total: [usize; 3],
}

impl Tally {
    fn record(&mut self, direction: Direction, passed: bool) {
        self.total[direction.index()] += 1;
        if passed {
            self.passed[direction.index()] += 1;
        }
    }

    fn passed(&self) -> usize {
        self.passed.iter().sum()
    }

    fn total(&self) -> usize {
        self.total.iter().sum()
    }

    fn assert_complete(&self, corpus: &str) {
        assert!(self.total() > 0, "{corpus}: corpus contains no cases");
        for direction in [Direction::Ltr, Direction::Rtl, Direction::Auto] {
            let index = direction.index();
            assert_eq!(
                self.passed[index], self.total[index],
                "{corpus}: failed cases for {direction:?}"
            );
        }
    }
}

#[test]
fn full_corpus_outcome_rejects_a_failed_direction() {
    for direction in [Direction::Ltr, Direction::Rtl, Direction::Auto] {
        let mut tally = Tally::default();
        tally.record(direction, false);
        assert!(std::panic::catch_unwind(|| tally.assert_complete("fixture")).is_err());
    }
}

#[test]
fn full_corpus_outcome_rejects_an_empty_file() {
    assert!(std::panic::catch_unwind(|| Tally::default().assert_complete("fixture")).is_err());
}

/// Resolved paragraph level, per-scalar line levels, and visual order.
fn evaluate(scalars: &[char], direction: Direction) -> Option<(u8, Vec<u8>, Vec<usize>)> {
    let text: String = scalars.iter().collect();
    if direction == Direction::Ltr {
        let owners: Vec<BidiOwner<'_>> = text
            .char_indices()
            .map(|(start, scalar)| BidiOwner {
                text: &text[start..start + scalar.len_utf8()],
                width: 1,
            })
            .collect();
        return match BidiLayout::plan(&owners, &[owners.len()]) {
            BidiLayout::Identity(BidiIdentityReason::LeftToRightOnly) => {
                Some((0, vec![0; owners.len()], (0..owners.len()).collect()))
            }
            BidiLayout::Identity(_) => None,
            BidiLayout::Reordered(plan) => Some((
                plan.paragraph_level(),
                (0..owners.len())
                    .map(|owner| plan.owner_level(owner).unwrap_or(u8::MAX))
                    .collect(),
                plan.visual_owners(0)?.collect(),
            )),
        };
    }
    let base = (direction == Direction::Rtl).then_some(1);
    let paragraph = resolve_paragraph(&text, base);
    let mut levels = paragraph.levels.clone();
    apply_line_rules(&paragraph.classes, &mut levels, paragraph.paragraph);
    let order = visual_order(&levels);
    Some((paragraph.paragraph, levels, order))
}

/// Compare against expected levels (`None` for scalars removed by X9) and the
/// expected visual order of the retained scalars.
fn matches(
    scalars: &[char],
    direction: Direction,
    paragraph_level: Option<u8>,
    levels: &[Option<u8>],
    order: &[usize],
) -> bool {
    let Some((actual_paragraph, actual_levels, actual_order)) = evaluate(scalars, direction) else {
        return false;
    };
    if paragraph_level.is_some_and(|level| level != actual_paragraph) {
        return false;
    }
    if levels.len() != actual_levels.len() {
        return false;
    }
    let levels_ok = levels
        .iter()
        .zip(&actual_levels)
        .all(|(expected, actual)| expected.is_none_or(|level| level == *actual));
    let retained: Vec<usize> = actual_order
        .into_iter()
        .filter(|index| levels[*index].is_some())
        .collect();
    levels_ok && retained == order
}

fn parse_levels(field: &str) -> Vec<Option<u8>> {
    field
        .split_whitespace()
        .map(|level| level.parse().ok())
        .collect()
}

fn parse_order(field: &str) -> Vec<usize> {
    field
        .split_whitespace()
        .filter_map(|index| index.parse().ok())
        .collect()
}

fn run_character_tests(corpus: &str) -> Tally {
    let mut tally = Tally::default();
    for line in corpus.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(';').collect();
        assert_eq!(fields.len(), 5, "malformed BidiCharacterTest line: {line}");
        let scalars: Vec<char> = fields[0]
            .split_whitespace()
            .map(|hex| {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .expect("scalar value")
            })
            .collect();
        let direction = match fields[1] {
            "0" => Direction::Ltr,
            "1" => Direction::Rtl,
            _ => Direction::Auto,
        };
        let paragraph = fields[2].trim().parse().ok();
        let passed = matches(
            &scalars,
            direction,
            paragraph,
            &parse_levels(fields[3]),
            &parse_order(fields[4]),
        );
        tally.record(direction, passed);
    }
    tally
}

/// A representative scalar for each bidi class; none is a paired bracket,
/// as BidiTest.txt assumes.
fn class_scalar(class: &str) -> char {
    match class {
        "L" => 'a',
        "R" => '\u{05D0}',
        "AL" => '\u{0627}',
        "EN" => '0',
        "ES" => '+',
        "ET" => '$',
        "AN" => '\u{0660}',
        "CS" => ',',
        "NSM" => '\u{0300}',
        "BN" => '\u{00AD}',
        "B" => '\u{2029}',
        "S" => '\t',
        "WS" => ' ',
        "ON" => '!',
        "LRE" => '\u{202A}',
        "LRO" => '\u{202D}',
        "RLE" => '\u{202B}',
        "RLO" => '\u{202E}',
        "PDF" => '\u{202C}',
        "LRI" => '\u{2066}',
        "RLI" => '\u{2067}',
        "FSI" => '\u{2068}',
        "PDI" => '\u{2069}',
        other => panic!("unknown bidi class {other}"),
    }
}

fn run_class_tests(corpus: &str) -> Tally {
    let mut tally = Tally::default();
    let mut levels: Vec<Option<u8>> = Vec::new();
    let mut order: Vec<usize> = Vec::new();
    for line in corpus.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@Levels:") {
            levels = parse_levels(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("@Reorder:") {
            order = parse_order(rest);
            continue;
        }
        if line.starts_with('@') {
            continue;
        }
        let (input, bitset) = line.split_once(';').expect("BidiTest data line");
        let scalars: Vec<char> = input.split_whitespace().map(class_scalar).collect();
        let bits = u8::from_str_radix(bitset.trim(), 16).expect("bitset");
        for (bit, direction) in [
            (1, Direction::Auto),
            (2, Direction::Ltr),
            (4, Direction::Rtl),
        ] {
            if bits & bit != 0 {
                tally.record(
                    direction,
                    matches(&scalars, direction, None, &levels, &order),
                );
            }
        }
    }
    tally
}

#[test]
fn bidi_character_test_subset_passes_exactly() {
    let tally = run_character_tests(CHARACTER_SUBSET);
    assert_eq!(
        (tally.passed, tally.total),
        ([1433, 1433, 28], [1433, 1433, 28]),
        "BidiCharacterTest-17.0.0 subset: {} of {} pass",
        tally.passed(),
        tally.total()
    );
}

#[test]
fn bidi_class_test_subset_passes_exactly() {
    let tally = run_class_tests(CLASS_SUBSET);
    assert_eq!(
        (tally.passed, tally.total),
        ([3997, 4023, 4002], [3997, 4023, 4002]),
        "BidiTest-17.0.0 subset: {} of {} pass",
        tally.passed(),
        tally.total()
    );
}

/// The full files, outside the committed subset. Run with
/// `ODYTTY_UCD_BIDI_DIR=<dir> cargo test --release full_unicode_bidi_corpus -- --ignored --nocapture`.
#[test]
#[ignore = "needs the full Unicode 17.0.0 files in ODYTTY_UCD_BIDI_DIR"]
fn full_unicode_bidi_corpus() {
    let dir = std::env::var_os("ODYTTY_UCD_BIDI_DIR").expect("ODYTTY_UCD_BIDI_DIR");
    let dir = std::path::PathBuf::from(dir);
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("Unicode test file");
    let character = run_character_tests(&read("BidiCharacterTest.txt"));
    let class = run_class_tests(&read("BidiTest.txt"));
    println!(
        "BidiCharacterTest: {character:?} -> {} / {}",
        character.passed(),
        character.total()
    );
    println!(
        "BidiTest: {class:?} -> {} / {}",
        class.passed(),
        class.total()
    );
    character.assert_complete("BidiCharacterTest");
    class.assert_complete("BidiTest");
}
