// SPDX-License-Identifier: GPL-3.0-only
//! Display-plan contract: owner units, per-row line rules, reversible maps,
//! mirroring flags, and exact cap boundaries.

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

fn row_order(plan: &BidiPlan, row: usize) -> Vec<usize> {
    plan.visual_owners(row).expect("row").collect()
}

/// Every owner appears exactly once in the visual order of its own row, the
/// two maps invert each other, and a wide owner covers adjacent columns with
/// its lead first.
fn assert_maps_are_reversible(plan: &BidiPlan, rows: &[usize]) {
    let mut first = 0;
    for (row, count) in rows.iter().enumerate() {
        let mut order = row_order(plan, row);
        order.sort_unstable();
        assert_eq!(order, (first..first + count).collect::<Vec<_>>());
        for owner in first..first + count {
            let (owner_row, span) = plan.owner_visual_span(owner).expect("span");
            assert_eq!(owner_row, row, "an owner never leaves its physical row");
            for (subcell, column) in span.enumerate() {
                assert_eq!(
                    plan.visual_cell(row, column),
                    Some(BidiVisualCell { owner, subcell })
                );
            }
        }
        let columns = plan.row_columns(row).expect("columns");
        assert_eq!(plan.visual_cell(row, columns), None);
        first += count;
    }
}

#[test]
fn left_to_right_text_takes_the_identity_layout_without_bidi_work() {
    let text = "plain ASCII 123, (brackets) and \u{00E9}\u{0301} marks";
    let owners = owners_of(text);
    assert_eq!(
        BidiLayout::plan(&owners, &[owners.len()]),
        BidiLayout::Identity(BidiIdentityReason::LeftToRightOnly)
    );
    assert_eq!(
        BidiLayout::plan(&[], &[]),
        BidiLayout::Identity(BidiIdentityReason::LeftToRightOnly)
    );
}

#[test]
fn hebrew_run_reverses_inside_left_to_right_paragraph() {
    // "ab " + alef bet gimel + " cd"
    let text = "ab \u{05D0}\u{05D1}\u{05D2} cd";
    let owners = owners_of(text);
    let plan = plan_of(BidiLayout::plan(&owners, &[owners.len()]));
    assert_eq!(plan.paragraph_level(), 0);
    assert_eq!(row_order(&plan, 0), vec![0, 1, 2, 5, 4, 3, 6, 7, 8]);
    assert_eq!(plan.owner_level(3), Some(1));
    assert_eq!(plan.owner_level(2), Some(0));
    assert_maps_are_reversible(&plan, &[owners.len()]);
}

#[test]
fn numbers_inside_right_to_left_text_keep_their_own_order() {
    // alef, space, "12", space, bet: the number stays 1 then 2 at level 2.
    let text = "\u{05D0} 12 \u{05D1}";
    let owners = owners_of(text);
    let plan = plan_of(BidiLayout::plan(&owners, &[owners.len()]));
    assert_eq!(row_order(&plan, 0), vec![5, 4, 2, 3, 1, 0]);
    assert_eq!(plan.owner_level(2), Some(2));
    assert_eq!(plan.owner_level(3), Some(2));
}

#[test]
fn wide_owners_and_attached_marks_move_as_one_unit() {
    // An Arabic letter with a fatha mark, then two fullwidth digits (two cells
    // each) that resolve as Arabic numbers after it, inside a Hebrew run.
    let owners = [
        BidiOwner {
            text: "a",
            width: 1,
        },
        BidiOwner {
            text: " ",
            width: 1,
        },
        BidiOwner {
            text: "\u{05D0}",
            width: 1,
        },
        BidiOwner {
            text: "\u{0628}\u{064E}",
            width: 1,
        },
        BidiOwner {
            text: "\u{FF11}",
            width: 2,
        },
        BidiOwner {
            text: "\u{FF12}",
            width: 2,
        },
    ];
    let plan = plan_of(BidiLayout::plan(&owners, &[owners.len()]));
    assert_eq!(row_order(&plan, 0), vec![0, 1, 4, 5, 3, 2]);
    assert_eq!(plan.owner_level(4), Some(2));
    assert_eq!(plan.owner_level(3), Some(1), "the mark rides with its base");
    // Each wide owner keeps two adjacent columns, lead first.
    assert_eq!(plan.owner_visual_span(4), Some((0, 2..4)));
    assert_eq!(plan.owner_visual_span(5), Some((0, 4..6)));
    assert_eq!(
        plan.visual_cell(0, 2),
        Some(BidiVisualCell {
            owner: 4,
            subcell: 0
        })
    );
    assert_eq!(
        plan.visual_cell(0, 3),
        Some(BidiVisualCell {
            owner: 4,
            subcell: 1
        })
    );
    assert_eq!(plan.owner_width(4), Some(2));
    assert_eq!(plan.row_columns(0), Some(8));
    assert_maps_are_reversible(&plan, &[owners.len()]);
}

#[test]
fn levels_resolve_on_the_paragraph_but_reorder_per_physical_row() {
    // One paragraph wrapped into two rows of four: "\u{05D0}\u{05D1} a" / "b \u{05D2}\u{05D3}".
    let text = "\u{05D0}\u{05D1} ab \u{05D2}\u{05D3}";
    let owners = owners_of(text);
    let rows = [4, 4];
    let plan = plan_of(BidiLayout::plan(&owners, &rows));
    assert_eq!(plan.row_count(), 2);
    assert_eq!(row_order(&plan, 0), vec![1, 0, 2, 3]);
    assert_eq!(row_order(&plan, 1), vec![4, 5, 7, 6]);
    assert_maps_are_reversible(&plan, &rows);
}

#[test]
fn trailing_whitespace_of_each_row_resets_to_the_paragraph_level() {
    // Row 0 ends in spaces inside a Hebrew run; L1 resets them to level 0 on
    // that row even though the paragraph continues with Hebrew on row 1.
    let text = "\u{05D0}\u{05D1}  \u{05D2}\u{05D3}";
    let owners = owners_of(text);
    let rows = [4, 2];
    let plan = plan_of(BidiLayout::plan(&owners, &rows));
    assert_eq!(plan.owner_level(2), Some(0));
    assert_eq!(plan.owner_level(3), Some(0));
    assert_eq!(row_order(&plan, 0), vec![1, 0, 2, 3]);
    // Unwrapped, the same spaces sit between R letters and take level 1.
    let single = plan_of(BidiLayout::plan(&owners, &[owners.len()]));
    assert_eq!(single.owner_level(2), Some(1));
}

#[test]
fn mirrored_glyph_flags_follow_odd_levels_only() {
    // "a(b)" then Hebrew with brackets: only the brackets at an odd level flag.
    let text = "a(b) \u{05D0}(\u{05D1})";
    let owners = owners_of(text);
    let plan = plan_of(BidiLayout::plan(&owners, &[owners.len()]));
    assert!(!plan.is_mirrored(1), "a bracket at level 0 is not mirrored");
    assert!(plan.is_mirrored(6));
    assert!(plan.is_mirrored(8));
    assert!(!plan.is_mirrored(5), "a letter is never mirrored");
    assert!(is_bidi_mirrored('('));
    assert!(is_bidi_mirrored('\u{2264}'));
    assert!(!is_bidi_mirrored('a'));
}

#[test]
fn malformed_input_gets_the_complete_identity_layout() {
    let hebrew = BidiOwner {
        text: "\u{05D0}",
        width: 1,
    };
    let cases: [(&[BidiOwner<'_>], &[usize]); 4] = [
        (&[hebrew, hebrew], &[1]),
        (&[hebrew, BidiOwner { text: "", width: 1 }], &[2]),
        (
            &[
                hebrew,
                BidiOwner {
                    text: "\u{05D1}",
                    width: 0,
                },
            ],
            &[2],
        ),
        (
            &[
                hebrew,
                BidiOwner {
                    text: "\u{05D1}",
                    width: MAX_BIDI_OWNER_WIDTH + 1,
                },
            ],
            &[2],
        ),
    ];
    for (owners, rows) in cases {
        assert_eq!(
            BidiLayout::plan(owners, rows),
            BidiLayout::Identity(BidiIdentityReason::MalformedInput)
        );
    }
    assert_eq!(
        BidiLayout::plan(&[hebrew], &[usize::MAX, 2]),
        BidiLayout::Identity(BidiIdentityReason::MalformedInput)
    );
}

#[test]
fn the_owner_cap_is_exact() {
    let hebrew = BidiOwner {
        text: "\u{05D0}",
        width: 1,
    };
    let at_cap = vec![hebrew; MAX_BIDI_PARAGRAPH_OWNERS];
    let plan = plan_of(BidiLayout::plan(&at_cap, &[at_cap.len()]));
    assert_eq!(plan.owner_count(), MAX_BIDI_PARAGRAPH_OWNERS);
    assert_eq!(
        row_order(&plan, 0).first().copied(),
        Some(MAX_BIDI_PARAGRAPH_OWNERS - 1),
        "a full-cap Hebrew paragraph reorders completely"
    );
    let over = vec![hebrew; MAX_BIDI_PARAGRAPH_OWNERS + 1];
    assert_eq!(
        BidiLayout::plan(&over, &[over.len()]),
        BidiLayout::Identity(BidiIdentityReason::OwnerCap)
    );
}

#[test]
fn the_byte_cap_is_exact() {
    // Sixteen-byte owners (a Hebrew letter with seven sheva points) at the
    // owner cap fill the byte budget exactly; one more byte crosses it while
    // the owner count stays at its cap.
    let pointed = "\u{05D0}\u{05B0}\u{05B0}\u{05B0}\u{05B0}\u{05B0}\u{05B0}\u{05B0}";
    assert_eq!(pointed.len(), 16);
    let owner = BidiOwner {
        text: pointed,
        width: 1,
    };
    let mut owners = vec![owner; MAX_BIDI_PARAGRAPH_OWNERS];
    let used: usize = owners.iter().map(|owner| owner.text.len()).sum();
    assert_eq!(used, MAX_BIDI_PARAGRAPH_BYTES);
    assert!(!BidiLayout::plan(&owners, &[owners.len()]).is_identity());
    let longer = format!("{pointed}a");
    owners[0] = BidiOwner {
        text: &longer,
        width: 1,
    };
    assert_eq!(
        BidiLayout::plan(&owners, &[owners.len()]),
        BidiLayout::Identity(BidiIdentityReason::ByteCap)
    );
}

#[test]
fn the_row_cap_is_exact() {
    let hebrew = BidiOwner {
        text: "\u{05D0}",
        width: 1,
    };
    let owners = vec![hebrew; MAX_BIDI_PARAGRAPH_ROWS];
    let rows = vec![1; MAX_BIDI_PARAGRAPH_ROWS];
    let plan = plan_of(BidiLayout::plan(&owners, &rows));
    assert_eq!(plan.row_count(), MAX_BIDI_PARAGRAPH_ROWS);
    let mut over_rows = rows.clone();
    over_rows.push(0);
    assert_eq!(
        BidiLayout::plan(&owners, &over_rows),
        BidiLayout::Identity(BidiIdentityReason::RowCap)
    );
}

#[test]
fn empty_rows_inside_a_paragraph_are_kept() {
    let text = "\u{05D0}\u{05D1}";
    let owners = owners_of(text);
    let rows = [0, 2, 0];
    let plan = plan_of(BidiLayout::plan(&owners, &rows));
    assert_eq!(plan.row_count(), 3);
    assert_eq!(row_order(&plan, 0), Vec::<usize>::new());
    assert_eq!(row_order(&plan, 1), vec![1, 0]);
    assert_eq!(plan.row_columns(2), Some(0));
    assert_maps_are_reversible(&plan, &rows);
}

/// Deterministic adversarial sequences of directional controls, brackets,
/// marks, digits, and letters never panic, and every reordered plan is a
/// reversible per-row permutation.
#[test]
fn adversarial_control_and_bracket_sequences_always_yield_a_valid_plan() {
    const ALPHABET: &[&str] = &[
        "a",
        "\u{05D0}",
        "\u{0627}",
        "1",
        "\u{0661}",
        "(",
        ")",
        "[",
        "]",
        " ",
        "\u{0300}",
        "\u{202A}",
        "\u{202B}",
        "\u{202C}",
        "\u{202D}",
        "\u{202E}",
        "\u{2066}",
        "\u{2067}",
        "\u{2068}",
        "\u{2069}",
        "\u{200E}",
        "\u{200F}",
        "\u{061C}",
        "\u{00AD}",
        "\t",
        "\u{2029}",
        ",",
        "+",
        "$",
        "\u{1F600}",
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2_000 {
        let len = (next() % 160) as usize;
        let owners: Vec<BidiOwner<'_>> = (0..len)
            .map(|_| BidiOwner {
                text: ALPHABET[(next() % ALPHABET.len() as u64) as usize],
                width: 1 + (next() % 2) as u8,
            })
            .collect();
        let mut rows = Vec::new();
        let mut left = len;
        while left > 0 {
            let take = 1 + (next() as usize % left.min(17));
            rows.push(take);
            left -= take;
        }
        if let BidiLayout::Reordered(plan) = BidiLayout::plan(&owners, &rows) {
            assert_maps_are_reversible(&plan, &rows);
        }
    }
    // Explicit embeddings nested past the UAX #9 depth limit stay bounded.
    let deep: Vec<BidiOwner<'_>> = (0..400)
        .map(|index| BidiOwner {
            text: if index % 2 == 0 {
                "\u{202B}"
            } else {
                "\u{202A}"
            },
            width: 1,
        })
        .chain([BidiOwner {
            text: "\u{05D0}",
            width: 1,
        }])
        .collect();
    let plan = plan_of(BidiLayout::plan(&deep, &[deep.len()]));
    assert!((0..deep.len()).all(|owner| plan.owner_level(owner) <= Some(126)));
}
