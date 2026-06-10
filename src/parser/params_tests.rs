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
fn from_vte_preserves_grouping() {
    // Drive a real vte parser and capture its Params via a tiny recorder, then
    // round-trip through `from_vte` and assert identical grouping.
    use vte::{Params as VteParams, Perform};

    #[derive(Default)]
    struct Capture(Vec<Vec<Vec<u16>>>);
    impl Perform for Capture {
        fn csi_dispatch(&mut self, params: &VteParams, _: &[u8], _: bool, _: char) {
            let owned = Params::from_vte(params);
            self.0.push(groups(&owned));
            // Cross-check the owned copy matches vte's own grouping.
            let direct: Vec<Vec<u16>> = params.iter().map(<[u16]>::to_vec).collect();
            assert_eq!(groups(&owned), direct, "from_vte must mirror vte grouping");
        }
    }

    let mut cap = Capture::default();
    let mut parser = vte::Parser::new();
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
