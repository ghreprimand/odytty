// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for the owned [`Params`] container.

use super::params::{MAX_PARAMS, Params};

/// Collect the iterator into owned groups for easy assertions.
fn groups(p: &Params) -> Vec<Vec<u16>> {
    p.iter().map(<[u16]>::to_vec).collect()
}

#[test]
fn empty_params_iterate_to_nothing() {
    let p = Params::new();
    assert!(p.is_empty());
    assert_eq!(p.len(), 0);
    assert_eq!(groups(&p), Vec::<Vec<u16>>::new());
}

#[test]
fn semicolon_params_are_single_value_groups() {
    // Models `1;2;3` then a terminating push.
    let mut p = Params::new();
    p.push(1);
    p.push(2);
    p.push(3);
    assert_eq!(groups(&p), vec![vec![1], vec![2], vec![3]]);
    assert_eq!(p.len(), 3);
}

#[test]
fn colon_subparams_stay_in_one_group() {
    // Models `38:2::1:2:3` (extend for each `:`, push for the final value).
    let mut p = Params::new();
    p.extend(38);
    p.extend(2);
    p.extend(0);
    p.extend(1);
    p.extend(2);
    p.push(3);
    assert_eq!(groups(&p), vec![vec![38, 2, 0, 1, 2, 3]]);
    assert_eq!(p.len(), 6);
}

#[test]
fn mixed_groups_reconstruct_boundaries() {
    // `38:5:200;1` — a 3-value subparam group then a single-value group.
    let mut p = Params::new();
    p.extend(38);
    p.extend(5);
    p.push(200);
    p.push(1);
    assert_eq!(groups(&p), vec![vec![38, 5, 200], vec![1]]);
}

#[test]
fn fills_and_refuses_overflow() {
    let mut p = Params::new();
    for _ in 0..MAX_PARAMS {
        assert!(!p.is_full());
        p.push(7);
    }
    assert!(p.is_full());
    assert_eq!(p.len(), MAX_PARAMS);
    // Further pushes are no-ops once full.
    p.push(9);
    p.extend(9);
    assert_eq!(p.len(), MAX_PARAMS);
}

#[test]
fn clear_resets_to_empty() {
    let mut p = Params::new();
    p.push(1);
    p.extend(2);
    p.clear();
    assert!(p.is_empty());
    assert_eq!(groups(&p), Vec::<Vec<u16>>::new());
}

#[test]
fn grouping_matches_parser_sequences() {
    use super::VtDispatch;
    use super::driver::OdyParser;

    #[derive(Default)]
    struct Capture(Vec<Vec<Vec<u16>>>);
    impl VtDispatch for Capture {
        fn csi_dispatch(&mut self, params: &Params, _: &[u8], _: bool, _: char) {
            self.0.push(groups(params));
        }
    }

    let mut cap = Capture::default();
    let mut parser = OdyParser::new();
    parser.advance(
        &mut cap,
        b"\x1b[1;2;3m\x1b[38:2::10:20:30m\x1b[m\x1b[0;38;5;9m",
    );
    assert_eq!(
        cap.0,
        vec![
            vec![vec![1], vec![2], vec![3]],
            vec![vec![38, 2, 0, 10, 20, 30]],
            vec![vec![0]],
            vec![vec![0], vec![38], vec![5], vec![9]],
        ]
    );
}

// ---------------------------------------------------------------------------
// Emptiness contract
// ---------------------------------------------------------------------------

#[test]
fn is_empty_is_false_once_any_slot_is_stored() {
    // `is_empty` is public API with no in-tree production caller, so the
    // negative case is only observable from a direct assertion. Both insertion
    // paths must flip it, and `clear` must flip it back.
    let mut pushed = Params::new();
    assert!(pushed.is_empty());
    pushed.push(0);
    assert!(
        !pushed.is_empty(),
        "a stored parameter must make is_empty false, even when its value is 0"
    );
    assert_eq!(pushed.len(), 1);

    let mut extended = Params::new();
    extended.extend(7);
    assert!(
        !extended.is_empty(),
        "an open subparameter counts as stored"
    );

    extended.clear();
    assert!(extended.is_empty(), "clear returns the list to empty");
}

#[test]
fn parsed_control_sequences_never_dispatch_empty_parameters() {
    // The machine's terminating push means a bare `CSI m` still carries one
    // parameter (`0`). Nothing in the parser produces an empty list at
    // dispatch, which is why `is_empty` has no production caller.
    use super::VtDispatch;
    use super::driver::OdyParser;

    #[derive(Default)]
    struct Capture {
        seen: usize,
        empty: usize,
    }
    impl VtDispatch for Capture {
        fn csi_dispatch(&mut self, params: &Params, _: &[u8], _: bool, _: char) {
            self.seen += 1;
            if params.is_empty() {
                self.empty += 1;
            }
        }
    }

    let mut cap = Capture::default();
    let mut parser = OdyParser::new();
    parser.advance(&mut cap, b"\x1b[m\x1b[H\x1b[;H\x1b[0m\x1b[:m");
    assert_eq!(cap.seen, 5, "every sequence dispatched");
    assert_eq!(cap.empty, 0, "no dispatch carried an empty parameter list");
}

// ---------------------------------------------------------------------------
// Equality contract
//
// Equality compares `len`, the first `len` values, and the group boundaries
// within those slots. Tail slots, boundary bits beyond `len`, and the transient
// `closed` flag are deliberately excluded. Nothing in the tree compares two
// `Params` values, so every rule below is only observable from these tests.
// ---------------------------------------------------------------------------

/// `1;2;3` — three single-value groups.
fn semicolon_triple() -> Params {
    let mut p = Params::new();
    p.push(1);
    p.push(2);
    p.push(3);
    p
}

#[test]
fn identically_built_lists_compare_equal() {
    let a = semicolon_triple();
    let b = semicolon_triple();
    assert_eq!(a, b);
    assert_eq!(b, a);
    assert_eq!(a, a.clone(), "equality is reflexive");

    let mut sub_a = Params::new();
    sub_a.extend(38);
    sub_a.extend(2);
    sub_a.push(9);
    let mut sub_b = Params::new();
    sub_b.extend(38);
    sub_b.extend(2);
    sub_b.push(9);
    assert_eq!(sub_a, sub_b, "subparameter groups compare equal too");

    assert_eq!(
        Params::new(),
        Params::new(),
        "two empty lists compare equal"
    );
}

#[test]
fn differing_values_compare_unequal() {
    let a = semicolon_triple();
    let mut b = Params::new();
    b.push(1);
    b.push(2);
    b.push(4);
    assert_ne!(a, b);
    assert_ne!(b, a);
}

#[test]
fn differing_lengths_compare_unequal() {
    let a = semicolon_triple();
    let mut b = Params::new();
    b.push(1);
    b.push(2);
    assert_ne!(a, b);
    assert_ne!(b, a);
    assert_ne!(a, Params::new());
    assert_ne!(Params::new(), a);
}

#[test]
fn same_values_with_different_group_boundaries_compare_unequal() {
    // `1;2` stores the same two values as `1:2`, but the first is two groups
    // and the second is one. Only the boundary bitmap distinguishes them, so
    // this is the assertion that pins the mask construction.
    let mut separate = Params::new();
    separate.push(1);
    separate.push(2);
    let mut joined = Params::new();
    joined.extend(1);
    joined.push(2);

    assert_eq!(separate.len(), joined.len());
    assert_eq!(
        separate.iter().map(<[u16]>::to_vec).collect::<Vec<_>>(),
        vec![vec![1], vec![2]]
    );
    assert_eq!(
        joined.iter().map(<[u16]>::to_vec).collect::<Vec<_>>(),
        vec![vec![1, 2]]
    );
    // Both directions: the masked comparison must be symmetric.
    assert_ne!(separate, joined);
    assert_ne!(joined, separate);
}

#[test]
fn full_lists_compare_by_group_boundaries_not_only_values() {
    // At `MAX_PARAMS` slots the mask covers every bit, so this is the only
    // assertion that exercises the saturated branch of the mask construction.
    let mut all_separate = Params::new();
    for _ in 0..MAX_PARAMS {
        all_separate.push(7);
    }
    let mut first_pair_joined = Params::new();
    first_pair_joined.extend(7);
    for _ in 1..MAX_PARAMS {
        first_pair_joined.push(7);
    }

    assert!(all_separate.is_full() && first_pair_joined.is_full());
    assert_eq!(all_separate.len(), first_pair_joined.len());
    assert_eq!(all_separate.iter().count(), MAX_PARAMS);
    assert_eq!(first_pair_joined.iter().count(), MAX_PARAMS - 1);
    assert_ne!(all_separate, first_pair_joined);
    assert_ne!(first_pair_joined, all_separate);

    let mut also_all_separate = Params::new();
    for _ in 0..MAX_PARAMS {
        also_all_separate.push(7);
    }
    assert_eq!(all_separate, also_all_separate);
}

#[test]
fn equality_ignores_the_transient_open_parameter_flag() {
    // `extend(5)` leaves the parameter open, `push(5)` closes it. Both store
    // one group holding one value, so they must compare equal.
    let mut open = Params::new();
    open.extend(5);
    let mut closed = Params::new();
    closed.push(5);
    assert_eq!(open, closed);
    assert_eq!(closed, open);
}

#[test]
fn equality_ignores_slots_left_behind_by_clear() {
    // `clear` does not scrub the value array; only the first `len` slots and
    // their boundary bits may participate in equality.
    let mut reused = Params::new();
    reused.push(11);
    reused.extend(22);
    reused.push(33);
    reused.clear();
    reused.push(9);

    let mut fresh = Params::new();
    fresh.push(9);

    assert_eq!(reused, fresh);
    assert_eq!(fresh, reused);
}
