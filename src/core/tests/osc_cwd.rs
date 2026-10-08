// SPDX-License-Identifier: GPL-3.0-only
//! OSC 7 working-directory tracking (SI1).
//!
//! Covers parsing of `file://host/path`, hostname policy (empty / localhost
//! accepted, foreign hosts ignored), percent-decoding edge cases, RIS reset
//! semantics, and the security invariants: malformed payloads are ignored
//! non-panicking and OSC 7 never leaks into the grid or emits a response.

use super::*;

/// Wrap a payload in an OSC 7 sequence terminated by BEL.
fn osc7_bel(payload: &str) -> Vec<u8> {
    let mut bytes = b"\x1b]7;".to_vec();
    bytes.extend_from_slice(payload.as_bytes());
    bytes.push(0x07);
    bytes
}

#[test]
fn osc7_sets_working_directory() {
    let mut terminal = Terminal::new(8, 3);
    assert_eq!(terminal.current_working_directory(), None);
    assert!(!terminal.take_working_directory_changed());

    terminal.advance(&osc7_bel("file://localhost/home/user/projects"));
    assert_eq!(
        terminal.current_working_directory(),
        Some("/home/user/projects")
    );
    assert!(terminal.take_working_directory_changed());
    // Flag clears after a poll.
    assert!(!terminal.take_working_directory_changed());
}

#[test]
fn osc7_empty_host_is_accepted() {
    let mut terminal = Terminal::new(8, 3);
    // file:///path - empty authority (the common shell form).
    terminal.advance(&osc7_bel("file:///var/log"));
    assert_eq!(terminal.current_working_directory(), Some("/var/log"));
}

#[test]
fn osc7_accepts_st_terminator() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"\x1b]7;file://localhost/tmp\x1b\\");
    assert_eq!(terminal.current_working_directory(), Some("/tmp"));
}

#[test]
fn osc7_scheme_and_host_are_case_insensitive() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("FILE://LocalHost/etc"));
    assert_eq!(terminal.current_working_directory(), Some("/etc"));
}

#[test]
fn osc7_foreign_host_is_ignored() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/local"));
    assert_eq!(terminal.current_working_directory(), Some("/local"));
    assert!(terminal.take_working_directory_changed());

    // A path on another machine must not overwrite the local cwd.
    terminal.advance(&osc7_bel("file://otherbox/remote/path"));
    assert_eq!(terminal.current_working_directory(), Some("/local"));
    assert!(!terminal.take_working_directory_changed());
}

#[test]
fn osc7_real_host_is_ignored_without_injected_local_hostname() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/local"));
    assert_eq!(terminal.current_working_directory(), Some("/local"));
    assert!(terminal.take_working_directory_changed());

    terminal.advance(&osc7_bel("file://testhost/home/user/project"));
    assert_eq!(terminal.current_working_directory(), Some("/local"));
    assert!(!terminal.take_working_directory_changed());
}

#[test]
fn osc7_accepts_injected_local_hostname() {
    let mut terminal = Terminal::new(8, 3);
    terminal.set_local_hostname(Some("testhost".to_owned()));

    terminal.advance(&osc7_bel("file://testhost/home/user/project"));

    assert_eq!(
        terminal.current_working_directory(),
        Some("/home/user/project")
    );
    assert!(terminal.take_working_directory_changed());
}

#[test]
fn osc7_injected_hostname_exact_match_is_case_insensitive() {
    let mut terminal = Terminal::new(8, 3);
    terminal.set_local_hostname(Some("fixture.example.invalid".to_owned()));
    terminal.advance(&osc7_bel("file://FIXTURE.EXAMPLE.INVALID/fixture"));
    assert_eq!(terminal.current_working_directory(), Some("/fixture"));
    assert!(terminal.take_working_directory_changed());
}

#[test]
fn osc7_shared_hostname_label_does_not_establish_local_authority() {
    for local in ["fixture.example.invalid", "fixture"] {
        let mut terminal = Terminal::new(8, 3);
        terminal.set_local_hostname(Some(local.to_owned()));
        terminal.advance(&osc7_bel("file://localhost/safe"));
        assert!(terminal.take_working_directory_changed());
        for host in ["fixture.remote.invalid", "fixture.other.invalid"] {
            terminal.advance(&osc7_bel(&format!("file://{host}/remote")));
            assert_eq!(terminal.current_working_directory(), Some("/safe"));
            assert!(!terminal.take_working_directory_changed());
            assert!(terminal.take_host_output().is_empty());
        }
        if local.contains('.') {
            terminal.advance(&osc7_bel("file://fixture/remote"));
            assert_eq!(terminal.current_working_directory(), Some("/safe"));
            assert!(!terminal.take_working_directory_changed());
        }
    }
}

#[test]
fn osc7_injected_hostname_still_rejects_remote_hosts() {
    let mut terminal = Terminal::new(8, 3);
    terminal.set_local_hostname(Some("testhost".to_owned()));
    terminal.advance(&osc7_bel("file://testhost/home/user/local"));
    assert_eq!(
        terminal.current_working_directory(),
        Some("/home/user/local")
    );
    assert!(terminal.take_working_directory_changed());

    terminal.advance(&osc7_bel("file://otherbox/home/user/remote"));

    assert_eq!(
        terminal.current_working_directory(),
        Some("/home/user/local")
    );
    assert!(!terminal.take_working_directory_changed());
}

#[test]
fn osc7_percent_decodes_path() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/home/user/My%20Projects"));
    assert_eq!(
        terminal.current_working_directory(),
        Some("/home/user/My Projects")
    );
}

#[test]
fn osc7_percent_decodes_utf8_path() {
    let mut terminal = Terminal::new(8, 3);
    // "/café" percent-encoded (é = C3 A9).
    terminal.advance(&osc7_bel("file://localhost/caf%C3%A9"));
    assert_eq!(terminal.current_working_directory(), Some("/café"));
}

#[test]
fn osc7_malformed_percent_escape_is_ignored() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/ok"));
    assert_eq!(terminal.current_working_directory(), Some("/ok"));

    // Truncated escape at end-of-payload: ignore, leave cwd unchanged.
    terminal.advance(&osc7_bel("file://localhost/bad%2"));
    assert_eq!(terminal.current_working_directory(), Some("/ok"));
    // Non-hex digits after '%'.
    terminal.advance(&osc7_bel("file://localhost/bad%zz"));
    assert_eq!(terminal.current_working_directory(), Some("/ok"));
    // Bare trailing '%'.
    terminal.advance(&osc7_bel("file://localhost/bad%"));
    assert_eq!(terminal.current_working_directory(), Some("/ok"));
}

#[test]
fn osc7_decoded_nul_is_rejected() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/safe"));
    assert_eq!(terminal.current_working_directory(), Some("/safe"));

    // %00 decodes to NUL, which is never valid in a path - ignore the update.
    terminal.advance(&osc7_bel("file://localhost/bad%00path"));
    assert_eq!(terminal.current_working_directory(), Some("/safe"));
}

#[test]
fn osc7_non_file_scheme_is_ignored() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("http://localhost/page"));
    assert_eq!(terminal.current_working_directory(), None);
}

#[test]
fn osc7_missing_path_is_ignored() {
    let mut terminal = Terminal::new(8, 3);
    // No '/' after the authority: no directory component.
    terminal.advance(&osc7_bel("file://localhost"));
    assert_eq!(terminal.current_working_directory(), None);
}

#[test]
fn osc7_empty_payload_is_ignored() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel(""));
    assert_eq!(terminal.current_working_directory(), None);
}

#[test]
fn osc7_preserves_embedded_semicolon_in_path() {
    let mut terminal = Terminal::new(8, 3);
    // The OSC parser splits payloads on ';'; the path must be rejoined intact.
    terminal.advance(&osc7_bel("file://localhost/a;b/c"));
    assert_eq!(terminal.current_working_directory(), Some("/a;b/c"));
}

#[test]
fn osc7_invalid_utf8_is_replaced_lossily() {
    let mut terminal = Terminal::new(8, 3);
    // %FF is not valid UTF-8; it must be replaced, not panic or be rejected.
    terminal.advance(&osc7_bel("file://localhost/bad%FFdir"));
    let cwd = terminal
        .current_working_directory()
        .expect("cwd set despite invalid byte");
    assert!(cwd.contains('\u{FFFD}'));
}

#[test]
fn osc7_payload_does_not_leak_into_grid() {
    let mut terminal = Terminal::new(40, 3);
    terminal.advance(b"A");
    terminal.advance(&osc7_bel("file://localhost/secret/dir"));
    terminal.advance(b"B");
    // Only the printed characters reach the grid; the URL does not.
    assert!(terminal.screen().plain_text().starts_with("AB"));
    assert!(!terminal.screen().plain_text().contains("secret"));
    assert!(!terminal.screen().plain_text().contains("file://"));
}

#[test]
fn osc7_emits_no_host_response() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/home"));
    // OSC 7 is a report from the shell; the terminal must not reply.
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn osc7_cwd_survives_ris() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/persist"));
    assert_eq!(terminal.current_working_directory(), Some("/persist"));

    // RIS resets the terminal, not the shell; the reported cwd remains valid.
    terminal.advance(b"\x1bc");
    assert_eq!(terminal.current_working_directory(), Some("/persist"));
}

#[test]
fn osc7_oversized_payload_does_not_panic() {
    let mut terminal = Terminal::new(8, 3);
    // Far beyond the parser's OSC cap; must be handled without panic.
    let mut payload = String::from("file://localhost/");
    payload.push_str(&"a".repeat(300_000));
    terminal.advance(&osc7_bel(&payload));
    // Whatever the cap retains, the prefix is a valid path, so cwd is set and
    // no panic occurred.
    assert!(
        terminal
            .current_working_directory()
            .is_some_and(|c| c.starts_with("/a"))
    );
}

#[test]
fn osc6_is_accepted_and_ignored() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file://localhost/from7"));
    // OSC 6 carries no directory semantics we track: it must not change cwd.
    terminal.advance(b"\x1b]6;file://localhost/from6\x07");
    assert_eq!(terminal.current_working_directory(), Some("/from7"));
    assert!(terminal.take_host_output().is_empty());
}

#[cfg(windows)]
#[test]
fn osc7_windows_network_and_device_paths_leave_cwd_unchanged() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file:///C:/fixture"));
    assert_eq!(terminal.current_working_directory(), Some("C:/fixture"));
    terminal.take_working_directory_changed();
    for payload in [
        "file:////fixture.invalid/share",
        "file:///%2Ffixture.invalid/share",
        "file:///%5Cfixture.invalid/share",
        "file:///%5C%5C?%5CC:%5Cfixture",
        "file:///%5C%5C.%5Cpipe%5Cfixture",
    ] {
        terminal.advance(&osc7_bel(payload));
        assert_eq!(terminal.current_working_directory(), Some("C:/fixture"));
        assert!(!terminal.take_working_directory_changed());
        assert!(terminal.take_host_output().is_empty());
    }
}

// Project-authored file URL fixtures exercise the real OSC parser on every OS.
fn assert_osc7_refused(payload: &str) {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"AB");
    terminal.advance(&osc7_bel("file://localhost/safe"));
    assert!(terminal.take_working_directory_changed());
    let before = terminal.snapshot();
    terminal.advance(&osc7_bel(payload));
    assert_eq!(
        terminal.current_working_directory(),
        Some("/safe"),
        "{payload}"
    );
    assert!(!terminal.take_working_directory_changed(), "{payload}");
    assert!(terminal.take_host_output().is_empty(), "{payload}");
    assert_eq!(terminal.snapshot(), before, "{payload}");
}

#[test]
fn osc7_network_shaped_paths_are_refused_on_every_platform() {
    for payload in [
        "file:////fixture.invalid/share",
        "file://localhost//fixture.invalid/share",
        "file:///%2Ffixture.invalid/share",
        "file://localhost/%2ffixture.invalid/share",
        "file://localhost/%2F%2Ffixture.invalid/share",
    ] {
        assert_osc7_refused(payload);
    }
}

#[test]
fn osc7_literal_and_decoded_backslashes_are_refused() {
    for payload in [
        r"file:///fixture\name",
        "file:///fixture%5Cname",
        "file:///fixture%5cname",
        "file:///%5Cfixture.invalid/share",
        "file:///%5C%5C?%5CC:%5Cfixture",
        "file:///%5C%5C.%5Cpipe%5Cfixture",
        "file:///C:/fixture%5Cname",
    ] {
        assert_osc7_refused(payload);
    }
}

#[test]
fn osc7_decoded_ascii_controls_are_refused() {
    for byte in (0u8..=31).chain(std::iter::once(127)) {
        assert_osc7_refused(&format!("file:///fixture%{byte:02X}name"));
    }
}

#[test]
fn osc7_decoded_unicode_controls_are_refused() {
    for byte in 0x80u8..=0x9f {
        assert_osc7_refused(&format!("file:///fixture%C2%{byte:02X}name"));
    }
}

#[test]
fn osc7_slash_drive_paths_and_safe_escaped_characters_remain_valid() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(&osc7_bel("file:///C:/fixture%20name%3Bpart%25"));
    let expected = if cfg!(windows) {
        "C:/fixture name;part%"
    } else {
        "/C:/fixture name;part%"
    };
    assert_eq!(terminal.current_working_directory(), Some(expected));
    assert!(terminal.take_working_directory_changed());
    assert!(terminal.take_host_output().is_empty());
}
