// SPDX-License-Identifier: GPL-3.0-only
//! Paragraph direction in the display plan: right-to-left and auto paragraph
//! levels, and explicit-mode paragraphs (BiDi recommendation BDSM reset).

use super::*;

fn owners_of(text: &str) -> Vec<BidiOwner<'_>> {
    text.char_indices()
        .map(|(start, scalar)| BidiOwner {
            text: &text[start..start + scalar.len_utf8()],
            width: 1,
        })
        .collect()
}

fn plan_of(layout: BidiLayout) -> BidiPlan {
    match layout {
        BidiLayout::Reordered(plan) => *plan,
        BidiLayout::Identity(reason) => panic!("expected a reordered plan, got {reason:?}"),
    }
}

fn order(plan: &BidiPlan, row: usize) -> Vec<usize> {
    plan.visual_owners(row).expect("row").collect()
}

#[test]
fn right_to_left_paragraph_runs_left_to_right_text_from_the_right_edge() {
    // "ab" then two blanks: at paragraph level 1 the letters are a level-2
    // run and the trailing blanks return to level 1 (rule L1), so the row
    // reads blank, blank, a, b from the left.
    let owners = owners_of("ab  ");
    let plan = plan_of(BidiLayout::plan_in(
        &owners,
        &[4],
        BidiBaseDirection::RightToLeft,
    ));
    assert_eq!(plan.paragraph_level(), 1);
    assert_eq!(order(&plan, 0), vec![3, 2, 0, 1]);
    assert_eq!(plan.owner_level(0), Some(2));
    assert_eq!(plan.owner_level(3), Some(1));
}

#[test]
fn right_to_left_paragraph_is_never_the_identity_shortcut() {
    let owners = owners_of("abc");
    assert!(matches!(
        BidiLayout::plan_in(&owners, &[3], BidiBaseDirection::RightToLeft),
        BidiLayout::Reordered(_)
    ));
    assert_eq!(
        BidiLayout::plan_in(&owners, &[3], BidiBaseDirection::Auto),
        BidiLayout::Identity(BidiIdentityReason::LeftToRightOnly)
    );
}

#[test]
fn auto_paragraph_takes_the_first_strong_character_outside_isolates() {
    // First strong is Hebrew alef: level 1.
    let hebrew_first = owners_of("\u{05D0}b ");
    let plan = plan_of(BidiLayout::plan_in(
        &hebrew_first,
        &[3],
        BidiBaseDirection::Auto,
    ));
    assert_eq!(plan.paragraph_level(), 1);
    assert_eq!(order(&plan, 0), vec![2, 1, 0]);
    // First strong outside the isolate is Latin: level 0, though Hebrew
    // comes first inside the isolate.
    let isolated = owners_of("\u{2067}\u{05D0}\u{2069}b");
    let plan = plan_of(BidiLayout::plan_in(
        &isolated,
        &[4],
        BidiBaseDirection::Auto,
    ));
    assert_eq!(plan.paragraph_level(), 0);
    // A paragraph with only digits and Arabic-Indic digits has no strong
    // character: level 0, the default.
    let weak = owners_of("1\u{0661}");
    let plan = plan_of(BidiLayout::plan_in(&weak, &[2], BidiBaseDirection::Auto));
    assert_eq!(plan.paragraph_level(), 0);
}

#[test]
fn every_row_of_a_right_to_left_paragraph_reorders_on_its_own() {
    let owners = owners_of("ab\u{05D0}\u{05D1}cd");
    let plan = plan_of(BidiLayout::plan_in(
        &owners,
        &[3, 3],
        BidiBaseDirection::RightToLeft,
    ));
    // Row 0: a b (level 2) then alef (level 1): alef, a, b.
    assert_eq!(order(&plan, 0), vec![2, 0, 1]);
    // Row 1: bet (1) then c d (2): c, d, bet.
    assert_eq!(order(&plan, 1), vec![4, 5, 3]);
}

#[test]
fn explicit_left_to_right_paragraph_is_the_identity_layout() {
    let owners = owners_of("\u{05D0}\u{05D1} ab");
    assert_eq!(
        BidiLayout::plan_explicit(&owners, &[owners.len()], false),
        BidiLayout::Identity(BidiIdentityReason::ExplicitLeftToRight)
    );
}

#[test]
fn explicit_right_to_left_reverses_every_row_exactly_and_mirrors() {
    // Mixed text is not resolved: each row is reversed owner by owner, a
    // wide owner keeps its lead first, and every owner is at level 1, so the
    // bracket presents mirrored.
    let text = "(a\u{05D0}\u{4E00}b";
    let mut owners = owners_of(text);
    owners[3].width = 2;
    let plan = plan_of(BidiLayout::plan_explicit(&owners, &[2, 3], true));
    assert_eq!(plan.paragraph_level(), 1);
    assert_eq!(order(&plan, 0), vec![1, 0]);
    assert_eq!(order(&plan, 1), vec![4, 3, 2]);
    assert_eq!(plan.owner_visual_span(3), Some((1, 1..3)));
    assert_eq!(
        plan.visual_cell(1, 1),
        Some(BidiVisualCell {
            owner: 3,
            subcell: 0
        })
    );
    assert!((0..owners.len()).all(|owner| plan.owner_level(owner) == Some(1)));
    assert!(plan.is_mirrored(0));
    assert!(!plan.is_mirrored(1));
}

#[test]
fn explicit_plans_keep_the_caps_and_shape_rules() {
    let owners = owners_of("ab");
    assert_eq!(
        BidiLayout::plan_explicit(&owners, &[1], true),
        BidiLayout::Identity(BidiIdentityReason::MalformedInput)
    );
    let many = "a".repeat(MAX_BIDI_PARAGRAPH_OWNERS + 1);
    let owners = owners_of(&many);
    assert_eq!(
        BidiLayout::plan_explicit(&owners, &[owners.len()], true),
        BidiLayout::Identity(BidiIdentityReason::OwnerCap)
    );
    assert_eq!(
        BidiLayout::plan_in(&owners, &[owners.len()], BidiBaseDirection::RightToLeft),
        BidiLayout::Identity(BidiIdentityReason::OwnerCap)
    );
}

/// I-24: at every paragraph level and in explicit mode the plan is a
/// reversible owner permutation per physical row, blanks included.
#[test]
fn every_direction_gives_a_reversible_permutation_per_row() {
    let samples = [
        "ab \u{05D0}\u{05D1} 12 (x) \u{0627}\u{0644}  ",
        "\u{05D0} [1] c\u{2067}d\u{05D1}\u{2069} ",
        "   ",
        "\u{0661}\u{0662}-3 e",
    ];
    for text in samples {
        let owners = owners_of(text);
        let half = owners.len() / 2;
        let rows = [half, owners.len() - half];
        let layouts = [
            BidiLayout::plan_in(&owners, &rows, BidiBaseDirection::LeftToRight),
            BidiLayout::plan_in(&owners, &rows, BidiBaseDirection::RightToLeft),
            BidiLayout::plan_in(&owners, &rows, BidiBaseDirection::Auto),
            BidiLayout::plan_explicit(&owners, &rows, true),
        ];
        for layout in layouts {
            let BidiLayout::Reordered(plan) = layout else {
                continue;
            };
            let mut first = 0;
            for (row, count) in rows.iter().enumerate() {
                let mut seen = order(&plan, row);
                seen.sort_unstable();
                assert_eq!(seen, (first..first + count).collect::<Vec<_>>(), "{text:?}");
                for owner in first..first + count {
                    let (owner_row, span) = plan.owner_visual_span(owner).expect("span");
                    assert_eq!(owner_row, row);
                    for (subcell, column) in span.enumerate() {
                        assert_eq!(
                            plan.visual_cell(row, column),
                            Some(BidiVisualCell { owner, subcell })
                        );
                    }
                }
                first += count;
            }
        }
    }
}
