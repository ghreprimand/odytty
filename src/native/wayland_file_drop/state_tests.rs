// SPDX-License-Identifier: GPL-3.0-only
//! Deterministic coverage for the wl-object-free listener core: the
//! `text/uri-list` parser (scheme/authority validation, percent-decoding,
//! byte-faithful non-UTF-8, NUL / malformed-escape / query-fragment rejection,
//! ordering) and the state machine (offer lifecycle and bound, per-seat
//! current-enter-offer vs accepted drag, Leave always destroys the enter offer,
//! seat removal, the Copy-only gate, transfer cap and timeout). The live
//! protocol path is exercised on-device against a conforming compositor.

use super::*;
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

fn strs(list: &[u8]) -> Vec<String> {
    parse_uri_list(list)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

// ---- parser ----

#[test]
fn single_and_multi_file_order_preserved() {
    assert_eq!(strs(b"file:///tmp/a\r\n"), ["/tmp/a"]);
    assert_eq!(
        strs(b"file:///tmp/one\r\nfile:///tmp/two\r\nfile:///tmp/three\r\n"),
        ["/tmp/one", "/tmp/two", "/tmp/three"]
    );
}

#[test]
fn lf_only_and_missing_terminator_accepted() {
    assert_eq!(strs(b"file:///tmp/a\n"), ["/tmp/a"]);
    assert_eq!(strs(b"file:///tmp/a"), ["/tmp/a"]);
}

#[test]
fn comments_blanks_skipped() {
    assert_eq!(strs(b"# c\r\n\r\nfile:///tmp/real\r\n"), ["/tmp/real"]);
}

#[test]
fn spaces_and_unicode_percent_decoded() {
    assert_eq!(strs(b"file:///tmp/a%20b\r\n"), ["/tmp/a b"]);
    assert_eq!(strs(b"file:///tmp/caf%C3%A9\r\n"), ["/tmp/caf\u{e9}"]);
}

#[test]
fn non_utf8_bytes_survive() {
    let parsed = parse_uri_list(b"file:///tmp/bad%FFname\r\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].as_os_str().as_bytes(), b"/tmp/bad\xFFname");
}

#[test]
fn localhost_authority_accepted_remote_rejected() {
    assert_eq!(strs(b"file://localhost/tmp/a\r\n"), ["/tmp/a"]);
    assert_eq!(strs(b"file://LOCALHOST/tmp/a\r\n"), ["/tmp/a"]);
    assert!(strs(b"file://remote.example/tmp/a\r\n").is_empty());
}

#[test]
fn hostile_schemes_rejected() {
    let list = b"http://x/y\r\nhttps://x/y\r\njavascript:alert(1)\r\ndata:text/plain,hi\r\nftp://h/f\r\nfile:relative\r\n";
    assert!(strs(list).is_empty());
}

#[test]
fn file_scheme_case_insensitive() {
    assert_eq!(strs(b"FILE:///tmp/a\r\n"), ["/tmp/a"]);
}

#[test]
fn mixed_keeps_only_valid_file_uris_in_order() {
    assert_eq!(
        strs(b"http://evil/x\r\nfile:///tmp/k1\r\njavascript:void\r\nfile:///tmp/k2\r\n"),
        ["/tmp/k1", "/tmp/k2"]
    );
}

#[test]
fn malformed_percent_escape_rejected_not_reinterpreted() {
    // A '%' not followed by two hex digits invalidates the whole URI; it must
    // NOT be reinterpreted as a literal-percent path.
    assert!(strs(b"file:///tmp/50%off\r\n").is_empty());
    assert!(strs(b"file:///tmp/tail%A\r\n").is_empty());
    assert!(strs(b"file:///tmp/x%\r\n").is_empty());
    assert!(strs(b"file:///tmp/x%ZZ\r\n").is_empty());
}

#[test]
fn nul_byte_in_decoded_path_rejected() {
    assert!(strs(b"file:///tmp/a%00b\r\n").is_empty());
}

#[test]
fn query_or_fragment_rejected() {
    assert!(strs(b"file:///tmp/a?x=1\r\n").is_empty());
    assert!(strs(b"file:///tmp/a#frag\r\n").is_empty());
}

#[test]
fn uri_without_absolute_path_rejected() {
    assert!(strs(b"file://justhost\r\n").is_empty());
}

#[test]
fn empty_payload_no_paths() {
    assert!(parse_uri_list(b"").is_empty());
    assert!(parse_uri_list(b"\r\n\r\n").is_empty());
}

// ---- Copy-only gate ----

#[test]
fn gate_confirms_only_copy_after_preference() {
    assert!(copy_drop_confirmed(Some(DropAction::Copy), true));
    assert!(!copy_drop_confirmed(Some(DropAction::Copy), false));
    assert!(!copy_drop_confirmed(Some(DropAction::Move), true));
    assert!(!copy_drop_confirmed(Some(DropAction::Ask), true));
    assert!(!copy_drop_confirmed(Some(DropAction::Other), true));
    assert!(!copy_drop_confirmed(None, true));
}

// ---- transfer bounds ----

#[test]
fn uri_cap_boundary() {
    assert!(!would_exceed_uri_cap(0, MAX_URI_BYTES));
    assert!(would_exceed_uri_cap(1, MAX_URI_BYTES));
    assert!(would_exceed_uri_cap(MAX_URI_BYTES, 1));
    // Saturating: no panic on absurd inputs.
    assert!(would_exceed_uri_cap(usize::MAX, usize::MAX));
}

#[test]
fn transfer_deadline() {
    let start = Instant::now();
    assert!(!transfer_expired(start, start));
    assert!(!transfer_expired(start, start + Duration::from_millis(10)));
    assert!(transfer_expired(start, start + TRANSFER_TIMEOUT));
    assert!(transfer_expired(
        start,
        start + TRANSFER_TIMEOUT + Duration::from_secs(1)
    ));
}

// ---- offer lifecycle + bound ----

#[test]
fn offer_table_is_bounded_and_evicts_oldest() {
    let mut core = DropCore::default();
    for id in 0..MAX_OFFERS as u32 {
        assert_eq!(core.register_offer(id), None);
    }
    assert_eq!(core.offer_count(), MAX_OFFERS);
    // One more evicts the oldest (id 0) and reports it for destruction.
    assert_eq!(core.register_offer(999), Some(0));
    assert_eq!(core.offer_count(), MAX_OFFERS);
}

#[test]
fn declined_enter_offer_is_destroyed_on_leave() {
    let mut core = DropCore::default();
    core.register_offer(7);
    // No uri support -> declined; but the enter offer is still tracked.
    let outcome = core.enter(1, 7, 0xdead, None);
    assert!(!outcome.accept);
    assert_eq!(core.current_enter_offer(1), Some(7));
    // Leave MUST return the enter offer to destroy (previously leaked).
    assert_eq!(core.leave(1), Some(7));
    assert_eq!(core.current_enter_offer(1), None);
}

#[test]
fn superseding_enter_destroys_prior_enter_offer() {
    let mut core = DropCore::default();
    core.register_offer(7);
    core.register_offer(8);
    core.enter(1, 7, 0, None);
    let outcome = core.enter(1, 8, 0, None);
    assert_eq!(outcome.stale_offer, Some(7));
    assert_eq!(core.current_enter_offer(1), Some(8));
}

#[test]
fn accepted_drop_needs_confirmed_copy() {
    let mut core = DropCore::default();
    core.register_offer(7);
    core.set_supports_uri(7);
    let ident = SurfaceIdent {
        window: 3,
        generation: 5,
    };
    let outcome = core.enter(1, 7, 0xabc, Some(ident));
    assert!(outcome.accept);
    core.mark_preference_sent(7);
    // No confirmed Copy yet -> refuse.
    match core.drop(1) {
        DropOutcome::Refuse {
            offer: 7,
            was_uri: true,
        } => {}
        other => panic!("expected refuse, got {other:?}"),
    }
    // Re-enter and confirm Copy after preference -> receive.
    core.register_offer(7);
    core.set_supports_uri(7);
    core.enter(1, 7, 0xabc, Some(ident));
    core.mark_preference_sent(7);
    core.note_action(7, Some(DropAction::Copy));
    match core.drop(1) {
        DropOutcome::Receive { drag } => {
            assert_eq!(drag.offer, 7);
            assert_eq!(drag.surface_ptr, 0xabc);
            assert_eq!(drag.ident, Some(ident));
        }
        other => panic!("expected receive, got {other:?}"),
    }
}

#[test]
fn move_after_preference_is_refused() {
    let mut core = DropCore::default();
    core.register_offer(4);
    core.set_supports_uri(4);
    core.enter(1, 4, 0, None);
    core.mark_preference_sent(4);
    core.note_action(4, Some(DropAction::Move));
    assert!(matches!(
        core.drop(1),
        DropOutcome::Refuse {
            offer: 4,
            was_uri: true
        }
    ));
}

#[test]
fn simultaneous_drops_on_two_seats_do_not_overwrite() {
    let mut core = DropCore::default();
    core.register_offer(10);
    core.register_offer(20);
    core.set_supports_uri(10);
    core.set_supports_uri(20);
    core.enter(1, 10, 0x1000, None);
    core.enter(2, 20, 0x2000, None);
    core.mark_preference_sent(10);
    core.mark_preference_sent(20);
    core.note_action(10, Some(DropAction::Copy));
    // Seat 2 stays Move; seat 1 confirmed Copy. Each resolves independently.
    core.note_action(20, Some(DropAction::Move));
    assert!(matches!(core.drop(1), DropOutcome::Receive { .. }));
    assert!(matches!(
        core.drop(2),
        DropOutcome::Refuse {
            offer: 20,
            was_uri: true
        }
    ));
}

#[test]
fn drop_with_no_drag_is_idle() {
    let mut core = DropCore::default();
    assert!(matches!(core.drop(1), DropOutcome::Idle));
}

#[test]
fn seat_removal_returns_enter_offer_for_destruction() {
    let mut core = DropCore::default();
    core.register_offer(9);
    core.enter(1, 9, 0, None);
    assert_eq!(core.remove_seat(1), vec![9]);
    assert_eq!(core.current_enter_offer(1), None);
}

#[test]
fn remove_offer_clears_seat_references() {
    let mut core = DropCore::default();
    core.register_offer(5);
    core.set_supports_uri(5);
    core.enter(1, 5, 0, None);
    core.remove_offer(5);
    assert_eq!(core.current_enter_offer(1), None);
    assert_eq!(core.offer_count(), 0);
}
