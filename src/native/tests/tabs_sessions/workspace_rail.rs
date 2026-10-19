// SPDX-License-Identifier: GPL-3.0-only
//! Workspace rail tests for the multi-session tabs feature.

use super::*;

// --- workspace rail chrome ---

/// A New Tab in a workspace bound to a host that is no longer configured
/// falls back to a local tab and raises a one-line notice - a stale binding
/// never blocks opening a tab. (The connect path itself needs a live `ssh`; the
/// stale-alias fallback is the headless-observable decision.)
#[test]
fn bound_workspace_stale_host_falls_back_to_local_with_notice() {
    let mut app = tab_bar_app();
    app.set_workspace_binding_for_test(Some("ghost-host".to_owned()));
    assert_eq!(
        app.active_workspace_binding_for_test().as_deref(),
        Some("ghost-host")
    );
    // No hosts.conf is resolvable in the headless harness, so the alias never
    // matches: New Tab takes the local fallback and reports it.
    app.new_tab_for_test();
    let notice = app
        .open_notice_message_for_test()
        .expect("a stale binding raises a fallback notice");
    assert!(
        notice.contains("ghost-host") && notice.contains("local tab"),
        "notice names the stale host and the local fallback: {notice}"
    );
    // The binding is left intact - only the resolution failed.
    assert_eq!(
        app.active_workspace_binding_for_test().as_deref(),
        Some("ghost-host")
    );
}

/// The App-side unbind clears the active workspace's host binding and
/// reports it; New Tab then returns to the local path.
#[test]
fn unbind_active_workspace_clears_the_binding() {
    let mut app = tab_bar_app();
    app.set_workspace_binding_for_test(Some("edge".to_owned()));
    app.unbind_active_workspace();
    assert_eq!(app.active_workspace_binding_for_test(), None);
    // Unbind confirms that new tabs are local again (the message
    // is host-agnostic - the escape is stating the new behavior, not the host).
    let notice = app
        .open_notice_message_for_test()
        .expect("unbind emits a one-line notice");
    assert!(
        notice.contains("unbound") && notice.contains("local"),
        "notice states new tabs open locally: {notice}"
    );
}

#[test]
fn single_workspace_default_shows_no_workspace_rail() {
    // A single-workspace launch is ZERO chrome change from a top-only tab
    // bar - the auto rail stays hidden until a second workspace exists.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_rail_width_manual_for_test(16);
    // Two tabs → top bar; one workspace + auto rail → no rail band.
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 0),
        "one workspace: top bar only, no rail"
    );

    // A second workspace makes the auto rail appear; the active (new) workspace
    // is single-tab, so the top bar drops and the rail band takes over.
    add_workspace(&mut app);
    assert_eq!(app.workspace_count_for_test(), 2);
    assert_eq!(
        app.tab_reserve_for_test(),
        (0, 16),
        "two workspaces: the auto rail appears"
    );
}

#[test]
fn workspace_rail_always_shows_with_a_single_workspace() {
    // `workspace_rail = always` pins the rail even with one workspace
    // (alongside the top bar).
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_rail_width_manual_for_test(16);
    app.set_workspace_rail_for_test("always");
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 16),
        "always: top bar row + rail band with one workspace"
    );
}

#[test]
fn clicking_a_workspace_rail_slot_switches_the_active_workspace() {
    // A click on a rail slot dispatches to `switch_workspace`,
    // not `switch_tab`; the band reports the WORKSPACE surface. a
    // press now ARMS a drag-to-reorder gesture and the switch fires on release
    // (a click that never crossed the movement threshold), so the click is a
    // press+release pair with the pointer parked on the same slot.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app); // two workspaces; active = the new one (index 1)
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Slot 0 body (top-margin row 1, label col) → the pointer is over the
    // WORKSPACE band, and a click (press+release, no drag) switches to
    // workspace 0.
    app.set_pointer_px_for_test(12.0, 24.0);
    assert_eq!(
        app.chrome_hit_band_for_test(),
        Some("workspace"),
        "the rail band is a workspace surface, not a tab surface"
    );
    app.mouse_left_press_for_test();
    assert_eq!(
        app.active_workspace_index_for_test(),
        1,
        "a bare press only arms the gesture; the switch waits for release"
    );
    app.mouse_left_release_for_test();
    assert_eq!(
        app.active_workspace_index_for_test(),
        0,
        "a rail slot click switches the active workspace on release"
    );
}

#[test]
fn workspace_drag_rearms_after_activation_without_pointer_motion() {
    let mut app = rail_drag_app();

    // Drive the real CursorMoved + MouseInput route once, then activate a
    // different workspace on release. Activation clears every session's
    // terminal-content pointer cache. A second press at the unchanged window
    // position must still resolve the same chrome slot and arm its drag.
    app.pointer_move_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();
    assert_eq!(app.active_workspace_index_for_test(), 0);

    app.mouse_left_press_for_test();
    assert_eq!(
        app.rail_ws_drag_for_test(),
        Some((false, 0)),
        "the unchanged window pointer re-arms the activated workspace slot"
    );
}

/// RAIL-DRAG shared setup: a left-rail app with three named workspaces (a, b, c)
/// and the active one at index 2 (c), the cell + rail geometry the drop-target
/// math is expressed against.
fn rail_drag_app() -> App {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app); // index 1
    add_workspace(&mut app); // index 2 (active)
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    app.rename_workspace_for_test(2, "c");
    assert_eq!(app.active_workspace_index_for_test(), 2);
    assert_eq!(app.workspace_names_for_test(), vec!["a", "b", "c"]);
    app
}

#[test]
fn dragging_a_workspace_slot_reorders_it_and_follows_the_active_by_identity() {
    // Press slot 0 (a), drag past every slot midpoint (cell 8x16 → midpoints at
    // y = 32/80/128 px), and release: `a` lands at the end and the active
    // workspace still follows its identity (c), reusing the shipped
    // `move_workspace` engine for the commit.
    let mut app = rail_drag_app();
    // Press on slot 0's label cell (row 1) - arms the gesture, no switch yet.
    app.set_pointer_px_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    assert_eq!(
        app.rail_ws_drag_for_test(),
        Some((false, 0)),
        "a bare press arms but does not yet drag"
    );
    // Drag below the last midpoint → the drop target is the append slot (count).
    app.pointer_move_for_test(12.0, 140.0);
    assert_eq!(
        app.rail_ws_drag_for_test(),
        Some((true, 3)),
        "past the threshold and every midpoint → armed, dropping at the end"
    );
    // Release commits the reorder.
    app.mouse_left_release_for_test();
    assert_eq!(
        app.rail_ws_drag_for_test(),
        None,
        "the drag clears on release"
    );
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["b", "c", "a"],
        "the dragged workspace moved to the end"
    );
    assert_eq!(
        app.active_workspace_index_for_test(),
        1,
        "the active workspace still follows c by identity"
    );
}

#[test]
fn workspace_drag_real_route_commits_a_neighbour_drop() {
    let mut app = rail_drag_app();

    // Lift c from slot 2 and hover the compacted b slot. This is the adjacent
    // route that previously resolved back to origin_idx and silently reverted.
    app.set_pointer_px_for_test(12.0, 120.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(12.0, 60.0);
    assert_eq!(app.rail_ws_drag_for_test(), Some((true, 1)));
    app.mouse_left_release_for_test();

    assert_eq!(app.workspace_names_for_test(), vec!["a", "c", "b"]);
    assert_eq!(app.active_workspace_index_for_test(), 1);
}

#[test]
fn workspace_drag_threshold_is_symmetric_across_real_neighbour_midpoints() {
    let mut up = rail_drag_app();
    // Slot midpoints are y=32, 80, 128. Grab the middle slot at its center;
    // both directions remain no-op until the proxy center crosses the adjacent
    // slot's real midpoint.
    up.set_pointer_px_for_test(12.0, 80.0);
    up.mouse_left_press_for_test();
    up.pointer_move_for_test(12.0, 33.0);
    assert_eq!(up.rail_ws_drag_for_test(), Some((true, 2)));
    up.pointer_move_for_test(12.0, 31.0);
    assert_eq!(up.rail_ws_drag_for_test(), Some((true, 0)));

    let mut down = rail_drag_app();
    down.set_pointer_px_for_test(12.0, 80.0);
    down.mouse_left_press_for_test();
    down.pointer_move_for_test(12.0, 127.0);
    assert_eq!(down.rail_ws_drag_for_test(), Some((true, 2)));
    down.pointer_move_for_test(12.0, 129.0);
    assert_eq!(down.rail_ws_drag_for_test(), Some((true, 3)));
}

#[test]
fn workspace_drag_invalidates_retained_chrome_on_arm_move_and_release() {
    // The rail is chrome, not terminal content. Every gesture phase must bump
    // the presentation epoch or the render cache can retain the prior frame
    // until an unrelated timer or terminal update changes its signature.
    let mut app = rail_drag_app();
    app.set_pointer_px_for_test(12.0, 24.0);
    let before_arm = app.presentation_epoch_for_test();
    app.mouse_left_press_for_test();
    let after_arm = app.presentation_epoch_for_test();
    assert!(after_arm > before_arm, "press feedback invalidates chrome");

    app.pointer_move_for_test(12.0, 140.0);
    let after_move = app.presentation_epoch_for_test();
    assert!(after_move > after_arm, "live proxy invalidates chrome");

    app.clear_needs_rebuild_for_test();
    app.mouse_left_release_for_test();
    assert!(
        app.presentation_epoch_for_test() > after_move,
        "release invalidates retained geometry immediately"
    );
    assert!(
        app.needs_rebuild_for_test(),
        "release opens the frame rebuild gate"
    );
    assert_eq!(app.workspace_names_for_test(), vec!["b", "c", "a"]);
}

#[test]
fn a_sub_threshold_press_stays_a_click_and_does_not_reorder() {
    // A tiny move under the movement threshold keeps the gesture a click: the
    // slot activates on release and the rail order is untouched (no accidental
    // reorder from click jitter).
    let mut app = rail_drag_app();
    app.set_pointer_px_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    // Move ~2.8px (< 5px threshold): never arms.
    app.pointer_move_for_test(14.0, 26.0);
    assert_eq!(
        app.rail_ws_drag_for_test(),
        Some((false, 0)),
        "a sub-threshold move never arms the drag"
    );
    app.mouse_left_release_for_test();
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["a", "b", "c"],
        "a click never reorders the rail"
    );
    assert_eq!(
        app.active_workspace_index_for_test(),
        0,
        "the click activated the pressed workspace"
    );
}

#[test]
fn escape_cancels_a_workspace_drag_leaving_the_order_untouched() {
    // Once armed, Escape aborts the gesture with the rail order unchanged and the
    // drag state cleared - the cancel-on-escape ergonomic.
    let mut app = rail_drag_app();
    app.set_pointer_px_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(12.0, 140.0);
    assert_eq!(app.rail_ws_drag_for_test(), Some((true, 3)), "armed drag");
    app.drive_named_key_for_test(NamedKey::Escape);
    assert_eq!(
        app.rail_ws_drag_for_test(),
        None,
        "Escape clears the in-flight drag"
    );
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["a", "b", "c"],
        "a cancelled drag leaves the order untouched"
    );
    assert_eq!(
        app.active_workspace_index_for_test(),
        2,
        "the active workspace is unchanged by a cancelled drag"
    );
}

#[test]
fn a_workspace_drag_holds_the_autohide_rail_open() {
    // The rail must not vanish mid-gesture: an in-flight drag pins the auto-hide
    // rail open (via `rail_pinned_open`), and the hold releases when the drag
    // ends. Verified through the same predicate `rail_overlay_visible` consults.
    let mut app = rail_drag_app();
    assert!(!app.rail_pinned_open_for_test(), "no hold before a drag");
    app.set_pointer_px_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    assert!(
        app.rail_pinned_open_for_test(),
        "an armed-or-pending drag holds the rail open"
    );
    app.mouse_left_release_for_test();
    assert!(
        !app.rail_pinned_open_for_test(),
        "the hold releases when the drag ends"
    );
}

#[test]
fn an_autohide_workspace_drag_uses_cursor_moved_and_reorders() {
    // Exercise the live route for the floating rail: CursorMoved first lands on
    // a revealed workspace slot, MouseInput arms the gesture, and later
    // CursorMoved must bypass auto-hide hover to advance the drag itself.
    let mut app = rail_drag_app();
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();

    // Slot 0's label row in the floating left rail. This production move makes
    // the overlay own the pointer before the production press starts the drag.
    app.pointer_move_for_test(20.0, 24.0);
    assert_eq!(app.chrome_hit_band_for_test(), Some("workspace"));
    app.mouse_left_press_for_test();
    assert_eq!(app.rail_ws_drag_for_test(), Some((false, 0)));
    assert!(app.rail_pinned_open_for_test());

    // The next real CursorMoved crosses the drag threshold and reaches the
    // append drop target, rather than returning through the auto-hide hover
    // path. The rail stays visible for the gesture.
    app.pointer_move_for_test(20.0, 140.0);
    assert_eq!(app.rail_ws_drag_for_test(), Some((true, 3)));
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Grabbing
    );
    assert!(app.rail_overlay_visible_for_test());

    app.mouse_left_release_for_test();
    assert_eq!(app.workspace_names_for_test(), vec!["b", "c", "a"]);
    assert!(!app.rail_pinned_open_for_test());
}

#[test]
fn autohide_overlay_retains_reorder_quads_on_both_sides() {
    for side in ["left", "right"] {
        let mut app = rail_drag_app();
        app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
        app.set_workspace_rail_for_test(side);
        app.set_tab_rail_autohide_for_test(true);
        app.force_rail_reveal_for_test();

        let resting = app.rail_overlay_widget_quads_for_test();
        assert!(resting.is_empty(), "{side} uses the embedded active fill");

        let pointer_x = if side == "left" { 20.0 } else { 680.0 };
        app.pointer_move_for_test(pointer_x, 24.0);
        app.mouse_left_press_for_test();
        app.pointer_move_for_test(pointer_x, 140.0);
        assert_eq!(app.rail_ws_drag_for_test(), Some((true, 3)));

        let dragging = app.rail_overlay_widget_quads_for_test();
        assert_eq!(
            dragging.len(),
            1,
            "{side} overlay retains the insertion indicator"
        );
        assert!(
            dragging.iter().any(|quad| {
                quad.rect[2] - quad.rect[0] > 100.0
                    && (quad.rect[3] - quad.rect[1] - 2.0).abs() < 0.01
            }),
            "{side} overlay retains the horizontal reorder insertion indicator"
        );
        app.mouse_left_release_for_test();
    }
}

#[test]
fn a_dragged_workspace_order_persists_through_the_shape_snapshot() {
    // The reorder rides the shape-snapshot autosave path (the same one the
    // context-menu Move Up/Down uses), so a dragged order survives a restart.
    let mut app = rail_drag_app();
    app.set_pointer_px_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(12.0, 140.0);
    app.mouse_left_release_for_test();
    let shape = app.capture_shape_for_test();
    let order: Vec<&str> = shape.workspaces.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(
        order,
        vec!["b", "c", "a"],
        "the snapshot captures the dragged rail order"
    );
    assert_eq!(
        shape.active_workspace, 1,
        "the snapshot captures the active index after the reorder"
    );
}

/// RAIL-DRAG (Bug 3 / split): `rail_drag_app` whose ACTIVE workspace tab is a
/// two-pane split, plus a headless surface so `multipane_geometry` resolves.
/// This is the case the shipped drag tests missed -- they only drove a
/// single-pane active tab, so the multipane left-press branch (which swallowed
/// every rail press once the active tab was split) was never on the path.
fn rail_drag_split_app() -> App {
    let mut app = rail_drag_app();
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);
    // `test_cell` / `test_surface` are per-session (reached via Deref to the
    // active pane); the split focused a fresh pane, so re-apply them there. A
    // window wide enough that the reserved left rail leaves a real content region
    // beside it, so a rail-slot press sits at x < content.x.
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "the active workspace tab is a two-pane split"
    );
    app
}

#[test]
fn a_rail_drag_reorders_even_when_the_active_tab_is_split() {
    // Bug 3: with a LEFT rail and a split active tab, the multipane left-press
    // branch used to swallow the rail-slot press -- its guard checked only the
    // y-bound, so a press with x in the rail but y in the content matched, found
    // no pane, and the bare return killed the gesture. The added x-bound restores
    // chrome routing: the press arms the drag, motion advances it, and release
    // commits the reorder, exactly as on a single-pane tab.
    let mut app = rail_drag_split_app();
    // Press rail slot 0 (a): with the split active tab this must still ARM the
    // gesture rather than being swallowed by the multipane branch.
    app.set_pointer_px_for_test(12.0, 24.0);
    assert_eq!(
        app.chrome_hit_band_for_test(),
        Some("workspace"),
        "the pointer is over a rail slot"
    );
    app.mouse_left_press_for_test();
    assert_eq!(
        app.rail_ws_drag_for_test(),
        Some((false, 0)),
        "the rail press arms the gesture even with a split active tab"
    );
    // Drag past every slot midpoint -> the append drop slot.
    app.pointer_move_for_test(12.0, 140.0);
    assert_eq!(
        app.rail_ws_drag_for_test(),
        Some((true, 3)),
        "motion advances the drag across a split active tab"
    );
    app.mouse_left_release_for_test();
    assert_eq!(app.rail_ws_drag_for_test(), None, "release clears the drag");
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["b", "c", "a"],
        "the release committed the reorder"
    );
}

#[test]
fn a_plain_rail_click_switches_workspace_when_the_active_tab_is_split() {
    // The click sibling of the drag: a press+release (no motion) on a rail slot
    // switches the active workspace even with a split active tab -- direct proof
    // the rail press is no longer swallowed by the multipane branch.
    let mut app = rail_drag_split_app();
    // Active starts at c (index 2). Click rail slot 0 (a).
    app.set_pointer_px_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();
    assert_eq!(
        app.active_workspace_index_for_test(),
        0,
        "a rail click switches the active workspace across a split active tab"
    );
}

#[test]
fn a_revealed_autohide_rail_click_switches_workspace_over_a_split_tab() {
    // With auto-hide the floating rail reserves no columns, so its overlay sits
    // inside the split content rect. A revealed rail press must bypass pane focus
    // and reach the workspace chrome route.
    let mut app = rail_drag_split_app();
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();
    app.set_pointer_px_for_test(12.0, 24.0);
    assert!(app.rail_overlay_visible_for_test());
    assert_eq!(app.chrome_hit_band_for_test(), Some("workspace"));

    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();

    assert_eq!(
        app.active_workspace_index_for_test(),
        0,
        "a floating rail click switches workspace instead of focusing a pane"
    );
}

#[test]
fn a_hidden_autohide_rail_does_not_steal_a_split_pane_press() {
    // The same x coordinate belongs to terminal content while the floating rail
    // is hidden. It must keep the existing focus-follows-click behavior.
    let mut app = rail_drag_split_app();
    app.set_tab_rail_autohide_for_test(true);
    app.set_pointer_px_for_test(12.0, 24.0);
    assert!(!app.rail_overlay_visible_for_test());
    assert_eq!(app.chrome_hit_band_for_test(), None);
    let initially_focused = app.focused_pane_id_for_test();

    app.mouse_left_press_for_test();

    assert_ne!(
        app.focused_pane_id_for_test(),
        initially_focused,
        "the hidden overlay leaves its columns available for pane focus"
    );
    assert_eq!(
        app.active_workspace_index_for_test(),
        2,
        "a hidden rail press does not switch workspace"
    );
}

#[test]
fn a_revealed_autohide_rail_click_keeps_the_single_pane_route() {
    // The multipane guard is absent on a single-pane tab, so the established
    // workspace chrome route remains unchanged.
    let mut app = rail_drag_app();
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();
    app.set_pointer_px_for_test(12.0, 24.0);

    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();

    assert_eq!(app.active_workspace_index_for_test(), 0);
}

#[test]
fn the_rail_plus_slot_resolves_to_a_new_workspace_hit() {
    // §7.4: the rail `+` slot is New Workspace - it resolves to a `NewTab` hit on
    // the WORKSPACE band (dispatched to `handle_new_workspace`).
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");

    // The `+` anchors a dead gap below workspace 1's end row
    // (row 7 centre y = 7*16 + 8 = 120), with the dead separator on row 6,
    // mirroring the tab-rail `+` geometry.
    app.set_pointer_px_for_test(20.0, 120.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new"), "+ slot → new");
    assert_eq!(
        app.chrome_hit_band_for_test(),
        Some("workspace"),
        "the + is on the workspace band → New Workspace"
    );
}

#[test]
fn the_shared_rename_field_retargets_the_workspace_name() {
    // §7.1: the rename field is shared with the tab bar; on the rail it re-targets
    // `Workspace.name`.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    app.rename_workspace_for_test(0, "old");
    let after = app.rename_workspace_via_field_for_test(0, "renamed");
    assert_eq!(
        after.as_deref(),
        Some("renamed"),
        "committing the rename field writes Workspace.name"
    );
    assert!(
        !app.rename_overlay_open_for_test(),
        "Enter closes the rename field"
    );
}

// ---- debounced workspace-shape autosave ----

/// A burst of shape mutations coalesces into exactly one autosave write once the
/// debounce quiet window elapses - a drag/rapid-edit stream is one write, not N.
#[test]
fn shape_autosave_debounces_a_mutation_burst_into_one_write() {
    let (mut app, _fixtures) = app_with_two_sessions();
    app.set_primary_instance_for_test(true);

    let t0 = Instant::now();
    // First pass establishes the fingerprint baseline: no write, nothing pending.
    app.run_about_to_wait_maintenance_for_test(t0);
    assert_eq!(app.autosave_saves_for_test(), 0);
    assert!(!app.autosave_pending_for_test());

    // A shape mutation arms the debounce but does not write within the window.
    add_workspace(&mut app);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(10));
    assert!(
        app.autosave_pending_for_test(),
        "a mutation arms the debounce"
    );
    assert_eq!(
        app.autosave_saves_for_test(),
        0,
        "not written within the window"
    );

    // A further mutation re-arms (coalesces) rather than writing twice.
    add_workspace(&mut app);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(500));
    assert_eq!(app.autosave_saves_for_test(), 0);

    // Once the quiet window elapses, exactly one write fires and clears pending.
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_secs(3));
    assert_eq!(
        app.autosave_saves_for_test(),
        1,
        "the burst coalesced into a single write"
    );
    assert!(!app.autosave_pending_for_test());

    // No further writes without a further mutation.
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_secs(6));
    assert_eq!(app.autosave_saves_for_test(), 1);
}

/// A non-primary instance never writes the shape snapshot, even across a
/// mutation and a full debounce window (only the lock holder saves).
#[test]
fn shape_autosave_is_inert_on_a_non_primary_instance() {
    let (mut app, _fixtures) = app_with_two_sessions();
    // Default construction is non-primary (set_primary_instance never called).
    let t0 = Instant::now();
    app.run_about_to_wait_maintenance_for_test(t0);
    add_workspace(&mut app);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_secs(3));
    assert_eq!(app.autosave_saves_for_test(), 0);
    assert!(!app.autosave_pending_for_test());
}

/// FEEL-FIX (Bug 6): returning to a backgrounded tab must not strand the
/// viewport near the top of scrollback with fresh output hidden below. A tab
/// keeps producing output while backgrounded but is not rendered, so its
/// scrollback-growth baseline freezes; without reconciling it on activation the
/// first switch-back render treats all the backgrounded growth as one huge
/// `added` and the "stay scrolled" anchor yanks a scrolled-up viewport to the
/// top. Driven through the real switch (`on_active_session_changed`) + the real
/// render anchor (`anchor_viewport_for_render`). Viewport/scroll is
/// platform-neutral (Unix and Windows share this path).
#[test]
fn switching_back_to_a_scrolled_up_tab_does_not_strand_the_viewport() {
    let (mut app, _fx) = app_with_two_sessions();
    app.set_test_cell_for_test(cell(8, 16));
    // Fill tab 0's scrollback past its 24-row screen, then baseline it with a
    // render frame (as the live path would once it has been on screen).
    let seed: Vec<u8> = (0..200)
        .flat_map(|i| format!("line{i}\r\n").into_bytes())
        .collect();
    app.advance_tab_bytes_at_position_for_test(0, &seed);
    app.anchor_viewport_for_render_frame_for_test();
    let baseline_scrollback = app.scrollback_len_for_test();
    assert!(baseline_scrollback > 24, "scrollback exceeds the screen");

    // The user scrolls up to read history, then switches to tab 1.
    app.scroll_up_for_test(50);
    assert_eq!(app.viewport_offset_for_test(), 50, "scrolled up 50 rows");
    assert!(app.switch_to_session_for_test(1), "switch away to tab 1");

    // Tab 0 keeps producing a large amount of output while backgrounded.
    let background: Vec<u8> = (0..500)
        .flat_map(|i| format!("bg{i}\r\n").into_bytes())
        .collect();
    app.advance_tab_bytes_at_position_for_test(0, &background);

    // Switch back. Activation must reconcile the frozen baseline so the
    // backgrounded growth is treated as already-past, not a fresh jump.
    assert!(app.switch_to_session_for_test(0), "switch back to tab 0");
    assert_eq!(
        app.last_scrollback_len_for_test(),
        app.scrollback_len_for_test(),
        "activation reconciles the scrollback baseline to the current length",
    );

    // The first render after switch-back must NOT yank the viewport toward the
    // top: the scrolled-up offset is preserved relative to the now-current
    // bottom (still 50), not pinned into deep history.
    let offset = app.anchor_viewport_for_render_frame_for_test();
    assert_eq!(
        offset, 50,
        "switch-back keeps the relative scroll position; it does not strand near the top",
    );

    // Typing still snaps to the live bottom so the prompt + new output show.
    app.drive_text_key_for_test("l");
    let after = app.anchor_viewport_for_render_frame_for_test();
    assert_eq!(
        after, 0,
        "a keystroke returns the viewport to the live bottom"
    );
}

/// FEEL-FIX (Bug 6) companion: a tab left at the live bottom (offset 0) stays
/// live across a background switch - backgrounded growth must not push the view
/// off the tail, and no reconcile can make it drift.
#[test]
fn switching_back_to_a_live_bottom_tab_stays_live() {
    let (mut app, _fx) = app_with_two_sessions();
    app.set_test_cell_for_test(cell(8, 16));
    let seed: Vec<u8> = (0..200)
        .flat_map(|i| format!("line{i}\r\n").into_bytes())
        .collect();
    app.advance_tab_bytes_at_position_for_test(0, &seed);
    app.anchor_viewport_for_render_frame_for_test();
    assert_eq!(
        app.viewport_offset_for_test(),
        0,
        "starts at the live bottom"
    );

    assert!(app.switch_to_session_for_test(1), "switch away");
    let background: Vec<u8> = (0..500)
        .flat_map(|i| format!("bg{i}\r\n").into_bytes())
        .collect();
    app.advance_tab_bytes_at_position_for_test(0, &background);
    assert!(app.switch_to_session_for_test(0), "switch back");

    let offset = app.anchor_viewport_for_render_frame_for_test();
    assert_eq!(
        offset, 0,
        "a live-bottom tab stays live across a background switch"
    );
}

// MENU-THEME PARITY regression (board 46195bed): the multi-pane window overlay
// (Settings / context menu / palette / connections / replay) is built by
// `build_overlay_top`, which must seed its snapshot from the live terminal
// palette -- the same colors the single-pane path resolves the panel's
// `Color::Default` fill against -- so the panel is the same themed color in both
// pane layouts. It previously seeded `DynamicColors::default()`, so the panel
// took an off-theme gray only after a split.
#[test]
fn multipane_overlay_top_uses_the_live_terminal_palette_not_default() {
    let (mut app, _fixtures) = app_with_two_sessions();
    // Split the active tab so the multi-pane overlay path is exercised.
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);
    assert_eq!(app.active_pane_count_for_test(), 2);

    // Headless geometry so `multipane_geometry()` resolves without a GPU.
    app.set_test_cell_for_test(cell(10, 20));
    app.set_test_surface_for_test(
        dims.columns as u32 * 10,
        dims.rows as u32 * 20,
        WindowPadding::ZERO,
    );

    // Install a distinctive default background on the primary terminal (OSC 11),
    // so a stale `DynamicColors::default()` seed would be detectable.
    app.advance_primary_terminal_for_test(b"\x1b]11;rgb:10/20/18\x1b\\");
    let distinctive = crate::core::RgbColor::new(0x10, 0x20, 0x18);

    // Open the settings overlay so `build_overlay_top` produces a snapshot.
    app.open_settings_overlay_for_test();

    let (overlay_colors, terminal_colors) = app
        .overlay_top_colors_for_test()
        .expect("multi-pane overlay snapshot with an open panel");

    // PARITY: the overlay snapshot resolves `Color::Default` against the SAME
    // palette the single-pane path uses (the live terminal).
    assert_eq!(
        overlay_colors, terminal_colors,
        "the overlay snapshot must be seeded from the live terminal palette"
    );
    // ...and that palette carries the distinctive background, proving the seed is
    // the theme, not the off-theme `DynamicColors::default()`.
    assert_eq!(
        overlay_colors.background, distinctive,
        "the overlay must follow the live terminal background"
    );
    assert_ne!(
        overlay_colors.background,
        crate::core::DynamicColors::default().background,
        "the overlay must NOT fall back to the default off-theme palette"
    );
}
