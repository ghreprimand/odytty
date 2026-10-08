// SPDX-License-Identifier: GPL-3.0-only
//! App-level checks that presentation state follows the model through the
//! production input and frame paths: chrome reservation after a settings or
//! config change, the rail seam with a hidden top bar, activation after a tab
//! move, the status gutter during a scroll glide, and synchronized output per
//! split pane.

use super::super::session::SessionToken;
use super::*;
use crate::native::app::PanePaintProbe;
use crate::settings::{TabBarHeight, TabRailWidth};

fn headless_tab(app: &mut App) -> usize {
    app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        crate::native::test_support::headless_writer(),
        Dimensions::new(80, 24),
    )
}

/// A headless window with two tabs (so the top bar shows) and an injected
/// 640x384 surface of 8x16 cells.
fn top_bar_app() -> App {
    let (mut app, _) = headless_app_for_test();
    headless_tab(&mut app);
    let cell = cell(8, 16);
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    app.resize_grid_with_padding_for_test(cell, WindowPadding::ZERO, 640, 384);
    assert!(app.tab_bar_visible_for_test());
    app
}

#[test]
fn tab_bar_height_typed_in_settings_reflows_a_visible_top_bar() {
    use winit::keyboard::{Key as WinitKey, NamedKey};
    let mut app = top_bar_app();
    assert_eq!(app.tab_reserve_for_test().0, 1);
    let recomputes = app.chrome_recomputes_for_test();

    app.open_layout_settings_overlay_for_test();
    let signature = app.overlay_signature_for_test();
    let target = signature
        .panel
        .entries
        .iter()
        .position(|entry| entry.key == "tab_bar_height")
        .expect("tab height row in Layout");
    for _ in signature.panel.selected..target {
        app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::ArrowDown), false, false);
    }
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    app.drive_overlay_key_for_test(WinitKey::Character("3".into()), false, false);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);

    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(3));
    assert_eq!(app.tab_reserve_for_test().0, 3);
    assert!(
        app.chrome_recomputes_for_test() > recomputes,
        "the taller bar's rows are taken from the content grid"
    );
}

#[test]
fn tab_bar_height_from_a_config_reload_reflows_a_visible_top_bar() {
    let mut app = top_bar_app();
    let recomputes = app.chrome_recomputes_for_test();
    app.apply_reloaded_settings_for_test(Settings {
        tab_bar_height: TabBarHeight::Manual(2),
        ..Settings::default()
    });

    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(2));
    assert_eq!(app.tab_reserve_for_test().0, 2);
    assert!(app.chrome_recomputes_for_test() > recomputes);
}

#[test]
fn pinned_rail_seam_drags_while_the_top_bar_is_hidden() {
    for (side, seam_x, drag_x) in [("left", 128.0, 80.0), ("right", 512.0, 560.0)] {
        let (mut app, _) = headless_app_for_test();
        let cell = cell(8, 16);
        app.set_test_cell_for_test(cell);
        app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
        app.set_tab_rail_width_manual_for_test(16);
        app.set_workspace_rail_for_test(side);
        app.resize_grid_with_padding_for_test(cell, WindowPadding::ZERO, 640, 384);
        assert!(!app.tab_bar_visible_for_test(), "one tab: no top bar");
        assert_eq!(app.tab_reserve_for_test(), (0, 16), "{side} rail pinned");

        app.set_pointer_px_for_test(seam_x, 100.0);
        app.mouse_left_press_for_test();
        assert!(
            app.rail_seam_dragging_for_test(),
            "a press on the {side} rail seam arms a drag"
        );
        app.pointer_move_for_test(drag_x, 100.0);
        assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(10));
        app.mouse_left_release_for_test();
        assert!(!app.rail_seam_dragging_for_test());
    }
}

/// Window with workspace 0 holding `ws0_tabs` headless tabs and workspace 1
/// holding one, every terminal reporting focus. Returns the ws0 tokens, the
/// ws1 token, with workspace 0 active on its last tab.
fn two_workspaces(ws0_tabs: usize) -> (App, Vec<SessionToken>, SessionToken) {
    let (mut app, _) = headless_app_for_test();
    app.enable_focus_reporting_for_test();
    for _ in 1..ws0_tabs {
        let position = headless_tab(&mut app);
        assert!(app.switch_to_session_for_test(position));
        app.enable_focus_reporting_for_test();
    }
    let ws0: Vec<SessionToken> = (0..ws0_tabs)
        .filter_map(|position| app.session_token_at_position_for_test(position))
        .collect();
    app.push_headless_workspace_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        crate::native::test_support::headless_writer(),
        Dimensions::new(80, 24),
    );
    app.enable_focus_reporting_for_test();
    let ws1 = app.active_session_token_for_test();
    app.handle_palette_action_for_test("workspace-switch-0");
    assert_eq!(app.active_workspace_index_for_test(), 0);
    let _ = app.take_focus_reports_for_test();
    (app, ws0, ws1)
}

#[test]
fn moving_the_active_tab_out_of_a_workspace_activates_the_survivor() {
    let (mut app, ws0, _ws1) = two_workspaces(2);
    let moved = app.active_session_token_for_test();
    assert_eq!(moved, ws0[1]);

    app.move_tab_to_workspace_for_test(moved, 1);

    assert_eq!(app.active_workspace_index_for_test(), 0);
    let survivor = app.active_session_token_for_test();
    assert_eq!(survivor, ws0[0]);
    assert_eq!(
        app.last_active_session_for_test(),
        survivor,
        "activation was reconciled for the surviving tab"
    );
    assert_eq!(
        app.take_focus_reports_for_test(),
        vec![(moved, false), (survivor, true)]
    );
}

#[test]
fn moving_the_last_tab_of_workspace_zero_activates_its_neighbor() {
    let (mut app, ws0, ws1) = two_workspaces(1);
    let moved = app.active_session_token_for_test();
    assert_eq!(moved, ws0[0]);

    app.move_tab_to_workspace_for_test(moved, 1);

    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(app.active_workspace_index_for_test(), 0);
    assert_eq!(app.active_session_token_for_test(), ws1);
    assert_eq!(app.last_active_session_for_test(), ws1);
    assert_eq!(
        app.take_focus_reports_for_test(),
        vec![(moved, false), (ws1, true)]
    );
}

#[test]
fn single_pane_gutter_follows_the_glide_render_row_and_sub_row_shift() {
    let settings = Settings {
        command_status_gutter: true,
        ..Settings::default()
    };
    let (mut app, terminal) =
        headless_app_with(NativeOptions::default(), Dimensions::new(40, 6), settings);
    let cell = cell(8, 16);
    app.set_test_cell_for_test(cell);
    {
        let mut t = terminal.lock().expect("terminal");
        for _ in 0..10 {
            t.advance(b"before\r\n");
        }
        t.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        for _ in 0..15 {
            t.advance(b"after\r\n");
        }
    }
    let scrollback_len = app.scrollback_len_for_test();
    assert!(scrollback_len >= 10, "history to glide through");
    // Logical offset shows the prompt on screen row 5; the glide follower is
    // still at render offset `logical - 4` plus half a row.
    let logical = scrollback_len - 5;
    let render = logical - 4;
    let token = app.active_session_token_for_test();
    {
        let session = app.sessions_mut_for_test().get_mut(token).expect("session");
        session.viewport.scroll_up(logical, scrollback_len);
        session.glide_active = true;
        session.glide_target = logical;
        session.glide_visual = render as f32 + 0.5;
        session.scroll_frac_offset = 8.0;
    }
    let snapshot = terminal
        .lock()
        .expect("terminal")
        .snapshot_with_scrollback(render);
    let columns = snapshot.dimensions.columns;
    let prompt_row = (0..snapshot.dimensions.rows)
        .find(|row| snapshot.cells[row * columns].ch == '$')
        .expect("the prompt is on screen at the render offset");

    let input = app.gutter_frame_input_for_test();
    let quads = app.command_status_gutter_overlays(
        input.as_ref(),
        scrollback_len,
        cell,
        WindowPadding::ZERO,
    );
    assert_eq!(quads.len(), 1, "one finished command, one bar");
    let inset = 16.0 * 0.12;
    let expected_top = 8.0 + prompt_row as f32 * 16.0 + inset;
    assert!(
        (quads[0].rect[1] - expected_top).abs() < 1e-3,
        "bar top {} beside prompt row {prompt_row} drawn at {expected_top}",
        quads[0].rect[1]
    );

    // Settled at the same offset: whole rows, no sub-row shift.
    {
        let session = app.sessions_mut_for_test().get_mut(token).expect("session");
        session.glide_active = false;
        session.glide_visual = render as f32;
        session.scroll_frac_offset = 0.0;
        session.viewport.scroll_down(4);
    }
    let input = app.gutter_frame_input_for_test();
    let quads = app.command_status_gutter_overlays(
        input.as_ref(),
        scrollback_len,
        cell,
        WindowPadding::ZERO,
    );
    assert_eq!(quads.len(), 1);
    assert!((quads[0].rect[1] - (prompt_row as f32 * 16.0 + inset)).abs() < 1e-3);
}

fn write(terminal: &Arc<Mutex<Terminal>>, bytes: &[u8]) {
    terminal.lock().expect("terminal").advance(bytes);
}

fn top_rows(probes: &[PanePaintProbe]) -> Vec<String> {
    probes
        .iter()
        .map(|probe| {
            probe
                .rows
                .first()
                .map(|row| row.trim_end().to_owned())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn synchronized_output_holds_only_the_pane_inside_its_batch() {
    let (mut app, first_terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    let first = app.active_session_token_for_test();
    let second_terminal = Arc::new(Mutex::new(Terminal::new(40, 24)));
    app.seed_headless_split_pane_for_test(
        true,
        Arc::clone(&second_terminal),
        crate::native::test_support::headless_writer(),
        Dimensions::new(40, 24),
    );
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(cell(8, 16));
        app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    }
    app.focus_session_token_for_test(first);
    app.reflow_active_panes_for_test();

    write(&first_terminal, b"\x1b[HA1");
    write(&second_terminal, b"\x1b[HB1");
    assert_eq!(
        top_rows(&app.redraw_multipane_probe_for_test()),
        vec!["A1", "B1"]
    );

    // The focused pane opens a batch; the other pane keeps updating.
    write(&first_terminal, b"\x1b[?2026h\x1b[HA2");
    write(&second_terminal, b"\x1b[HB2");
    assert_eq!(
        top_rows(&app.redraw_multipane_probe_for_test()),
        vec!["A1", "B2"],
        "only the focused pane holds"
    );

    // The batches swap owners: the background pane holds, the focused pane
    // shows its finished batch.
    write(&first_terminal, b"\x1b[HA3\x1b[?2026l");
    write(&second_terminal, b"\x1b[?2026h\x1b[HB3");
    assert_eq!(
        top_rows(&app.redraw_multipane_probe_for_test()),
        vec!["A3", "B2"],
        "only the background pane holds"
    );

    write(&second_terminal, b"\x1b[?2026l");
    assert_eq!(
        top_rows(&app.redraw_multipane_probe_for_test()),
        vec!["A3", "B3"]
    );
}

/// After a split collapses to one pane, the survivor's retained snapshot is
/// the pane-local one its split frames stored. A synchronized-output batch
/// that starts right then must not re-present that snapshot as the window: the
/// first single-pane frame draws normally and stores a window frame.
#[test]
fn a_hold_after_a_split_collapses_draws_a_window_frame_first() {
    let (mut app, first_terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    let first = app.active_session_token_for_test();
    let second_terminal = Arc::new(Mutex::new(Terminal::new(40, 24)));
    app.seed_headless_split_pane_for_test(
        true,
        Arc::clone(&second_terminal),
        crate::native::test_support::headless_writer(),
        Dimensions::new(40, 24),
    );
    let second = app.active_session_token_for_test();
    assert_ne!(first, second);
    app.reflow_active_panes_for_test();
    write(&first_terminal, b"\x1b[HA1");
    write(&second_terminal, b"\x1b[HB1");
    // The split frame's focused-cursor pass, as `rebuild_multipane` runs it,
    // stores the focused pane's own pane-local snapshot.
    let mut pane = second_terminal.lock().expect("terminal").snapshot();
    let _ = app.advance_multipane_cursor_effects_for_test(
        std::time::Instant::now(),
        &mut pane,
        cell(8, 16),
        [400.0, 0.0],
    );
    let (pane_dims, _) = app
        .held_snapshot_geometry_for_test()
        .expect("the split frame stored the focused pane's snapshot");
    assert!(
        pane_dims.columns < 80,
        "a pane-local snapshot: {pane_dims:?}"
    );

    // Collapse the split so the second pane survives alone, then open a batch.
    app.focus_session_token_for_test(first);
    app.close_focused_pane_for_test();
    assert_eq!(app.active_session_token_for_test(), second);
    // Test geometry is held per session; give the survivor the window's.
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    app.reflow_active_panes_for_test();
    assert!(
        app.active_session_grid_dims_for_test().0 > pane_dims.columns,
        "the survivor takes the window width"
    );
    write(&second_terminal, b"\x1b[?2026h\x1b[HB2");
    let _ = app.redraw_single_pane_probe_for_test();
    let (dims, _) = app
        .held_snapshot_geometry_for_test()
        .expect("a single-pane frame was stored");
    assert!(
        dims.columns > pane_dims.columns,
        "the first frame after the collapse is a window frame, not the pane-local one: {dims:?}"
    );
}
