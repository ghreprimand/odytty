// SPDX-License-Identifier: GPL-3.0-only
//! Component-level tests for [`super::segmenter`] — the Layer-1 Ground sweep
//! and UTF-8 partial-codepoint carry in isolation. Parser golden and
//! self-consistency tests cover the full corpus + every byte split; these focus
//! on specific cells.

use super::VtDispatch;
use super::params::Params;
use super::segmenter::{GroundResult, Segmenter};

#[derive(Debug, Default, PartialEq, Eq)]
struct Rec {
    prints: Vec<char>,
    executes: Vec<u8>,
}

impl VtDispatch for Rec {
    fn print(&mut self, c: char) {
        self.prints.push(c);
    }
    fn execute(&mut self, byte: u8) {
        self.executes.push(byte);
    }
    fn csi_dispatch(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn esc_dispatch(&mut self, _: &[u8], _: bool, _: u8) {}
    fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {}
    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
    fn apc_dispatch(&mut self, _: &[u8]) {}
}

fn run(bytes: &[u8]) -> (Rec, GroundResult, usize) {
    let mut s = Segmenter::new();
    let mut r = Rec::default();
    let (result, consumed) = s.run_ground(&mut r, bytes);
    (r, result, consumed)
}

#[test]
fn plain_ascii_bulk_prints() {
    let (r, result, consumed) = run(b"Hello");
    assert_eq!(result, GroundResult::Drained);
    assert_eq!(consumed, 5);
    assert_eq!(r.prints, vec!['H', 'e', 'l', 'l', 'o']);
    assert!(r.executes.is_empty());
}

#[test]
fn mixed_ascii_and_c0_execute_in_order() {
    let (r, result, consumed) = run(b"a\nb");
    assert_eq!(result, GroundResult::Drained);
    assert_eq!(consumed, 3);
    assert_eq!(r.prints, vec!['a', 'b']);
    assert_eq!(r.executes, vec![b'\n']);
}

#[test]
fn esc_byte_returns_saw_esc() {
    let (r, result, consumed) = run(b"hi\x1bX");
    assert_eq!(result, GroundResult::SawEsc);
    // The ESC byte is INCLUDED in `consumed`; the 'X' is not yet processed.
    assert_eq!(consumed, 3);
    assert_eq!(r.prints, vec!['h', 'i']);
}

#[test]
fn utf8_2byte_scalar_prints() {
    let bytes = "café".as_bytes();
    let (r, _result, consumed) = run(bytes);
    assert_eq!(consumed, bytes.len());
    assert_eq!(r.prints, vec!['c', 'a', 'f', 'é']);
}

#[test]
fn c1_via_utf8_whole_executes() {
    // Even when bytes arrive whole in Ground, U+0085 (NEL) is a C1 → execute.
    let (r, _, _) = run(b"\xc2\x85");
    assert_eq!(r.prints, Vec::<char>::new());
    assert_eq!(r.executes, vec![0x85]);
}

#[test]
fn c1_via_utf8_split_executes_uniform() {
    let mut s = Segmenter::new();
    let mut r = Rec::default();
    let (r1, c1) = s.run_ground(&mut r, &[0xC2]);
    assert_eq!(r1, GroundResult::Drained);
    assert_eq!(c1, 1);
    assert!(s.has_partial());
    let (r2, c2) = s.run_ground(&mut r, &[0x85]);
    assert_eq!(r2, GroundResult::Drained);
    assert_eq!(c2, 1);
    assert!(!s.has_partial());
    assert_eq!(r.executes, vec![0x85]);
    assert!(r.prints.is_empty());
}

#[test]
fn lone_c1_byte_executes() {
    // 0x85 alone (invalid UTF-8 lead in 0x80..=0x9F) → execute as C1.
    let (r, _, _) = run(b"\x85");
    assert_eq!(r.executes, vec![0x85]);
}

#[test]
fn invalid_high_byte_prints_fffd() {
    let (r, _, _) = run(b"\xfe");
    assert_eq!(r.prints, vec!['\u{FFFD}']);
    assert!(r.executes.is_empty());
}

#[test]
fn partial_4byte_codepoint_carries_then_completes() {
    // U+1F600 = F0 9F 98 80
    let mut s = Segmenter::new();
    let mut r = Rec::default();
    let (r1, _) = s.run_ground(&mut r, &[0xF0]);
    assert_eq!(r1, GroundResult::Drained);
    assert!(s.has_partial());
    let (r2, _) = s.run_ground(&mut r, &[0x9F]);
    assert_eq!(r2, GroundResult::Drained);
    assert!(s.has_partial());
    let (r3, _) = s.run_ground(&mut r, &[0x98]);
    assert_eq!(r3, GroundResult::Drained);
    assert!(s.has_partial());
    let (r4, _) = s.run_ground(&mut r, &[0x80]);
    assert_eq!(r4, GroundResult::Drained);
    assert!(!s.has_partial());
    assert_eq!(r.prints, vec!['\u{1F600}']);
}

#[test]
fn partial_completion_preserves_following_ascii() {
    let mut s = Segmenter::new();
    let mut r = Rec::default();
    let (r1, c1) = s.run_ground(&mut r, &[0xC3]);
    assert_eq!(r1, GroundResult::Drained);
    assert_eq!(c1, 1);
    assert!(s.has_partial());

    let (r2, c2) = s.run_ground(&mut r, &[0xA9, b'A']);
    assert_eq!(r2, GroundResult::Drained);
    assert_eq!(c2, 2);
    assert!(!s.has_partial());
    assert_eq!(r.prints, vec!['é', 'A']);
}

#[test]
fn partial_completion_preserves_following_partial_lead() {
    let mut s = Segmenter::new();
    let mut r = Rec::default();
    let _ = s.run_ground(&mut r, &[0xC3]);
    assert!(s.has_partial());

    let (r2, c2) = s.run_ground(&mut r, &[0xA9, b'A', 0xC3]);
    assert_eq!(r2, GroundResult::Drained);
    assert_eq!(c2, 3);
    assert!(s.has_partial());
    assert_eq!(r.prints, vec!['é', 'A']);

    let (r3, c3) = s.run_ground(&mut r, &[0xA9]);
    assert_eq!(r3, GroundResult::Drained);
    assert_eq!(c3, 1);
    assert!(!s.has_partial());
    assert_eq!(r.prints, vec!['é', 'A', 'é']);
}

#[test]
fn reset_clears_partial() {
    let mut s = Segmenter::new();
    let mut r = Rec::default();
    let _ = s.run_ground(&mut r, &[0xC2]);
    assert!(s.has_partial());
    s.reset();
    assert!(!s.has_partial());
}

/// A garbage-heavy chunk — many consecutive invalid subparts before an ESC —
/// dispatches every replacement in order and still hands off at the ESC. This
/// pins the hoisted-ESC-scan restructuring: the invalid-subpart loop consumes
/// within one chunk without rescanning, and the chunk boundary semantics are
/// unchanged.
#[test]
fn consecutive_invalid_subparts_before_esc_dispatch_in_order() {
    // 0xFE/0xFF are never valid UTF-8 (one U+FFFD each); 0x85 is a C1
    // execute; 'A' is plain text; then ESC.
    let (r, result, consumed) = run(b"\xFF\xFE\x85A\xFF\x1bZtail");
    assert_eq!(result, GroundResult::SawEsc);
    assert_eq!(consumed, 6, "everything through the ESC byte is consumed");
    assert_eq!(
        r.prints,
        vec!['\u{FFFD}', '\u{FFFD}', 'A', '\u{FFFD}'],
        "each invalid subpart emits exactly one replacement, in order"
    );
    assert_eq!(r.executes, vec![0x85]);
}

/// Same garbage-heavy shape with no ESC at all: the chunk drains fully.
#[test]
fn consecutive_invalid_subparts_without_esc_drain() {
    let (r, result, consumed) = run(b"\xFF\xFF\xFFx\xFE");
    assert_eq!(result, GroundResult::Drained);
    assert_eq!(consumed, 5);
    assert_eq!(
        r.prints,
        vec!['\u{FFFD}', '\u{FFFD}', '\u{FFFD}', 'x', '\u{FFFD}']
    );
}
