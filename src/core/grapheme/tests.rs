// SPDX-License-Identifier: GPL-3.0-only
//! Conformance of the incremental segmenter against the whole Unicode 17.0.0
//! GraphemeBreakTest.txt, and self-checks of the generated property table.

use super::{GraphemeState, data, packed};

const BREAK_TEST: &str =
    include_str!("../../../tests/fixtures/unicode-grapheme/GraphemeBreakTest-17.0.0.txt");

/// Every data line of GraphemeBreakTest-17.0.0.txt.
const BREAK_TEST_CASES: usize = 766;
/// Every boundary decision between two adjacent scalars in those lines.
const BREAK_TEST_DECISIONS: usize = 1_224;

fn scalar(field: &str) -> char {
    char::from_u32(u32::from_str_radix(field, 16).expect("hex scalar")).expect("valid scalar")
}

#[test]
fn grapheme_break_test_17_every_case() {
    assert!(BREAK_TEST.starts_with("# GraphemeBreakTest-17.0.0.txt"));
    let (mut cases, mut decisions) = (0, 0);
    let mut failures = Vec::new();
    for line in BREAK_TEST.lines() {
        let body = line.split('#').next().unwrap_or_default().trim();
        if body.is_empty() {
            continue;
        }
        cases += 1;
        let fields: Vec<&str> = body.split_whitespace().collect();
        assert_eq!(fields.first(), Some(&"÷"), "{line}");
        assert_eq!(fields.last(), Some(&"÷"), "{line}");
        let mut state = GraphemeState::new(scalar(fields[1]));
        for pair in fields[2..fields.len() - 1].chunks(2) {
            let (op, next) = (pair[0], scalar(pair[1]));
            let expected = match op {
                "×" => true,
                "÷" => false,
                other => panic!("unknown operator {other} in {line}"),
            };
            decisions += 1;
            let mut probe = state;
            if probe.accepts(next) != expected {
                failures.push(line.to_owned());
                break;
            }
            state = if expected {
                probe
            } else {
                assert_eq!(probe, state, "a boundary leaves the state unchanged");
                GraphemeState::new(next)
            };
        }
    }
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(cases, BREAK_TEST_CASES);
    assert_eq!(decisions, BREAK_TEST_DECISIONS);
}

#[test]
fn property_table_is_sorted_disjoint_and_valid() {
    let mut last = None;
    for &(start, end, value) in data::PROPERTIES {
        assert!(start <= end, "{start:x}..{end:x}");
        assert!(
            last.is_none_or(|previous| previous < start),
            "{start:x} overlaps"
        );
        last = Some(end);
        assert_ne!(value, 0, "{start:x}: empty entries are omitted");
        assert!(value & 0x0f <= 13, "{start:x}: class {value:#x}");
        assert_eq!(value & 0x80, 0, "{start:x}: unused bit");
    }
}

#[test]
fn ascii_fast_path_matches_the_table() {
    for cp in 0u32..0x80 {
        let index = data::PROPERTIES.partition_point(|&(_, end, _)| end < cp);
        let table = data::PROPERTIES
            .get(index)
            .filter(|&&(start, _, _)| start <= cp)
            .map_or(0, |&(_, _, value)| value);
        let ch = char::from_u32(cp).expect("ascii");
        assert_eq!(packed(ch), table, "U+{cp:04X}");
        if (0x20..0x7f).contains(&cp) {
            assert_eq!(
                table, 0,
                "printable ASCII U+{cp:04X} has no grapheme property"
            );
        }
    }
}

#[test]
fn boundary_leaves_state_unchanged_and_bounded() {
    // Representative rows outside the conformance file: a long conjunct
    // chain and a long regional indicator run keep a fixed-size state.
    let mut state = GraphemeState::new('\u{0915}');
    for _ in 0..64 {
        assert!(state.accepts('\u{094d}'));
        assert!(state.accepts('\u{0915}'));
    }
    let before = state;
    assert!(!state.accepts('a'));
    assert_eq!(state, before);
    let mut flags = GraphemeState::new('\u{1f1e6}');
    for index in 1..64 {
        let joined = flags.accepts('\u{1f1e8}');
        assert_eq!(joined, index % 2 == 1, "pair {index}");
        if !joined {
            flags = GraphemeState::new('\u{1f1e8}');
        }
    }
}
