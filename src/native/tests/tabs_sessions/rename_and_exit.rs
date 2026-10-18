// SPDX-License-Identifier: GPL-3.0-only
//! Rename and exit tests for the multi-session tabs feature.

use super::*;

/// A pane whose real shell child has already exited with `status`. This is the
/// one fixture here that needs a real PTY: the held-exit tests assert the
/// status the child actually reported. It fails the test loudly when no shell
/// can start, instead of skipping.
#[allow(clippy::type_complexity)]
fn recorded_exited_session(
    dims: Dimensions,
    status: i32,
) -> (
    Arc<Mutex<Terminal>>,
    PtyWriter,
    Arc<Mutex<PtySession>>,
    Arc<Mutex<Vec<u8>>>,
) {
    let command = format!("exit {status}");
    let session =
        PtySession::spawn_shell_command(dims, &command).expect("spawn the exiting test shell");
    let _ = crate::test_stall::read_to_end_within(
        &session,
        std::time::Duration::from_secs(60),
        "read exited session output",
    );
    let _ = session.take_writer().expect("take the exited shell writer");
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    (terminal, writer, pty, bytes)
}

// --- right rail (mirror of the left rail) ---

#[test]
fn right_rail_hit_test_is_x_flipped_to_the_far_side() {
    // The right rail is the mirror of the left: its X-band sits at the FAR side
    // (after the content columns), so a click on the rail band there is a tab
    // hit while a click in the LEFT content area is not (the flip of
    // `click_right_of_the_rail_is_not_a_tab_hit`).
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("right");
    // Two workspaces give the rail two slots; the active workspace holds a
    // single tab so no top bar competes for the corner. Short names keep the
    // slot geometry deterministic (single label row), matching the labels the
    // pre-workspace rail tabs used.
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");

    // Headless: no live resize, so `self.grid` is the full grid and the right
    // rail band starts at column `cols` (content stays at column 0; pad/gap 0).
    // Slot 0 body: top-margin row 1, label col 2 → centre ((cols+2)·8+4, 24).
    let (cols, _rows) = app.grid_dims_for_test();
    let rail_x0 = cols as f64 * 8.0;
    app.set_pointer_px_for_test(rail_x0 + 2.0 * 8.0 + 4.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("switch"),
        "right rail slot 0 body → switch"
    );

    // A click in the LEFT content area (well before the rail band) is not a tab
    // hit - the X-band gate excludes it, mirroring the left rail's exclusion of
    // the right content.
    app.set_pointer_px_for_test(8.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "left content click → no tab hit (mirror)"
    );
}

#[test]
fn right_and_left_rails_grow_the_decorated_snapshot_identically() {
    // Both rails grow the single-pane decorated snapshot by `rail_cols` columns
    // (never rows); only the side the content sits on differs. The dims are
    // therefore identical - the mirror is a placement, not a size, difference.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_session_title_override_for_test(0, Some("aa"));

    app.set_tab_bar_placement_for_test("left");
    let left = app
        .decorated_snapshot_dims_for_test()
        .expect("left decorated dims");
    app.set_tab_bar_placement_for_test("right");
    let right = app
        .decorated_snapshot_dims_for_test()
        .expect("right decorated dims");
    assert_eq!(
        left, right,
        "left and right decorations differ only by side, not size"
    );
}

#[test]
fn right_rail_leaves_the_content_origin_unshifted_for_overlays() {
    // A right rail reserves columns off the RIGHT, so the content origin is
    // unmoved: IME-candidate / click-hint / overlay quads anchored in content
    // space need no shift. This is the asymmetry vs the left rail (which shifts
    // content right by the reserved band).
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));

    app.set_tab_bar_placement_for_test("right");
    // A second workspace makes the auto rail appear; the active workspace's
    // single tab means no top bar, so the only offset is the rail's (0 for a
    // right rail, positive for a left rail).
    add_workspace(&mut app);
    assert_eq!(
        app.tab_chrome_offset_px_for_test(),
        Some((0.0, 0.0)),
        "right rail: content origin unmoved"
    );

    // Guard that the (0, 0) above is a real right-rail property, not a constant:
    // the left rail DOES shift the content origin right by the reserved band.
    app.set_tab_bar_placement_for_test("left");
    let (dx, dy) = app
        .tab_chrome_offset_px_for_test()
        .expect("left chrome offset");
    assert!(dx > 0.0, "left rail shifts content right");
    assert_eq!(dy, 0.0, "a rail never shifts content down");
}

#[test]
fn right_rail_scrollbar_stays_in_the_content_not_under_the_rail() {
    // MANDATORY collision test: with a right rail, the scroll thumb hugs
    // the content grid's right edge, which sits at/left of the rail band's left
    // edge - never under it. So a press resolves to exactly one target: the thumb
    // grabs the scrollbar (and is not a tab hit), and the rail band is a tab
    // action (and never grabs the scrollbar).
    let (mut app, fixtures) = app_with_two_sessions();
    // Scrollback on the active (first) session so the thumb is visible.
    {
        let mut terminal = fixtures[0].0.lock().expect("terminal");
        terminal.advance(&scrollback_bytes(200));
    }
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("right");
    // Force the workspace rail on the right so the collision geometry is
    // exercised (the rail lists workspaces now).
    app.set_workspace_rail_for_test("right");
    app.scroll_up_for_test(usize::MAX);
    let len = app.scrollback_len_for_test();
    assert!(
        len > 0,
        "scrolling up must materialize scrollback; the precondition is not environmental"
    );
    let (cols, rows) = app.grid_dims_for_test();
    let offset = app.viewport_offset_for_test();
    let thumb = scroll_indicator_quad(
        offset,
        len,
        Dimensions::new(cols, rows),
        cell(8, 16),
        [1.0, 1.0, 1.0, 0.62],
    )
    .expect("thumb visible while scrolled back");

    // Press on the thumb → scrollbar grab, and NOT a tab hit (it is left of the
    // rail band).
    let tx = ((thumb.rect[0] + thumb.rect[2]) / 2.0) as f64;
    let ty = ((thumb.rect[1] + thumb.rect[3]) / 2.0) as f64;
    app.set_pointer_px_for_test(tx, ty);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "the thumb is not under the rail"
    );
    assert_eq!(
        app.left_button_outcome_for_test(true),
        "grab",
        "thumb press grabs the scrollbar"
    );
    app.left_button_outcome_for_test(false); // release the grab

    // Press on the rail band → a tab action, never a scrollbar grab.
    let rail_x = cols as f64 * 8.0 + 2.0 * 8.0 + 4.0;
    app.set_pointer_px_for_test(rail_x, 24.0);
    assert!(
        app.tab_bar_hit_for_test().is_some(),
        "the rail band is a tab hit"
    );
    assert_ne!(
        app.left_button_outcome_for_test(true),
        "grab",
        "a rail press must not grab the scrollbar"
    );
}

#[test]
fn visible_tab_bar_row_has_no_default_background_cells() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));

    let backgrounds = app
        .tab_bar_row_backgrounds_for_test()
        .expect("tab bar backgrounds");
    assert!(!backgrounds.is_empty());
    assert!(backgrounds.iter().all(|bg| *bg != Color::Default));
}

#[test]
fn title_override_controls_effective_tab_label_and_clear_restores_osc_title() {
    let (mut app, _fixtures) = app_with_two_sessions();

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
    let (mut app, fixtures) = app_with_two_sessions();
    let [(_, bytes_a), _] = fixtures;

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
    assert!(!app.rename_overlay_open_for_test());
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

/// Replicate the rename modal's editable-input origin from the grid dims - the
/// same math `rename_layout` uses, so a click column maps to a known character.
fn rename_input_origin(cols: usize, rows: usize) -> (usize, usize) {
    let width = cols.clamp(8, 48);
    let left = (cols - width) / 2;
    let top = (rows - 3) / 2;
    let input_row = top + 1;
    let input_left = left + 2 + "Tab name: ".chars().count();
    (input_row, input_left)
}

#[test]
fn rename_field_click_places_caret_and_drag_selects() {
    // A click positions the caret, a drag extends a selection,
    // and typing replaces the selected span.
    let (mut app, _fixtures) = app_with_two_sessions();
    let (cols, rows) = app.grid_dims_for_test();
    let (input_row, input_left) = rename_input_origin(cols, rows);
    // The drag is delivered as a real pointer position, which needs a cell size.
    app.set_test_cell_for_test(cell(8, 16));

    app.set_session_title_override_for_test(0, Some("hello world"));
    assert!(app.begin_rename_tab_for_test(0));
    assert_eq!(
        app.rename_cursor_for_test(),
        Some(11),
        "caret starts at end"
    );

    // Click on character index 2 ('l' of "hello").
    app.rename_pointer_press_for_test(input_row, input_left + 2);
    assert_eq!(
        app.rename_cursor_for_test(),
        Some(2),
        "click places the caret"
    );
    assert_eq!(
        app.rename_selection_for_test(),
        None,
        "a plain click makes no selection"
    );

    // Drag to character index 5 → selection [2, 5) = "llo".
    app.rename_pointer_drag_for_test(input_row, input_left + 5);
    assert_eq!(app.rename_selection_for_test(), Some((2, 5)));
    app.rename_pointer_release_for_test();
    assert_eq!(
        app.rename_selection_for_test(),
        Some((2, 5)),
        "release keeps the finalized selection"
    );

    // Typing replaces the selected span.
    app.drive_text_key_for_test("X");
    assert_eq!(app.rename_text_for_test().as_deref(), Some("heX world"));
    assert_eq!(app.rename_selection_for_test(), None, "insertion clears it");
}

#[test]
fn rename_field_double_click_selects_word_and_backspace_replaces_it() {
    // A double-click selects the word under the pointer, and a
    // subsequent Backspace deletes the whole selection.
    let (mut app, _fixtures) = app_with_two_sessions();
    let (cols, rows) = app.grid_dims_for_test();
    let (input_row, input_left) = rename_input_origin(cols, rows);

    app.set_session_title_override_for_test(0, Some("hello world"));
    assert!(app.begin_rename_tab_for_test(0));

    // Two quick presses on the same cell (char 7, inside "world") → word select.
    app.rename_pointer_press_for_test(input_row, input_left + 7);
    app.rename_pointer_press_for_test(input_row, input_left + 7);
    assert_eq!(
        app.rename_selection_for_test(),
        Some((6, 11)),
        "double-click selects the whole word"
    );

    app.drive_named_key_for_test(NamedKey::Backspace);
    assert_eq!(
        app.rename_text_for_test().as_deref(),
        Some("hello "),
        "Backspace replaces the selected word"
    );
    assert_eq!(app.rename_selection_for_test(), None);
}

#[test]
fn shell_exit_for_last_session_requests_app_exit() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let mut app = App::new_headless(
        options,
        terminal,
        writer,
        headless,
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
fn held_exit_reports_numeric_status_and_waits_for_a_non_release_key() {
    let options = NativeOptions {
        hold: true,
        ..NativeOptions::default()
    };
    let dims = options.initial_grid;
    let (terminal, writer, pty, bytes) = recorded_exited_session(dims, 7);
    let mut app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let session = app.active_session_token_for_test();

    assert!(!app.dispatch_user_event_for_test(UserEvent::ShellExited { session }));
    assert!(!app.pending_exit_for_test());
    assert_eq!(app.session_count_for_test(), 1);
    assert!(
        app.tab_plain_text_at_position_for_test(0)
            .expect("held status text")
            .contains("Process exited with status 7. Press any key to close."),
        "the held pane reports the real child status"
    );

    let logical = WinitKey::Character("x".into());
    app.drive_raw_key_event_for_test(
        logical.clone(),
        logical.clone(),
        PhysicalKey::Code(KeyCode::KeyX),
        Modifiers::default(),
        KeyEventType::Release,
    );
    assert!(
        !app.pending_exit_for_test(),
        "a release does not dismiss hold"
    );
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "held input never reaches the exited child"
    );

    app.drive_raw_key_event_for_test(
        logical.clone(),
        logical,
        PhysicalKey::Code(KeyCode::KeyX),
        Modifiers::default(),
        KeyEventType::Press,
    );
    assert!(
        app.pending_exit_for_test(),
        "the first press dismisses hold"
    );
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "the dismissal key is consumed"
    );
}

#[test]
fn hold_is_initial_session_only_and_dismissal_focuses_a_survivor() {
    let options = NativeOptions {
        hold: true,
        ..NativeOptions::default()
    };
    let dims = options.initial_grid;
    let (initial_terminal, initial_writer, initial_pty, _) = recorded_exited_session(dims, 7);
    let (later_terminal, later_writer, later_headless, _) = recorded_session(dims);
    let mut app = App::new(
        options,
        initial_terminal,
        initial_writer,
        initial_pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let initial = app.active_session_token_for_test();
    app.push_headless_session_with_for_test(later_terminal, later_writer, later_headless);
    let survivor = app
        .session_token_at_position_for_test(1)
        .expect("later session");

    assert!(!app.dispatch_user_event_for_test(UserEvent::ShellExited { session: initial }));
    assert_eq!(app.session_count_for_test(), 2, "initial exit is held");

    app.drive_text_key_for_test("x");
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(
        app.active_session_token_for_test(),
        survivor,
        "dismissal collapses the exited tab and focuses its survivor"
    );

    // The CLI hold marker was consumed with the initial session. A later
    // session's ordinary EOF keeps the pre-existing exit behavior.
    assert!(app.dispatch_user_event_for_test(UserEvent::ShellExited { session: survivor }));
    assert!(app.pending_exit_for_test());
}

#[test]
fn switching_sessions_restores_per_session_title_viewport_selection_and_search() {
    let (mut app, _fixtures) = app_with_two_sessions();

    app.advance_tab_bytes_at_position_for_test(0, b"\x1b]2;session-alpha\x07");
    app.advance_tab_bytes_at_position_for_test(1, b"\x1b]2;session-beta\x07");
    app.advance_tab_bytes_at_position_for_test(0, &scrollback_bytes(40));
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
    let (mut app, _fixtures) = app_with_two_sessions();

    app.set_tab_needs_rebuild_at_position_for_test(0, false);
    app.set_tab_needs_rebuild_at_position_for_test(1, false);

    let session = app
        .session_token_at_position_for_test(1)
        .expect("session token");
    let should_exit = app.dispatch_user_event_for_test(UserEvent::Redraw { session });

    assert!(!should_exit);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert_eq!(app.tab_needs_rebuild_at_position_for_test(0), Some(false));
    assert_eq!(app.tab_needs_rebuild_at_position_for_test(1), Some(true));
}

#[test]
fn stale_shell_exit_for_closed_first_tab_is_a_no_op() {
    let (mut app, _fixtures) = app_with_two_sessions();

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
fn stale_shell_exit_after_prefix_close_keeps_split_collapsed() {
    let (mut app, _bytes) = single_session_app();
    let (terminal, writer, headless, _) = recorded_session(NativeOptions::default().initial_grid);

    app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);
    let closed = app.active_session_token_for_test();
    assert_eq!(app.active_pane_count_for_test(), 2);

    // Exercise the user-facing close chord. The focused second pane is removed
    // before its PTY teardown can later emit ShellExited.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('x', false, false);
    assert_eq!(app.active_pane_count_for_test(), 1);
    assert_eq!(app.session_count_for_test(), 1);

    // The deferred event must remain a no-op: it must not restore the removed
    // leaf, create a replacement pane, or mark the application for exit.
    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session: closed });

    assert!(!should_exit);
    assert_eq!(app.active_pane_count_for_test(), 1);
    assert_eq!(app.session_count_for_test(), 1);
    assert_ne!(app.active_session_token_for_test(), closed);
    assert!(!app.pending_exit_for_test());
}

#[test]
fn closing_middle_tab_preserves_remaining_session_tokens_and_active_session() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let (terminal_c, writer_c, headless_c, _) = recorded_session(dims);
    let mut app = App::new_headless(
        options,
        terminal_a,
        writer_a,
        headless_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.push_headless_session_with_for_test(terminal_b, writer_b, headless_b);
    app.push_headless_session_with_for_test(terminal_c, writer_c, headless_c);

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
    let (mut app, _fixtures) = app_with_two_sessions();

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
    let (mut app, _fixtures) = app_with_two_sessions();

    app.close_all_sessions_for_test();

    assert_eq!(app.session_count_for_test(), 0);
}
