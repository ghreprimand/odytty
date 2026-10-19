// SPDX-License-Identifier: GPL-3.0-only
//! Rail autohide tests for the multi-session tabs feature.

use super::*;

// --- rail auto-hide ---

#[test]
fn autohide_removes_the_rail_reservation() {
    // The load-bearing rule: enabling auto-hide drops the rail's content
    // reservation to zero (one reflow at toggle time) - reveal is a pure
    // overlay, never a reflow. Pinned left reserves the band + gap; auto-hidden
    // reserves nothing.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    // A second workspace makes the auto rail appear (left side, inherited
    // from placement); the active workspace has a single tab, so no top bar is
    // reserved and the reservation is the rail band alone.
    add_workspace(&mut app);
    assert_eq!(
        app.tab_reserve_for_test(),
        (0, 16),
        "pinned left rail reserves its band"
    );

    app.set_tab_rail_autohide_for_test(true);
    assert!(app.rail_autohide_active_for_test());
    assert_eq!(
        app.tab_reserve_for_test(),
        (0, 0),
        "auto-hide removes the reservation entirely (content is full-width)"
    );
    // The overlay band still resolves its width (for the floating strip),
    // independent of the now-zero reservation.
    assert_eq!(app.rail_overlay_cols_for_test(), 16);

    // Toggling back off restores the reservation (the reverse reflow).
    app.set_tab_rail_autohide_for_test(false);
    assert_eq!(app.tab_reserve_for_test(), (0, 16));
}

#[test]
fn pinned_rail_gap_consumes_grid_columns_and_the_autohide_toggle_is_stable() {
    // CHROME-GAP grid sizing: with a pinned left rail and nonzero window
    // padding, the chrome-facing gap comes out of the content's pixel budget,
    // so the grid loses whatever whole cells the gap displaces (here exactly
    // one 8px column for an 8px padding). Toggling auto-hide reflows to
    // full-bleed and back to the SAME gap-inset width every time - the fit is a
    // pure function of the inputs, so no column oscillation is possible.
    let mut app = tab_bar_app();
    let cell = cell(8, 16);
    let padding = WindowPadding::from_logical(8.0, 1.0);
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(1280, 800, padding);
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    assert_eq!(app.tab_reserve_for_test(), (0, 16), "pinned left rail");

    // Pinned: (1280 - 2*8 - 16*8 - 8) / 8 = 141 columns - one column fewer
    // than the flush historical fit (142), consumed by the 8px gap.
    app.resize_grid_with_padding_for_test(cell, padding, 1280, 800);
    assert_eq!(app.grid_dims_for_test(), (141, 49), "gap costs one column");

    // Auto-hidden: no reservation, no gap - full-bleed content.
    app.set_tab_rail_autohide_for_test(true);
    app.resize_grid_with_padding_for_test(cell, padding, 1280, 800);
    assert_eq!(
        app.grid_dims_for_test(),
        (158, 49),
        "full-bleed under autohide"
    );

    // Re-pinned: the same 141 columns again; repeat the round trip to pin the
    // no-oscillation contract (requirement: seam toggles converge in one step).
    for _ in 0..2 {
        app.set_tab_rail_autohide_for_test(false);
        app.resize_grid_with_padding_for_test(cell, padding, 1280, 800);
        assert_eq!(app.grid_dims_for_test(), (141, 49), "stable re-pinned fit");
        app.set_tab_rail_autohide_for_test(true);
        app.resize_grid_with_padding_for_test(cell, padding, 1280, 800);
        assert_eq!(app.grid_dims_for_test(), (158, 49), "stable full-bleed fit");
    }

    // Flush-at-zero identity: with zero padding there is no gap and the fit is
    // the historical `(width / cell) - reserved` exactly.
    app.set_tab_rail_autohide_for_test(false);
    app.set_test_surface_for_test(1280, 800, WindowPadding::ZERO);
    app.resize_grid_with_padding_for_test(cell, WindowPadding::ZERO, 1280, 800);
    assert_eq!(
        app.grid_dims_for_test(),
        (144, 50),
        "padding 0 keeps the flush fit (1280/8 - 16, 800/16)"
    );
}

#[test]
fn chrome_offsets_grow_by_exactly_the_padding_gap_per_shown_band() {
    // CHROME-GAP hit-test alignment: the single placement-aware pointer
    // transform (`tab_chrome_offset_px`) grows by exactly one window padding on
    // each axis that has a pinned band, and not otherwise. Every single-pane
    // hit path (cell mapping, selection, drag autoscroll, SGR-pixel reports,
    // scrollbar) subtracts this transform, so asserting it here pins the whole
    // family to the gap-inset content origin.
    let mut app = tab_bar_app();
    let cell = cell(8, 16);
    app.set_test_cell_for_test(cell);
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);

    // Zero padding: the offset is the bare rail band (flush identity).
    app.set_test_surface_for_test(1280, 800, WindowPadding::ZERO);
    let (dx_flush, dy_flush) = app
        .tab_chrome_offset_px_for_test()
        .expect("left-rail chrome offset");
    assert_eq!(dx_flush, 128.0, "16 cols * 8 px, no gap at padding 0");
    assert_eq!(dy_flush, 0.0, "a rail never shifts content down");

    // 8px padding: the same reservation now carries an 8px chrome-facing gap.
    app.set_test_surface_for_test(1280, 800, WindowPadding::from_logical(8.0, 1.0));
    let (dx_gap, dy_gap) = app
        .tab_chrome_offset_px_for_test()
        .expect("left-rail chrome offset with padding");
    assert_eq!(dx_gap, dx_flush + 8.0, "the gap equals the window padding");
    assert_eq!(dy_gap, 0.0, "no top band, no vertical gap");

    // A right rail keeps the content origin unmoved: no offset, no gap (the
    // BAND moves instead; the content-side transform stays zero).
    app.set_tab_bar_placement_for_test("right");
    assert_eq!(
        app.tab_chrome_offset_px_for_test(),
        Some((0.0, 0.0)),
        "right rail: content origin unmoved even with padding"
    );
}

#[test]
fn rail_autohide_control_click_toggles_and_persists() {
    // Clicking the bottom-edge control flips the real
    // `tab_rail_autohide` setting through the live-apply + config-writeback path,
    // both directions, so the panel/env/config stay coherent.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    let (_conf_dir, conf) = temp_rail_conf("autohide-ctl");
    app.set_config_path_for_test(conf.clone());
    assert!(
        !app.tab_rail_autohide_setting_for_test(),
        "auto-hide starts off"
    );

    // The pinned rail shows the control at its bottom edge: a click there hits
    // the toggle (armed ahead of the workspace slots), not a slot.
    let (cx, cy) = app
        .rail_autohide_center_px_for_test()
        .expect("pinned rail exposes the auto-hide control");
    app.set_pointer_px_for_test(cx, cy);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("autohide"),
        "the bottom-edge control hit-tests as the auto-hide toggle"
    );
    app.mouse_left_press_for_test();

    assert!(
        app.tab_rail_autohide_setting_for_test(),
        "the click turned auto-hide on"
    );
    assert!(app.rail_autohide_active_for_test());
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(written.contains("# kept"), "existing config preserved");
    assert!(
        written.contains("tab_rail_autohide = on"),
        "auto-hide-on persisted; got: {written:?}"
    );

    // With auto-hide on the rail floats; the reveal-zone summons it, and the
    // control on the revealed overlay toggles it back off.
    app.force_rail_reveal_for_test();
    let (rx, ry) = app
        .rail_autohide_center_px_for_test()
        .expect("revealed rail overlay exposes the control");
    app.set_pointer_px_for_test(rx, ry);
    app.mouse_left_press_for_test();

    assert!(
        !app.tab_rail_autohide_setting_for_test(),
        "clicking the control again turned auto-hide back off"
    );
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(
        written.contains("tab_rail_autohide = off"),
        "auto-hide-off persisted; got: {written:?}"
    );
}

#[test]
fn rail_autohide_chevron_toggle_keeps_settings_panel_row_in_sync() {
    // RAIL-AUTOHIDE-CTL panel coherence: an external chevron toggle routes
    // through the reload seam as an ExternalChrome mutation, so the settings
    // panel's Layout row must reflect the live value in BOTH directions:
    //   (a) panel already open when the toggle fires -> the row updates live;
    //   (b) toggle fires first, panel opened afterwards -> the row shows current.
    // Fails-before: the toggle used OverlayEdit, whose panel path re-derives from
    // the panel's own (stale) edit overlay and ignores the external value.

    // Direction (a): panel open, then toggle.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    let (_conf_dir, conf) = temp_rail_conf("autohide-panel-a");
    app.set_config_path_for_test(conf.clone());

    app.open_settings_overlay_for_test();
    assert_eq!(
        app.settings_panel_displayed_value_for_test("tab_rail_autohide"),
        Some("off".to_owned()),
        "row starts off with the open panel"
    );
    app.toggle_tab_rail_autohide_for_test();
    assert!(
        app.tab_rail_autohide_setting_for_test(),
        "the external toggle turned auto-hide on"
    );
    assert_eq!(
        app.settings_panel_displayed_value_for_test("tab_rail_autohide"),
        Some("on".to_owned()),
        "(a) the open panel's row updates live to the external toggle"
    );
    // Toggling back is likewise reflected.
    app.toggle_tab_rail_autohide_for_test();
    assert_eq!(
        app.settings_panel_displayed_value_for_test("tab_rail_autohide"),
        Some("off".to_owned()),
        "(a) the open panel's row tracks the reverse toggle too"
    );

    // Direction (b): toggle first (panel closed), then open the panel.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    let (_conf_dir, conf) = temp_rail_conf("autohide-panel-b");
    app.set_config_path_for_test(conf.clone());

    assert!(
        !app.overlay_open_for_test(),
        "panel closed for direction (b)"
    );
    app.toggle_tab_rail_autohide_for_test();
    assert!(app.tab_rail_autohide_setting_for_test());
    app.open_settings_overlay_for_test();
    assert_eq!(
        app.settings_panel_displayed_value_for_test("tab_rail_autohide"),
        Some("on".to_owned()),
        "(b) a panel opened after the toggle shows the current value, not a stale copy"
    );
}

#[test]
fn autohide_only_applies_to_side_rails_not_the_top_bar() {
    // Auto-hide is a rail feature; the top bar keeps `always_show_tab_bar`
    // semantics, so the knob is inert (and the reservation unchanged) on top.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("top");
    app.set_tab_rail_autohide_for_test(true);
    assert!(
        !app.rail_autohide_active_for_test(),
        "auto-hide never applies to the top bar"
    );
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 0),
        "the top bar still reserves its row under the (ignored) autohide knob"
    );
}

#[test]
fn reveal_edge_zone_is_the_window_edge_and_band_extends_to_the_seam() {
    // The trigger zone is within `tab_rail_reveal_px` of the window edge; the
    // keep-alive band runs from the edge to the seam (⊇ the trigger zone).
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128] (pad 0 headless)
    app.set_tab_rail_autohide_for_test(true);

    // Default reveal_px = 16 (logical) at scale 1.0: x=2 is in the edge zone;
    // x=40 (deep in the band) is not the trigger but IS the keep-alive band;
    // x=200 (past the seam) is neither.
    assert_eq!(app.reveal_contact_for_test(2.0), Some((true, true)));
    assert_eq!(
        app.reveal_contact_for_test(40.0),
        Some((false, true)),
        "mid-band holds the reveal but does not re-trigger"
    );
    assert_eq!(
        app.reveal_contact_for_test(200.0),
        Some((false, false)),
        "past the seam is off the rail entirely"
    );
}

#[test]
fn reveal_zone_is_logical_px_scaled_for_hidpi() {
    // REGRESSION (trigger zone too thin on fractional/HiDPI): the reveal zone is
    // a LOGICAL-px width, scaled to the physical-px space winit reports pointer
    // coordinates in. At scale 2.0 the default 16 logical px must trigger out to
    // physical x=32 - with the old physical-px zone it would have stopped at x=8
    // and the rail would be unreachable on a scaled display.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);
    app.set_test_scale_for_test(2.0);

    // Physical x=30 is within 16 logical px (32 physical) of the edge → triggers.
    assert_eq!(
        app.reveal_contact_for_test(30.0).map(|(edge, _)| edge),
        Some(true),
        "16 logical px at 2x scale reaches physical x=30"
    );
    // Physical x=40 is beyond the scaled zone → no trigger (still in the band).
    assert_eq!(
        app.reveal_contact_for_test(40.0).map(|(edge, _)| edge),
        Some(false),
        "past 32 physical px the edge no longer triggers"
    );
}

#[test]
fn reveal_wiring_reaches_interior_and_holds_at_scale_1_5_with_padding() {
    // Live-wiring regression: reported (scale ~1.5, real
    // window padding) that the reveal only triggered at the very window edge and
    // would not stay up with the pointer over the band. This exercises the FULL
    // live path - `update_rail_autohide_pointer` (real contact geometry) + the
    // machine - with an injected clock at scale 1.5 with real padding, so the
    // interior-reach + keep-alive behavior is pinned at true device values, not
    // just the pad-0 / scale-1 headless case the earlier tests covered.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(12, 24)); // ~8x16 logical at 1.5x
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band_w = 16*12 = 192; seam at pad+192
    app.set_tab_rail_autohide_for_test(true);
    app.set_test_scale_for_test(1.5);
    let pad = WindowPadding::from_logical(8.0, 1.5); // 12 physical px
    app.set_test_surface_for_test(1000, 800, pad);
    // reach = pad(12) + reveal_px(16)*scale(1.5)=24 → 36. seam_x = 12 + 192 = 204.

    let t0 = std::time::Instant::now();

    // (1) INTERIOR REACH: a pointer 8px INTO the visible content (x=20, past the
    // 12px padding margin) must arm the reveal - not require the bare edge.
    app.feed_rail_pointer_for_test(20.0, t0);
    // Not visible yet (show debounce), but a poll past the debounce reveals it.
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "interior contact arms the debounce, not an instant reveal"
    );
    let revealed_at = t0 + std::time::Duration::from_millis(130); // past the show debounce
    app.feed_rail_pointer_for_test(20.0, revealed_at);
    assert!(
        app.rail_autohide_is_visible_for_test(revealed_at),
        "reveal triggers from an INTERIOR position (x=20, well inside the window)"
    );

    // (2) KEEP-ALIVE OVER THE BAND: moving deeper onto the drawn band (x=100,
    // mid-band) must HOLD the reveal - not drop it.
    let hold_at = revealed_at + std::time::Duration::from_millis(50);
    app.feed_rail_pointer_for_test(100.0, hold_at);
    assert!(
        app.rail_autohide_is_visible_for_test(hold_at),
        "pointer over the drawn band holds the reveal (keep-alive union)"
    );
    // A stationary pointer over the band across a maintenance poll stays up.
    let dwell = hold_at + std::time::Duration::from_millis(700); // past a hide grace
    assert!(
        !app.poll_rail_autohide_for_test(dwell),
        "no visibility change"
    );
    assert!(
        app.rail_autohide_is_visible_for_test(dwell),
        "a pointer parked over the band never times out"
    );

    // (3) HIDE ON LEAVE: past the seam (x=250) starts the grace; it hides only
    // after the grace elapses (not instantly).
    let leave_at = dwell + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(250.0, leave_at);
    assert!(
        app.rail_autohide_is_visible_for_test(leave_at),
        "leaving the band starts the hide grace: still visible through it"
    );
    let hidden_at = leave_at + std::time::Duration::from_millis(700); // > 600ms grace
    app.poll_rail_autohide_for_test(hidden_at);
    assert!(
        !app.rail_autohide_is_visible_for_test(hidden_at),
        "after the grace, the rail hides"
    );
}

#[test]
fn reveal_visibility_flip_marks_the_frame_for_rebuild() {
    // Live-trace regression: state reported visible at +27px but the rail was
    // not painted until crossing off the window edge. The reveal state
    // machine was already correct - the trace showed `visible=true` on-window -
    // but the paint never landed. Root cause: the rail overlay is assembled ONLY
    // inside the `should_rebuild_frame` gate (`build_rail_overlay`), and that gate
    // reads `needs_rebuild`. The rail-reveal paths requested a redraw WITHOUT
    // marking the frame dirty, so `RedrawRequested` skipped the rebuild and
    // re-presented the previous, rail-less frame. Over a quiescent terminal
    // (nothing else setting `needs_rebuild`) the reveal only painted when an
    // unrelated event - the pointer crossing off-window - happened to dirty a
    // frame. Both the maintenance-poll flip and the pointer-driven flip must now
    // set `needs_rebuild`.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    assert!(
        app.rail_autohide_active_for_test(),
        "precondition: a left rail with autohide on is active"
    );

    let t0 = std::time::Instant::now();

    // (A) MAINTENANCE-POLL FLIP: the exact reported scenario. Arm at the edge,
    // then STOP moving the pointer; the show-debounce elapses while the loop is
    // parked, so the flip to Revealed happens in the about-to-wait maintenance
    // poll, not in a pointer event. Clear the rebuild flag right after arming so
    // the only thing that can re-set it is the reveal flip itself.
    app.feed_rail_pointer_for_test(8.0, t0); // edge → arms Revealing (not visible)
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "edge contact arms the debounce, not an instant reveal"
    );
    app.clear_needs_rebuild_for_test();
    let revealed_at = t0 + std::time::Duration::from_millis(130); // past show debounce
    app.run_about_to_wait_maintenance_for_test(revealed_at);
    assert!(
        app.rail_autohide_is_visible_for_test(revealed_at),
        "the maintenance poll crosses the debounce and reveals the rail"
    );
    assert!(
        app.needs_rebuild_for_test(),
        "the reveal flip in the maintenance poll must mark the frame for rebuild: \
         a bare redraw request is dropped by the `should_rebuild_frame` gate and \
         the overlay never paints (the live-trace bug)"
    );

    // (B) HIDE FLIP - leaving the band starts the grace; when the grace elapses in
    // the maintenance poll the rail must be REMOVED from the frame, which likewise
    // needs a rebuild.
    let leave_at = revealed_at + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(400.0, leave_at); // off the band → hide grace
    app.clear_needs_rebuild_for_test();
    let hidden_at = leave_at + std::time::Duration::from_millis(700); // > 600ms grace
    app.run_about_to_wait_maintenance_for_test(hidden_at);
    assert!(
        !app.rail_autohide_is_visible_for_test(hidden_at),
        "after the grace the rail hides"
    );
    assert!(
        app.needs_rebuild_for_test(),
        "the hide flip must also rebuild so the overlay is dropped from the frame"
    );

    // (C) POINTER-DRIVEN FLIP - when the debounce elapses on a pointer sample
    // (the pointer is still moving over the band as it reveals), that path must
    // mark the frame dirty too.
    let t1 = hidden_at + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(8.0, t1); // re-arm at the edge
    app.clear_needs_rebuild_for_test();
    let t2 = t1 + std::time::Duration::from_millis(130); // past the debounce, on a sample
    app.feed_rail_pointer_for_test(50.0, t2); // still over the band → reveals here
    assert!(
        app.rail_autohide_is_visible_for_test(t2),
        "the reveal completes on the pointer sample past the debounce"
    );
    assert!(
        app.needs_rebuild_for_test(),
        "the pointer-driven reveal flip must mark the frame for rebuild"
    );
}

#[test]
fn reveal_arms_at_the_edge_and_the_band_carries_the_debounce() {
    // A brief edge touch during a fast approach must still reveal: the edge
    // contact ARMS the debounce, and the keep-alive band then CARRIES it to
    // completion even as the pointer moves off the thin trigger zone into the
    // band (the pointer does not have to sit pinned in the ≤reveal_px strip for
    // the whole debounce). Drives the real feed path with an injected clock.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    // pad 0, scale 1 → reach = 16 (default reveal_px). Zone is [0, 16].
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);

    let t0 = std::time::Instant::now();
    // Touch the edge once (x=8) → arms Revealing (not visible yet).
    app.feed_rail_pointer_for_test(8.0, t0);
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "edge contact arms the debounce, not an instant reveal"
    );
    // Move off the trigger zone but stay over the band (x=50 < seam 128). The
    // band keep-alive holds the arm; past the show debounce it reveals.
    let t1 = t0 + std::time::Duration::from_millis(90);
    app.feed_rail_pointer_for_test(50.0, t1);
    assert!(
        app.rail_autohide_is_visible_for_test(t1),
        "the band carries the armed reveal through the debounce: no need to \
         sit pinned in the thin edge strip"
    );
}

#[test]
fn reveal_arms_and_holds_across_the_live_trace_sequences() {
    // Round-5 regression covering two pointer sequences that were once broken,
    // replayed through the real feed path:
    //   (1) a fast approach that overshoots OFF the window edge (samples jump
    //       30–200px and hop over a static point zone) must still arm+reveal;
    //   (2) a fast in-then-past-the-seam FOLLOW-THROUGH within the confirm window
    //       (the trace's 7.9px→214px in 74ms) must NOT abort the armed reveal -
    //       the motion-aware trigger keeps `in_edge` set so the confirm completes.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    // pad 0, scale 1 → reach = 16 (default reveal_px). Trigger zone [0, 16].
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);

    let t0 = std::time::Instant::now();

    // (1) FAST APPROACH OVERSHOOTING OFF THE WINDOW. A sample at x=200 lands in
    // content (no arm); the next sample jumps clean over [0,16] to x=−8 (past the
    // left edge, where a tiling compositor clamps). The segment 200→−8 sweeps the
    // trigger band → arm.
    app.feed_rail_pointer_for_test(200.0, t0);
    app.feed_rail_pointer_for_test(-8.0, t0);
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "the overshoot arms the confirm, not an instant reveal"
    );
    let armed = t0 + std::time::Duration::from_millis(40); // past the 30ms confirm
    app.feed_rail_pointer_for_test(-8.0, armed);
    assert!(
        app.rail_autohide_is_visible_for_test(armed),
        "a fast approach that overshoots the edge reveals (not only a precise \
         landing inside the thin zone)"
    );

    // Settle back to hidden so the follow-through starts from a clean Hidden.
    let leave = armed + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(400.0, leave); // off the band → hide grace
    let hidden = leave + std::time::Duration::from_millis(700); // > 600ms grace
    app.poll_rail_autohide_for_test(hidden);
    assert!(
        !app.rail_autohide_is_visible_for_test(hidden),
        "precondition: back to Hidden before the follow-through sequence"
    );

    // (2) FAST FOLLOW-THROUGH PAST THE SEAM. A deliberate quick approach lands at
    // the edge (x=8, arms), then ONE fast sample overshoots past the seam
    // (x=214) inside the confirm window. The segment 8→214 still sweeps the
    // trigger band, so `in_edge` stays set and the confirm is NOT aborted - the
    // reveal completes. (Pre-fix, the out-of-band sample aborted the arm and the
    // rail "wouldn't open" on a fast approach.)
    let t2 = hidden + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(8.0, t2);
    assert!(
        !app.rail_autohide_is_visible_for_test(t2),
        "edge contact arms the confirm"
    );
    let follow = t2 + std::time::Duration::from_millis(10); // within the 30ms confirm
    app.feed_rail_pointer_for_test(214.0, follow);
    let done = t2 + std::time::Duration::from_millis(40); // past the confirm
    app.poll_rail_autohide_for_test(done);
    assert!(
        app.rail_autohide_is_visible_for_test(done),
        "a fast follow-through past the seam does not abort the armed reveal"
    );
}

#[test]
fn reveal_yields_to_an_active_scrollbar_drag() {
    // Coexistence: while a scroll-thumb drag is in progress, a pointer
    // even at the very edge reports no reveal contact - the drag wins the edge.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    // Without a drag, the edge triggers reveal.
    assert_eq!(app.reveal_contact_for_test(2.0), Some((true, true)));
    // With a scroll-thumb drag in progress, the same edge yields (no contact).
    app.begin_scrollbar_drag_for_test();
    assert_eq!(
        app.reveal_contact_for_test(2.0),
        Some((false, false)),
        "a scrollbar drag suppresses reveal at the edge"
    );
}

#[test]
fn autohide_seam_resizes_only_while_the_floating_rail_is_revealed() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // floating seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    let (_conf_dir, conf) = temp_rail_conf("autohide-seam-drag");
    app.set_config_path_for_test(conf.clone());

    assert_eq!(
        app.pointer_over_rail_seam_for_test(128.0),
        Some(false),
        "a hidden floating rail exposes no resize seam"
    );
    app.force_rail_reveal_for_test();
    assert_eq!(app.pointer_over_rail_seam_for_test(128.0), Some(true));
    assert_eq!(app.pointer_over_rail_seam_for_test(144.0), Some(false));
    assert_eq!(app.rail_width_from_pointer_for_test(80.0), Some(10));
    app.pointer_move_for_test(128.0, 100.0);
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::ColResize,
        "the revealed floating seam advertises its resize affordance"
    );

    app.set_pointer_px_for_test(128.0, 100.0);
    app.mouse_left_press_for_test();
    assert!(
        app.rail_seam_dragging_for_test(),
        "the revealed floating seam arms a resize drag"
    );
    assert!(app.rail_pinned_open_for_test());

    // Leaving the band and advancing beyond any hide grace cannot dismiss the
    // overlay while the seam gesture owns it.
    let outside = std::time::Instant::now();
    app.feed_rail_pointer_for_test(400.0, outside);
    app.poll_rail_autohide_for_test(outside + std::time::Duration::from_secs(10));
    assert!(app.rail_overlay_visible_for_test());

    app.pointer_move_for_test(80.0, 100.0);
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(10));
    assert!(
        app.rail_overlay_visible_for_test(),
        "an in-flight seam drag holds the floating rail revealed"
    );
    app.mouse_left_release_for_test();
    assert!(!app.rail_seam_dragging_for_test());
    assert!(!app.rail_pinned_open_for_test());

    // The revealed floating seam retains the pinned rail's double-click reset.
    let t0 = std::time::Instant::now();
    app.mouse_left_press_at_for_test(t0);
    app.mouse_left_release_for_test();
    app.mouse_left_press_at_for_test(t0 + std::time::Duration::from_millis(50));
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Auto);
    assert!(!app.rail_seam_dragging_for_test());
}

#[test]
fn autohide_seam_hover_holds_the_floating_rail_revealed() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // floating seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();

    // The content-facing half of the grab band lies just outside the drawn
    // overlay. It must still feed the reveal hold while advertising ColResize.
    let seam_hover_x = 132.0;
    assert_eq!(
        app.pointer_over_rail_seam_for_test(seam_hover_x),
        Some(true)
    );
    let hovered_at = std::time::Instant::now();
    app.feed_rail_pointer_for_test(seam_hover_x, hovered_at);
    app.poll_rail_autohide_for_test(hovered_at + std::time::Duration::from_secs(10));
    assert!(
        app.rail_autohide_is_visible_for_test(hovered_at + std::time::Duration::from_secs(10)),
        "hovering the revealed seam must hold the floating rail open"
    );
}

#[test]
fn top_tab_hover_keeps_the_arrow_while_the_workspace_rail_is_autohidden() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);
    assert!(app.rail_autohide_active_for_test());
    assert!(!app.rail_overlay_visible_for_test());

    app.pointer_move_for_test(12.0, 8.0);
    assert_eq!(app.chrome_hit_band_for_test(), Some("tab"));
    assert!(app.top_tab_hovered_for_test());
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Default,
        "the pinned top strip keeps the arrow while the rail is auto-hidden"
    );
}

#[test]
fn hidden_autohide_rail_columns_do_not_create_a_phantom_chrome_hover() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    app.pointer_move_for_test(12.0, 24.0);
    assert_eq!(app.chrome_hit_band_for_test(), None);
    assert!(!app.top_tab_hovered_for_test());
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Text,
        "hidden rail columns remain terminal content"
    );
}

#[test]
fn pinned_rail_keeps_the_existing_top_tab_hover_route() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(false);

    // The pinned left rail reserves 128 px, so the top strip starts there.
    app.pointer_move_for_test(140.0, 8.0);
    assert_eq!(app.chrome_hit_band_for_test(), Some("tab"));
    assert!(app.top_tab_hovered_for_test());
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Default
    );
}

#[test]
fn revealed_right_autohide_seam_uses_right_edge_width_mapping() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("right");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [672, 800), seam at 672
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();

    assert_eq!(app.pointer_over_rail_seam_for_test(672.0), Some(true));
    assert_eq!(
        app.rail_width_from_pointer_for_test(720.0),
        Some(10),
        "right-side width measures from the pointer to the surface edge"
    );
}

#[test]
fn revealed_band_click_hits_the_rail_hidden_does_not() {
    // While revealed, a press on the overlay band resolves to a rail TabHit (the
    // floating rail owns the pointer); while hidden, the same press falls through
    // (no chrome), so content clicks are never eaten by an invisible rail.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    // Two workspaces give the rail two slots; the active workspace holds a
    // single tab so no top bar competes for the corner. Short names keep the
    // slot geometry deterministic (single label row), matching the labels the
    // pre-workspace rail tabs used.
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    app.set_tab_rail_autohide_for_test(true);

    // Slot 0 body (top-margin row 1, label col 2 → centre (2·8+4, 24)).
    app.set_pointer_px_for_test(2.0 * 8.0 + 4.0, 24.0);
    // Hidden: no tab hit (falls through to content).
    assert!(!app.rail_overlay_visible_for_test());
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "a hidden auto-hide rail eats no clicks"
    );
    // Revealed: the same press is a rail switch hit.
    app.force_rail_reveal_for_test();
    assert!(app.rail_overlay_visible_for_test());
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("switch"),
        "a revealed overlay band press hits the rail"
    );
}

// --- the rail stays revealed under its own menu / rename prompt ---

#[test]
fn autohide_rail_stays_revealed_under_its_own_workspace_menu() {
    // Right-clicking a workspace to open its context menu must NOT
    // hide the rail out from under the menu. Before the fix the draw was gated
    // on `!overlay.is_open()`, so opening the rail's own menu blanked the rail.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    app.set_tab_rail_autohide_for_test(true);

    // Reveal the rail (as a pointer at the edge would), then open the workspace
    // slot's context menu - the rail must remain drawn.
    app.force_rail_reveal_for_test();
    assert!(
        app.rail_overlay_visible_for_test(),
        "precondition: revealed"
    );
    app.open_workspace_rail_menu_for_test(0);
    assert!(
        app.rail_overlay_visible_for_test(),
        "the rail stays revealed under its own context menu"
    );
}

#[test]
fn move_workspace_at_reorders_the_rail_and_keeps_focus() {
    // The App-side reorder (as a context-menu Move Up/Down would
    // fire) swaps adjacent slots and follows the active workspace by identity,
    // so the focused workspace never changes under the user.
    let mut app = tab_bar_app();
    add_workspace(&mut app);
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    app.rename_workspace_for_test(2, "c");
    // add_workspace switches to the newest, so ws2 ("c") is active.
    assert_eq!(app.workspace_names_for_test(), vec!["a", "b", "c"]);
    assert_eq!(app.active_workspace_index_for_test(), 2);

    // Move the active workspace ("c", idx 2) up: it swaps with "b" and the
    // active index follows to 1 -- still "c".
    app.move_workspace_at_for_test(2, true);
    assert_eq!(app.workspace_names_for_test(), vec!["a", "c", "b"]);
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Move a background workspace ("a", idx 0) down past the active: active
    // shifts to 0 so it still points at "c".
    app.move_workspace_at_for_test(0, false);
    assert_eq!(app.workspace_names_for_test(), vec!["c", "a", "b"]);
    assert_eq!(app.active_workspace_index_for_test(), 0);

    // Guard: the first slot cannot move up -- order and focus unchanged.
    app.move_workspace_at_for_test(0, true);
    assert_eq!(app.workspace_names_for_test(), vec!["c", "a", "b"]);
    assert_eq!(app.active_workspace_index_for_test(), 0);
}

#[test]
fn autohide_rail_reveals_for_a_workspace_rename_prompt() {
    // A workspace rename prompt is rail-anchored, so the rail reveals
    // (and holds) for it - even from a hidden state - so the user sees which
    // workspace they are renaming. A tab rename does NOT pin the rail.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    app.set_tab_rail_autohide_for_test(true);

    // Rail hidden (machine at rest); a workspace rename must reveal it.
    assert!(
        !app.rail_overlay_visible_for_test(),
        "precondition: hidden at rest"
    );
    app.enter_rename_workspace_for_test(0);
    assert!(
        app.rename_overlay_open_for_test(),
        "workspace rename prompt is open"
    );
    assert!(
        app.rail_overlay_visible_for_test(),
        "the rail reveals for a workspace rename prompt"
    );
}

#[test]
fn autohide_rail_menu_box_clears_the_floating_rail_band() {
    // A WorkspaceSlot menu opened while the auto-hide rail is
    // revealed keeps the rail visible - the rail composites topmost,
    // so the menu box must land BESIDE the band, never under it, or its edge is
    // occluded. The box's left column must clear the reserved rail band.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();

    // Right-click lands inside the floating rail band (column 1 of the left
    // rail); the menu spawns there but must be shifted clear of the band.
    app.set_pointer_cell_for_test(2, 1);
    app.open_workspace_rail_menu_for_test(0);
    assert!(
        app.context_menu_open_for_test(),
        "the workspace slot menu is open"
    );
    let band = app.rail_overlay_cols_for_test();
    let rect = app.overlay_rect_for_test().expect("menu rect");
    assert!(
        rect.left >= band,
        "menu box left ({}) must clear the {}-col floating rail band",
        rect.left,
        band
    );
}

// --- Rail auto-hide REGRESSION repros (auto-hide breaks mouse routing) ---

#[test]
fn repro_right_click_in_content_opens_menu_under_autohide() {
    // REGRESSION: with auto-hide on and the rail hidden, a right-click in the
    // content area must open the context menu exactly as with auto-hide off.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    // Pointer deep in content, far from the reveal edge; rail hidden.
    app.pointer_move_for_test(400.0, 200.0);
    assert!(
        !app.rail_overlay_visible_for_test(),
        "rail hidden in content"
    );
    app.mouse_right_press_for_test();
    assert!(
        app.context_menu_open_for_test(),
        "right-click in content must open the context menu under auto-hide"
    );
}

#[test]
fn repro_reveal_holds_across_maintenance_polls_no_flicker() {
    // REGRESSION (flicker): once revealed with the pointer parked over the band,
    // the maintenance poll must keep it revealed - it must not oscillate.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let t0 = std::time::Instant::now();
    // Pointer at the edge → arm the reveal, then poll comfortably past the show
    // debounce (SHOW_DEBOUNCE is 80ms; +150ms clears it with margin).
    app.pointer_move_for_test(2.0, 100.0);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(150));
    assert!(
        app.rail_overlay_visible_for_test(),
        "revealed after debounce"
    );

    // Pointer now sits mid-band (x=40, past the edge zone, inside the band).
    app.pointer_move_for_test(40.0, 100.0);
    // Several maintenance polls with the pointer parked must all stay revealed.
    for ms in [200u64, 400, 800, 1600, 3200] {
        app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(ms));
        assert!(
            app.rail_overlay_visible_for_test(),
            "still revealed at +{ms}ms mid-band (no flicker)"
        );
    }
}

#[test]
fn repro_revealed_content_right_click_opens_menu_not_swallowed() {
    // REGRESSION: while the rail is REVEALED (the flicker state), a right-click
    // in CONTENT (past the band seam) must still open the content context menu -
    // the floating overlay must not swallow content-area clicks.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();
    assert!(app.rail_overlay_visible_for_test());

    // Pointer far into content, well past the seam.
    app.pointer_move_for_test(400.0, 200.0);
    app.mouse_right_press_for_test();
    assert!(
        app.context_menu_open_for_test(),
        "content right-click must open the menu even while the rail is revealed"
    );
}

#[test]
fn repro_open_menu_suppresses_the_rail_overlay_no_occlusion() {
    // REGRESSION ("can't right-click Settings"): the revealed rail strip is
    // composited topmost - OVER any open window overlay. So while a context menu
    // (or Settings / palette) is open, the rail must NOT be drawn, or it paints
    // over the menu and hides the items the pointer is clicking. An open overlay
    // owns the screen; the floating rail steps aside until it closes.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    // Rail revealed with no overlay → drawn.
    app.force_rail_reveal_for_test();
    assert!(
        app.rail_overlay_visible_for_test(),
        "revealed rail is drawn when nothing overlays it"
    );

    // Open a context menu (right-click in content) → the rail must step aside so
    // it does not paint over the menu.
    app.pointer_move_for_test(400.0, 200.0);
    app.mouse_right_press_for_test();
    assert!(app.context_menu_open_for_test(), "menu is open");
    assert!(
        !app.rail_overlay_visible_for_test(),
        "an open overlay suppresses the rail so it can't occlude the menu"
    );
}

// --- Rail auto-hide REGRESSION: phantom top-bar leak + idle self-wake ---

#[test]
fn autohide_left_decoration_grows_no_phantom_top_bar() {
    // REGRESSION (top-bar leak): with auto-hide on and placement=left, the
    // single-pane decoration must NOT grow a top bar. The dispatch keys off
    // `rail_side()`, which reads the (deliberately zeroed) auto-hide reservation
    // and reports `None`; without the auto-hide guard an auto-hidden LEFT rail
    // fell through to the top-bar branch and grew a one-row bar across the top
    // (decorated rows = raw rows + 1). The rail draws only as a floating overlay,
    // so the decorated snapshot must equal the raw snapshot - no rows off the
    // top, no columns off the side - in BOTH the hidden and revealed phases.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // A single-tab active workspace means no top bar, so the autohidden
    // rail (a floating overlay) is the ONLY chrome - the decoration must add
    // nothing. A second workspace keeps the rail shown.
    add_workspace(&mut app);
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let raw = app.raw_snapshot_dims_for_test().expect("raw dims");

    // Hidden: no chrome decorated at all.
    assert!(!app.rail_overlay_visible_for_test());
    assert_eq!(
        app.decorated_snapshot_dims_for_test(),
        Some(raw),
        "auto-hidden left rail must not grow a phantom top bar (hidden)"
    );

    // Revealed: still no pinned decoration - the reveal is a separate floating
    // overlay, not a snapshot grow.
    app.force_rail_reveal_for_test();
    assert!(app.rail_overlay_visible_for_test());
    assert_eq!(
        app.decorated_snapshot_dims_for_test(),
        Some(raw),
        "a revealed auto-hide rail must not grow a phantom top bar (revealed)"
    );
}

#[test]
fn autohide_right_decoration_grows_no_phantom_top_bar() {
    // Mirror of the left case for placement=right - the same `rail_side()`
    // predicate leaked a top bar on a right-placed auto-hidden rail too.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("right");
    // A single-tab active workspace means no top bar, so the autohidden
    // rail (a floating overlay) is the ONLY chrome - the decoration must add
    // nothing. A second workspace keeps the rail shown.
    add_workspace(&mut app);
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let raw = app.raw_snapshot_dims_for_test().expect("raw dims");
    assert_eq!(
        app.decorated_snapshot_dims_for_test(),
        Some(raw),
        "auto-hidden right rail must not grow a phantom top bar"
    );
}

#[test]
fn autohide_top_placement_still_decorates_its_bar() {
    // Guard the fix's blast radius: the auto-hide early-return must NOT fire on
    // the top bar (auto-hide never applies there), so a top placement still
    // grows its one-row bar exactly as before.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("top");
    app.set_tab_rail_autohide_for_test(true); // inert on top
    assert!(!app.rail_autohide_active_for_test());

    let raw = app.raw_snapshot_dims_for_test().expect("raw dims");
    let decorated = app
        .decorated_snapshot_dims_for_test()
        .expect("decorated dims");
    assert_eq!(decorated.0, raw.0, "top bar grows rows, not columns");
    assert_eq!(
        decorated.1,
        raw.1 + 1,
        "the top bar still reserves + decorates its one row"
    );
}

#[test]
fn autohide_idle_states_schedule_no_self_wake() {
    // REGRESSION (CPU spin): an idle auto-hidden rail must not schedule a
    // wake - neither steady Hidden nor Revealed-with-the-pointer-parked. A past
    // or immediate wake here would spin the event loop (`WaitUntil(now)` → wake →
    // poll → no change → repeat) at frame rate, pegging a core. Both idle phases
    // must add nothing to `next_wake_deadline`.
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // Force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let t0 = std::time::Instant::now();

    // Idle Hidden (pointer parked deep in content): no wake.
    app.pointer_move_for_test(400.0, 200.0);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(50));
    assert!(!app.rail_overlay_visible_for_test(), "hidden at rest");
    assert!(
        !app.rail_autohide_wants_wake_for_test(t0 + std::time::Duration::from_millis(60)),
        "an idle hidden rail schedules no self-wake"
    );

    // Reveal by parking the pointer at the edge, poll past the debounce, then
    // confirm the settled Revealed state also schedules no wake.
    app.pointer_move_for_test(2.0, 200.0);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(200));
    assert!(
        app.rail_overlay_visible_for_test(),
        "revealed after debounce"
    );
    // Several settled polls with the pointer parked at the edge: still no wake.
    for ms in [400u64, 800, 1600, 3200] {
        let now = t0 + std::time::Duration::from_millis(ms);
        app.run_about_to_wait_maintenance_for_test(now);
        assert!(
            app.rail_overlay_visible_for_test(),
            "still revealed at +{ms}ms (parked at edge)"
        );
        assert!(
            !app.rail_autohide_wants_wake_for_test(now),
            "an idle revealed rail schedules no self-wake at +{ms}ms"
        );
    }
}
