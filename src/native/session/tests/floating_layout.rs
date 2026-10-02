// SPDX-License-Identifier: GPL-3.0-only
//! Stacked and floating tab arrangements over the session model: default
//! tiled, geometry, focus and z-order, move and resize limits, clamping,
//! pane-set changes while arranged, the saved shape, and cross-window moves.
//! Headless: no GPU, no PTY.

use super::*;
use crate::native::float_layout::{Arrangement, CellRect, FloatEntry, FloatLayout};
use crate::native::persistence::{ArrangementShape, FloatPaneShape, ShapeSnapshot};
use crate::native::session::reparent::MoveScope;
use crate::native::session::{ArrangeOutcome, FloatStep};

const CELL: (u32, u32) = (10, 20);
const DIVIDER: f32 = 1.0;

fn content() -> PaneRect {
    PaneRect::new(0.0, 0.0, 800.0, 400.0)
}

/// A set with `n` panes in one tab (tokens 0..n), the last split focused.
fn tab_of(n: u64) -> WorkspaceSet {
    let mut set = WorkspaceSet::new(build_session(), None);
    for token in 1..n {
        set.split_active_for_test(
            SplitAxis::Columns,
            build_session_with_id(SessionToken(token)),
        );
    }
    set
}

fn rects(set: &WorkspaceSet) -> Vec<(SessionToken, PaneRect)> {
    set.active_pane_rects(content(), DIVIDER, CELL)
}

fn float(set: &mut WorkspaceSet) {
    assert_eq!(
        set.set_active_floating(content(), DIVIDER, CELL),
        ArrangeOutcome::Changed
    );
}

fn dims(set: &WorkspaceSet, token: u64) -> (usize, usize) {
    let d = set
        .get(SessionToken(token))
        .expect("pane")
        .terminal
        .lock()
        .expect("terminal")
        .screen()
        .dimensions();
    (d.columns, d.rows)
}

#[test]
fn a_tab_is_tiled_by_default_and_a_single_pane_refuses_a_layout() {
    let mut set = WorkspaceSet::new(build_session(), None);
    assert!(set.active_arrangement_is_tiled());
    assert!(!set.active_is_floating());
    assert_eq!(set.set_active_stacked(), ArrangeOutcome::NeedsSplit);
    assert_eq!(
        set.set_active_floating(content(), DIVIDER, CELL),
        ArrangeOutcome::NeedsSplit
    );
    assert_eq!(set.set_active_tiled(), ArrangeOutcome::Unchanged);
    assert!(set.active_arrangement_is_tiled());
}

#[test]
fn stacked_shows_only_the_focused_pane_and_cycling_moves_the_front() {
    let mut set = tab_of(3);
    assert_eq!(set.set_active_stacked(), ArrangeOutcome::Changed);
    assert_eq!(set.set_active_stacked(), ArrangeOutcome::Unchanged);
    assert!(set.active_shows_only_focused());
    let shown = rects(&set);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].0, SessionToken(2));
    assert_eq!(shown[0].1, content());
    assert_eq!(set.active_visible_tokens(), vec![SessionToken(2)]);
    assert!(!set.is_visible_pane(SessionToken(0)));

    // Cycling follows the stable tree order and wraps; every pane stays alive.
    assert!(set.focus_next_pane());
    assert_eq!(rects(&set)[0].0, SessionToken(0));
    assert!(set.focus_next_pane());
    assert_eq!(rects(&set)[0].0, SessionToken(1));
    assert_eq!(set.len(), 3, "hidden panes keep their sessions");
    let order: Vec<_> = set
        .active_pane_order()
        .into_iter()
        .map(|(t, _)| t.0)
        .collect();
    assert_eq!(
        order,
        vec![0, 1, 2],
        "the accessible order is the tree order"
    );
}

#[test]
fn stacked_sizes_the_front_pane_to_the_content_and_leaves_the_rest() {
    let mut set = tab_of(2);
    set.resize_all_panes(content(), CELL.0, CELL.1, DIVIDER, 0.0);
    let tiled_other = dims(&set, 0);
    assert_eq!(set.set_active_stacked(), ArrangeOutcome::Changed);
    set.resize_all_panes(content(), CELL.0, CELL.1, DIVIDER, 0.0);
    assert_eq!(dims(&set, 1), (80, 20), "the front pane fills the content");
    assert_eq!(dims(&set, 0), tiled_other, "hidden panes keep their size");
}

#[test]
fn tiling_again_restores_the_exact_tiled_geometry() {
    let mut set = tab_of(3);
    let before = rects(&set);
    float(&mut set);
    assert!(set.set_active_stacked() == ArrangeOutcome::Changed);
    assert_eq!(set.set_active_tiled(), ArrangeOutcome::Changed);
    assert_eq!(rects(&set), before);
    assert_eq!(set.set_active_tiled(), ArrangeOutcome::Unchanged);
}

#[test]
fn floating_starts_where_the_tiles_were_without_overlap() {
    let mut set = tab_of(2);
    let tiled = rects(&set);
    float(&mut set);
    assert!(set.active_is_floating());
    let floating = rects(&set);
    assert_eq!(floating.len(), 2);
    for (token, rect) in &floating {
        let (_, tile) = tiled.iter().find(|(t, _)| t == token).expect("pane");
        assert!(
            (rect.x - tile.x).abs() <= CELL.0 as f32,
            "{rect:?} {tile:?}"
        );
        assert!((rect.w - tile.w).abs() <= CELL.0 as f32 + 1.0);
        assert!((rect.h - tile.h).abs() <= CELL.1 as f32);
        assert_eq!(rect.x % CELL.0 as f32, 0.0, "whole-cell origin");
        assert_eq!(rect.w % CELL.0 as f32, 0.0, "whole-cell width");
    }
    let a = floating[0].1;
    let b = floating[1].1;
    assert!(
        a.x + a.w <= b.x + 0.01 || b.x + b.w <= a.x + 0.01,
        "no overlap"
    );
}

#[test]
fn the_focused_pane_paints_last_and_the_topmost_pane_wins_a_click() {
    let mut set = tab_of(2);
    float(&mut set);
    // Grow the right-hand (focused) pane over the left one to force overlap.
    for _ in 0..30 {
        set.step_active_floating(
            FloatStep::Move {
                d_col: -1,
                d_row: 0,
            },
            content(),
            CELL,
        );
    }
    assert_eq!(rects(&set).last().expect("top").0, SessionToken(1));
    let overlap_x = 15.0 * CELL.0 as f32;
    assert_eq!(
        set.active_pane_at_point(content(), DIVIDER, CELL, overlap_x, 10.0),
        Some(SessionToken(1)),
        "the pane on top takes the click"
    );
    // Focusing the other pane raises it above the first.
    assert!(set.set_active_focus(SessionToken(0)));
    assert_eq!(rects(&set).last().expect("top").0, SessionToken(0));
    assert_eq!(
        set.active_pane_at_point(content(), DIVIDER, CELL, overlap_x, 10.0),
        Some(SessionToken(0))
    );
    // Cycling focus raises too, and visits every pane.
    assert!(set.focus_next_pane());
    assert_eq!(rects(&set).last().expect("top").0, SessionToken(1));
}

#[test]
fn keyboard_steps_move_and_resize_within_the_minimum_and_the_grid() {
    let mut set = tab_of(2);
    float(&mut set);
    let grid = (80usize, 20usize);
    let focused_rect = |set: &WorkspaceSet| rects(set).last().expect("top").1;

    for _ in 0..200 {
        set.step_active_floating(
            FloatStep::Resize {
                d_cols: -1,
                d_rows: -1,
            },
            content(),
            CELL,
        );
    }
    let small = focused_rect(&set);
    assert_eq!(
        (small.w / CELL.0 as f32, small.h / CELL.1 as f32),
        (8.0, 2.0),
        "shrinking stops at the minimum size"
    );
    for _ in 0..200 {
        set.step_active_floating(FloatStep::Move { d_col: 1, d_row: 1 }, content(), CELL);
    }
    let moved = focused_rect(&set);
    assert_eq!(moved.x + moved.w, grid.0 as f32 * CELL.0 as f32);
    assert_eq!(moved.y + moved.h, grid.1 as f32 * CELL.1 as f32);
    assert!(
        !set.step_active_floating(FloatStep::Move { d_col: 1, d_row: 1 }, content(), CELL),
        "a move into the wall changes nothing"
    );
    for _ in 0..200 {
        set.step_active_floating(
            FloatStep::Resize {
                d_cols: 1,
                d_rows: 1,
            },
            content(),
            CELL,
        );
    }
    let big = focused_rect(&set);
    assert_eq!(
        (big.w, big.h),
        (grid.0 as f32 * CELL.0 as f32, grid.1 as f32 * CELL.1 as f32)
    );
}

#[test]
fn steps_do_nothing_on_a_tiled_or_stacked_tab() {
    let mut set = tab_of(2);
    let step = FloatStep::Move { d_col: 1, d_row: 0 };
    assert!(!set.step_active_floating(step, content(), CELL));
    set.set_active_stacked();
    assert!(!set.step_active_floating(step, content(), CELL));
}

#[test]
fn a_smaller_window_clamps_every_pane_and_deletes_none() {
    let mut set = tab_of(3);
    float(&mut set);
    let small = PaneRect::new(0.0, 0.0, 200.0, 60.0);
    let shrunk = set.active_pane_rects(small, DIVIDER, CELL);
    assert_eq!(shrunk.len(), 3, "no pane vanishes");
    for (_, rect) in shrunk {
        assert!(rect.x >= 0.0 && rect.y >= 0.0);
        assert!(rect.x + rect.w <= small.w + 0.01 && rect.y + rect.h <= small.h + 0.01);
        assert!(rect.w >= 8.0 * CELL.0 as f32 - 0.01 || rect.w == small.w);
    }
    // Growing it back resolves the stored rectangles again, unchanged.
    let again = set.active_pane_rects(content(), DIVIDER, CELL);
    assert_eq!(again.len(), 3);
}

#[test]
fn floating_panes_are_sized_to_their_own_rectangles() {
    let mut set = tab_of(2);
    float(&mut set);
    set.resize_all_panes(content(), CELL.0, CELL.1, DIVIDER, 0.0);
    for (token, rect) in rects(&set) {
        assert_eq!(
            dims(&set, token.0),
            (
                (rect.w / CELL.0 as f32) as usize,
                (rect.h / CELL.1 as f32) as usize
            ),
            "pane {token:?} matches its rectangle"
        );
    }
}

#[test]
fn a_pane_added_or_closed_while_floating_joins_or_leaves_the_layout() {
    let mut set = tab_of(2);
    float(&mut set);
    let added = set.split_active_for_test(SplitAxis::Rows, build_session_with_id(SessionToken(9)));
    assert_eq!(added, SessionToken(9));
    let shown = rects(&set);
    assert_eq!(shown.len(), 3, "the new pane is a floating pane too");
    assert_eq!(shown.last().expect("top").0, SessionToken(9), "in front");
    for (_, rect) in &shown {
        assert!(rect.w >= 8.0 * CELL.0 as f32 && rect.h >= 2.0 * CELL.1 as f32);
    }
    assert!(!set.close(SessionToken(9)), "other panes remain");
    assert_eq!(rects(&set).len(), 2, "a closed pane leaves the layout");
}

#[test]
fn closing_down_to_one_pane_renders_as_a_plain_single_pane() {
    let mut set = tab_of(2);
    float(&mut set);
    set.close(SessionToken(1));
    assert!(set.active_is_single_pane());
    assert!(!set.active_is_floating(), "one pane is never floating");
    assert_eq!(rects(&set).len(), 1);
}

#[test]
fn the_shape_round_trips_the_mode_the_rectangles_and_the_z_order() {
    let mut set = tab_of(3);
    float(&mut set);
    set.set_active_focus(SessionToken(0));
    set.step_active_floating(FloatStep::Move { d_col: 3, d_row: 2 }, content(), CELL);
    let before = rects(&set);
    let shape = set.capture_shape();
    let text = shape.to_json_pretty();
    assert!(text.contains("\"arrangement\""));
    assert!(text.contains("\"floating\""));
    let loaded = ShapeSnapshot::from_json_str(&text).expect("parse");
    assert_eq!(loaded, shape);

    let mut restored = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    restored.restore_from_snapshot_with(
        &loaded,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(restored.active_is_floating());
    let after = restored.active_pane_rects(content(), DIVIDER, CELL);
    // Token numbers differ after a restore, so compare by position: the same
    // rectangles in the same paint order.
    let strip = |v: &[(SessionToken, PaneRect)]| v.iter().map(|(_, r)| *r).collect::<Vec<_>>();
    assert_eq!(strip(&after), strip(&before));
}

#[test]
fn a_stacked_tab_restores_stacked_with_the_same_front_pane() {
    let mut set = tab_of(3);
    set.set_active_stacked();
    set.focus_next_pane();
    let shape = set.capture_shape();
    assert_eq!(
        shape.workspaces[0].tabs[0].arrangement,
        ArrangementShape::Stacked
    );
    let text = shape.to_json_pretty();
    let loaded = ShapeSnapshot::from_json_str(&text).expect("parse");
    let mut restored = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    restored.restore_from_snapshot_with(
        &loaded,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert!(restored.active_shows_only_focused());
    let order: Vec<bool> = restored
        .active_pane_order()
        .into_iter()
        .map(|(_, focused)| focused)
        .collect();
    assert_eq!(
        order,
        vec![true, false, false],
        "the saved front pane returns"
    );
}

#[test]
fn a_tiled_layout_writes_no_arrangement_fields() {
    let set = tab_of(2);
    let text = set.capture_shape().to_json_pretty();
    assert!(!text.contains("arrangement"), "tiled bytes are unchanged");
    assert!(!text.contains("floating") && !text.contains("stacked"));
}

#[test]
fn an_older_file_without_the_field_loads_tiled() {
    let set = tab_of(2);
    let text = set.capture_shape().to_json_pretty();
    let loaded = ShapeSnapshot::from_json_str(&text).expect("parse");
    assert_eq!(
        loaded.workspaces[0].tabs[0].arrangement,
        ArrangementShape::Tiled
    );
}

fn with_arrangement(mode_json: &str) -> ShapeSnapshot {
    let set = tab_of(2);
    let text = set.capture_shape().to_json_pretty();
    // Splice an arrangement object into the first tab.
    let marker = "\"focused_leaf\"";
    let at = text.find(marker).expect("tab field");
    let spliced = format!(
        "{}\"arrangement\": {mode_json},\n          {}",
        &text[..at],
        &text[at..]
    );
    ShapeSnapshot::from_json_str(&spliced).expect("parse")
}

#[test]
fn an_unknown_mode_or_a_malformed_list_loads_tiled_without_failing_the_restore() {
    let unknown = with_arrangement(r#"{"mode": "carousel"}"#);
    assert_eq!(
        unknown.workspaces[0].tabs[0].arrangement,
        ArrangementShape::Tiled
    );
    let not_an_object = with_arrangement("17");
    assert_eq!(
        not_an_object.workspaces[0].tabs[0].arrangement,
        ArrangementShape::Tiled
    );
    let bad_pane = with_arrangement(r#"{"mode": "floating", "panes": [{"leaf": "x"}]}"#);
    assert_eq!(
        bad_pane.workspaces[0].tabs[0].arrangement,
        ArrangementShape::Tiled
    );
    let no_list = with_arrangement(r#"{"mode": "floating"}"#);
    assert_eq!(
        no_list.workspaces[0].tabs[0].arrangement,
        ArrangementShape::Tiled
    );
}

#[test]
fn a_half_written_rectangle_is_never_half_applied() {
    let partial = with_arrangement(
        r#"{"mode": "floating", "panes": [{"leaf": 0, "col": 3, "row": 1}, {"leaf": 1, "col": 0, "row": 0, "cols": 20, "rows": 5}]}"#,
    );
    let ArrangementShape::Floating(panes) = &partial.workspaces[0].tabs[0].arrangement else {
        panic!("floating expected");
    };
    assert_eq!(panes[0].rect, None, "col/row without an extent is dropped");
    assert_eq!(panes[1].rect, Some((0, 0, 20, 5)));
}

#[test]
fn a_floating_list_that_does_not_match_the_panes_restores_tiled() {
    // Two panes but only one entry, and a duplicate leaf: neither is applied.
    for json in [
        r#"{"mode": "floating", "panes": [{"leaf": 0}]}"#,
        r#"{"mode": "floating", "panes": [{"leaf": 1}, {"leaf": 1}]}"#,
        r#"{"mode": "floating", "panes": [{"leaf": 0}, {"leaf": 7}]}"#,
    ] {
        let loaded = with_arrangement(json);
        let mut restored = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        restored.restore_from_snapshot_with(
            &loaded,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        assert!(restored.active_arrangement_is_tiled(), "{json}");
        assert_eq!(restored.active_pane_count(), 2, "the panes still restore");
    }
}

#[test]
fn a_newer_schema_is_ignored_whole_and_applies_no_rectangles() {
    let set = tab_of(2);
    let text =
        set.capture_shape()
            .to_json_pretty()
            .replacen("\"version\": 1", "\"version\": 999", 1);
    assert!(matches!(
        ShapeSnapshot::from_json_str(&text),
        Err(crate::native::persistence::LoadError::VersionSkew { found: 999 })
    ));
}

#[test]
fn restoring_the_same_file_at_the_same_size_gives_the_same_rectangles() {
    let layout = Arrangement::Floating(FloatLayout::from_entries(vec![
        FloatEntry {
            token: SessionToken(0),
            rect: Some(CellRect::new(4, 1, 30, 8)),
        },
        FloatEntry {
            token: SessionToken(1),
            rect: None,
        },
    ]));
    let mut set = tab_of(2);
    set.workspaces[0].tabs[0].arrangement = layout;
    let text = set.capture_shape().to_json_pretty();
    let resolve = || {
        let loaded = ShapeSnapshot::from_json_str(&text).expect("parse");
        let mut restored = WorkspaceSet::new(build_session(), None);
        let mut handed = Vec::new();
        restored.restore_from_snapshot_with(
            &loaded,
            None,
            fake_spawner(&mut handed),
            no_remote_spawner(),
        );
        restored
            .active_pane_rects(content(), DIVIDER, CELL)
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
    };
    assert_eq!(resolve(), resolve(), "no random cascade on load");
}

#[test]
fn the_autosave_fingerprint_tracks_the_mode_and_the_rectangles() {
    let mut set = tab_of(2);
    let tiled = set.structural_fingerprint();
    set.set_active_stacked();
    let stacked = set.structural_fingerprint();
    assert_ne!(tiled, stacked, "stacking re-arms the autosave");
    set.set_active_tiled();
    assert_eq!(
        set.structural_fingerprint(),
        tiled,
        "back to the same shape"
    );
    float(&mut set);
    let floating = set.structural_fingerprint();
    assert_ne!(floating, tiled);
    set.step_active_floating(
        FloatStep::Move {
            d_col: -1,
            d_row: 0,
        },
        content(),
        CELL,
    );
    assert_ne!(set.structural_fingerprint(), floating, "moving re-arms it");
}

#[test]
fn a_moved_pane_joins_a_floating_destination_tab_and_otherwise_becomes_a_tab() {
    const SRC: u64 = 1 << 40;
    let source = || {
        let mut set = WorkspaceSet::new(build_session_with_id(SessionToken(SRC)), None);
        set.split_active_for_test(
            SplitAxis::Columns,
            build_session_with_id(SessionToken(SRC + 1)),
        );
        set
    };

    // Floating destination: the pane joins the active tab, in front.
    let mut dest = tab_of(2);
    float(&mut dest);
    let mut src = source();
    let content_moved = src.detach_for_move(MoveScope::ActivePane).expect("detach");
    dest.attach_moved(content_moved).expect("attach");
    assert_eq!(dest.active_workspace().tabs.len(), 1, "no new tab");
    assert_eq!(dest.active_pane_count(), 3);
    assert!(dest.active_is_floating());
    assert_eq!(rects(&dest).last().expect("top").0, SessionToken(SRC + 1));

    // Tiled destination: the pane becomes a new tab as before.
    let mut dest = tab_of(2);
    let mut src = source();
    let content_moved = src.detach_for_move(MoveScope::ActivePane).expect("detach");
    dest.attach_moved(content_moved).expect("attach");
    assert_eq!(dest.active_workspace().tabs.len(), 2);

    // A whole tab always arrives as a tab, floating or not.
    let mut dest = tab_of(2);
    float(&mut dest);
    let mut src = source();
    let content_moved = src.detach_for_move(MoveScope::ActiveTab).expect("detach");
    dest.attach_moved(content_moved).expect("attach");
    assert_eq!(dest.active_workspace().tabs.len(), 2);
}

#[test]
fn a_floating_tab_keeps_its_arrangement_when_the_whole_tab_moves() {
    let mut src = tab_of(2);
    float(&mut src);
    let before = rects(&src);
    let mut dest = WorkspaceSet::new(build_session_with_id(SessionToken(1 << 41)), None);
    let moved = src.detach_for_move(MoveScope::ActiveTab);
    // The only tab of the only workspace moves: the source empties.
    let moved = moved.expect("detach");
    dest.attach_moved(moved).expect("attach");
    assert!(dest.active_is_floating());
    assert_eq!(rects(&dest).len(), before.len());
}

#[test]
fn a_pane_move_that_fails_restores_the_floating_layout_exactly() {
    let mut src = tab_of(3);
    float(&mut src);
    let before = rects(&src);
    let moved = src.detach_for_move(MoveScope::ActivePane).expect("detach");
    src.restore_moved(moved);
    assert!(src.active_is_floating());
    assert_eq!(rects(&src), before, "rectangles and z-order are unchanged");
}

#[test]
fn explicit_entries_for_missing_panes_are_ignored_and_new_panes_cascade() {
    let mut set = tab_of(2);
    set.workspaces[0].tabs[0].arrangement = Arrangement::Floating(FloatLayout::from_entries(vec![
        FloatEntry {
            token: SessionToken(77),
            rect: Some(CellRect::new(0, 0, 10, 3)),
        },
        FloatEntry {
            token: SessionToken(0),
            rect: Some(CellRect::new(0, 0, 20, 5)),
        },
    ]));
    let shown = rects(&set);
    assert_eq!(shown.len(), 2, "only real panes are laid out");
    assert!(shown.iter().all(|(token, _)| token.0 < 2));
    // The pane with no stored rectangle gets a default, whole-cell slot.
    let default = shown.iter().find(|(t, _)| t.0 == 1).expect("pane 1").1;
    assert_eq!(default.x % CELL.0 as f32, 0.0);
    assert!(default.w >= 8.0 * CELL.0 as f32);
}

#[test]
fn float_pane_shape_has_no_hidden_state() {
    // The saved shape of a layout with no stored rectangles lists the panes
    // without any rectangle keys at all.
    let mut set = tab_of(2);
    set.workspaces[0].tabs[0].arrangement = Arrangement::Floating(FloatLayout::default());
    let shape = set.capture_shape();
    let ArrangementShape::Floating(panes) = &shape.workspaces[0].tabs[0].arrangement else {
        panic!("floating");
    };
    assert_eq!(
        panes,
        &vec![
            FloatPaneShape {
                leaf: 0,
                rect: None
            },
            FloatPaneShape {
                leaf: 1,
                rect: None
            }
        ]
    );
    let text = shape.to_json_pretty();
    assert!(!text.contains("\"col\""));
}

#[test]
fn phase9_foreign_schema_with_floating_geometry_is_rejected_before_restore() {
    let mut set = tab_of(3);
    float(&mut set);
    set.step_active_floating(
        FloatStep::Move {
            d_col: -3,
            d_row: 1,
        },
        content(),
        CELL,
    );
    let snapshot = set.capture_shape();
    assert!(matches!(
        snapshot.workspaces[0].tabs[0].arrangement,
        ArrangementShape::Floating(_)
    ));
    let json = snapshot.to_json_pretty();
    assert!(json.contains("\"arrangement\""));
    assert!(json.contains("\"floating\""));
    for version in [0, crate::native::persistence::SNAPSHOT_VERSION + 1] {
        let foreign = json.replacen(
            &format!(
                "\"version\": {}",
                crate::native::persistence::SNAPSHOT_VERSION
            ),
            &format!("\"version\": {version}"),
            1,
        );
        assert_ne!(foreign, json);
        assert!(
            matches!(
                ShapeSnapshot::from_json_str(&foreign),
                Err(crate::native::persistence::LoadError::VersionSkew { found }) if found == version
            ),
            "foreign schema must not best-effort apply floating geometry"
        );
    }
    assert_eq!(
        ShapeSnapshot::from_json_str(&json).expect("current schema"),
        snapshot
    );
}

#[test]
fn phase9_older_layout_without_arrangement_restores_all_panes_as_tiled() {
    let original = tab_of(3);
    let json = original.capture_shape().to_json_pretty();
    assert!(!json.contains("\"arrangement\""));
    let loaded = ShapeSnapshot::from_json_str(&json).expect("legacy tiled shape");
    let mut restored = WorkspaceSet::new(build_session(), None);
    let mut handed = Vec::new();
    restored.restore_from_snapshot_with(
        &loaded,
        None,
        fake_spawner(&mut handed),
        no_remote_spawner(),
    );
    assert_eq!(restored.active_pane_count(), 3);
    assert!(restored.active_arrangement_is_tiled());
    assert_eq!(
        rects(&restored)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect::<Vec<_>>(),
        rects(&original)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect::<Vec<_>>()
    );
}

#[test]
fn phase9_unknown_layout_fields_do_not_bypass_newer_schema_rejection() {
    let mut original = tab_of(2);
    float(&mut original);
    let shape = original.capture_shape();
    let json = shape.to_json_pretty();
    let extended = json
        .replacen("{", "{\"future_root\": {\"nested\": [1, true]},", 1)
        .replacen(
            "\"focused_leaf\"",
            "\"future_tab\": [false, 7], \"focused_leaf\"",
            1,
        )
        .replacen(
            "\"mode\"",
            "\"future_arrangement\": {\"policy\": \"future\"}, \"mode\"",
            1,
        );
    assert!(extended.contains("future_arrangement"));
    assert_eq!(
        ShapeSnapshot::from_json_str(&extended).expect("compatible extra fields"),
        shape
    );
    let future_version = crate::native::persistence::SNAPSHOT_VERSION + 1;
    let future = extended.replacen(
        &format!(
            "\"version\": {}",
            crate::native::persistence::SNAPSHOT_VERSION
        ),
        &format!("\"version\": {future_version}"),
        1,
    );
    assert_ne!(future, extended);
    assert!(matches!(ShapeSnapshot::from_json_str(&future),
        Err(crate::native::persistence::LoadError::VersionSkew { found }) if found == future_version));
}
