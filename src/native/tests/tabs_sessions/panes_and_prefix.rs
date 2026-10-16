// SPDX-License-Identifier: GPL-3.0-only
//! Panes and prefix tests for the multi-session tabs feature.

use super::*;

// ----- per-pane overlays in the multi-pane render path (1c-3c / P3-CUT2) -----

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

/// Whether any cell in `row` (a `columns`-wide grid) differs between two
/// equal-shaped snapshots - used to check whether a paint touched a specific
/// row (e.g. the search bar's last row).
fn row_changed(before: &Snapshot, after: &Snapshot, row: usize, columns: usize) -> bool {
    let start = row * columns;
    before.cells[start..start + columns] != after.cells[start..start + columns]
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
    app.advance_tab_bytes_at_position_for_test(0, b"needle\r\n");
    app.drive_search_for_test("needle");
    // Require a real match so the assertion is meaningful.
    assert!(
        app.search_match_count_for_test() > 0,
        "the written text must register a search match"
    );

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

#[test]
fn per_pane_overlay_paints_a_selection_even_for_a_background_pane() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // A wrapped selection on absolute row 0, columns 0..=70.
    app.set_selection_range_for_test(0, 0, 0, 70);
    let pane_grid = Dimensions::new(40, 4);
    let mut snap = snapshot(&["", "", "", ""], 40);
    let before = snap.clone();
    // Paint as a BACKGROUND pane (`focused = false`): a pane's own selection is
    // NOT gated on focus, so it still highlights. Reading the state by ref is
    // exactly what the per-pane render loop does for each pane's Session.
    app.paint_pane_overlays(
        &mut snap,
        pane_grid,
        0,
        0,
        &app.selection,
        app.selection_block,
        &app.search,
        false,
        &app.effective_theme_for_test(),
    );
    assert_eq!(
        changed_cells(&before, &snap),
        40,
        "a background pane's selection clamps to and fills its 40-col row 0"
    );
}

#[test]
fn per_pane_overlay_gates_the_search_bar_to_the_focused_pane() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.advance_tab_bytes_at_position_for_test(0, b"needle\r\n");
    app.drive_search_for_test("needle");
    assert!(
        app.search_match_count_for_test() > 0,
        "the written text must register a search match"
    );
    let sbl = app.scrollback_len_for_test();
    let pane_grid = Dimensions::new(40, 4);
    let last_row = pane_grid.rows - 1;

    // Focused pane: match highlights AND the interactive query bar (last row).
    let mut focused_snap = snapshot(&["needle", "", "", ""], 40);
    let fb = focused_snap.clone();
    app.paint_pane_overlays(
        &mut focused_snap,
        pane_grid,
        0,
        sbl,
        &app.selection,
        app.selection_block,
        &app.search,
        true,
        &app.effective_theme_for_test(),
    );

    // Background pane: the same match highlights, but NO query bar - the bar is
    // keyboard-interactive and belongs to the focused pane only.
    let mut bg_snap = snapshot(&["needle", "", "", ""], 40);
    let bb = bg_snap.clone();
    app.paint_pane_overlays(
        &mut bg_snap,
        pane_grid,
        0,
        sbl,
        &app.selection,
        app.selection_block,
        &app.search,
        false,
        &app.effective_theme_for_test(),
    );

    assert!(
        row_changed(&fb, &focused_snap, last_row, 40),
        "focused pane paints the search bar on its last row"
    );
    assert!(
        !row_changed(&bb, &bg_snap, last_row, 40),
        "background pane must not paint the search bar over its content"
    );
    // Both panes still highlight the on-screen match (row 0).
    assert!(
        row_changed(&fb, &focused_snap, 0, 40),
        "focused pane highlights its match row"
    );
    assert!(
        row_changed(&bb, &bg_snap, 0, 40),
        "background pane highlights its own match row"
    );
}

// ----- §7 prefix engine wired into the App key path -----

#[test]
fn bare_pane_chord_without_prefix_is_plain_input() {
    let Some((mut app, bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // `%` (Shift+%) with no pending prefix is ordinary text - the engine returns
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
    // the tab is split - see `split_tab_engages_prefix_capture`.
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

/// FIX-C (Bug 4): the Ctrl+click open ladder now runs inside a split. Builds a
/// two-pane columns split whose ORIGINAL (left) pane -- deliberately NOT the
/// focused pane after the split -- holds a resolvable `:line` file path, so the
/// scenario exercises hover recompute after the focus switch (hover resolution
/// is suppressed over a non-focused pane, so the latched span is stale/None until
/// the click focuses the pane). A bogus editor override keeps the open a no-op
/// (Command spawn fails NotFound before any process starts), so the test never
/// spawns a real opener on the shared box. Returns the app, the left pane's id,
/// and both panes' recorded PTY writes (left, right).
type SplitPathFixture = (
    App,
    usize,
    [Arc<Mutex<Vec<u8>>>; 2],
    crate::test_dirs::AbsentProgram,
);

fn split_app_with_path_in_left_pane() -> Option<SplitPathFixture> {
    let cols = 40usize;
    let rows = 20usize;
    let cw = 8u32;
    let ch = 16u32;
    let dims = Dimensions::new(cols, rows);
    // The left/original pane holds the path.
    let (t_left, w_left, p_left, b_left) = recorded_session(dims)?;
    t_left
        .lock()
        .expect("left terminal")
        .advance(b"/proj/src/main.rs:42");
    let left_id = 0usize; // the first session's token id
    let mut app = App::new(
        NativeOptions::default(),
        t_left,
        w_left,
        p_left,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    // `test_cell` / `test_surface` / `test_path_probe` are per-session; set them on
    // the left pane now (while it is active) so its geometry AND path stat-gate
    // resolve after a focus switch back to it, then again on the right pane below.
    app.set_test_cell_for_test(cell(cw, ch));
    app.set_test_surface_for_test(cols as u32 * cw, rows as u32 * ch, WindowPadding::ZERO);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    // Split along columns; focus lands on the NEW (right) pane, so the left pane
    // with the path is the non-focused one.
    let (t_right, w_right, p_right, b_right) = recorded_session(dims)?;
    let right_id = app.seed_split_pane_for_test(true, t_right, w_right, p_right);
    assert_ne!(right_id, left_id, "focus is on the new right pane");
    app.set_test_cell_for_test(cell(cw, ch));
    app.set_test_surface_for_test(cols as u32 * cw, rows as u32 * ch, WindowPadding::ZERO);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    // interactive_paths + the editor override are App-level settings (one set).
    app.set_interactive_paths_for_test(true);
    // A guaranteed-absent opener: the `:line` file routes through the editor
    // matrix, so this argv[0] is what Command tries to spawn -> NotFound, no
    // process, and the ladder still reports the path handled.
    let opener = crate::test_dirs::AbsentProgram::new();
    app.set_interactive_paths_editor_for_test(&opener.program);
    Some((app, left_id, [b_left, b_right], opener))
}

#[test]
fn ctrl_click_in_a_split_runs_the_open_ladder_instead_of_selecting() {
    // Bug 4: the single-pane press path tries the Ctrl+click open helpers (OSC 8
    // hyperlink, interactive path incl. the inline image viewer, bare URL) before
    // beginning a selection; the multipane branch used to begin a selection
    // directly, so Ctrl+click never reached the ladder in a split. With the fix,
    // a Ctrl+click on a resolved path in a split focuses the pane, re-resolves the
    // hover span against it, and runs the ladder -- consuming the press so NO
    // selection begins.
    let Some((mut app, left_id, written, opener)) = split_app_with_path_in_left_pane() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Hover col ~5 of the path at row 0 of the LEFT pane (x well inside the left
    // half). The left pane is not focused, so hover resolution is suppressed and
    // the span does not latch yet.
    app.pointer_move_for_test(8.0 * 5.5, 16.0 * 0.5);
    assert!(
        app.hovered_path_for_test().is_none(),
        "hover is suppressed over the non-focused pane, so nothing latches yet"
    );
    // Ctrl+press: the branch focuses the left pane, re-resolves the hover span
    // there, and the ladder fires -- no selection begins.
    hold_open_modifier(&mut app, true);
    app.mouse_left_press_for_test();
    assert_eq!(
        app.focused_pane_id_for_test(),
        left_id,
        "the press focused the clicked (left) pane"
    );
    assert!(
        !app.selecting_for_test(),
        "the open ladder consumed the Ctrl+press, so no selection began"
    );
    // Positive evidence that the ladder ran: the guaranteed-absent editor made
    // the open fail, which raises the notice. A press that was merely swallowed
    // (focus change, no selection, no open) raises none.
    let notice = app
        .open_notice_message_for_test()
        .expect("the open ladder ran and reported the failed editor spawn");
    assert!(
        notice.contains(&opener.program),
        "the notice names the absent editor that was tried: {notice}"
    );
    for pane in &written {
        assert!(
            pane.lock().expect("bytes").is_empty(),
            "the open press writes nothing to either pane's PTY"
        );
    }
    hold_open_modifier(&mut app, false);
    app.mouse_left_release_for_test();
}

#[test]
fn a_plain_click_in_a_split_still_begins_a_selection() {
    // The modifier-gated control: WITHOUT the open modifier, the ladder no-ops and
    // the split branch falls through to begin a selection exactly as before, so
    // plain click-drag selection in a split is unchanged.
    let Some((mut app, left_id, _written, _opener)) = split_app_with_path_in_left_pane() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(8.0 * 5.5, 16.0 * 0.5);
    // No modifier held.
    app.mouse_left_press_for_test();
    assert_eq!(
        app.focused_pane_id_for_test(),
        left_id,
        "the plain press still focuses the clicked pane"
    );
    assert!(
        app.selecting_for_test(),
        "a plain press over the path begins a selection (ladder is modifier-gated)"
    );
    app.mouse_left_release_for_test();
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
    // Ctrl-b Ctrl-b → the focused pane's PTY receives a literal 0x02, so a
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

#[test]
fn wheel_scrolls_the_pane_under_the_cursor_not_the_focused_pane() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    {
        let mut terminal = terminal_b.lock().expect("terminal b");
        terminal.advance(&scrollback_bytes(80));
    }
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_a = app.focused_pane_id_for_test();
    let pane_b = app.seed_split_pane_for_test(true, terminal_b, writer_b, pty_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    assert_eq!(app.focused_pane_id_for_test(), pane_b);

    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_a,
        "precondition: focus is pane A while pointer is over pane B"
    );
    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_a),
        Some(0),
        "pane A starts at the live tail"
    );
    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_b),
        Some(0),
        "pane B starts at the live tail"
    );

    app.set_pointer_px_for_test(
        (COLS as u32 * CW * 3 / 4) as f64,
        (ROWS as u32 * CH / 2) as f64,
    );
    app.dispatch_wheel_for_test(1.0);

    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_a),
        Some(0),
        "focused pane A must not scroll when the pointer is over pane B"
    );
    assert!(
        app.viewport_offset_for_token_for_test(pane_b)
            .unwrap_or_default()
            > 0,
        "wheel over pane B should scroll pane B's scrollback"
    );
}

#[test]
fn continuous_scroll_eligible_of_is_per_pane_in_a_split() {
    // The continuous (pixel_scroll) lane is no longer single-pane-only.
    // Each pane of a split is independently eligible while it is on its primary
    // screen and the knob is on - the gate reads the pane, not active_is_single_pane.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_a = app.focused_pane_id_for_test();
    let pane_b = app.seed_split_pane_for_test(true, terminal_b, writer_b, pty_b);
    app.reflow_active_panes_for_test();
    // Re-inject the cell + surface AFTER seeding the split (the seed path drops
    // the test-cell override), so the wheel handler resolves a real cell height.
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    assert!(
        !app.active_is_single_pane_for_test(),
        "precondition: a split"
    );

    // Both panes are eligible on the primary screen with the knob on - the old
    // single-pane restriction is gone.
    assert!(
        app.continuous_scroll_eligible_of_for_test(pane_a),
        "pane A is eligible in a split"
    );
    assert!(
        app.continuous_scroll_eligible_of_for_test(pane_b),
        "pane B is eligible in a split"
    );
    // Turning the knob off makes every pane ineligible (falls back to notches).
    app.set_pixel_scroll_for_test(false);
    assert!(!app.continuous_scroll_eligible_of_for_test(pane_a));
    assert!(!app.continuous_scroll_eligible_of_for_test(pane_b));
}

#[test]
fn pixel_wheel_drives_the_continuous_lane_on_the_pane_under_the_pointer() {
    // Cut 3 + smell a high-resolution PixelDelta wheel over the NON-focused
    // pane drives THAT pane's continuous (sub-cell) scroll, not the focused pane.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    {
        let mut terminal = terminal_b.lock().expect("terminal b");
        terminal.advance(&scrollback_bytes(80));
    }
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_a = app.focused_pane_id_for_test();
    let pane_b = app.seed_split_pane_for_test(true, terminal_b, writer_b, pty_b);
    app.reflow_active_panes_for_test();
    // Re-inject the cell + surface AFTER seeding the split (the seed path drops
    // the test-cell override), so the wheel handler resolves a real cell height.
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    // Move focus back to pane A so focus != the pointed pane.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_a,
        "precondition: focus is pane A while the pointer is over pane B"
    );

    // Pointer over pane B (right half of the column split); a 40px glide at a
    // 16px cell = 2.5 rows: two whole rows into the offset, half a row (8px) of
    // sub-cell remainder.
    app.set_pointer_px_for_test(
        (COLS as u32 * CW * 3 / 4) as f64,
        (ROWS as u32 * CH / 2) as f64,
    );
    app.dispatch_pixel_wheel_for_test(40.0);

    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_b),
        Some(2),
        "pane B (under the pointer) carried two whole rows"
    );
    assert_eq!(
        app.scroll_frac_offset_for_token_for_test(pane_b),
        Some(8.0),
        "pane B holds the half-row sub-cell remainder the split render clips"
    );
    // The focused pane A never moved - routing followed the pointer, not focus.
    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_a),
        Some(0),
        "focused pane A must not scroll when the pointer is over pane B"
    );
    assert_eq!(
        app.scroll_frac_offset_for_token_for_test(pane_a),
        Some(0.0),
        "focused pane A has no sub-cell offset"
    );
}

#[test]
fn sub_notch_pixel_scroll_moves_only_the_sub_cell_offset_in_a_split() {
    // Sub-notch pixel travel (< one row) now produces pure sub-cell motion
    // inside a split - previously the continuous lane was disabled in multipane,
    // so a fractional glide was truncated to nothing. The offset stays put; only
    // the clipped sub-cell remainder advances.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    {
        let mut terminal = terminal_b.lock().expect("terminal b");
        terminal.advance(&scrollback_bytes(80));
    }
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_b = app.seed_split_pane_for_test(true, terminal_b, writer_b, pty_b);
    app.reflow_active_panes_for_test();
    // Re-inject the cell + surface AFTER seeding the split (the seed path drops
    // the test-cell override), so the wheel handler resolves a real cell height.
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.set_pointer_px_for_test(
        (COLS as u32 * CW * 3 / 4) as f64,
        (ROWS as u32 * CH / 2) as f64,
    );
    // 6px at a 16px cell = 0.375 row: no whole row carries, the sub-cell offset
    // advances to exactly 6px. This is the motion that was truncated before.
    app.dispatch_pixel_wheel_for_test(6.0);
    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_b),
        Some(0),
        "sub-notch travel keeps the integer offset at the tail"
    );
    assert_eq!(
        app.scroll_frac_offset_for_token_for_test(pane_b),
        Some(6.0),
        "the sub-cell remainder advanced by the pixel travel the split clips"
    );
}

#[test]
fn wheel_scroll_still_scrolls_the_lone_single_pane() {
    let Some((mut app, _bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    if let Ok(mut terminal) = app.terminal.lock() {
        terminal.advance(&scrollback_bytes(80));
    }
    assert_eq!(app.viewport_offset_for_test(), 0, "starts at the live tail");

    app.dispatch_wheel_for_test(1.0);

    assert!(
        app.viewport_offset_for_test() > 0,
        "single-pane wheel routing should still scroll the active viewport"
    );
}

/// Focus-follows-click into a split must anchor the selection under the
/// LIVE click, not the newly-focused pane's stale per-session `pointer_px`.
///
/// `pointer_px`/`pointer_cell` are per-session; before the fix the press
/// switched focus to pane B and THEN read `self.pointer_px`, which now derefed
/// to B's own last-stored (stale) coordinate - anchoring the first drag at the
/// wrong cell. Seed B with a stale coord at its top-left (cell 0,0), focus A,
/// click deep inside B, and assert the anchor lands under the click (row 10),
/// not B's stale (0,0).
#[test]
fn focus_follows_click_anchors_under_the_live_click_not_stale_coords() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_a = app.focused_pane_id_for_test();
    let pane_b = app.seed_split_pane_for_test(true, terminal_b, writer_b, pty_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    // The split leaves focus on pane B; seed B's stale pointer at its top-left.
    assert_eq!(app.focused_pane_id_for_test(), pane_b);
    app.set_pointer_px_for_test(0.0, 0.0);

    // Move focus back to pane A (Ctrl-b o), so B is unfocused and holds its
    // stale coordinate while A is the active session.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_a,
        "precondition: focus is pane A"
    );

    // Live click deep inside pane B (right 3/4 width, row 10). This is the
    // active session's (A's) live pointer, the coords the press resolves with.
    app.set_pointer_px_for_test((COLS as u32 * CW * 3 / 4) as f64, (10 * CH) as f64);
    let outcome = app.left_button_outcome_for_test(true);

    assert_eq!(
        outcome, "select",
        "an in-pane left press begins a selection"
    );
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_b,
        "the click focuses pane B (focus-follows-click)"
    );
    let anchor = app
        .pointer_cell_for_test()
        .expect("the press seeds a selection anchor");
    assert_eq!(
        anchor.row, 10,
        "anchor row must follow the live click (row 10), not stale (0)"
    );
    assert!(
        anchor.column > 0,
        "anchor column must follow the live click deep in pane B, not stale column 0"
    );
}
