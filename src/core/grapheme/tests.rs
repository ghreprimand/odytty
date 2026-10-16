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
fn prepend_table_matches_the_property_table() {
    for &(start, end, value) in data::PROPERTIES {
        for cp in [start, end] {
            let ch = char::from_u32(cp).expect("scalar");
            assert_eq!(super::is_prepend(ch), value & 0x0f == 7, "U+{cp:04X}");
        }
    }
    let total: u32 = data::PREPEND.iter().map(|&(a, b)| b - a + 1).sum();
    let counted: u32 = data::PROPERTIES
        .iter()
        .filter(|&&(_, _, value)| value & 0x0f == 7)
        .map(|&(a, b, _)| b - a + 1)
        .sum();
    assert_eq!(total, counted);
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

/// Owners formed by the print path for each GraphemeBreakTest line, as
/// strings, read from the row up to the cursor.
fn printed_owners(scalars: &[char]) -> Vec<String> {
    let mut terminal = crate::core::Terminal::new(1024, 2);
    terminal.advance(scalars.iter().collect::<String>().as_bytes());
    let cursor = terminal.screen().cursor();
    (0..cursor.column)
        .filter_map(|column| terminal.screen().cell(0, column))
        .filter(|cell| !cell.wide_continuation && !cell.layout_padding)
        .map(|cell| cell.grapheme())
        .collect()
}

/// The profile class that explains a line whose owners differ from its
/// Unicode clusters, by the first matching property of its scalars or
/// clusters. `other` is an unexplained difference.
fn profile_class(scalars: &[char], clusters: &[String]) -> &'static str {
    use super::{Class, class};
    let any = |test: &dyn Fn(char) -> bool| scalars.iter().any(|&ch| test(ch));
    if any(&|ch| matches!(class(ch), Class::Cr | Class::Lf | Class::Control)) {
        "control"
    } else if any(&|ch| {
        super::is_emoji_sequence_scalar(ch) || matches!(ch, '\u{1f3fb}'..='\u{1f3ff}')
    }) {
        "emoji"
    } else if any(&|ch| crate::core::indic::in_tuned_script(ch)) {
        "tuned"
    } else if clusters.iter().any(|cluster| {
        let mut chars = cluster.chars();
        let base = chars.next().expect("clusters are not empty");
        let rest: Vec<char> = chars.collect();
        crate::core::owner_rules::general_width(base, &rest, false) > 2
    }) {
        "width_overflow"
    } else if any(&|ch| crate::core::char_width::char_display_width(ch, false) == 0) {
        "zero_width"
    } else {
        "other"
    }
}

#[test]
fn owner_profile_against_grapheme_break_test_17() {
    let mut matched = 0;
    let mut classes = std::collections::BTreeMap::<&str, usize>::new();
    let mut other = Vec::new();
    for line in BREAK_TEST.lines() {
        let body = line.split('#').next().unwrap_or_default().trim();
        if body.is_empty() {
            continue;
        }
        let mut expected: Vec<String> = Vec::new();
        let mut scalars = Vec::new();
        for field in body.split_whitespace() {
            match field {
                "÷" => expected.push(String::new()),
                "×" => {}
                hex => {
                    let ch = scalar(hex);
                    scalars.push(ch);
                    expected.last_mut().expect("line starts with ÷").push(ch);
                }
            }
        }
        expected.retain(|cluster| !cluster.is_empty());
        if printed_owners(&scalars) == expected {
            matched += 1;
            continue;
        }
        let class = profile_class(&scalars, &expected);
        *classes.entry(class).or_default() += 1;
        if class == "other" {
            other.push(line.to_owned());
        }
    }
    assert!(
        other.is_empty(),
        "unattributed differences:\n{}",
        other.join("\n")
    );
    // Exact counts, so a change in either direction is seen. Every line that
    // differs is explained by a declared profile point: controls end owners
    // and never print; emoji sequences keep their own ownership; the tuned
    // script groups keep theirs; an owner never exceeds two cells; and
    // zero-width scalars attach to the open owner (or are dropped when no
    // owner is open) whatever the Unicode boundary.
    let expected: std::collections::BTreeMap<&str, usize> = [
        ("control", 211),
        ("emoji", 30),
        ("tuned", 53),
        ("width_overflow", 7),
        ("zero_width", 91),
    ]
    .into();
    assert_eq!(classes, expected);
    assert_eq!(matched, 374);
    assert_eq!(matched + classes.values().sum::<usize>(), BREAK_TEST_CASES);
}
