// SPDX-License-Identifier: GPL-3.0-only
//! Session presentation tests: pane geometry, dividers, visibility, viewport
//! and scrollback reconciliation, zoom rendering, and titles.

use super::*;

#[test]
fn nested_high_padding_drag_keeps_collapsed_pty_valid_and_restores() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let middle =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let wide =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(2)));
    assert_eq!(middle, SessionToken(1));
    assert_eq!(wide, SessionToken(2));

    let content = PaneRect::new(0.0, 0.0, 600.0, 240.0);
    let divider = 1.0;
    let cell_w = 10;
    let cell_h = 20;
    let pad = 64.0;

    // Production drag math clamps both the root split and the nested split
    // to 5%. Token 0 then has one divider-facing padded edge; token 1 is
    // sandwiched between two dividers and loses padding on both edges.
    assert_eq!(
        set.drag_active_divider(content, divider, 0, content.x, content.y),
        Some(crate::native::layout::MIN_RATIO)
    );
    let right = layout_rects(set.active_layout().expect("layout"), content, divider)[1].1;
    assert_eq!(
        set.drag_active_divider(content, divider, 1, right.x, right.y),
        Some(crate::native::layout::MIN_RATIO)
    );

    set.reflow_all_panes_for_drag(content, cell_w, cell_h, divider, pad);
    for token in [SessionToken(0), SessionToken(1)] {
        let dims = set
            .get(token)
            .expect("pane")
            .terminal
            .lock()
            .expect("terminal")
            .screen()
            .dimensions();
        assert_eq!(dims.columns, 1, "collapsed backing model stays valid");
        assert_eq!(
            set.get(token)
                .and_then(Session::headless_session)
                .expect("headless")
                .resize_call_count(),
            0,
            "live drag never resizes the PTY"
        );
    }

    set.resize_all_panes(content, cell_w, cell_h, divider, pad);
    for token in [SessionToken(0), SessionToken(1)] {
        let headless = set
            .get(token)
            .and_then(Session::headless_session)
            .expect("headless");
        assert_eq!(headless.dimensions().columns, 1);
        assert_eq!(headless.resize_call_count(), 1);
    }

    // Drag both dividers back to roomy positions. The same production
    // reflow restores real multi-cell grids without a stale collapsed size.
    assert!(
        set.drag_active_divider(content, divider, 0, content.x + 200.0, content.y)
            .is_some()
    );
    assert!(
        set.drag_active_divider(content, divider, 1, content.x + 400.0, content.y)
            .is_some()
    );
    set.reflow_all_panes_for_drag(content, cell_w, cell_h, divider, pad);
    for token in [SessionToken(0), SessionToken(1), SessionToken(2)] {
        let dims = set
            .get(token)
            .expect("pane")
            .terminal
            .lock()
            .expect("terminal")
            .screen()
            .dimensions();
        assert!(dims.columns > 1, "pane {token:?} restores after expansion");
    }
}

#[test]
fn scrollback_front_trim_clears_absolute_coordinate_state() {
    let mut set = WorkspaceSet::new(build_session(), None);
    {
        let mut terminal = set.active().terminal.lock().expect("terminal lock");
        terminal.set_scrollback_limit(2);
        terminal.advance(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\nj\r\n");
    }
    // Reconcile output that arrived before the selection was made.
    set.reconcile_scrollback_trims();
    set.active_mut().selection.set_range(test_selection());
    assert!(set.active().selection.range().is_some());

    set.active()
        .terminal
        .lock()
        .expect("terminal lock")
        .advance(b"k\r\nl\r\nm\r\n");
    set.reconcile_scrollback_trims();

    assert!(
        set.active().selection.range().is_none(),
        "front eviction must clear rather than silently retarget a selection"
    );
}

#[test]
fn session_title_defaults_to_odytty() {
    let session = build_session();
    assert_eq!(session.tab_title, "odytty");
}

#[test]
fn zoomed_tab_renders_only_the_focused_leaf_full_content() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let content = PaneRect::new(5.0, 7.0, 401.0, 200.0);
    // Un-zoomed: two tiled rects.
    assert_eq!(set.active_pane_rects(content, 1.0, (8, 16)).len(), 2);

    assert!(set.toggle_active_zoom());
    let rects = set.active_pane_rects(content, 1.0, (8, 16));
    assert_eq!(rects.len(), 1, "only the focused pane renders while zoomed");
    let (token, rect) = rects[0];
    assert_eq!(token, right, "the focused pane is the one shown");
    // It spans the whole content rect.
    assert!((rect.x - content.x).abs() < f32::EPSILON);
    assert!((rect.y - content.y).abs() < f32::EPSILON);
    assert!((rect.w - content.w).abs() < f32::EPSILON);
    assert!((rect.h - content.h).abs() < f32::EPSILON);
}

#[test]
fn unzoom_restores_the_prior_pane_rects_exactly() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let content = PaneRect::new(5.0, 7.0, 401.0, 200.0);
    let before = set.active_pane_rects(content, 1.0, (8, 16));

    assert!(set.toggle_active_zoom());
    assert!(set.toggle_active_zoom());
    let after = set.active_pane_rects(content, 1.0, (8, 16));

    assert_eq!(before.len(), after.len());
    for ((tb, rb), (ta, ra)) in before.iter().zip(after.iter()) {
        assert_eq!(tb, ta);
        assert!((rb.x - ra.x).abs() < f32::EPSILON);
        assert!((rb.y - ra.y).abs() < f32::EPSILON);
        assert!((rb.w - ra.w).abs() < f32::EPSILON);
        assert!((rb.h - ra.h).abs() < f32::EPSILON);
    }
}

#[test]
fn resize_sizes_the_zoomed_focused_pane_to_full_content() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
    assert!(set.toggle_active_zoom());
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);
    // The focused (zoomed) pane fills the whole content → 80 cols, 20 rows.
    assert_eq!(pane_dims(&set, right), (80, 20));
    // The background pane keeps its split sub-rect (40 cols) so un-zoom is
    // instantly correct.
    assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));
}

#[test]
fn zoom_resize_invalidates_only_the_pane_that_reflows() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let focused =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);
    set.get_mut(SessionToken(0))
        .expect("background pane")
        .selection
        .set_range(test_selection());
    set.get_mut(focused)
        .expect("focused pane")
        .selection
        .set_range(test_selection());

    assert!(set.toggle_active_zoom());
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);

    assert!(
        set.get(focused)
            .expect("focused pane")
            .selection
            .range()
            .is_none(),
        "zoom reflow clears the focused pane's stale selection"
    );
    assert!(
        set.get(SessionToken(0))
            .expect("unchanged background pane")
            .selection
            .range()
            .is_some(),
        "a pane whose grid dimensions did not change keeps its selection"
    );
}

#[test]
fn zoom_hides_background_panes_from_visibility() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    assert!(set.toggle_active_zoom());
    // While zoomed only the focused pane is visible (drives redraw).
    assert!(set.is_visible_pane(right));
    assert!(!set.is_visible_pane(SessionToken(0)));
    // Un-zoom restores both as visible.
    assert!(set.toggle_active_zoom());
    assert!(set.is_visible_pane(right));
    assert!(set.is_visible_pane(SessionToken(0)));
}

#[test]
fn is_visible_pane_covers_every_pane_of_the_active_tab_only() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let sibling =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // Both panes of the active tab are visible (focused + background).
    assert!(set.is_visible_pane(SessionToken(0)));
    assert!(set.is_visible_pane(sibling));
    // A pane that does not exist is never visible.
    assert!(!set.is_visible_pane(SessionToken(99)));

    // Open a second tab; its pane is not visible while tab 0 is active.
    let other_tab = SessionToken(2);
    set.push(build_session_with_id(other_tab));
    assert!(!set.is_visible_pane(other_tab));
    assert!(set.is_visible_pane(SessionToken(0)));
}

#[test]
fn visible_pane_rebuild_flag_helpers_span_the_whole_active_tab() {
    // NF21-7: the render gate ORs `needs_rebuild` across every visible pane
    // of the active tab (not just the focused one), and the multi-pane
    // rebuild clears every visible pane's flag. A dirtied NON-focused split
    // pane must therefore be both SEEN by the OR and CLEARED by the sweep —
    // otherwise its output freezes (gate never opens) or storms (flag never
    // clears).
    let mut set = WorkspaceSet::new(build_session(), None);
    let sibling =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // A second, non-visible tab whose flag must be ignored by both helpers.
    let other_tab = SessionToken(2);
    set.push(build_session_with_id(other_tab));

    set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = false;
    set.get_mut(sibling).expect("sibling").needs_rebuild = false;
    set.get_mut(other_tab).expect("other tab").needs_rebuild = false;
    assert!(
        !set.any_visible_pane_needs_rebuild(),
        "no visible pane dirty → gate stays closed"
    );

    // Output into the NON-focused visible pane (focus is on `sibling`).
    set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = true;
    assert!(
        set.any_visible_pane_needs_rebuild(),
        "a dirtied non-focused visible pane opens the tab-wide gate"
    );

    // A dirty pane on an INACTIVE tab must not open the active tab's gate.
    set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = false;
    set.get_mut(other_tab).expect("other tab").needs_rebuild = true;
    assert!(
        !set.any_visible_pane_needs_rebuild(),
        "an off-tab pane's flag is not a visible-pane rebuild"
    );

    // The sweep clears every visible pane, leaving the off-tab flag alone.
    set.get_mut(SessionToken(0)).expect("pane 0").needs_rebuild = true;
    set.get_mut(sibling).expect("sibling").needs_rebuild = true;
    set.clear_visible_pane_rebuild_flags();
    assert!(!set.get(SessionToken(0)).expect("pane 0").needs_rebuild);
    assert!(!set.get(sibling).expect("sibling").needs_rebuild);
    assert!(
        set.get(other_tab).expect("other tab").needs_rebuild,
        "the sweep clears only the active tab's visible panes"
    );
}

#[test]
fn active_pane_rects_tiles_the_content_without_overlap() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let content = PaneRect::new(5.0, 7.0, 401.0, 200.0);
    let rects = set.active_pane_rects(content, 1.0, (8, 16));
    assert_eq!(rects.len(), 2);
    let (_, left) = rects[0];
    let (_, right) = rects[1];
    // Left + divider + right spans exactly the content width; no overlap.
    assert!((left.x - content.x).abs() < f32::EPSILON);
    assert!((right.x - (left.x + left.w + 1.0)).abs() < f32::EPSILON);
    assert!(((right.x + right.w) - (content.x + content.w)).abs() < f32::EPSILON);
}

#[test]
fn active_pane_at_point_resolves_focus_follows_click() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // 801px wide, 1px divider at x=400 → left pane [0,400), right [401,801).
    let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
    assert_eq!(
        set.active_pane_at_point(content, 1.0, (8, 16), 100.0, 50.0),
        Some(SessionToken(0))
    );
    assert_eq!(
        set.active_pane_at_point(content, 1.0, (8, 16), 600.0, 50.0),
        Some(right)
    );
    // The 1px divider gap (x=400) belongs to no pane.
    assert_eq!(
        set.active_pane_at_point(content, 1.0, (8, 16), 400.0, 50.0),
        None
    );
}

#[test]
fn active_divider_at_point_grabs_only_near_the_divider() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
    // Within the 6px grab band of the x=400 divider → index 0.
    assert_eq!(
        set.active_divider_at_point(content, 1.0, 402.0, 50.0, 6.0),
        Some(0)
    );
    // Far from the divider → no grab.
    assert_eq!(
        set.active_divider_at_point(content, 1.0, 100.0, 50.0, 6.0),
        None
    );
    // A single-pane active tab has no dividers to grab.
    let single = WorkspaceSet::new(build_session(), None);
    assert_eq!(
        single.active_divider_at_point(content, 1.0, 402.0, 50.0, 6.0),
        None
    );
}

#[test]
fn drag_active_divider_reflows_the_active_split_ratio() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
    // Drag the column divider to x=200 → ratio ≈ 200/800.
    let new = set
        .drag_active_divider(content, 1.0, 0, 200.0, 50.0)
        .expect("active split exists");
    assert!((new - 200.0 / 800.0).abs() < 1e-3);
    // The new ratio re-tiles the panes: left pane now ~200px wide.
    let rects = set.active_pane_rects(content, 1.0, (8, 16));
    let (_, left) = rects[0];
    assert!((left.w - 200.0).abs() < 1.0);
    // An out-of-range divider index leaves the tree unchanged.
    assert_eq!(set.drag_active_divider(content, 1.0, 9, 50.0, 50.0), None);
}

#[test]
fn focus_move_active_lands_on_the_spatial_neighbor() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // After the split focus is on the right pane.
    assert_eq!(set.active_id(), right);
    let content = PaneRect::new(0.0, 0.0, 801.0, 200.0);
    // Move focus left → the original pane; returns true (focus changed).
    assert!(set.focus_move_active(content, 1.0, (8, 16), FocusDir::Left));
    assert_eq!(set.active_id(), SessionToken(0));
    // No neighbor to the left of the leftmost pane → no change, false.
    assert!(!set.focus_move_active(content, 1.0, (8, 16), FocusDir::Left));
    assert_eq!(set.active_id(), SessionToken(0));
    // Move right → back to the right pane.
    assert!(set.focus_move_active(content, 1.0, (8, 16), FocusDir::Right));
    assert_eq!(set.active_id(), right);
}

/// Input latches are swept on every session of every workspace (the outgoing,
/// the incoming and the background ones); selection and viewport are not
/// latches and survive.
#[test]
fn clearing_input_latches_sweeps_every_workspace_and_keeps_selection_and_viewport() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    for token in [SessionToken(0), SessionToken(1)] {
        let session = set.sessions.get_mut(&token).expect("session");
        session.pointer_cell = Some(crate::selection::CellPoint { row: 1, column: 2 });
        session.pointer_px = Some((3.0, 4.0));
        session.report_button = Some(crate::core::MouseButton::Left);
        session.selection.set_range(test_selection());
        session.viewport.scroll_up(3, 10);
    }
    set.clear_all_input_latches();
    for token in [SessionToken(0), SessionToken(1)] {
        let session = set.get(token).expect("session");
        assert!(session.pointer_cell.is_none(), "{token:?}");
        assert!(session.pointer_px.is_none(), "{token:?}");
        assert!(session.report_button.is_none(), "{token:?}");
        assert!(session.selection.range().is_some(), "{token:?} selection");
        assert_eq!(session.viewport.offset(), 3, "{token:?} viewport");
    }
}

/// A reflow restarts the viewport, fades, overlays and absolute coordinates of
/// every session, including a background workspace's.
#[test]
fn layout_invalidation_resets_viewport_and_coordinates_for_background_sessions() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    for token in [SessionToken(0), SessionToken(1)] {
        let session = set.sessions.get_mut(&token).expect("session");
        session.selection.set_range(test_selection());
        session.viewport.scroll_up(4, 10);
        session.hovered_url = Some("https://example.invalid/".to_owned());
        session.search_restore_viewport = Some(2);
        session.search.open();
    }
    for session in set.sessions.values_mut() {
        session.invalidate_layout_dependent_state();
    }
    for token in [SessionToken(0), SessionToken(1)] {
        let session = set.get(token).expect("session");
        assert!(session.selection.range().is_none(), "{token:?}");
        assert!(session.hovered_url.is_none(), "{token:?}");
        assert_eq!(session.viewport.offset(), 0, "{token:?}");
        assert!(session.search_restore_viewport.is_none(), "{token:?}");
        assert!(!session.search.is_open(), "{token:?} search closed");
    }
}

/// Only the focused pane of the active tab keeps its cursor timers: a visible
/// split sibling parks the cursor timers only, and a pane of a background
/// workspace parks every timer.
#[test]
fn background_timer_parking_follows_who_can_be_seen() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // Workspace 0 stays active; workspace 1 is the background one.
    set.push_workspace(build_session_with_id(SessionToken(2)));
    let focused = set.active_focused_token();
    let sibling = if focused == SessionToken(0) {
        SessionToken(1)
    } else {
        SessionToken(0)
    };
    for session in set.sessions.values_mut() {
        session.cursor_ease_deadline = Some(deadline);
        session.cursor_slide_deadline = Some(deadline);
    }
    set.park_background_timers();
    let ease = |token: SessionToken| set.get(token).expect("session").cursor_ease_deadline;
    assert_eq!(
        ease(focused),
        Some(deadline),
        "the focused pane keeps its timers"
    );
    assert_eq!(
        ease(sibling),
        None,
        "a visible sibling parks its cursor timers"
    );
    assert_eq!(
        ease(SessionToken(2)),
        None,
        "a background workspace parks everything"
    );
}

fn ring(set: &WorkspaceSet, token: SessionToken) {
    set.get(token)
        .expect("session")
        .terminal
        .lock()
        .expect("terminal")
        .advance(b"\x07");
}

/// Bells route by where the ringing pane is: the focused one drives the flash,
/// any other raises background urgency, and only a pane outside the visible
/// tab latches its tab's activity. Viewing a tab clears its own latch.
#[test]
fn bells_route_to_the_focused_flash_or_a_background_tab_latch() {
    let now = std::time::Instant::now();
    let mut set = WorkspaceSet::new(build_session(), None);
    // Workspace 0: tab 0 (pane 0, focused) and tab 1 (pane 1); workspace 1: pane 2.
    let second = set.push_arena_only(build_session_with_id(SessionToken(1)));
    set.workspaces[0].tabs.push(Tab::single(second));
    set.push_workspace(build_session_with_id(SessionToken(2)));
    assert_eq!(set.active_focused_token(), SessionToken(0));

    ring(&set, SessionToken(0));
    let sweep = set.drain_bells(false, now);
    assert!(sweep.focused_bell);
    assert!(!sweep.background_bell);
    assert!(!set.tab_activity(0, 1) && !set.tab_activity(1, 0));

    ring(&set, SessionToken(1));
    ring(&set, SessionToken(2));
    let sweep = set.drain_bells(false, now);
    assert!(!sweep.focused_bell);
    assert!(sweep.background_bell);
    assert!(set.tab_activity(0, 1), "a background tab latches");
    assert!(
        set.tab_activity(1, 0),
        "a background workspace's tab latches"
    );
    assert!(!set.tab_activity(0, 0), "the viewed tab never latches");

    // The latch survives a quiet pass and clears once the tab is viewed.
    assert_eq!(
        set.drain_bells(false, now),
        crate::native::session::presentation::BellSweep::default()
    );
    assert!(set.tab_activity(0, 1));
    assert!(set.switch(SessionToken(1)));
    set.drain_bells(false, now);
    assert!(!set.tab_activity(0, 1), "viewing the tab clears its latch");
}

/// A bell in a non-focused pane of the tab being viewed is "seen": it raises
/// background urgency but latches no tab.
#[test]
fn a_bell_in_a_visible_sibling_pane_latches_nothing() {
    let mut set = WorkspaceSet::new(build_session(), None);
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    let focused = set.active_focused_token();
    let sibling = if focused == SessionToken(0) {
        SessionToken(1)
    } else {
        SessionToken(0)
    };
    ring(&set, sibling);
    let sweep = set.drain_bells(false, std::time::Instant::now());
    assert!(!sweep.focused_bell);
    assert!(sweep.background_bell);
    assert!(!set.tab_activity(0, 0));
}

/// Notifications are attributed to their pane: one from a background workspace
/// asks for background attention, one from the visible focused pane with the
/// window focused does not, and a disabled setting drops both.
#[test]
fn notifications_are_attributed_to_their_pane_and_respect_the_setting() {
    let now = std::time::Instant::now();
    let mut set = WorkspaceSet::new(build_session(), None);
    set.push_workspace(build_session_with_id(SessionToken(1)));
    let notify = |set: &WorkspaceSet, token: SessionToken, text: &[u8]| {
        set.get(token)
            .expect("session")
            .terminal
            .lock()
            .expect("terminal")
            .advance(text);
    };

    notify(&set, SessionToken(1), b"\x1b]9;from the background\x07");
    let sweep = set.drain_notifications(now, true, true);
    assert!(sweep.changed);
    assert!(sweep.background_request, "a hidden pane asks for attention");

    notify(&set, SessionToken(0), b"\x1b]9;from the focused pane\x07");
    let sweep = set.drain_notifications(now, true, true);
    assert!(sweep.changed);
    assert!(!sweep.background_request, "a focused visible pane does not");
    // The notice never echoes the program-supplied text.
    assert_eq!(
        sweep.focused_notice.as_deref(),
        Some("Terminal requested attention.")
    );

    notify(&set, SessionToken(0), b"\x1b]9;dropped\x07");
    let sweep = set.drain_notifications(now, true, false);
    assert!(!sweep.background_request);
    assert_eq!(
        sweep.focused_notice, None,
        "a disabled setting shows nothing"
    );
    // The sidecar was drained, so enabling later does not replay it.
    assert_eq!(
        set.drain_notifications(now, true, true).focused_notice,
        None
    );
}

/// Rebasing the scroll baseline of the active tab's panes makes background
/// growth count as already seen, skips a pane on the alternate screen, and
/// leaves other tabs and workspaces alone.
#[test]
fn active_tab_scroll_baselines_catch_up_and_leave_other_panes_alone() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let other = set.push_arena_only(build_session_with_id(SessionToken(1)));
    set.workspaces[0].tabs.push(Tab::single(other));
    let mut lines = Vec::new();
    for row in 0..60 {
        lines.extend_from_slice(format!("line {row}\r\n").as_bytes());
    }
    for token in [SessionToken(0), SessionToken(1)] {
        set.get(token)
            .expect("session")
            .terminal
            .lock()
            .expect("terminal")
            .advance(&lines);
    }
    let len = |set: &WorkspaceSet, token: SessionToken| {
        set.get(token).expect("session").last_scrollback_len
    };
    assert_eq!(len(&set, SessionToken(0)), 0);
    set.reconcile_active_tab_scroll_baselines();
    let history = set
        .get(SessionToken(0))
        .expect("session")
        .terminal
        .lock()
        .expect("terminal")
        .screen()
        .primary_scrollback_metrics()
        .0;
    assert!(history > 0, "the fixture produced history");
    assert_eq!(len(&set, SessionToken(0)), history);
    assert_eq!(len(&set, SessionToken(1)), 0, "another tab is untouched");

    // A pane on the alternate screen keeps its primary baseline.
    set.get(SessionToken(0))
        .expect("session")
        .terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b[?1049h");
    set.get_mut(SessionToken(0))
        .expect("session")
        .last_scrollback_len = 1;
    set.reconcile_active_tab_scroll_baselines();
    assert_eq!(len(&set, SessionToken(0)), 1, "alternate screen is skipped");
}
