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
    assert_eq!(set.active_pane_rects(content, 1.0).len(), 2);

    assert!(set.toggle_active_zoom());
    let rects = set.active_pane_rects(content, 1.0);
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
    let before = set.active_pane_rects(content, 1.0);

    assert!(set.toggle_active_zoom());
    assert!(set.toggle_active_zoom());
    let after = set.active_pane_rects(content, 1.0);

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
    let rects = set.active_pane_rects(content, 1.0);
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
        set.active_pane_at_point(content, 1.0, 100.0, 50.0),
        Some(SessionToken(0))
    );
    assert_eq!(
        set.active_pane_at_point(content, 1.0, 600.0, 50.0),
        Some(right)
    );
    // The 1px divider gap (x=400) belongs to no pane.
    assert_eq!(set.active_pane_at_point(content, 1.0, 400.0, 50.0), None);
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
    let rects = set.active_pane_rects(content, 1.0);
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
    assert!(set.focus_move_active(content, 1.0, FocusDir::Left));
    assert_eq!(set.active_id(), SessionToken(0));
    // No neighbor to the left of the leftmost pane → no change, false.
    assert!(!set.focus_move_active(content, 1.0, FocusDir::Left));
    assert_eq!(set.active_id(), SessionToken(0));
    // Move right → back to the right pane.
    assert!(set.focus_move_active(content, 1.0, FocusDir::Right));
    assert_eq!(set.active_id(), right);
}
