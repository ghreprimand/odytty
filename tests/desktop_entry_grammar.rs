// SPDX-License-Identifier: GPL-3.0-only
// Project-authored metadata; no application is discovered or spawned.
use odytty::desktop::exec_to_argv;

#[test]
fn unknown_field_codes_reject_the_entry() {
    assert!(exec_to_argv("fixture-viewer %z %f", "/fixture/document").is_none());
}

#[test]
fn unterminated_double_quotes_reject_the_entry() {
    assert!(exec_to_argv("fixture-viewer \"unfinished", "/fixture/document").is_none());
}
