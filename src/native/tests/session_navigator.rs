// SPDX-License-Identifier: GPL-3.0-only
//! Adversarial coverage for the unified navigator read model and selection map.

use super::super::overlay::OverlayInput;
use super::super::session::{HeadlessSession, Session, SessionToken, WorkspaceSet};
use super::super::session_attach_overlay::{SessionAttachOverlay, SessionAttachOverlayOutcome};
use super::super::session_navigator::{
    MAX_NAVIGATOR_ROWS, NavigatorAction, NavigatorEntry, NavigatorTarget, append_detached,
    live_entries,
};
use crate::core::{Attrs, Cell, Dimensions, Position, Snapshot};
use crate::native::test_support::headless_writer;
use crate::session_host::ListedSession;
use std::sync::{Arc, Mutex};

fn detached(id: &str) -> ListedSession {
    ListedSession {
        id: id.to_owned(),
        name: format!("session-{id}"),
        state: "idle",
        age_ms: 1,
        pane_count: 1,
    }
}

#[test]
fn navigator_uppercase_x_only_targets_detached_sessions() {
    let mut overlay = SessionAttachOverlay::new();
    overlay.open(vec![live(9), NavigatorEntry::from(detached("s-kill"))]);
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('X')),
        SessionAttachOverlayOutcome::Consumed,
        "X never turns a live focus-list row into a kill request"
    );
    let _ = overlay.handle_input(OverlayInput::Down);
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('X')),
        SessionAttachOverlayOutcome::NavigatorAction(NavigatorAction::Close(
            NavigatorTarget::Detached("s-kill".to_owned())
        ))
    );
}

fn live(token: u64) -> NavigatorEntry {
    NavigatorEntry {
        target: NavigatorTarget::Live(SessionToken(token)),
        stable_id: format!("live:{token}"),
        name: "local session".to_owned(),
        detail: "local cwd unavailable".to_owned(),
        status: "running".to_owned(),
        unread: false,
        profile: Some("default".to_owned()),
        preview: Vec::new(),
    }
}

#[test]
fn navigator_maps_live_and_detached_selection_to_existing_stable_owners() {
    let mut overlay = SessionAttachOverlay::new();
    overlay.open(vec![live(41), NavigatorEntry::from(detached("s-041"))]);

    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        SessionAttachOverlayOutcome::Focus(SessionToken(41))
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        SessionAttachOverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        SessionAttachOverlayOutcome::Attach("s-041".to_owned())
    );
}

#[test]
fn navigator_detached_append_is_bounded_and_keeps_stable_registry_ids() {
    let mut entries = vec![live(7)];
    let source = (0..MAX_NAVIGATOR_ROWS + 9)
        .map(|index| detached(&format!("s-{index:03}")))
        .collect();
    append_detached(&mut entries, source);

    assert_eq!(entries.len(), MAX_NAVIGATOR_ROWS);
    assert_eq!(entries[0].stable_id, "live:7");
    assert_eq!(entries[1].stable_id, "detached:s-000");
    assert_eq!(
        entries.last().map(|entry| entry.stable_id.as_str()),
        Some("detached:s-1022")
    );
}

#[test]
fn navigator_rows_strip_control_characters_from_every_displayed_field() {
    let entry = NavigatorEntry {
        target: NavigatorTarget::Detached("s-unsafe".to_owned()),
        stable_id: "detached:s-unsafe".to_owned(),
        name: "title\u{1b}[31m".to_owned(),
        detail: "remote example.invalid\nleak".to_owned(),
        status: "running\rerror".to_owned(),
        unread: true,
        profile: Some("profile\tunsafe".to_owned()),
        preview: Vec::new(),
    };

    let row = entry.row_label();
    assert!(row.chars().all(|character| !character.is_control()));
    assert!(row.contains("unread"));
    assert!(row.contains("profile:profileunsafe"));
}

#[test]
fn navigator_lifecycle_race_actions_keep_the_original_stable_target() {
    let target = NavigatorTarget::Live(SessionToken(91));
    let mut overlay = SessionAttachOverlay::new();
    overlay.open(vec![live(91)]);

    let expected = [
        (
            OverlayInput::Char('r'),
            NavigatorAction::Rename(target.clone()),
        ),
        (
            OverlayInput::Char('d'),
            NavigatorAction::Duplicate(target.clone()),
        ),
        (
            OverlayInput::Char('m'),
            NavigatorAction::Move(target.clone()),
        ),
        (
            OverlayInput::Char('x'),
            NavigatorAction::Close(target.clone()),
        ),
    ];
    for (input, action) in expected {
        assert_eq!(
            overlay.handle_input(input),
            SessionAttachOverlayOutcome::NavigatorAction(action),
            "the frozen selection must retain its token while the arena changes underneath it"
        );
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('o')),
        SessionAttachOverlayOutcome::NavigatorAction(NavigatorAction::Reopen)
    );
}

#[test]
fn navigator_refresh_reorder_preserves_selection_by_stable_id_not_row_index() {
    let mut overlay = SessionAttachOverlay::new();
    let selected = NavigatorTarget::Live(SessionToken(17));
    overlay.open_selected(
        vec![live(3), live(17), live(42)],
        Some(&selected.stable_id()),
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        SessionAttachOverlayOutcome::Focus(SessionToken(17))
    );

    // A concurrent close can reorder the fresh snapshot. Selection must still
    // target token 17 rather than whichever item occupies the old row.
    overlay.open_selected(
        vec![live(42), live(17), live(3)],
        Some(&selected.stable_id()),
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        SessionAttachOverlayOutcome::Focus(SessionToken(17))
    );
}

#[test]
fn navigator_search_reaches_a_live_row_beyond_the_display_cap() {
    let mut overlay = SessionAttachOverlay::new();
    let entries = (0..96)
        .map(|token| NavigatorEntry {
            name: format!("pane-{token:03}"),
            ..live(token)
        })
        .collect::<Vec<_>>();
    overlay.open(entries);
    assert_eq!(overlay.entry_count(), 96);

    for character in "pane-095".chars() {
        assert_eq!(
            overlay.handle_input(OverlayInput::Char(character)),
            SessionAttachOverlayOutcome::Consumed
        );
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        SessionAttachOverlayOutcome::Focus(SessionToken(95)),
        "filtering must search the full frozen model, not only its first 40 rendered rows"
    );
}

#[test]
fn navigator_focus_of_background_workspace_switches_workspace_by_token() {
    let dimensions = Dimensions::new(40, 8);
    let mut sessions = workspace_set(dimensions);
    let background_token = SessionToken(1);
    sessions.push_workspace(headless_session(background_token, dimensions));
    assert!(sessions.switch_workspace(1));
    assert!(sessions.switch_workspace(0));

    assert!(sessions.switch(background_token));
    assert_eq!(sessions.locate_token(background_token), Some((1, 0)));
    assert_eq!(sessions.active_id(), background_token);
}

#[test]
fn navigator_remote_disconnect_is_exited_and_preview_is_opt_in_and_redacted() {
    let dimensions = Dimensions::new(48, 10);
    let mut sessions = workspace_set(dimensions);
    let token = sessions.active_id();
    let session = sessions.get_mut(token).expect("active session");
    session.remote_destination = Some("tester@edge.example.test:2200".to_owned());
    session.launch_profile = Some("restored-profile".to_owned());
    session.last_presented_snapshot = Some(snapshot(&[
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "KEY=secret TOKEN=token PASSWORD=password AWS_SECRET=aws ssh tester@edge.example.test:2200",
        "nine",
    ]));

    let without_preview = live_entries(&sessions, false);
    let without_preview = live_entry(&without_preview, token);
    assert_eq!(without_preview.status, "exited");
    assert!(without_preview.detail.contains("remote edge.example.test"));
    assert_eq!(without_preview.profile.as_deref(), Some("restored-profile"));
    assert!(without_preview.preview.is_empty(), "preview is opt-in");

    let with_preview = live_entries(&sessions, true);
    let with_preview = live_entry(&with_preview, token);
    assert_eq!(
        with_preview.preview.len(),
        8,
        "preview has a strict row cap"
    );
    let preview = with_preview.preview.join(" ");
    for secret in [
        "secret",
        "token",
        "password",
        "aws",
        "tester@edge.example.test:2200",
    ] {
        assert!(
            !preview.contains(secret),
            "preview leaked sensitive value {secret:?}: {preview:?}"
        );
    }
}

#[test]
fn stale_registry_entry_is_error_and_never_emits_attach() {
    let mut overlay = SessionAttachOverlay::new();
    overlay.open(vec![NavigatorEntry::from(ListedSession {
        id: "s-stale".to_owned(),
        name: "stale socket".to_owned(),
        state: "error",
        age_ms: 0,
        pane_count: 0,
    })]);
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        SessionAttachOverlayOutcome::Consumed,
        "a dead registry record is informative only; it must not invoke attach"
    );
}

#[test]
fn navigator_preview_redacts_aws_secret_access_key() {
    assert_preview_redacts("AWS_SECRET_ACCESS_KEY=aws-secret", "aws-secret");
}

#[test]
fn navigator_preview_redacts_github_token_export() {
    assert_preview_redacts(
        "export GITHUB_TOKEN=ghp_example_secret",
        "ghp_example_secret",
    );
}

#[test]
fn navigator_preview_redacts_windows_and_powershell_assignments() {
    for (line, secret) in [
        ("set PASSWD=windows-secret", "windows-secret"),
        ("$env:API_KEY=\"powershell-secret\"", "powershell-secret"),
    ] {
        assert_preview_redacts(line, secret);
    }
}

#[test]
fn navigator_preview_redacts_json_and_yaml_secret_fields() {
    for (line, secret) in [
        (r#"{\"password\": \"json-secret\"}"#, "json-secret"),
        ("'secret': 'single-quoted-secret'", "single-quoted-secret"),
        ("password: yaml-secret", "yaml-secret"),
    ] {
        assert_preview_redacts(line, secret);
    }
}

#[test]
fn navigator_preview_redacts_bearer_and_url_and_ssh_credentials() {
    for (line, secret) in [
        (
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload",
            "eyJhbGciOiJIUzI1NiJ9.payload",
        ),
        ("https://user:pass@example.test/path", "pass"),
        ("ssh://tester@example.test:2222", "tester"),
    ] {
        assert_preview_redacts(line, secret);
    }
}

#[test]
fn navigator_preview_redacts_private_key_and_unlabelled_high_entropy_tokens() {
    // The PEM header is assembled at runtime so the tracked source never
    // contains a complete private-key marker for secret scanners to flag.
    let pem_header = format!("-----BEGIN OPENSSH {} {}-----", "PRIVATE", "KEY");
    for (line, secret) in [
        (pem_header.as_str(), "OPENSSH"),
        (
            "0123456789abcdef0123456789abcdef01234567",
            "0123456789abcdef0123456789abcdef01234567",
        ),
        (
            "QWxhZGRpbjpvcGVuIHNlc2FtZSB0b2tlbi1yZWdyZXNzaW9u",
            "QWxhZGRpbjpvcGVu",
        ),
    ] {
        assert_preview_redacts(line, secret);
    }
}

#[test]
fn navigator_preview_handles_mixed_case_unicode_controls_and_long_lines_without_leaking() {
    assert_preview_redacts("PaSsWoRd=mixed-case-secret", "mixed-case-secret");
    let homoglyph = preview_for_line("pаssword=unicode-lookalike");
    assert!(!homoglyph.contains('\u{1b}'));
    let long_secret = "x".repeat(10 * 1024);
    let preview = preview_for_line(&format!("TOKEN={long_secret}\u{1b}[31m"));
    assert!(preview.len() <= 96, "one preview row is bounded");
    assert!(!preview.contains('\u{1b}'));
    assert!(!preview.contains(&long_secret));
}

#[test]
fn navigator_preview_preserves_benign_terminal_and_documentation_text() {
    for line in [
        "PATH=/usr/bin",
        "ls -la",
        "the key= form accepts an argument",
        "git log --oneline",
        "cd token/",
    ] {
        let preview = preview_for_line(line);
        assert!(
            preview.contains(line),
            "benign text was over-redacted: {line:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn navigator_windows_snapshot_has_no_detached_rows_or_preview_unavailable_claim() {
    let dimensions = Dimensions::new(40, 8);
    let sessions = workspace_set(dimensions);
    let entries = live_entries(&sessions, false);
    assert!(
        entries
            .iter()
            .all(|entry| !matches!(entry.target, NavigatorTarget::Detached(_)))
    );
    assert!(
        entries
            .iter()
            .flat_map(|entry| entry.preview.iter())
            .all(|line| line != "preview unavailable")
    );
}

fn workspace_set(dimensions: Dimensions) -> WorkspaceSet {
    WorkspaceSet::new(headless_session(SessionToken(0), dimensions), None)
}

fn headless_session(token: SessionToken, dimensions: Dimensions) -> Session {
    Session::new_headless(
        token,
        Arc::new(Mutex::new(crate::core::Terminal::new(
            dimensions.columns,
            dimensions.rows,
        ))),
        headless_writer(),
        Arc::new(HeadlessSession::new(dimensions)),
    )
}

fn live_entry(entries: &[NavigatorEntry], token: SessionToken) -> &NavigatorEntry {
    entries
        .iter()
        .find(|entry| entry.target == NavigatorTarget::Live(token))
        .expect("live navigator row")
}

fn snapshot(rows: &[&str]) -> Snapshot {
    let columns = rows
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(1);
    let mut cells = Vec::with_capacity(columns * rows.len());
    for row in rows {
        cells.extend(
            row.chars()
                .map(|character| Cell::new(character, Attrs::default()))
                .chain(std::iter::repeat_with(Cell::blank).take(columns - row.chars().count())),
        );
    }
    Snapshot {
        dimensions: Dimensions::new(columns, rows.len()),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: Default::default(),
        cells,
    }
}

fn preview_for_line(line: &str) -> String {
    let dimensions = Dimensions::new(line.chars().count().max(1), 1);
    let mut sessions = workspace_set(dimensions);
    let token = sessions.active_id();
    sessions
        .get_mut(token)
        .expect("active session")
        .last_presented_snapshot = Some(snapshot(&[line]));
    live_entry(&live_entries(&sessions, true), token)
        .preview
        .join(" ")
}

fn assert_preview_redacts(line: &str, secret: &str) {
    let preview = preview_for_line(line);
    assert!(
        preview.contains("[redacted]"),
        "redaction marker missing: {preview:?}"
    );
    assert!(
        !preview.contains(secret),
        "preview leaked {secret:?}: {preview:?}"
    );
}
