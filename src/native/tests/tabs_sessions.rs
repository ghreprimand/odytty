// SPDX-License-Identifier: GPL-3.0-only
//! Headless multi-session foundation tests for the tabs packet.

use crate::core::Color;

use super::super::pty::UserEvent;
use super::*;

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type SessionFixture = (
    Arc<Mutex<Terminal>>,
    Arc<Mutex<PtySession>>,
    Arc<Mutex<Vec<u8>>>,
);

#[allow(clippy::type_complexity)]
fn recorded_session(
    dims: Dimensions,
) -> Option<(
    Arc<Mutex<Terminal>>,
    PtyWriter,
    Arc<Mutex<PtySession>>,
    Arc<Mutex<Vec<u8>>>,
)> {
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let _ = session.take_writer().ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    Some((terminal, writer, pty, bytes))
}

fn app_with_two_sessions() -> Option<(App, [SessionFixture; 2])> {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal_a, writer_a, pty_a, bytes_a) = recorded_session(dims)?;
    let (terminal_b, writer_b, pty_b, bytes_b) = recorded_session(dims)?;
    let mut app = App::new(
        options,
        terminal_a.clone(),
        writer_a,
        pty_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.push_session_for_test(terminal_b.clone(), writer_b, pty_b.clone());
    Some((
        app,
        [(terminal_a, pty_a, bytes_a), (terminal_b, pty_b, bytes_b)],
    ))
}

fn tab_bar_app() -> Option<App> {
    let (app, _fixtures) = app_with_two_sessions()?;
    Some(app)
}

fn scrollback_bytes(lines: usize) -> Vec<u8> {
    let mut text = String::new();
    for i in 0..lines {
        text.push_str(&format!("row-{i:02}\r\n"));
    }
    text.into_bytes()
}

#[test]
fn input_routes_to_the_active_session_writer_after_switch() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let [(_, _, bytes_a), (_, _, bytes_b)] = fixtures;

    app.drive_text_key_for_test("a");
    assert_eq!(&*bytes_a.lock().expect("bytes a"), b"a");
    assert!(bytes_b.lock().expect("bytes b").is_empty());

    assert!(app.switch_to_session_for_test(1));
    app.drive_text_key_for_test("b");

    assert_eq!(&*bytes_a.lock().expect("bytes a"), b"a");
    assert_eq!(&*bytes_b.lock().expect("bytes b"), b"b");
}

#[test]
fn session_output_and_scrollback_stay_independent() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.advance_session_bytes_for_test(0, b"alpha-session\r\n");
    app.advance_session_bytes_for_test(1, b"beta-session\r\n");

    let plain_a = app.session_plain_text_for_test(0).expect("plain text a");
    let plain_b = app.session_plain_text_for_test(1).expect("plain text b");

    assert!(plain_a.contains("alpha-session"));
    assert!(!plain_a.contains("beta-session"));
    assert!(plain_b.contains("beta-session"));
    assert!(!plain_b.contains("alpha-session"));
}

#[test]
fn resize_updates_both_terminals_and_ptys() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let cell = cell(8, 16);
    assert!(app.resize_grid(cell, 512, 256));
    let expected = Dimensions::new(64, 15);

    assert_eq!(app.session_dimensions_for_test(0), Some(expected));
    assert_eq!(app.session_dimensions_for_test(1), Some(expected));
    assert_eq!(app.session_pty_dimensions_for_test(0), Some(expected));
    assert_eq!(app.session_pty_dimensions_for_test(1), Some(expected));
}

#[test]
fn closing_active_session_activates_neighbor_and_last_close_sets_exit() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    assert!(app.switch_to_session_for_test(1));
    assert_eq!(app.active_session_id_for_test(), 1);

    assert!(!app.close_active_tab_for_test());
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert!(!app.pending_exit_for_test());

    assert!(app.close_active_tab_for_test());
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_window_title_for_test(), "OdyTTY");
    assert!(app.pending_exit_for_test());
}

#[test]
fn shell_exit_for_non_last_session_does_not_exit_app() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let session = app
        .session_token_at_position_for_test(1)
        .expect("session token");
    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });

    assert!(!should_exit);
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert!(!app.pending_exit_for_test());
}

#[test]
fn tab_bar_show_rule_is_hidden_for_one_session_and_visible_for_two() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal.clone(),
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    assert!(!app.tab_bar_visible_for_test());

    let Some((terminal_b, writer_b, pty_b, _bytes_b)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.push_session_for_test(terminal_b, writer_b, pty_b);
    assert!(app.tab_bar_visible_for_test());
}

#[test]
fn tab_bar_reservation_reduces_shell_rows_by_one_when_visible() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal.clone(),
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );

    let cell = cell(8, 16);
    assert!(!app.resize_grid(cell, 640, 384));
    assert_eq!(
        app.session_dimensions_for_test(0),
        Some(Dimensions::new(80, 24))
    );

    let Some((terminal_b, writer_b, pty_b, _bytes_b)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.push_session_for_test(terminal_b, writer_b, pty_b);
    assert!(app.resize_grid(cell, 640, 384));
    assert_eq!(
        app.session_dimensions_for_test(0),
        Some(Dimensions::new(80, 23))
    );
    assert_eq!(
        app.session_dimensions_for_test(1),
        Some(Dimensions::new(80, 23))
    );
}

#[test]
fn tab_bar_hit_test_reports_switch_close_and_new_actions() {
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));

    app.set_pointer_px_for_test(12.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("switch"));

    app.set_pointer_px_for_test(188.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("close"));

    app.set_pointer_px_for_test(628.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new"));
}

#[test]
fn visible_tab_bar_row_has_no_default_background_cells() {
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));

    let backgrounds = app
        .tab_bar_row_backgrounds_for_test()
        .expect("tab bar backgrounds");
    assert!(!backgrounds.is_empty());
    assert!(backgrounds.iter().all(|bg| *bg != Color::Default));
}

#[test]
fn title_override_controls_effective_tab_label_and_clear_restores_osc_title() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.set_session_tab_title_for_test(0, "osc-title");
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("osc-title")
    );

    app.set_session_title_override_for_test(0, Some("work"));
    assert_eq!(app.session_tab_title_for_test(0).as_deref(), Some("work"));

    app.set_session_tab_title_for_test(0, "shell-updated");
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("work"),
        "shell titles stop overriding a custom tab name"
    );

    app.set_session_title_override_for_test(0, None);
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shell-updated")
    );
}

#[test]
fn rename_modal_commits_edits_cancels_and_empty_commit_clears_override() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let [(_, _, bytes_a), _] = fixtures;

    app.set_session_tab_title_for_test(0, "shell");
    assert!(app.begin_rename_tab_for_test(0));
    app.drive_text_key_for_test("!");
    assert_eq!(
        &*bytes_a.lock().expect("bytes a"),
        b"",
        "rename modal captures typed text before the PTY path"
    );
    app.drive_named_key_for_test(NamedKey::ArrowLeft);
    app.drive_named_key_for_test(NamedKey::Backspace);
    app.drive_text_key_for_test("work");
    app.drive_named_key_for_test(NamedKey::Enter);
    assert!(!app.rename_active_for_test());
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shelwork!")
    );

    assert!(app.begin_rename_tab_for_test(0));
    app.drive_text_key_for_test(" discarded");
    app.drive_named_key_for_test(NamedKey::Escape);
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shelwork!"),
        "Esc cancels without changing the committed override"
    );

    assert!(app.begin_rename_tab_for_test(0));
    for _ in 0.."shelwork!".chars().count() {
        app.drive_named_key_for_test(NamedKey::Backspace);
    }
    app.drive_named_key_for_test(NamedKey::Enter);
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shell"),
        "empty commit clears the override and restores the OSC title"
    );
}

#[test]
fn shell_exit_for_last_session_requests_app_exit() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );

    let session = app
        .session_token_at_position_for_test(0)
        .expect("session token");
    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });

    assert!(should_exit);
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_window_title_for_test(), "OdyTTY");
    assert!(app.pending_exit_for_test());
}

#[test]
fn switching_sessions_restores_per_session_title_viewport_selection_and_search() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.advance_session_bytes_for_test(0, b"\x1b]2;session-alpha\x07");
    app.advance_session_bytes_for_test(1, b"\x1b]2;session-beta\x07");
    app.advance_session_bytes_for_test(0, &scrollback_bytes(40));
    app.scroll_up_for_test(3);
    app.open_search_for_test();
    app.force_selection_for_test(0, 0, 0, 4);

    assert_eq!(app.active_window_title_for_test(), "session-alpha");
    assert!(app.viewport_offset_for_test() > 0);
    assert!(app.selection_range_for_test().is_some());
    assert!(app.search_open_for_test());

    assert!(app.switch_to_session_for_test(1));
    assert_eq!(app.active_window_title_for_test(), "session-beta");
    assert_eq!(app.viewport_offset_for_test(), 0);
    assert_eq!(app.selection_range_for_test(), None);
    assert!(!app.search_open_for_test());

    assert!(app.switch_to_session_for_test(0));
    assert_eq!(app.active_window_title_for_test(), "session-alpha");
    assert!(app.viewport_offset_for_test() > 0);
    assert!(app.selection_range_for_test().is_some());
    assert!(app.search_open_for_test());
}

#[test]
fn redraw_for_non_active_session_only_marks_that_session_dirty() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.set_session_needs_rebuild_for_test(0, false);
    app.set_session_needs_rebuild_for_test(1, false);

    let session = app
        .session_token_at_position_for_test(1)
        .expect("session token");
    let should_exit = app.dispatch_user_event_for_test(UserEvent::Redraw { session });

    assert!(!should_exit);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert_eq!(app.session_needs_rebuild_for_test(0), Some(false));
    assert_eq!(app.session_needs_rebuild_for_test(1), Some(true));
}

#[test]
fn stale_shell_exit_for_closed_first_tab_is_a_no_op() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let removed = app
        .session_token_at_position_for_test(0)
        .expect("removed token");

    assert!(!app.close_active_tab_for_test());
    assert_eq!(app.session_count_for_test(), 1);

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session: removed });

    assert!(!should_exit);
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert!(!app.pending_exit_for_test());
}

#[test]
fn closing_middle_tab_preserves_remaining_session_tokens_and_active_session() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_c, writer_c, pty_c, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let mut app = App::new(
        options,
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.push_session_for_test(terminal_b, writer_b, pty_b);
    app.push_session_for_test(terminal_c, writer_c, pty_c);

    let first = app
        .session_token_at_position_for_test(0)
        .expect("first token");
    let middle = app
        .session_token_at_position_for_test(1)
        .expect("middle token");
    let last = app
        .session_token_at_position_for_test(2)
        .expect("last token");

    assert!(app.switch_to_session_for_test(2));
    assert_eq!(app.active_session_token_for_test(), last);

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session: middle });

    assert!(!should_exit);
    assert_eq!(app.session_count_for_test(), 2);
    assert_eq!(app.session_token_at_position_for_test(0), Some(first));
    assert_eq!(app.session_token_at_position_for_test(1), Some(last));
    assert_eq!(app.active_session_token_for_test(), last);
}

#[test]
fn token_position_round_trip_holds_after_switches() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    assert!(app.switch_to_session_for_test(1));
    let first = app
        .session_token_at_position_for_test(0)
        .expect("first token");
    let second = app
        .session_token_at_position_for_test(1)
        .expect("second token");

    assert_ne!(first, second);
    assert_eq!(app.session_token_at_position_for_test(0), Some(first));
    assert_eq!(app.session_token_at_position_for_test(1), Some(second));
}

#[test]
fn close_all_sessions_empties_and_terminates() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.close_all_sessions_for_test();

    assert_eq!(app.session_count_for_test(), 0);
}
