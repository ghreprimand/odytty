// SPDX-License-Identifier: GPL-3.0-only
//! Bracketed-paste end-marker sanitizer regression tests.
//!
//! Split into this submodule so `input.rs` stays under the production-file line
//! limit. These pin the convergence property of [`super::sanitize_paste`]: a
//! payload that reassembles `ESC[201~` after a deletion must not survive.

use super::sanitize_paste;

/// Deleting a match can reassemble a fresh end marker from the surrounding
/// bytes. A single forward pass that never revisits emitted output leaves
/// `ESC[201~` in the sanitized body.
#[test]
fn sanitize_paste_rejects_reassembled_end_marker() {
    let input = b"\x1b[2\x1b[201~01~";
    let sanitized = sanitize_paste(input);
    assert!(
        !sanitized.windows(6).any(|window| window == b"\x1b[201~"),
        "sanitized body must not contain the paste-end marker; got {sanitized:?}"
    );
}

/// Realistic splice: clipboard text that reconstitutes the end marker so the
/// shell treats the tail as typed input after paste ends.
#[test]
fn sanitize_paste_realistic_spliced_payload_leaves_no_end_marker() {
    let mut input = Vec::from(b"echo SAFE");
    input.extend_from_slice(b"\x1b[2");
    input.extend_from_slice(b"\x1b[201~");
    input.extend_from_slice(b"01~");
    input.extend_from_slice(b"id; echo PWNED\n");
    let sanitized = sanitize_paste(&input);
    assert!(
        !sanitized.windows(6).any(|window| window == b"\x1b[201~"),
        "spliced payload must not retain ESC[201~; got {sanitized:?}"
    );
}

/// Fixed-point property: sanitizing once must equal sanitizing again over a
/// small alphabet that can form the end marker by deletion splicing, plus
/// the known reassembled-marker counterexample.
#[test]
fn sanitize_paste_reaches_a_fixed_point_over_end_marker_alphabet() {
    const ALPHABET: &[u8] = b"\x1b[201~";
    fn enumerate(prefix: &mut Vec<u8>, max_len: usize, check: &mut dyn FnMut(&[u8])) {
        check(prefix);
        if prefix.len() >= max_len {
            return;
        }
        for &byte in ALPHABET {
            prefix.push(byte);
            enumerate(prefix, max_len, check);
            prefix.pop();
        }
    }
    let mut failures = Vec::new();
    let mut check = |input: &[u8]| {
        let once = sanitize_paste(input);
        let twice = sanitize_paste(&once);
        if once != twice || once.windows(6).any(|window| window == b"\x1b[201~") {
            failures.push((input.to_vec(), once, twice));
        }
    };
    check(b"\x1b[2\x1b[201~01~");
    enumerate(&mut Vec::new(), 7, &mut check);
    assert!(
        failures.is_empty(),
        "sanitize_paste must be a fixed point with no residual end marker; first failure input={:?} once={:?} twice={:?}",
        failures[0].0,
        failures[0].1,
        failures[0].2
    );
}
