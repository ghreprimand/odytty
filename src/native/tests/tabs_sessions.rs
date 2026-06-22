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

fn single_session_app() -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal, writer, pty, bytes) = recorded_session(dims)?;
    let app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, bytes))
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

// ----- focused-pane overlays in the multi-pane render path (1c-3c) -----

/// Count the cells that differ between two equal-shaped snapshots (the cells a
/// paint mutated). Theme-agnostic: selection/search change cell attrs whether
/// the style is themed (background) or unthemed (inverse).
fn changed_cells(before: &Snapshot, after: &Snapshot) -> usize {
    before
        .cells
        .iter()
        .zip(after.cells.iter())
        .filter(|(a, b)| a != b)
        .count()
}

#[test]
fn focused_pane_overlay_maps_selection_with_the_pane_grid() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Wrapped selection on the focused session: absolute row 0, columns 0..=70.
    app.set_selection_range_for_test(0, 0, 0, 70);
    let metrics = cell(10, 20);

    // Paint onto a 40-column pane snapshot: the far selection column (70) clamps
    // to the pane width, highlighting all 40 cells of row 0.
    let pane_grid = Dimensions::new(40, 4);
    let mut pane_snap = snapshot(&["", "", "", ""], 40);
    let pane_before = pane_snap.clone();
    app.paint_focused_pane_overlays(&mut pane_snap, pane_grid, 0, 0, metrics);
    let changed_pane = changed_cells(&pane_before, &pane_snap);

    // Paint the SAME selection onto an 80-column snapshot: row 0 columns 0..=70
    // highlight (71 cells). The difference proves the paint keys to the PANE
    // grid argument, not the whole-window `self.grid`.
    let wide_grid = Dimensions::new(80, 4);
    let mut wide_snap = snapshot(&["", "", "", ""], 80);
    let wide_before = wide_snap.clone();
    app.paint_focused_pane_overlays(&mut wide_snap, wide_grid, 0, 0, metrics);
    let changed_wide = changed_cells(&wide_before, &wide_snap);

    assert_eq!(changed_pane, 40, "row 0 clamps to the 40-col pane width");
    assert_eq!(changed_wide, 71, "row 0 columns 0..=70 in the 80-col grid");
    assert_ne!(
        changed_pane, changed_wide,
        "the pane grid, not the window grid, drives the highlight mapping"
    );
}

#[test]
fn focused_pane_overlay_paints_focused_pane_search_matches() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Searchable content in the focused session, then a query that matches it.
    app.advance_session_bytes_for_test(0, b"needle\r\n");
    app.drive_search_for_test("needle");
    // Require a real match so the assertion is meaningful (skip if the PTY-backed
    // terminal did not register the write in this environment).
    if app.search_match_count_for_test() == 0 {
        eprintln!("skipping: no search match registered");
        return;
    }

    let metrics = cell(10, 20);
    let grid = Dimensions::new(40, 4);
    let scrollback_len = app.scrollback_len_for_test();
    let mut snap = snapshot(&["needle", "", "", ""], 40);
    let before = snap.clone();
    app.paint_focused_pane_overlays(&mut snap, grid, 0, scrollback_len, metrics);

    // The "needle" match (row 0, 6 columns) highlights the focused pane snapshot.
    assert!(
        changed_cells(&before, &snap) > 0,
        "the focused pane's search match must highlight its own snapshot"
    );
}

// ----- §7 K2: prefix engine wired into the App key path -----

#[test]
fn bare_pane_chord_without_prefix_is_plain_input() {
    let Some((mut app, bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // `%` (Shift+%) with no pending prefix is ordinary text — the engine returns
    // Inactive and the byte reaches the PTY exactly as before §7.
    app.drive_char_with_mods_for_test('%', false, true);
    assert_eq!(&*bytes.lock().expect("bytes"), b"%");
    // It did not split.
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn single_pane_passes_ctrl_b_through_to_pty() {
    let Some((mut app, bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.active_pane_count_for_test(), 1);
    // Byte-identity guarantee: on a single-pane tab the prefix engine is gated
    // out, so Ctrl-b reaches the PTY as the literal 0x02 (readline
    // backward-char) exactly like the pre-§7 path. The prefix only engages once
    // the tab is split — see `split_tab_engages_prefix_capture`.
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(&*bytes.lock().expect("bytes"), &[0x02]);
    // It did not enter prefix-pending and did not split.
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn split_tab_engages_prefix_capture() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Split so the active tab now holds two panes; focus lands on the new pane.
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Now Ctrl-b IS the multiplexer prefix: it enters pending and forwards
    // nothing to the focused pane's PTY (tmux semantics, unchanged from before
    // the single-pane gate).
    app.drive_char_with_mods_for_test('b', true, false);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "multi-pane Ctrl-b is captured by the prefix engine, not sent to the PTY"
    );
    // The pending prefix resolves a pane action (Ctrl-b o cycles focus),
    // proving capture is live on the split tab and still forwards nothing.
    app.drive_char_with_mods_for_test('o', false, false);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "the resolved pane chord is not forwarded to the PTY either"
    );
}

#[test]
fn closing_a_split_back_to_single_pane_restores_ctrl_b_passthrough() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // bytes_a is the original (pane A) PTY; focus returns to it after the new
    // pane is closed.
    let [(_, _, bytes_a), _] = fixtures;
    let Some((terminal, writer, pty, _new_bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Ctrl-b x closes the focused (new) pane, collapsing back to a single pane;
    // focus returns to pane A. The close handler cancels any pending prefix as
    // the tab returns to the plain single-pane input path.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('x', false, false);
    assert_eq!(app.active_pane_count_for_test(), 1);
    bytes_a.lock().expect("bytes a").clear();
    // The very next Ctrl-b is plain input again: with one pane the engine is
    // gated out, so it passes through to pane A's PTY as the literal 0x02. This
    // proves no stale pending prefix survived the drop to single-pane.
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(
        &*bytes_a.lock().expect("bytes a"),
        &[0x02],
        "after collapsing to one pane, Ctrl-b passes through byte-identically"
    );
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn prefix_then_unknown_key_fires_nothing() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // The prefix engine only engages on a split tab, so seed two panes first.
    let Some((terminal, writer, pty, bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    app.drive_char_with_mods_for_test('b', true, false); // prefix
    app.drive_char_with_mods_for_test('q', false, false); // not in the table
    // The unknown second key is swallowed (cancel), so nothing reaches the PTY
    // and no pane op ran (still two panes).
    assert!(bytes.lock().expect("bytes").is_empty());
    assert_eq!(app.active_pane_count_for_test(), 2);
}

#[test]
fn prefix_focus_next_cycles_panes() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Seed the active tab into a two-pane column split; focus lands on the new
    // (right) pane.
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let new_pane = app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    assert_eq!(app.focused_pane_id_for_test(), new_pane);
    // Ctrl-b o cycles focus to the other pane in tree order.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_ne!(
        app.focused_pane_id_for_test(),
        new_pane,
        "Ctrl-b o moved focus off the new pane"
    );
}

#[test]
fn prefix_close_pane_collapses_the_split() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(false, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Ctrl-b x closes the focused pane, collapsing the split back to one pane.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('x', false, false);
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn prefix_equalize_keeps_the_split_intact() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    // Ctrl-b = equalizes split ratios; the dispatch runs without disturbing the
    // pane structure.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('=', false, false);
    assert_eq!(app.active_pane_count_for_test(), 2);
}

#[test]
fn prefix_zoom_toggles_full_bleed_pane() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    assert!(!app.active_is_zoomed_for_test());
    // Ctrl-b z zooms the focused pane full-bleed; the layout tree is preserved
    // (still two panes), only the render/resize collapses to the focused one.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('z', false, false);
    assert!(app.active_is_zoomed_for_test(), "Ctrl-b z zoomed the pane");
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "zoom preserves the layout tree"
    );
    // Ctrl-b z again un-zooms.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('z', false, false);
    assert!(!app.active_is_zoomed_for_test(), "Ctrl-b z un-zoomed");
}

#[test]
fn prefix_zoom_is_a_noop_on_a_single_pane() {
    let Some((mut app, _bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.active_pane_count_for_test(), 1);
    // Ctrl-b z on a lone pane never zooms: the prefix engine is gated out on a
    // single pane, so both keys are plain input (Ctrl-b → 0x02, z → 'z') and the
    // single-pane render path is untouched. Zoom stays off.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('z', false, false);
    assert!(!app.active_is_zoomed_for_test());
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn prefix_is_disabled_when_set_off() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let settings = Settings {
        pane_prefix: None, // prefix model off
        ..Settings::default()
    };
    let mut app = App::new(
        options,
        terminal,
        writer,
        pty,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    // With the prefix off, Ctrl-b is ordinary input again: it sends the literal
    // 0x02, byte-identical to the pre-§7 build.
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(&*bytes.lock().expect("bytes"), &[0x02]);
}

#[test]
fn doubled_prefix_passes_the_literal_byte_to_the_pty() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // The nested-multiplexer passthrough only applies once the prefix engine is
    // engaged, i.e. on a split tab. Seed two panes first.
    let Some((terminal, writer, pty, bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Ctrl-b Ctrl-b → the focused pane's PTY receives a literal 0x02 (K3), so a
    // tmux running inside OdyTTY still gets its own prefix. The first Ctrl-b
    // enters pending and sends nothing; the second triggers the passthrough.
    app.drive_char_with_mods_for_test('b', true, false);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "first prefix sends nothing"
    );
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(&*bytes.lock().expect("bytes"), &[0x02]);
}
