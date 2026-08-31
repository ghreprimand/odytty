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
fn softwrap_output_copy_reconstructs_the_logical_line_without_a_break() {
    // A single output line of 100 columns soft-wraps at the 80-column grid onto
    // two physical rows with no intervening CRLF. Copy/export must reconstruct
    // the one logical line - the soft-wrap continuation suppresses the newline
    // - and never inject a break at the wrap column or drop the wrapped tail.
    let long = "a".repeat(100);
    let mut bytes = b"\x1b]133;A\x07$ printf long\r\n\x1b]133;C\x07".to_vec();
    bytes.extend_from_slice(long.as_bytes());
    bytes.extend_from_slice(b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    let (mut app, _) = app_with(&bytes);

    let exported = app
        .command_output_text_for_export_for_test()
        .expect("verified soft-wrapped output");
    assert_eq!(
        exported, long,
        "soft-wrap must rejoin into one logical line"
    );
    assert!(
        !exported.contains('\n'),
        "a soft-wrap continuation must not become a hard newline"
    );

    app.copy_command_output_for_test(false);
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some(long.as_str()),
        "clipboard copy of soft-wrapped output matches the logical line"
    );
}

#[test]
fn silent_command_collapsing_c_and_d_fails_closed() {
    // A command that prints nothing collides its C (output start) and D (end) on
    // one row; there is no addressable output region, so the verified-range
    // contract must fail closed for both variants rather than select the prompt
    // row by guess.
    let bytes = b"\x1b]133;A\x07$ silent\r\n\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (mut app, _) = app_with(bytes);

    assert!(
        app.command_output_text_for_export_for_test().is_none(),
        "a silent command exposes no verified output to export"
    );
    app.select_command_output_for_test(false);
    assert!(app.selection_text_for_test().is_none());
    app.select_command_output_for_test(true);
    assert!(
        app.selection_text_for_test().is_none(),
        "with-prompt variant also fails closed when no complete range exists"
    );
    assert!(
        app.open_notice_message_for_test()
            .is_some_and(|message| message.contains("complete current OSC 133 range"))
    );
}

#[test]
fn export_projection_strips_sgr_color_escapes() {
    // SGR color sequences around the visible text must not reach the export.
    // The projection reads cell graphemes, so only "red" survives, with no ESC
    // bytes.
    let bytes = b"\x1b]133;A\x07$ color\r\n\x1b]133;C\x07\x1b[31mred\x1b[0m\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (app, _) = app_with(bytes);
    let text = app
        .command_output_text_for_export_for_test()
        .expect("verified output");
    assert_eq!(text, "red");
    assert!(!text.contains('\x1b'));
    assert!(!text.contains("31m"));
}

#[test]
fn failed_navigation_is_a_noop_when_no_command_failed() {
    // Every command succeeded (exit 0); previous/next failed navigation must be
    // a no-op that reports "none", never a jump to row 0 or a success block.
    let bytes = b"\x1b]133;A\x07$ ok1\r\n\x1b]133;C\x07fine\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ok2\r\n\x1b]133;C\x07fine\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let (mut app, _) = app_with(bytes);
    app.jump_failed_command_for_test(false);
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some("No previous failed command.")
    );
    app.jump_failed_command_for_test(true);
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some("No next failed command.")
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

#[test]
fn one_shot_completion_arms_only_for_a_running_osc133_command() {
    let (mut plain, _) = app_with(b"plain output");
    plain.notify_command_finished_for_test();
    assert!(!plain.command_notification_armed_for_test());

    let (mut app, terminal) = app_with(b"\x1b]133;A\x1b\\$ run\r\n\x1b]133;C\x1b\\working");
    app.notify_command_finished_for_test();
    assert!(app.command_notification_armed_for_test());
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\r\n\x1b]133;D;9\x1b\\");
    let (completion, badge) = app.drain_notifications_for_test(std::time::Instant::now());
    assert_eq!(completion, Some(true));
    assert!(badge);
    assert!(!app.command_notification_armed_for_test());
}

#[test]
fn pane_activity_and_failure_monitors_fire_once_from_explicit_events() {
    let now = std::time::Instant::now();
    let (mut activity, terminal) = app_with(b"quiet");
    activity.arm_pane_monitor_for_test(crate::native::notifications::PaneMonitorKind::Activity);
    terminal.lock().expect("terminal").advance(b" output");
    let (_, notice) = activity.drain_notifications_for_test(now);
    assert!(notice);

    let (mut failure, terminal) = app_with(b"\x1b]133;A\x1b\\$ run\r\n\x1b]133;C\x1b\\working");
    failure
        .arm_pane_monitor_for_test(crate::native::notifications::PaneMonitorKind::CommandFailure);
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\r\n\x1b]133;D;7\x1b\\");
    let (_, notice) = failure.drain_notifications_for_test(now);
    assert!(notice);
}

#[test]
fn newly_armed_completion_monitors_ignore_stale_edges_and_scan_new_statuses() {
    let now = std::time::Instant::now();
    let (mut completion, terminal) =
        app_with(b"\x1b]133;A\x1b\\$ old\r\n\x1b]133;C\x1b\\done\r\n\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ new\r\n\x1b]133;C\x1b\\working");
    completion.notify_command_finished_for_test();
    assert!(completion.command_notification_armed_for_test());
    let (event, _) = completion.drain_notifications_for_test(now);
    assert_eq!(event, None);
    assert!(completion.command_notification_armed_for_test());
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\r\n\x1b]133;D;0\x1b\\");
    assert_eq!(completion.drain_notifications_for_test(now).0, Some(false));

    let (mut failure, terminal) = app_with(b"quiet");
    failure
        .arm_pane_monitor_for_test(crate::native::notifications::PaneMonitorKind::CommandFailure);
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]133;D;0\x1b\\\x1b]133;D;7\x1b\\");
    assert!(failure.drain_notifications_for_test(now).1);
}

#[test]
fn silence_bell_and_process_monitors_are_pane_owned_and_one_shot() {
    let (mut silence, _) = app_with(b"quiet");
    silence.arm_pane_monitor_for_test(crate::native::notifications::PaneMonitorKind::Silence);
    let (_, notice) =
        silence.drain_notifications_for_test(std::time::Instant::now() + Duration::from_secs(31));
    assert!(notice);

    let (mut bell, terminal) = app_with(b"");
    let token = bell.active_session_id_for_test();
    bell.arm_pane_monitor_for_test(crate::native::notifications::PaneMonitorKind::Bell);
    terminal.lock().expect("terminal").advance(b"\x07");
    assert!(bell.drain_bells_for_test().0);
    assert!(bell.pane_attention_for_test(token).0);

    let (mut process, _) = app_with(b"");
    process.arm_pane_monitor_for_test(crate::native::notifications::PaneMonitorKind::ProcessFinish);
    assert!(process.fire_process_finish_monitor_for_test());
    assert!(!process.fire_process_finish_monitor_for_test());
}
