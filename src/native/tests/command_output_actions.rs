// SPDX-License-Identifier: GPL-3.0-only
//! Verified OSC 133 command-output action wiring and sanitized projection.

use super::*;

fn app_with(content: &[u8]) -> (App, Arc<Mutex<Terminal>>) {
    let (app, terminal) = headless_app_for_test();
    terminal.lock().expect("terminal").advance(content);
    (app, terminal)
}

#[test]
fn supported_shell_fixtures_select_copy_and_export_visible_output() {
    for fixture in crate::core::v013_fixtures::SHELL_OSC133_FIXTURES {
        let (mut app, _) = app_with(&fixture.stream());
        assert_eq!(
            app.command_output_text_for_export_for_test().as_deref(),
            Some(fixture.output),
            "{} export projection",
            fixture.shell
        );

        app.select_command_output_for_test(false);
        assert_eq!(
            app.selection_text_for_test().as_deref(),
            Some(fixture.output),
            "{} output selection",
            fixture.shell
        );

        app.copy_command_output_for_test(false);
        assert_eq!(
            app.last_clipboard_write_for_test().as_deref(),
            Some(fixture.output),
            "{} output copy",
            fixture.shell
        );

        app.select_command_output_for_test(true);
        let with_prompt = app.selection_text_for_test().expect("prompt selection");
        assert!(with_prompt.contains(fixture.prompt));
        assert!(with_prompt.contains(fixture.command));
        assert!(with_prompt.contains(fixture.output));
    }
}

#[test]
fn partial_and_stale_ranges_fail_closed() {
    let (mut partial, _) = app_with(b"\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07partial");
    partial.select_command_output_for_test(false);
    assert!(partial.selection_text_for_test().is_none());
    assert!(
        partial
            .open_notice_message_for_test()
            .is_some_and(|message| message.contains("complete current OSC 133 range"))
    );

    let fixture = crate::core::v013_fixtures::SHELL_OSC133_FIXTURES[0];
    let (mut stale, terminal) = app_with(&fixture.stream());
    let handle = stale.command_handle_for_test().expect("complete handle");
    terminal.lock().expect("terminal").advance(b"changed");
    stale.select_command_handle_for_test(handle);
    assert!(stale.selection_text_for_test().is_none());
}

#[test]
fn alternate_screen_disables_command_actions() {
    let fixture = crate::core::v013_fixtures::SHELL_OSC133_FIXTURES[0];
    let (mut app, terminal) = app_with(&fixture.stream());
    terminal.lock().expect("terminal").advance(b"\x1b[?1049h");
    app.copy_command_output_for_test(false);
    assert!(app.last_clipboard_write_for_test().is_none());
}

#[test]
fn scoped_search_excludes_matching_text_outside_the_command() {
    let bytes = b"before needle\r\n\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07inside needle\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (mut app, _) = app_with(bytes);
    app.search_command_output_for_test();
    app.drive_scoped_command_search_for_test("needle");
    assert_eq!(app.command_search_match_count_for_test(), 1);
}

#[test]
fn failed_navigation_uses_only_explicit_nonzero_statuses() {
    let bytes = b"\x1b]133;A\x07$ one\r\n\x1b]133;C\x07bad\r\n\x1b]133;D;7\x07\x1b]133;A\x07$ two\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (mut app, _) = app_with(bytes);
    app.jump_failed_command_for_test(false);
    assert!(
        app.open_notice_message_for_test()
            .is_none_or(|message| !message.contains("No previous failed"))
    );
}

#[test]
fn export_projection_excludes_osc_hyperlink_and_cwd_metadata() {
    let bytes = b"\x1b]133;A\x07$ show\r\n\x1b]133;C\x07safe \x1b]8;;https://example.invalid/private\x07label\x1b]8;;\x07\x1b]7;file://host.invalid/private/path\x07\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (app, _) = app_with(bytes);
    let text = app
        .command_output_text_for_export_for_test()
        .expect("verified output");
    assert_eq!(text, "safe label");
    assert!(!text.contains('\x1b'));
    assert!(!text.contains("example.invalid"));
    assert!(!text.contains("private/path"));
}

#[test]
fn export_projection_excludes_inline_image_payloads() {
    let bytes = b"\x1b]133;A\x07$ show\r\n\x1b]133;C\x07\x1b_Gf=32,a=T;AAAA\x1b\\visible\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (app, _) = app_with(bytes);
    let text = app
        .command_output_text_for_export_for_test()
        .expect("verified output");
    assert_eq!(text, "visible");
    assert!(!text.contains('\x1b'));
    assert!(!text.contains("AAAA"));
}

#[test]
fn output_without_trailing_newline_is_not_silently_truncated() {
    let bytes = b"\x1b]133;A\x07$ printf demo\r\n\x1b]133;C\x07alpha\r\nbeta\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (mut app, _) = app_with(bytes);
    assert_eq!(
        app.command_output_text_for_export_for_test().as_deref(),
        Some("alpha\nbeta")
    );
    app.search_command_output_for_test();
    app.drive_scoped_command_search_for_test("$");
    assert_eq!(app.command_search_match_count_for_test(), 0);
}

#[test]
fn unterminated_output_actions_use_the_focused_pane_width_after_reflow() {
    let bytes = b"\x1b]133;A\x07$ printf demo\r\n\x1b]133;C\x07alpha\r\nabcdefghij\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (app, terminal) = app_with(bytes);
    terminal.lock().expect("terminal").resize(4, 8);
    assert_ne!(
        app.grid_dims_for_test(),
        (4, 8),
        "the window grid stays wider than the focused pane in this regression"
    );

    assert_eq!(
        app.command_output_text_for_export_for_test().as_deref(),
        Some("alpha\nabcdefghij")
    );
}

#[test]
fn export_cancellation_writes_nothing_and_dialog_authority_is_bounded() {
    let fixture = crate::core::v013_fixtures::SHELL_OSC133_FIXTURES[0];
    let (mut app, _) = app_with(&fixture.stream());
    assert!(app.cancel_command_export_for_test());
    assert!(app.open_notice_message_for_test().is_none());
    assert!(app.command_export_dialog_is_bounded_for_test());
}

#[test]
fn context_menu_command_authority_is_bound_to_its_session() {
    let fixture = crate::core::v013_fixtures::SHELL_OSC133_FIXTURES[0];
    let (mut app, _) = app_with(&fixture.stream());
    assert!(app.context_command_session_mismatch_for_test());
}
