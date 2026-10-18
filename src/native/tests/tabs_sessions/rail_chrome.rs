// SPDX-License-Identifier: GPL-3.0-only
//! Rail chrome tests for the multi-session tabs feature.

use super::*;

// --- F4-V2 vertical tab rail integration ---

#[test]
fn tab_reserve_combines_top_bar_and_workspace_rail() {
    // The top tab bar (tabs) reserves a row off the top; the workspace rail
    // reserves columns off a side. They are independent bands that can coexist.
    // With one workspace and the default auto rail, only the top bar reserves
    // (byte-identical to a top-only bar).
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    // Pin a manual rail width so the reservation is name-length independent.
    app.set_tab_rail_width_manual_for_test(16);

    // Two tabs → top bar shown; one workspace → auto rail hidden. Top only.
    app.set_tab_bar_placement_for_test("top");
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 0),
        "top bar reserves one row, no rail"
    );

    // Force the workspace rail on the LEFT: BOTH bands reserve now.
    app.set_workspace_rail_for_test("left");
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 16),
        "top bar row + left rail band"
    );

    // The RIGHT rail reserves the same column count off the other side.
    app.set_workspace_rail_for_test("right");
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 16),
        "top bar row + right rail band (mirror)"
    );
}

#[test]
fn workspace_rail_grows_decorated_snapshot_by_columns_beside_the_top_bar() {
    // With the workspace rail shown alongside the top tab bar, the single-pane
    // decoration grows the snapshot by the rail's COLUMNS (content shifts right)
    // WITHOUT adding rows - the top bar's row is already present (the rail is a
    // full-height sidebar beside it). The grown axis must match `tab_reserve`.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    // Pin a manual rail width so the grown-columns delta is a fixed 16.
    app.set_tab_rail_width_manual_for_test(16);

    // Two tabs → top bar shown; auto rail hidden (one workspace). Top only.
    let (top_cols, top_rows) = app
        .decorated_snapshot_dims_for_test()
        .expect("top decorated dims");

    // Force the rail on the left: the decoration composes the top bar and the
    // full-height rail, growing columns by the 16-col band, rows unchanged.
    app.set_workspace_rail_for_test("left");
    let (rail_cols, rail_rows) = app
        .decorated_snapshot_dims_for_test()
        .expect("rail decorated dims");

    assert_eq!(
        rail_rows, top_rows,
        "the rail adds columns, not rows (the top bar row stays)"
    );
    assert_eq!(
        rail_cols,
        top_cols + 16,
        "the rail adds its 16-col band beside the content"
    );
}

#[test]
fn pinned_tab_chrome_preserves_single_pane_cursor_motion_and_large_jump_streaks() {
    let cases = [
        ("top tabs", None, false),
        ("left workspace rail", Some("left"), true),
        ("right workspace rail", Some("right"), false),
    ];
    let cell = CellSize {
        width: 11,
        height: 19,
        baseline: 14,
    };

    for (label, rail, shifts_content_right) in cases {
        let mut app = tab_bar_app();
        app.set_test_cell_for_test(cell);
        app.set_tab_rail_width_manual_for_test(16);
        if let Some(side) = rail {
            app.set_workspace_rail_for_test(side);
        }
        app.enable_cursor_effects_for_test();

        let (top_rows, rail_cols) = app.tab_reserve_for_test();
        assert!(top_rows > 0, "{label}: top tabs must be pinned");
        assert_eq!(
            rail_cols,
            if rail.is_some() { 16 } else { 0 },
            "{label}: expected pinned rail reservation"
        );

        let t0 = Instant::now();
        let mut prior = snapshot(&["                        "], 24);
        prior.cursor = Position { row: 0, column: 0 };
        let (decorated, content, _, _) =
            app.advance_single_pane_cursor_effects_with_chrome_for_test(t0, &prior, cell);
        assert_eq!(
            content.dimensions, prior.dimensions,
            "{label}: raw dimensions"
        );
        assert_eq!(content.cursor, prior.cursor, "{label}: raw cursor");
        assert_eq!(
            decorated.dimensions,
            Dimensions::new(
                prior.dimensions.columns + rail_cols,
                prior.dimensions.rows + top_rows,
            ),
            "{label}: rendering remains chrome-decorated"
        );
        assert_eq!(decorated.cursor.row, prior.cursor.row + top_rows);
        assert_eq!(
            decorated.cursor.column,
            prior.cursor.column + if shifts_content_right { rail_cols } else { 0 },
            "{label}: decorated cursor uses the visual content origin"
        );
        assert_eq!(
            app.held_snapshot_geometry_for_test(),
            Some((decorated.dimensions, decorated.cursor)),
            "{label}: held frames retain decorated GPU coordinates"
        );

        let mut nearby = prior.clone();
        nearby.cursor.column = 2;
        let (_, nearby_content, params, nearby_streak) = app
            .advance_single_pane_cursor_effects_with_chrome_for_test(
                t0 + Duration::from_millis(1),
                &nearby,
                cell,
            );
        assert_eq!(nearby_content.dimensions, nearby.dimensions);
        assert_eq!(nearby_content.cursor, nearby.cursor);
        assert_eq!(
            params.offset,
            [-2.0 * cell.width as f32, 0.0],
            "{label}: eligible nearby motion must not snap on chrome dimensions"
        );
        assert!(
            nearby_streak.is_none(),
            "{label}: nearby motion stays quiet"
        );

        let mut jumped = nearby.clone();
        jumped.cursor.column = 15;
        let started_at = t0 + Duration::from_millis(2);
        let (_, jumped_content, params, follower) =
            app.advance_single_pane_cursor_effects_with_chrome_for_test(started_at, &jumped, cell);
        assert_eq!(jumped_content.dimensions, jumped.dimensions);
        assert_eq!(jumped_content.cursor, jumped.cursor);
        assert!(
            params.follower_active,
            "{label}: destination cursor is suppressed"
        );
        let streak = follower.expect("large jump activates immediately through pinned chrome");
        assert_eq!(
            streak.destination,
            Position { row: 0, column: 15 },
            "{label}"
        );
        let expected_x0 = if shifts_content_right {
            rail_cols as f32 * cell.width as f32
        } else {
            0.0
        };
        let expected_y0 = top_rows as f32 * cell.height as f32;
        assert_eq!(
            streak.clip_rect,
            [
                expected_x0,
                expected_y0,
                expected_x0 + jumped.dimensions.columns as f32 * cell.width as f32,
                expected_y0 + jumped.dimensions.rows as f32 * cell.height as f32,
            ],
            "{label}: streak clip follows the scaled content rect"
        );
    }
}

#[test]
fn left_rail_hit_test_resolves_switch_close_and_new() {
    // Row-major rail hit-test round-trip: the same TabHit dispatch the bar uses,
    // resolved through the rail's X-band + row-stacked slots.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Pin a manual rail width (16) so the close-× column (rail_cols − 2)
    // is a fixed 14 regardless of the auto-sized width.
    app.set_tab_rail_width_manual_for_test(16);
    // Pin short single-line labels so the F4-P1 floating-`+` anchor (one gap
    // below the last slot's LABEL row) is deterministic regardless of the live
    // shell title's length.
    // Two workspaces give the rail two slots; the active workspace holds a
    // single tab so no top bar competes for the corner. Short names keep the
    // slot geometry deterministic (single label row), matching the labels the
    // pre-workspace rail tabs used.
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");

    // R1.1 geometry: a 1-row top margin, then tab 0 at rows [1,3), tab 1 at
    // [4,6). The close × sits inside the ring inset at col 14 (rail_cols 16 −
    // inset 1 − 1).
    //
    // Slot 0 body: row 1, label col → centre (12, 24).
    app.set_pointer_px_for_test(12.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("switch"),
        "slot 0 body → switch"
    );

    // Slot 0 close ×: inset top-right cell (row 1, col 14) → centre (116, 24).
    app.set_pointer_px_for_test(116.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("close"),
        "slot 0 × → close"
    );

    // The `+` anchors a dead gap below slot 1's end row (end 6 +
    // gap 1 = row 7); row 6 is the dead separator. Centre y = 7*16 + 8 = 120.
    app.set_pointer_px_for_test(20.0, 120.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new"), "+ slot → new");
    // The dead separator row (6, centre y = 104) is not a hit.
    app.set_pointer_px_for_test(64.0, 104.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "the dead separator row above the + is inert"
    );
}

#[test]
fn click_right_of_the_rail_is_not_a_tab_hit() {
    // A click in the content area (past the rail's right edge, x ≥ rail_w=128)
    // is never a tab hit - the X-band gate excludes it.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // A second workspace makes the auto rail appear on the left; the active
    // workspace's single tab means no top bar, so the content area is clear.
    add_workspace(&mut app);

    app.set_pointer_px_for_test(200.0, 8.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "content-area click → no tab hit"
    );
}

#[test]
fn rail_seam_drag_sets_and_persists_a_manual_width() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    // Pin a manual width so the seam is deterministic: left rail band [0, 128),
    // so the inner seam sits at x = 16*8 = 128 (headless origin at 0).
    app.set_tab_rail_width_manual_for_test(16);
    let (_conf_dir, conf) = temp_rail_conf("seam-drag");
    app.set_config_path_for_test(conf.clone());

    // A press ON the seam arms the drag; no width change yet (fails-before: the
    // pre-P4 rail had no draggable seam).
    app.set_pointer_px_for_test(128.0, 100.0);
    app.mouse_left_press_for_test();
    assert!(
        app.rail_seam_dragging_for_test(),
        "a press on the seam arms a drag"
    );
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(16));

    // Motion sets the manual width the pointer maps to: 80px / 8 = 10 cells.
    app.pointer_move_for_test(80.0, 100.0);
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(10));
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::ColResize,
        "a column-resize cursor is shown during the drag"
    );

    // Release disarms and persists the dragged width to the temp config, non-
    // destructively.
    app.mouse_left_release_for_test();
    assert!(
        !app.rail_seam_dragging_for_test(),
        "release disarms the drag"
    );
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(10));
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(written.contains("# kept"), "existing config preserved");
    assert!(
        written.contains("tab_rail_width = 10"),
        "the dragged manual width persisted; got: {written:?}"
    );
}

#[test]
fn double_click_rail_seam_resets_to_auto_and_persists() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    // Start from a manual width (seam at x = 20*8 = 160).
    app.set_tab_rail_width_manual_for_test(20);
    let (_conf_dir, conf) = temp_rail_conf("seam-dblclick");
    app.set_config_path_for_test(conf.clone());
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(20));

    // Two quick presses on the seam (a double-click) reset the rail to auto.
    app.set_pointer_px_for_test(160.0, 100.0);
    let t0 = std::time::Instant::now();
    app.mouse_left_press_at_for_test(t0);
    app.mouse_left_release_for_test();
    app.mouse_left_press_at_for_test(t0 + std::time::Duration::from_millis(50));
    assert_eq!(
        app.tab_rail_width_for_test(),
        TabRailWidth::Auto,
        "double-click the seam resets to auto width"
    );
    assert!(
        !app.rail_seam_dragging_for_test(),
        "the reset does not leave a drag armed"
    );
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(
        written.contains("tab_rail_width = auto"),
        "the auto reset persisted; got: {written:?}"
    );
}

#[test]
fn rail_seam_hover_shows_a_resize_cursor_off_the_tab_slots() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // seam at x = 128

    // Hovering the seam grab band shows a column-resize cursor.
    app.pointer_move_for_test(128.0, 100.0);
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::ColResize,
        "the seam grab band shows a resize cursor"
    );
    // Hovering a tab slot (well inside the band) does not.
    app.pointer_move_for_test(20.0, 24.0);
    assert_ne!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::ColResize,
        "a tab slot is not a resize target"
    );
}

#[test]
fn top_seam_owns_pinned_rail_junction_on_both_sides_at_common_scales() {
    for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
        let cell = cell((8.0 * scale).round() as u32, (16.0 * scale).round() as u32);
        let padding = WindowPadding::from_logical(4.0, scale);
        for side in ["left", "right"] {
            let config_dir = crate::test_dirs::ScratchDir::new("ottbc");
            let mut app = tab_bar_app();
            app.set_config_path_for_test(config_dir.path().join("odytty.conf"));
            app.set_test_cell_for_test(cell);
            app.set_test_scale_for_test(scale);
            app.set_test_surface_for_test(1000, 600, padding);
            app.set_workspace_rail_for_test(side);
            app.set_tab_rail_width_manual_for_test(16);

            let span = app.top_panel_span_for_test().expect("drawn top span");
            let seam_y = app.tab_bar_seam_y_for_test().expect("top seam");
            let junction_x = if side == "left" {
                span[0] + cell.width as f32 * 0.5
            } else {
                span[1] - cell.width as f32 * 0.5
            };
            app.pointer_move_for_test(junction_x as f64, seam_y as f64);
            assert_eq!(
                app.cursor_icon_for_test(),
                winit::window::CursorIcon::RowResize,
                "{side} rail junction owns the drawn top seam at scale {scale}"
            );

            app.set_pointer_px_for_test(junction_x as f64, seam_y as f64);
            app.mouse_left_press_for_test();
            assert!(
                app.tab_bar_seam_dragging_for_test(),
                "junction press arms row resize for {side} rail at scale {scale}"
            );
            assert!(!app.rail_seam_dragging_for_test());
            app.mouse_left_release_for_test();
        }
    }
}

#[test]
fn gap_strip_right_press_routes_to_content_not_chrome_menus() {
    // CHROME-GAP hit routing: the padding-wide neutral strips between the
    // content grid and the chrome bands belong to CONTENT for the empty-area
    // right-press route (consistent with left-click), while the drawn bands -
    // including the joined-band junction strip the bar background paints up to
    // the rail edge - keep their chrome menus.
    let mut app = tab_bar_app();
    let cell = cell(8, 16);
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(1000, 600, WindowPadding::from_logical(4.0, 1.0));
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    // Geometry: rail band x < 132 (pad 4 + 16 cols * 8); gap strip [132, 136);
    // content starts at x = 136, y = 24; bar band bottom (drawn) at y = 20.

    // Rail band proper (content rows) -> workspace chrome menu.
    app.set_pointer_px_for_test(100.0, 100.0);
    assert_eq!(app.empty_chrome_menu_surface_for_test(), Some("workspace"));
    // Neutral gap strip beside the content columns -> content (grid menu).
    app.set_pointer_px_for_test(133.0, 100.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        None,
        "the rail-side gap strip routes to content, not a chrome menu"
    );
    // Joined-band junction strip (within the bar band rows) stays chrome: the
    // bar background paints it out to the rail band edge.
    app.set_pointer_px_for_test(133.0, 10.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        Some("tab"),
        "the chrome-chrome junction strip keeps a chrome menu"
    );
    // Below-bar gap row (content columns) -> content.
    app.set_pointer_px_for_test(300.0, 22.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        None,
        "the below-bar gap row routes to content"
    );
    // Bar band proper -> tab chrome menu.
    app.set_pointer_px_for_test(300.0, 10.0);
    assert_eq!(app.empty_chrome_menu_surface_for_test(), Some("tab"));
}

#[test]
fn right_rail_gap_strip_routes_to_content_and_zero_padding_is_flush() {
    // Right-rail mirror of the gap-strip routing, plus the zero-padding
    // identity: with no padding there is no strip and the band boundary sits
    // exactly at the content edge, as before.
    let mut app = tab_bar_app();
    let cell = cell(8, 16);
    let padding = WindowPadding::from_logical(4.0, 1.0);
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(1000, 600, padding);
    app.set_workspace_rail_for_test("right");
    app.set_tab_rail_width_manual_for_test(16);
    // Sync the live grid to the injected surface: the band is painted from the
    // grid's column count, and the hit boundary now shares that basis.
    app.resize_grid_with_padding_for_test(cell, padding, 1000, 600);
    // Geometry (grid basis, matching the painted band): content columns end at
    // 4 + 107*8 = 860; gap strip [860, 864); rail band painted from
    // rail_origin = 4 + 107*8 + 4 = 864. The content rect's pixel width
    // carries a 4px sub-cell remainder (860 = 107*8 + 4), so a boundary at
    // content.x + content.w + gap (= 868) would misroute the innermost 4px of
    // the PAINTED band to content - the hit test must bind at the drawn seam.
    app.set_pointer_px_for_test(861.0, 100.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        None,
        "the right-rail gap strip routes to content"
    );
    app.set_pointer_px_for_test(865.0, 100.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        Some("workspace"),
        "the innermost sliver of the painted band is rail chrome, not content"
    );
    app.set_pointer_px_for_test(870.0, 100.0);
    assert_eq!(app.empty_chrome_menu_surface_for_test(), Some("workspace"));
    // The chrome-chrome junction over the band sliver stays chrome too: at bar
    // rows the rail owns its full-height column band from the same boundary.
    app.set_pointer_px_for_test(865.0, 10.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        Some("workspace"),
        "the band sliver at bar rows keeps the rail menu"
    );

    // Zero padding: no strip; the band starts at the shared flush edge.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(1000, 600, WindowPadding::ZERO);
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    // Rail band x < 128; content flush at x = 128, y = 16.
    app.set_pointer_px_for_test(127.0, 100.0);
    assert_eq!(app.empty_chrome_menu_surface_for_test(), Some("workspace"));
    app.set_pointer_px_for_test(128.0, 100.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        None,
        "flush layout keeps the historical boundary"
    );
    app.set_pointer_px_for_test(300.0, 8.0);
    assert_eq!(app.empty_chrome_menu_surface_for_test(), Some("tab"));
}

#[test]
fn right_rail_hit_boundary_is_grid_exact_at_cell_multiple_width() {
    // When the content width is an exact cell multiple there is no sub-cell
    // remainder, so the grid-basis boundary and the content-rect edge agree -
    // the historical routing is byte-identical. (The remainder case is pinned
    // in `right_rail_gap_strip_routes_to_content_and_zero_padding_is_flush`.)
    let mut app = tab_bar_app();
    let cell = cell(8, 16);
    app.set_test_cell_for_test(cell);
    // Zero padding, right rail 16 cols: content.w = 1000 - 128 = 872 = 109*8
    // exactly. Band painted (and hit) from x = 872, flush at the shared edge.
    app.set_test_surface_for_test(1000, 600, WindowPadding::ZERO);
    app.set_workspace_rail_for_test("right");
    app.set_tab_rail_width_manual_for_test(16);
    app.resize_grid_with_padding_for_test(cell, WindowPadding::ZERO, 1000, 600);
    app.set_pointer_px_for_test(871.0, 100.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        None,
        "flush cell-multiple layout keeps the historical content boundary"
    );
    app.set_pointer_px_for_test(872.0, 100.0);
    assert_eq!(
        app.empty_chrome_menu_surface_for_test(),
        Some("workspace"),
        "the band starts exactly at the shared grid edge"
    );
}

#[test]
fn revealed_autohide_rail_clips_and_owns_the_top_seam_junction() {
    for side in ["left", "right"] {
        let mut app = tab_bar_app();
        let cell = cell(8, 16);
        app.set_test_cell_for_test(cell);
        app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
        app.set_workspace_rail_for_test(side);
        app.set_tab_rail_width_manual_for_test(16);
        app.set_tab_rail_autohide_for_test(true);
        app.force_rail_reveal_for_test();

        let span = app.top_panel_span_for_test().expect("revealed top span");
        let seam_y = app.tab_bar_seam_y_for_test().expect("top seam");
        let junction_x = if side == "left" {
            span[0]
        } else {
            span[1] - 1.0
        };
        app.pointer_move_for_test(junction_x as f64, seam_y as f64);
        assert_eq!(
            app.cursor_icon_for_test(),
            winit::window::CursorIcon::RowResize,
            "revealed {side} junction owns the drawn horizontal seam"
        );

        let clipped_x = if side == "left" {
            span[0] - 8.0
        } else {
            span[1] + 8.0
        };
        app.pointer_move_for_test(clipped_x as f64, seam_y as f64);
        assert_ne!(
            app.cursor_icon_for_test(),
            winit::window::CursorIcon::RowResize,
            "the top seam is not hit-testable inside the revealed {side} overlay"
        );
    }
}

#[test]
fn top_seam_wins_its_right_junction_with_a_live_scrollbar() {
    let config_dir = crate::test_dirs::ScratchDir::new("ottbc");
    let (mut app, fixtures) = app_with_two_sessions();
    {
        let mut terminal = fixtures[0].0.lock().expect("terminal");
        terminal.advance(&scrollback_bytes(200));
    }
    let cell = cell(8, 16);
    app.set_config_path_for_test(config_dir.path().join("odytty.conf"));
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(900, 400, WindowPadding::ZERO);
    app.set_workspace_rail_for_test("right");
    app.set_tab_rail_width_manual_for_test(16);
    app.scroll_up_for_test(usize::MAX);
    assert!(
        app.scrollback_len_for_test() > 0,
        "scrolling up must materialize scrollback; the precondition is not environmental"
    );

    let span = app.top_panel_span_for_test().expect("right top span");
    let seam_y = app.tab_bar_seam_y_for_test().expect("top seam");
    let content_edge_x = span[1] - 1.0;
    app.set_pointer_px_for_test(content_edge_x as f64, seam_y as f64);
    assert!(
        app.scrollbar_hit_for_test(),
        "fixture overlaps the live thumb"
    );
    app.pointer_move_for_test(content_edge_x as f64, seam_y as f64);
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::RowResize
    );
    app.mouse_left_press_for_test();
    assert!(app.tab_bar_seam_dragging_for_test());
    assert!(!app.scrollbar_dragging_for_test());
    app.mouse_left_release_for_test();
}

#[test]
fn tab_bar_seam_drag_sets_and_persists_a_manual_height() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    // Default top-bar placement; two sessions => the bar shows. Start on the
    // classic one-row bar, so its bottom seam sits at y = 1*16 = 16 (headless
    // origin 0).
    let (_conf_dir, conf) = temp_rail_conf("tabbar-seam-drag");
    app.set_config_path_for_test(conf.clone());

    // A press ON the seam arms the drag; no height change yet.
    app.set_pointer_px_for_test(40.0, 16.0);
    app.mouse_left_press_for_test();
    assert!(
        app.tab_bar_seam_dragging_for_test(),
        "a press on the seam arms a drag"
    );
    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Auto);

    // Motion sets the manual height the pointer maps to: 48px / 16 = 3 rows.
    app.pointer_move_for_test(40.0, 48.0);
    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(3));
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::RowResize,
        "a row-resize cursor is shown during the drag"
    );

    // Release disarms and persists the dragged height to the temp config,
    // non-destructively.
    app.mouse_left_release_for_test();
    assert!(
        !app.tab_bar_seam_dragging_for_test(),
        "release disarms the drag"
    );
    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(3));
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(written.contains("# kept"), "existing config preserved");
    assert!(
        written.contains("tab_bar_height = 3"),
        "the dragged manual height persisted; got: {written:?}"
    );
}

#[test]
fn tab_bar_seam_drag_rebases_the_settings_panel_snapshot() {
    use winit::keyboard::Key as WinitKey;

    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    let (_conf_dir, conf) = temp_rail_conf("tabbar-panel-rebase");
    app.set_config_path_for_test(conf.clone());

    // Resize through the production seam path before opening Settings.
    app.set_pointer_px_for_test(40.0, 16.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(40.0, 48.0);
    app.mouse_left_release_for_test();
    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(3));

    app.open_settings_overlay_for_test();
    let signature = app.overlay_signature_for_test();
    let tab_height = signature
        .panel
        .entries
        .iter()
        .find(|entry| entry.key == "tab_bar_height")
        .expect("tab height row");
    assert_eq!(tab_height.value, "3");
    assert_eq!(
        signature.panel.changed_count, 0,
        "the mouse-set height is the clean panel baseline"
    );

    // Saving with no panel edit must not write the pre-drag snapshot back.
    app.drive_overlay_key_for_test(WinitKey::Character("s".into()), true, false);
    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(3));
}

#[test]
fn tab_bar_input_writers_leave_inherited_config_untouched() {
    let inherited = crate::test_dirs::ChildEnv::new();
    for test in [
        "native::tests::tabs_sessions::rail_chrome::tab_bar_height_typing_commits_through_the_production_overlay_key_route",
        "native::tests::tabs_sessions::rail_chrome::tab_bar_height_drag_centers_labels_in_the_decorated_band",
        "native::tests::tabs_sessions::rail_chrome::top_seam_owns_pinned_rail_junction_on_both_sides_at_common_scales",
        "native::tests::tabs_sessions::rail_chrome::top_seam_wins_its_right_junction_with_a_live_scrollbar",
        "native::tests::tabs_sessions::rail_chrome::tab_bar_seam_drag_clamps_to_min_and_max_rows",
        "native::tests::ui_state_sync::pinned_rail_seam_drags_while_the_top_bar_is_hidden",
        "native::app::scroll_coordinate_tests::input_events_do_not_wait_on_a_background_tab_terminal",
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
        crate::native::app::win_spawn::apply_no_console_window(&mut child);
        let output = child
            .envs(inherited.vars())
            .args([test, "--exact", "--test-threads=1", "--show-output"])
            .output()
            .expect("run input writer test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "input writer did not execute successfully: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_inherited_dirs_untouched(&inherited, test);
    }
}

/// Entries under `root` as relative paths, breadth first and sorted by name,
/// without following links. Bounded so a runaway writer cannot flood a failure.
fn written_entries(root: &std::path::Path) -> Vec<String> {
    const MAX_ENTRIES: usize = 40;
    const MAX_DEPTH: usize = 8;
    let mut found = Vec::new();
    let mut pending = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = pending.pop_front() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = read.flatten().collect();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            if found.len() >= MAX_ENTRIES {
                found.push("...".to_owned());
                return found;
            }
            let path = child.path();
            let is_dir = child.file_type().is_ok_and(|kind| kind.is_dir());
            let relative = path.strip_prefix(root).unwrap_or(&path);
            found.push(format!(
                "{}{}",
                relative.display(),
                if is_dir { "/" } else { "" }
            ));
            if is_dir && depth < MAX_DEPTH {
                pending.push_back((path, depth + 1));
            }
        }
    }
    found
}

/// Panic naming `writer` and every entry found when a directory the fixture
/// handed to a child is no longer empty. `HISTFILE` is a file path, not a
/// directory, and is exempt.
fn assert_inherited_dirs_untouched(inherited: &crate::test_dirs::ChildEnv, writer: &str) {
    let mut seen = std::collections::BTreeSet::new();
    for (key, path) in inherited.vars() {
        if key == "HISTFILE" || !seen.insert(path.clone()) {
            continue;
        }
        let entries = written_entries(&path);
        assert!(
            entries.is_empty(),
            "{writer} changed inherited {key}; entries found: {entries:?}"
        );
    }
}

#[test]
#[should_panic(expected = "fake_writer changed inherited HOME; entries found")]
fn an_inherited_directory_write_is_reported_with_the_writer_and_the_path() {
    let inherited = crate::test_dirs::ChildEnv::new();
    assert_inherited_dirs_untouched(&inherited, "quiet_writer");
    let nested = inherited.vars()[0].1.join("AppData").join("Local");
    std::fs::create_dir_all(&nested).expect("nested directory");
    std::fs::write(nested.join("startup.bin"), b"x").expect("write");
    assert_inherited_dirs_untouched(&inherited, "fake_writer");
}

#[test]
fn written_entries_lists_nested_files_in_order_and_bounds_a_flood() {
    let scratch = crate::test_dirs::ScratchDir::new("owe");
    assert!(written_entries(scratch.path()).is_empty());
    std::fs::create_dir_all(scratch.path().join("a").join("b")).unwrap();
    std::fs::write(scratch.path().join("a").join("b").join("f.txt"), b"x").unwrap();
    std::fs::write(scratch.path().join("z.txt"), b"x").unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        written_entries(scratch.path()),
        vec![
            "a/".to_owned(),
            "z.txt".to_owned(),
            format!("a{sep}b/"),
            format!("a{sep}b{sep}f.txt"),
        ]
    );
    let flood = crate::test_dirs::ScratchDir::new("owf");
    for index in 0..60 {
        std::fs::write(flood.path().join(format!("f{index:02}")), b"x").unwrap();
    }
    let listed = written_entries(flood.path());
    assert_eq!(listed.len(), 41, "40 entries plus the elision marker");
    assert_eq!(listed.last().map(String::as_str), Some("..."));
}

#[test]
fn tab_bar_height_typing_commits_through_the_production_overlay_key_route() {
    use winit::keyboard::{Key as WinitKey, NamedKey};

    let config_dir = crate::test_dirs::ScratchDir::new("ottbc");
    let mut app = tab_bar_app();
    let config = config_dir.path().join("odytty.conf");
    std::fs::write(&config, "# kept\n").expect("seed private config");
    app.set_config_path_for_test(config.clone());
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
    assert_eq!(
        app.overlay_signature_for_test().panel.editing_key,
        Some("tab_bar_height")
    );
    app.drive_overlay_key_for_test(WinitKey::Character("3".into()), false, false);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);

    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(3));
    assert_eq!(app.overlay_signature_for_test().panel.editing_key, None);
    app.drive_overlay_key_for_test(WinitKey::Character("s".into()), true, false);
    let saved = std::fs::read_to_string(&config).expect("input route persists private config");
    assert!(saved.contains("# kept"));
    assert!(saved.contains("tab_bar_height = 3"));
}

#[test]
fn tab_bar_height_drag_centers_labels_in_the_decorated_band() {
    let config_dir = crate::test_dirs::ScratchDir::new("ottbc");
    let mut app = tab_bar_app();
    let config = config_dir.path().join("odytty.conf");
    std::fs::write(&config, "# kept\n").expect("seed private config");
    app.set_config_path_for_test(config.clone());
    app.set_test_cell_for_test(cell(8, 16));

    // Drive the production pointer route from its default one-row seam to a
    // three-row band. The renderer must put label, close, and add glyphs on the
    // centered row rather than leaving them on the old top row.
    app.set_pointer_px_for_test(40.0, 16.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(40.0, 48.0);
    app.mouse_left_release_for_test();

    let rows = app
        .tab_bar_band_text_for_test()
        .expect("decorated top-tab-bar band");
    assert_eq!(rows.len(), 3, "the drag produces a three-row tab band");
    assert!(
        rows[0].chars().all(|ch| ch == ' '),
        "the top row is background-only after centering"
    );
    assert!(rows[1].contains('×'), "the close glyph is centered");
    assert!(rows[1].contains('+'), "the add glyph is centered");
    assert!(
        rows[1]
            .chars()
            .any(|ch| ch != ' ' && ch != '×' && ch != '+'),
        "a tab label is centered"
    );
    assert!(
        rows[2].chars().all(|ch| ch == ' '),
        "the bottom row is background-only after centering"
    );
    let saved = std::fs::read_to_string(&config).expect("input route persists private config");
    assert!(saved.contains("# kept"));
    assert!(saved.contains("tab_bar_height = 3"));
}

#[test]
fn double_click_tab_bar_seam_resets_to_auto_and_persists() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    // Start from a manual height (seam at y = 3*16 = 48).
    app.set_tab_bar_height_manual_for_test(3);
    let (_conf_dir, conf) = temp_rail_conf("tabbar-seam-dblclick");
    app.set_config_path_for_test(conf.clone());
    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(3));

    // Two quick presses on the seam (a double-click) reset the bar to auto.
    app.set_pointer_px_for_test(40.0, 48.0);
    let t0 = std::time::Instant::now();
    app.mouse_left_press_at_for_test(t0);
    app.mouse_left_release_for_test();
    app.mouse_left_press_at_for_test(t0 + std::time::Duration::from_millis(50));
    assert_eq!(
        app.tab_bar_height_for_test(),
        TabBarHeight::Auto,
        "double-click the seam resets to auto height"
    );
    assert!(
        !app.tab_bar_seam_dragging_for_test(),
        "the reset does not leave a drag armed"
    );
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(
        written.contains("tab_bar_height = auto"),
        "the auto reset persisted; got: {written:?}"
    );
}

#[test]
fn tab_bar_seam_drag_clamps_to_min_and_max_rows() {
    let config_dir = crate::test_dirs::ScratchDir::new("ottbc");
    let mut app = tab_bar_app();
    app.set_config_path_for_test(config_dir.path().join("odytty.conf"));
    app.set_test_cell_for_test(cell(8, 16));
    // Grab the classic one-row bar's seam (y = 16) and arm the drag.
    app.set_pointer_px_for_test(40.0, 16.0);
    app.mouse_left_press_for_test();
    // Drag far below the cap: the height clamps to the max (5 rows).
    app.pointer_move_for_test(40.0, 1000.0);
    assert_eq!(
        app.tab_bar_height_for_test(),
        TabBarHeight::Manual(5),
        "dragging past the cap clamps to the max height"
    );
    // Drag up above the bar top: the height clamps to the floor (1 row).
    app.pointer_move_for_test(40.0, -100.0);
    assert_eq!(
        app.tab_bar_height_for_test(),
        TabBarHeight::Manual(1),
        "dragging above the top clamps to the one-row floor"
    );
    app.mouse_left_release_for_test();
}

#[test]
fn tab_bar_height_reservation_reduces_shell_rows_by_the_chosen_count() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let mut app = App::new_headless(
        options,
        terminal.clone(),
        writer,
        headless,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let cell = cell(8, 16);
    // A second session makes the top bar visible.
    let (terminal_b, writer_b, headless_b, _bytes_b) = recorded_session(dims);
    app.push_headless_session_with_for_test(terminal_b, writer_b, headless_b);
    // A 3-row bar reserves 3 rows off the top: 384/16 = 24 total rows, minus 3
    // leaves 21 for the shell (the one-row bar leaves 23; see the sibling test).
    app.set_tab_bar_height_manual_for_test(3);
    assert!(app.resize_grid(cell, 640, 384));
    assert_eq!(app.tab_bar_rows_for_test(), 3);
    assert_eq!(
        app.tab_dimensions_at_position_for_test(0),
        Some(Dimensions::new(80, 21))
    );
    assert_eq!(
        app.tab_dimensions_at_position_for_test(1),
        Some(Dimensions::new(80, 21))
    );
}

/// FEEL-FIX (Bug 1): a multi-row top tab bar must float its label near the band
/// centre, never pinned to the top row. Driven through the real
/// `decorate_snapshot_with_tab_bar` decoration (the live frame path), not the
/// pure placement helper - the shipped centering biased every even height
/// upward (a two-row bar sat the label on row 0), which read as "hugging the
/// top". Round-half-to-lower centering puts the label on `rows / 2`.
#[test]
fn tab_bar_label_is_vertically_centered_in_a_multi_row_band() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    // (height, expected label row): the nearest row to the geometric centre,
    // rounding a half-row tie to the lower middle row. Height 1 keeps the
    // classic single-row placement; heights 2 and 4 must NOT land on row 0.
    for (height, expected_row) in [(1u16, 0usize), (2, 1), (3, 1), (4, 2), (5, 2)] {
        app.set_tab_bar_height_manual_for_test(height);
        let band = app
            .tab_bar_band_text_for_test()
            .expect("decorated top-tab-bar band");
        assert_eq!(
            band.len(),
            height as usize,
            "the band is {height} rows tall"
        );
        let label_row = band
            .iter()
            .position(|row| row.chars().any(|ch| !ch.is_whitespace()));
        assert_eq!(
            label_row,
            Some(expected_row),
            "a {height}-row tab bar must center its label on row {expected_row}",
        );
        if height > 1 {
            assert!(
                band[0].chars().all(char::is_whitespace),
                "a multi-row tab bar must never pin its label to the top row",
            );
        }
    }
}
