// SPDX-License-Identifier: GPL-3.0-only
//! Connection form tests with private access through the form module.
#[test]
fn query_backspace_removes_one_terminal_owner() {
    for owner in [
        "e\u{301}",
        "\u{1f469}\u{200d}\u{1f4bb}",
        "\u{1f1fa}\u{1f1f8}",
        "\u{915}\u{94d}\u{937}\u{93f}",
        "\u{0e01}\u{0e49}",
    ] {
        let mut field = ConnectionForm::new();
        field.open_add(Vec::new());
        field.open_key_browse(Vec::new());
        for ch in format!("x{owner}").chars() {
            field.handle_input(OverlayInput::Char(ch));
        }
        assert_eq!(
            field.browse.as_ref().expect("browser").query,
            format!("x{owner}")
        );
        field.handle_input(OverlayInput::Backspace);
        assert_eq!(
            field.browse.as_ref().expect("browser").query,
            "x",
            "{owner:?}"
        );
        field.handle_input(OverlayInput::Backspace);
        assert_eq!(field.browse.as_ref().expect("browser").query, "");
        field.handle_input(OverlayInput::Backspace);
        assert_eq!(field.browse.as_ref().expect("browser").query, "");
    }
}

use super::*;

#[test]
fn form_values_backspace_removes_whole_owners() {
    for field in [
        FormField::Alias,
        FormField::HostName,
        FormField::User,
        FormField::Port,
        FormField::IdentityFile,
        FormField::Theme,
        FormField::Font,
        FormField::Title,
    ] {
        for owner in [
            "e\u{301}",
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{1f1fa}\u{1f1f8}",
            "\u{915}\u{94d}\u{937}",
            "\u{e01}\u{e33}",
        ] {
            let mut form = ConnectionForm::new();
            form.focus = field;
            for ch in format!("x{owner}").chars() {
                form.handle_input(OverlayInput::Char(ch));
            }
            let before = form.render_signature();
            form.handle_input(OverlayInput::Backspace);
            assert_eq!(
                form.text_buf_mut(field).unwrap(),
                "x",
                "{field:?}: {owner:?}"
            );
            assert_ne!(form.render_signature(), before);
            for _ in 0..2 {
                form.handle_input(OverlayInput::Backspace);
            }
            assert!(form.text_buf_mut(field).unwrap().is_empty());
        }
    }
}

fn typed(form: &mut ConnectionForm, text: &str) {
    for ch in text.chars() {
        form.handle_input(OverlayInput::Char(ch));
    }
}

fn saved(form: &mut ConnectionForm) -> Option<(ConnectionHost, Option<String>)> {
    match form.handle_input(OverlayInput::Save) {
        ConnectionFormOutcome::Save { host, edit_target } => Some((*host, edit_target)),
        _ => None,
    }
}

#[test]
fn add_form_builds_a_host_and_appends() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    // Focus starts on Alias.
    typed(&mut form, "web1");
    form.handle_input(OverlayInput::Down); // HostName
    typed(&mut form, "gateway.example.invalid");
    form.handle_input(OverlayInput::Down); // User
    typed(&mut form, "deploy");
    form.handle_input(OverlayInput::Down); // Port
    typed(&mut form, "2222");
    let (host, edit_target) = saved(&mut form).expect("save");
    assert_eq!(edit_target, None, "Add appends");
    assert_eq!(host.alias, "web1");
    assert_eq!(host.host_name.as_deref(), Some("gateway.example.invalid"));
    assert_eq!(host.user.as_deref(), Some("deploy"));
    assert_eq!(host.port, Some(2222));
}

#[test]
fn empty_alias_blocks_save_with_an_inline_error() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    assert_eq!(
        form.handle_input(OverlayInput::Save),
        ConnectionFormOutcome::Consumed
    );
    let lines = form.visible_lines(72, 40);
    assert!(lines.iter().any(|l| l.text.contains("Alias is required")));
}

#[test]
fn bad_port_blocks_save() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    typed(&mut form, "h");
    form.handle_input(OverlayInput::Down); // HostName
    form.handle_input(OverlayInput::Down); // User
    form.handle_input(OverlayInput::Down); // Port
    typed(&mut form, "99999");
    assert_eq!(
        form.handle_input(OverlayInput::Save),
        ConnectionFormOutcome::Consumed
    );
    assert!(form.error.as_deref().unwrap().contains("Port"));
}

#[test]
fn alias_collision_blocks_add() {
    let mut form = ConnectionForm::new();
    form.open_add(vec!["taken".to_owned()]);
    typed(&mut form, "taken");
    assert_eq!(
        form.handle_input(OverlayInput::Save),
        ConnectionFormOutcome::Consumed
    );
    assert!(form.error.as_deref().unwrap().contains("already exists"));
}

#[test]
fn leading_dash_alias_is_rejected() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    typed(&mut form, "-bad");
    assert_eq!(
        form.handle_input(OverlayInput::Save),
        ConnectionFormOutcome::Consumed
    );
    assert!(form.error.is_some());
}

#[test]
fn edit_prefills_and_saves_with_the_original_alias() {
    let host = ConnectionHost {
        alias: "alpha".to_owned(),
        host_name: Some("alpha.example.invalid".to_owned()),
        user: Some("alice".to_owned()),
        port: Some(2200),
        theme: None,
        font: None,
        title: None,
        integration: Some(false),
        reuse: None,
        tmux: None,
        protocol: Some("ssh".to_owned()),
        persist: None,
        identity_file: Some("/keys/alpha.example".to_owned()),
        source: ConnectionHostSource::Odytty,
    };
    let mut form = ConnectionForm::new();
    form.open_edit(&host, Vec::new());
    // Rename alpha -> alpha2 (append a "2").
    typed(&mut form, "2");
    let (built, edit_target) = saved(&mut form).expect("save");
    assert_eq!(
        edit_target.as_deref(),
        Some("alpha"),
        "Edit targets the original block"
    );
    assert_eq!(built.alias, "alpha2");
    // Pre-filled fields survive.
    assert_eq!(built.user.as_deref(), Some("alice"));
    assert_eq!(built.port, Some(2200));
    assert_eq!(built.integration, Some(false));
    // Reserved protocol is carried through opaquely, not dropped.
    assert_eq!(built.protocol.as_deref(), Some("ssh"));
    // IdentityFile is pre-filled and preserved.
    assert_eq!(built.identity_file.as_deref(), Some("/keys/alpha.example"));
}

#[test]
fn advanced_toggle_reveals_and_hides_override_fields() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    assert!(!form.advanced);
    assert!(!form.fields().contains(&FormField::Integration));
    form.focus = FormField::Advanced;
    form.handle_input(OverlayInput::Activate);
    assert!(form.advanced);
    assert!(form.fields().contains(&FormField::Integration));
}

#[test]
fn tristate_cycles_inherit_on_off() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    form.focus = FormField::Reuse;
    assert_eq!(form.reuse, None);
    form.handle_input(OverlayInput::Right);
    assert_eq!(form.reuse, Some(true));
    form.handle_input(OverlayInput::Right);
    assert_eq!(form.reuse, Some(false));
    form.handle_input(OverlayInput::Right);
    assert_eq!(form.reuse, None);
    // Left cycles the other way.
    form.handle_input(OverlayInput::Left);
    assert_eq!(form.reuse, Some(false));
    // Space also cycles a tri-state (there is no text to type there).
    form.reuse = None;
    form.handle_input(OverlayInput::Char(' '));
    assert_eq!(form.reuse, Some(true));
}

#[test]
fn identity_file_field_round_trips_into_the_saved_host() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    typed(&mut form, "k");
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    typed(&mut form, "/keys/id_ed25519.example");
    let (host, _) = saved(&mut form).expect("save");
    assert_eq!(
        host.identity_file.as_deref(),
        Some("/keys/id_ed25519.example")
    );
}

#[test]
fn cancel_closes_without_saving() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.focus = FormField::Cancel;
    assert_eq!(
        form.handle_input(OverlayInput::Activate),
        ConnectionFormOutcome::Close
    );
}

#[test]
fn esc_closes() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    assert_eq!(
        form.handle_input(OverlayInput::Close),
        ConnectionFormOutcome::Close
    );
}

#[test]
fn click_focuses_a_field_and_acts_on_a_button() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.visible_lines(80, 40);
    // Row 1 is HostName (row 0 = Alias). A click focuses it.
    assert_eq!(
        form.handle_pointer_press(1, 0, 80),
        ConnectionFormOutcome::Consumed
    );
    assert_eq!(form.focus, FormField::HostName);
    // Find the Cancel row and click it → Close.
    let cancel_row = form
        .rows()
        .iter()
        .position(|r| matches!(r, FormRow::Field(FormField::Cancel)))
        .expect("cancel row");
    assert_eq!(
        form.handle_pointer_press(cancel_row, 0, 80),
        ConnectionFormOutcome::Close
    );
}

#[test]
fn test_action_validates_and_emits_a_probe_request() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    typed(&mut form, "reachable-host");
    form.focus = FormField::Test;
    match form.handle_input(OverlayInput::Activate) {
        ConnectionFormOutcome::Test(host) => assert_eq!(host.alias, "reachable-host"),
        other => panic!("expected Test, got {other:?}"),
    }
    assert_eq!(form.test, TestState::Running);
    // The Running status renders (no swatch while pending).
    let lines = form.visible_lines(72, 40);
    assert!(lines.iter().any(|l| l.text.contains("Testing")));
}

#[test]
fn test_action_on_an_invalid_host_shows_an_error_not_a_probe() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    // Empty alias: Test cannot build a host.
    form.focus = FormField::Test;
    assert_eq!(
        form.handle_input(OverlayInput::Activate),
        ConnectionFormOutcome::Consumed
    );
    assert!(form.error.is_some());
    assert_eq!(form.test, TestState::Idle);
}

#[test]
fn set_test_result_renders_auth_availability_with_a_swatch() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    // Amber reports an available method without promising successful authentication.
    form.set_test_result(Ok(ProbeClass::InteractiveAuth));
    let lines = form.visible_lines(72, 40);
    let status = lines
        .iter()
        .find(|l| l.swatch.is_some())
        .expect("a swatched status row");
    assert_eq!(status.swatch, Some(TEST_AMBER));
    assert!(status.text.contains("interactive authentication"));
    assert!(
        form.test_status()
            .unwrap()
            .0
            .contains("credentials required")
    );
    // A failure is red.
    form.set_test_result(Ok(ProbeClass::Unreachable));
    let red = form
        .visible_lines(72, 40)
        .into_iter()
        .find(|l| l.swatch == Some(TEST_RED));
    assert!(red.is_some());
}

#[test]
fn probe_failure_rows_distinguish_auth_and_key_trust() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    for (class, label) in [
        (ProbeClass::AuthRejected, "Authentication rejected"),
        (ProbeClass::HostKeyUntrusted, "Host key untrusted"),
        (ProbeClass::HostKeyMismatch, "Host key mismatch"),
    ] {
        form.set_test_result(Ok(class));
        let (text, color) = form.test_status().unwrap();
        assert!(text.starts_with(label));
        assert_eq!(color, Some(TEST_RED));
    }
}

#[test]
fn editing_a_field_invalidates_a_prior_test_result() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.set_test_result(Ok(ProbeClass::AuthOk));
    assert!(matches!(form.test, TestState::Done(_)));
    // Typing into the focused field clears the stale result.
    typed(&mut form, "x");
    assert_eq!(form.test, TestState::Idle);
}

#[test]
fn visible_lines_are_bounded_by_body_height() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    assert!(form.visible_lines(72, 5).len() <= 5);
}

#[test]
fn enter_on_empty_identity_file_requests_the_key_browser() {
    // FORM-UX IDENTITY-BROWSE: Enter on an EMPTY IdentityFile field asks the
    // App to scan ~/.ssh; a non-empty field advances instead (manual typing
    // is never interrupted).
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    assert_eq!(
        form.handle_input(OverlayInput::Activate),
        ConnectionFormOutcome::BrowseIdentityKeys
    );
    // With a typed path, Enter advances focus rather than browsing.
    typed(&mut form, "/keys/mine");
    assert_eq!(
        form.handle_input(OverlayInput::Activate),
        ConnectionFormOutcome::Consumed
    );
    assert_ne!(form.focus, FormField::IdentityFile);
}

/// The rendered IdentityFile row at the given body width (Advanced expanded).
#[cfg(test)]
fn identity_row_text(form: &ConnectionForm, width: usize) -> String {
    form.visible_lines(width, 40)
        .into_iter()
        .find(|l| l.text.contains("IdentityFile"))
        .map(|l| l.text)
        .expect("IdentityFile row")
}

#[test]
fn empty_identity_file_shows_the_browse_chip() {
    // BROWSE-UX: an empty IdentityFile value slot shows the persistent browse
    // chip (which doubles as the discoverability hint), rendered even when the
    // row is not focused.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    let row = identity_row_text(&form, 72);
    assert!(
        row.contains(BROWSE_CHIP_EMPTY),
        "empty IdentityFile row should show the empty browse chip: {row:?}"
    );
}

#[test]
fn filled_identity_file_keeps_a_trailing_browse_chip() {
    // BROWSE-UX BROWSE-PERSIST: a non-empty field still shows a [Browse] chip
    // so the browser stays reachable without deleting the path first.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    typed(&mut form, "/keys/mine");
    let row = identity_row_text(&form, 72);
    assert!(row.contains("/keys/mine"), "path shown: {row:?}");
    assert!(
        row.contains(BROWSE_CHIP),
        "filled row keeps a chip: {row:?}"
    );
    // The empty-state hint chip must not linger once a path is present.
    assert!(!row.contains(BROWSE_CHIP_EMPTY));
}

#[test]
fn browse_chip_never_saves_into_the_host() {
    // BROWSE-UX invariant: the chip is display-only and must never leak into a
    // saved connection's identity_file, whether the field is empty or filled.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    typed(&mut form, "hostk");
    form.advanced = true;
    // Render the form (which paints the chip) but leave the field untouched.
    let _ = form.visible_lines(72, 40);
    let (host, _) = saved(&mut form).expect("save");
    assert_eq!(host.identity_file, None);
}

#[test]
fn browse_chip_truncates_cleanly_on_a_narrow_body() {
    // A narrow body clips the chip through truncate_for_width without panic.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    let row = identity_row_text(&form, 20);
    assert!(row.chars().count() <= 20);
    // A body so narrow that even the chip marker is clipped away yields no
    // clickable span (rather than a bogus one) , the click falls through to
    // plain focus. The label occupies 13 columns; width 16 clips the marker.
    assert_eq!(form.identity_browse_chip_span(16), None);
    // A width that still shows the marker keeps a (possibly partial) span.
    assert!(form.identity_browse_chip_span(20).is_some());
}

#[test]
fn clicking_the_browse_chip_opens_the_browser_when_empty() {
    // BROWSE-MOUSE: a click on the empty-state chip opens the key browser
    // (the App round-trip that gathers ~/.ssh candidates).
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    let width = 72;
    form.visible_lines(width, 40);
    let (start, end) = form
        .identity_browse_chip_span(width)
        .expect("empty chip span");
    let row = form
        .rows()
        .iter()
        .position(|r| matches!(r, FormRow::Field(FormField::IdentityFile)))
        .expect("IdentityFile row");
    assert_eq!(
        form.handle_pointer_press(row, start, width),
        ConnectionFormOutcome::BrowseIdentityKeys
    );
    // A click just before the chip only focuses the field (no browse).
    assert_eq!(
        form.handle_pointer_press(row, start.saturating_sub(1), width),
        ConnectionFormOutcome::Consumed
    );
    // Sanity: end is one past the last chip column.
    assert!(end > start);
}

#[test]
fn clicking_the_browse_chip_opens_the_browser_when_filled() {
    // BROWSE-PERSIST + BROWSE-MOUSE: the chip stays clickable with a path set.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    typed(&mut form, "/keys/mine");
    let width = 72;
    form.visible_lines(width, 40);
    let (start, _) = form
        .identity_browse_chip_span(width)
        .expect("filled chip span");
    let row = form
        .rows()
        .iter()
        .position(|r| matches!(r, FormRow::Field(FormField::IdentityFile)))
        .expect("IdentityFile row");
    assert_eq!(
        form.handle_pointer_press(row, start, width),
        ConnectionFormOutcome::BrowseIdentityKeys
    );
}

#[test]
fn picking_a_key_replaces_an_existing_identity_path() {
    // BROWSE-UX: browsing over a set path REPLACES it with the picked key.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    typed(&mut form, "/old/key");
    form.open_key_browse(vec!["/keys/id_ed25519".to_owned()]);
    assert_eq!(
        form.handle_input(OverlayInput::Activate),
        ConnectionFormOutcome::Consumed
    );
    assert_eq!(form.identity_file, "/keys/id_ed25519");
}

#[test]
fn browse_chip_span_is_none_when_advanced_collapsed() {
    // The IdentityFile row lives under Advanced; collapsed, there is no chip
    // and therefore no clickable span.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = false;
    assert_eq!(form.identity_browse_chip_span(72), None);
}

fn short_key_browser() -> ConnectionForm {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.open_key_browse((0..10).map(|i| format!("/fixtures/keys/key{i}")).collect());
    form
}

#[test]
fn key_browser_footer_cannot_accept_an_invisible_candidate() {
    let mut form = short_key_browser();
    let lines = form.visible_lines(72, 5);
    assert_eq!(lines[3].text, "key2");
    assert!(lines[4].text.contains("[Enter]"));
    form.handle_pointer_press(4, 0, 72);
    assert!(form.browsing(), "footer is inert");
    assert!(form.identity_file.is_empty());
}

#[test]
fn key_browser_click_before_redraw_uses_the_last_visible_window() {
    let mut form = short_key_browser();
    form.visible_lines(72, 5);
    form.handle_input(OverlayInput::End);
    form.handle_pointer_press(1, 0, 72);
    assert_eq!(form.identity_file, "/fixtures/keys/key0");
}

#[test]
fn key_browser_query_change_invalidates_old_pointer_rows() {
    let mut form = short_key_browser();
    form.visible_lines(72, 5);
    typed(&mut form, "key9");
    form.handle_pointer_press(1, 0, 72);
    assert!(form.browsing());
    assert!(form.identity_file.is_empty());
    let lines = form.visible_lines(72, 5);
    assert_eq!(lines[1].text, "key9");
    form.handle_pointer_press(1, 0, 72);
    assert_eq!(form.identity_file, "/fixtures/keys/key9");
}

#[test]
fn key_browser_end_keeps_the_accepted_candidate_visible() {
    let mut form = short_key_browser();
    form.visible_lines(72, 5);
    form.handle_input(OverlayInput::End);
    let lines = form.visible_lines(72, 5);
    let focused: Vec<_> = lines.iter().filter(|line| line.focused).collect();
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].text, "key9");
    form.handle_input(OverlayInput::Activate);
    assert_eq!(form.identity_file, "/fixtures/keys/key9");
}

#[test]
fn key_browser_click_uses_the_rendered_scrolled_candidate() {
    let mut form = short_key_browser();
    form.visible_lines(72, 5);
    form.handle_input(OverlayInput::End);
    let lines = form.visible_lines(72, 5);
    assert_eq!(lines[1].text, "key7");
    form.handle_pointer_press(1, 0, 72);
    assert!(!form.browsing());
    assert_eq!(form.identity_file, "/fixtures/keys/key7");
}

#[test]
fn key_browser_pick_fills_the_field_and_closes() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    form.open_key_browse(vec![
        "/keys/id_ed25519".to_owned(),
        "/keys/work.pem".to_owned(),
    ]);
    assert!(form.browsing());
    // Browser rows render by file NAME, never the path's directory.
    let lines = form.visible_lines(72, 10);
    assert!(lines.iter().any(|l| l.text == "id_ed25519"));
    assert!(lines.iter().any(|l| l.text == "work.pem"));
    // Down then Enter fills the field with the second candidate's full path.
    form.handle_input(OverlayInput::Down);
    assert_eq!(
        form.handle_input(OverlayInput::Activate),
        ConnectionFormOutcome::Consumed
    );
    assert!(!form.browsing());
    assert_eq!(form.identity_file, "/keys/work.pem");
}

#[test]
fn key_browser_filters_by_name_and_esc_returns_without_change() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.open_key_browse(vec![
        "/k/id_ed25519".to_owned(),
        "/k/id_rsa".to_owned(),
        "/k/work.pem".to_owned(),
    ]);
    typed(&mut form, "rsa");
    let lines = form.visible_lines(72, 10);
    assert!(lines.iter().any(|l| l.text == "id_rsa"));
    assert!(!lines.iter().any(|l| l.text == "work.pem"));
    // Esc backs out of the browser without touching the field.
    assert_eq!(
        form.handle_input(OverlayInput::Close),
        ConnectionFormOutcome::Consumed
    );
    assert!(!form.browsing());
    assert!(form.identity_file.is_empty());
}

#[test]
fn empty_key_scan_shows_a_type_a_path_hint() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.open_key_browse(Vec::new());
    let lines = form.visible_lines(72, 10);
    assert!(
        lines
            .iter()
            .any(|l| l.text.contains("type a path manually")),
        "empty scan surfaces the manual-entry hint"
    );
    // Enter on the empty browser is inert (no path to fill) and closes it.
    assert_eq!(
        form.handle_input(OverlayInput::Activate),
        ConnectionFormOutcome::Consumed
    );
    assert!(!form.browsing());
    assert!(form.identity_file.is_empty());
}

#[test]
fn key_browser_click_selects_and_fills() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.open_key_browse(vec!["/k/id_ed25519".to_owned(), "/k/id_rsa".to_owned()]);
    form.visible_lines(80, 10);
    // Row 0 is the prompt; row 2 is the second candidate.
    assert_eq!(
        form.handle_pointer_press(2, 0, 80),
        ConnectionFormOutcome::Consumed
    );
    assert!(!form.browsing());
    assert_eq!(form.identity_file, "/k/id_rsa");
}

#[test]
fn focused_field_help_footer_follows_focus() {
    // FORM-UX FIELD-HELP: the footer shows the FOCUSED field's help.
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.focus = FormField::Alias;
    let alias_help = form.visible_lines(72, 40);
    assert!(
        alias_help
            .iter()
            .any(|l| l.text.contains("Quick-connect name")),
        "Alias help shows when Alias is focused"
    );
    // Focus IdentityFile: the footer switches to its help (mentions ssh -i).
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    let id_help = form.visible_lines(72, 40);
    assert!(
        id_help.iter().any(|l| l.text.contains("ssh -i")),
        "IdentityFile help mentions ssh -i"
    );
    assert!(
        !id_help
            .iter()
            .any(|l| l.text.contains("Quick-connect name")),
        "only the focused field's help renders"
    );
}

#[test]
fn help_footer_wraps_within_the_body_width() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    form.advanced = true;
    form.focus = FormField::IdentityFile;
    // Narrow body: every emitted line stays within the width (footer wraps
    // rather than truncating the long help).
    let width = 30;
    for line in form.visible_lines(width, 40) {
        assert!(
            line.text.chars().count() <= width,
            "line within width: {:?}",
            line.text
        );
    }
}

#[test]
fn wrap_for_width_hard_splits_overlong_words() {
    let out = wrap_for_width("short verylongwordthatexceeds", 8);
    assert!(out.iter().all(|l| l.chars().count() <= 8));
    assert_eq!(out[0], "short");
    assert!(wrap_for_width("anything", 0).is_empty());
}

#[test]
fn basename_handles_both_separators() {
    assert_eq!(basename("/keys/id_ed25519"), "id_ed25519");
    assert_eq!(basename(r"C:\keys\id_rsa"), "id_rsa");
    assert_eq!(basename("bare"), "bare");
}
