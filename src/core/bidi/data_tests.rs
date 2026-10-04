// SPDX-License-Identifier: GPL-3.0-only
//! Unicode 17.0.0 bidi data and width-0 format-control owners.
//!
//! The class samples are code points whose class differs between the
//! Unicode 16.0.0 tables bundled with `unicode-bidi` 0.3.18 and Unicode
//! 17.0.0 DerivedBidiClass.txt: 4,101 code points in all, 146 of them
//! assigned. Each transition kind is pinned here.

use unicode_bidi::BidiClass::{self, *};
use unicode_bidi::data_source::BidiDataSource;

use super::brackets::BIDI_BRACKETS;
use super::classes::BIDI_CLASSES;
use super::data::{Unicode17, bidi_class};
use super::*;

fn plan_of(layout: BidiLayout) -> BidiPlan {
    match layout {
        BidiLayout::Reordered(plan) => *plan,
        BidiLayout::Identity(reason) => panic!("expected a reordered plan, got {reason:?}"),
    }
}

fn one_cell(text: &str) -> BidiOwner<'_> {
    BidiOwner { text, width: 1 }
}

fn control(text: &str) -> BidiOwner<'_> {
    BidiOwner { text, width: 0 }
}

#[test]
fn classes_follow_unicode_17_where_the_bundled_unicode_16_tables_differ() {
    let cases: &[(char, BidiClass)] = &[
        // Assigned in Unicode 17.0.0.
        ('\u{1FAEA}', ON),  // DISTORTED FACE, L before
        ('\u{1CEBA}', ON),  // FRAGILE SYMBOL, L before
        ('\u{1E6E3}', NSM), // TAI YO SIGN UE, L before
        ('\u{11B60}', NSM), // SHARADA VOWEL SIGN OE, L before
        ('\u{1ACF}', NSM),  // COMBINING DOUBLE CARON, L before
        ('\u{10EC5}', AL),  // ARABIC SMALL YEH BARREE WITH TWO DOTS BELOW, R before
        ('\u{10ED0}', ON),  // ARABIC BIBLICAL END OF VERSE, R before
        ('\u{10EFA}', NSM), // ARABIC DOUBLE VERTICAL BAR BELOW, R before
        ('\u{FBC3}', ON),   // ARABIC LIGATURE JALLA WA-ALAA, AL before
        // Unassigned code points with block @missing defaults.
        ('\u{086B}', AL),   // Arabic default, R before
        ('\u{FDD0}', BN),   // noncharacter, L before
        ('\u{2065}', BN),   // default ignorable, L before
        ('\u{E0080}', BN),  // default ignorable, L before
        ('\u{10FFFF}', BN), // noncharacter, L before
    ];
    for &(scalar, expected) in cases {
        assert_eq!(bidi_class(scalar), expected, "U+{:04X}", u32::from(scalar));
        assert_eq!(Unicode17.bidi_class(scalar), expected);
    }
}

#[test]
fn classes_unchanged_between_versions_still_resolve() {
    let cases: &[(char, BidiClass)] = &[
        ('a', L),
        ('\u{0000}', BN),
        ('\t', S),
        ('\n', B),
        (' ', WS),
        ('1', EN),
        ('+', ES),
        ('$', ET),
        (',', CS),
        ('!', ON),
        ('\u{0300}', NSM),
        ('\u{05D0}', R),
        ('\u{0590}', R), // unassigned, Hebrew block default
        ('\u{0627}', AL),
        ('\u{0661}', AN),
        ('\u{20CF}', ET), // unassigned, currency block default
        ('\u{202A}', LRE),
        ('\u{202B}', RLE),
        ('\u{202C}', PDF),
        ('\u{202D}', LRO),
        ('\u{202E}', RLO),
        ('\u{2066}', LRI),
        ('\u{2067}', RLI),
        ('\u{2068}', FSI),
        ('\u{2069}', PDI),
        ('\u{4E00}', L),
        ('\u{10FFFD}', L),
    ];
    for &(scalar, expected) in cases {
        assert_eq!(bidi_class(scalar), expected, "U+{:04X}", u32::from(scalar));
    }
}

#[test]
fn unicode_17_classes_change_visual_order() {
    // Between two Hebrew letters an ON or NSM resolves to R, and a BN takes
    // the level before it, so all three reverse. With the Unicode 16 L class
    // the middle character stayed at level 0 and nothing moved.
    for middle in ["\u{1FAEA}", "\u{1E6E3}", "\u{FDD0}"] {
        let owners = [one_cell("\u{05D0}"), one_cell(middle), one_cell("\u{05D1}")];
        let plan = plan_of(BidiLayout::plan(&owners, &[3]));
        assert_eq!(
            plan.visual_owners(0).expect("row").collect::<Vec<_>>(),
            [2, 1, 0],
            "{middle:?}"
        );
    }
}

#[test]
fn bracket_pairs_use_unicode_17_data_with_canonical_openings() {
    let lookup = |scalar| {
        Unicode17
            .bidi_matched_opening_bracket(scalar)
            .map(|found| (found.opening, found.is_open))
    };
    assert_eq!(lookup('('), Some(('(', true)));
    assert_eq!(lookup(')'), Some(('(', false)));
    // BD16: U+2329 and U+3008 are canonically equivalent openings.
    assert_eq!(lookup('\u{2329}'), Some(('\u{3008}', true)));
    assert_eq!(lookup('\u{232A}'), Some(('\u{3008}', false)));
    assert_eq!(lookup('\u{3009}'), Some(('\u{3008}', false)));
    assert_eq!(lookup('a'), None);
    assert_eq!(lookup('\u{05D0}'), None);
}

#[test]
fn generated_tables_are_sorted_disjoint_and_complete() {
    assert!(
        BIDI_CLASSES
            .iter()
            .all(|(start, end, class)| start <= end && *class != L)
    );
    assert!(BIDI_CLASSES.windows(2).all(|pair| pair[0].1 < pair[1].0));
    let covered: u32 = BIDI_CLASSES
        .iter()
        .map(|(start, end, _)| u32::from(*end) - u32::from(*start) + 1)
        .sum();
    assert_eq!(BIDI_CLASSES.len(), 763);
    assert_eq!(covered, 18_705);
    assert_eq!(BIDI_BRACKETS.len(), 128);
    assert!(BIDI_BRACKETS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    for &(bracket, opening, is_open) in BIDI_BRACKETS {
        let found = Unicode17
            .bidi_matched_opening_bracket(bracket)
            .expect("listed bracket");
        assert_eq!((found.opening, found.is_open), (opening, is_open));
        assert!(
            Unicode17
                .bidi_matched_opening_bracket(opening)
                .is_some_and(|o| o.is_open)
        );
    }
}

#[test]
fn format_controls_keep_their_logical_position_without_a_cell() {
    // RLI at the very start of the paragraph, PDI before a Latin letter.
    let owners = [
        control("\u{2067}"),
        one_cell("\u{05D0}"),
        one_cell("\u{05D1}"),
        control("\u{2069}"),
        one_cell("x"),
    ];
    let plan = plan_of(BidiLayout::plan(&owners, &[5]));
    assert_eq!(plan.row_columns(0), Some(3));
    let painted: Vec<_> = (0..3)
        .map(|column| plan.visual_cell(0, column).map(|cell| cell.owner))
        .collect();
    assert_eq!(painted, [Some(2), Some(1), Some(4)]);
    for owner in [0, 3] {
        assert!(plan.is_format_control(owner));
        let (row, span) = plan.owner_visual_span(owner).expect("span");
        assert_eq!(row, 0);
        assert!(span.is_empty());
        assert!(!plan.is_mirrored(owner));
    }
    assert!(!plan.is_format_control(1));
    assert_eq!(plan.owner_level(1), Some(1));
    assert_eq!(plan.owner_level(4), Some(0));
    let mut order: Vec<_> = plan.visual_owners(0).expect("row").collect();
    order.sort_unstable();
    assert_eq!(order, [0, 1, 2, 3, 4], "controls stay in the owner order");
}

#[test]
fn format_controls_at_a_row_start_change_levels_but_not_columns() {
    // RLM and an RLE..PDF embedding open the second row; the embedding
    // raises the Latin letters to level 2 inside a level-1 run.
    let owners = [
        one_cell("a"),
        one_cell("\u{05D0}"),
        control("\u{200F}\u{202B}"),
        one_cell("b"),
        one_cell("c"),
        control("\u{202C}"),
    ];
    let plan = plan_of(BidiLayout::plan(&owners, &[2, 4]));
    assert_eq!(plan.row_columns(0), Some(2));
    assert_eq!(plan.row_columns(1), Some(2));
    assert_eq!(plan.owner_level(3), Some(2));
    assert_eq!(plan.owner_level(4), Some(2));
    assert_eq!(
        (0..2)
            .map(|column| plan.visual_cell(1, column).map(|cell| cell.owner))
            .collect::<Vec<_>>(),
        [Some(3), Some(4)]
    );
    assert!(plan.owner_visual_span(2).expect("span").1.is_empty());
}

#[test]
fn width_zero_owners_must_hold_only_format_controls() {
    let hebrew = one_cell("\u{05D0}");
    for text in ["\u{200F}a", "\u{200B}", " ", "\u{0300}"] {
        assert_eq!(
            BidiLayout::plan(&[hebrew, control(text)], &[2]),
            BidiLayout::Identity(BidiIdentityReason::MalformedInput),
            "{text:?}"
        );
    }
    // A left-to-right mark cannot raise a level on its own.
    assert_eq!(
        BidiLayout::plan(&[control("\u{200E}"), one_cell("a")], &[2]),
        BidiLayout::Identity(BidiIdentityReason::LeftToRightOnly)
    );
    for scalar in [
        '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}',
    ] {
        assert!(is_bidi_format_control(scalar));
    }
    for scalar in [
        '\u{200B}', '\u{200D}', '\u{2029}', '\u{2065}', '\u{206A}', 'a',
    ] {
        assert!(!is_bidi_format_control(scalar));
    }
}
