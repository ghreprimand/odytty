// SPDX-License-Identifier: GPL-3.0-only
//! The frame-to-frame bidi plan cache. A frame reuses the previous frame's
//! content plan while the terminal, its render revision, the viewport offset,
//! the dimensions, and `bidi_direction` are unchanged, and plans again after
//! every change that can move a cell: output, a resize, a history scroll, a
//! settings edit, a screen switch, and a session restore. A selection plans
//! nothing new. After each change the presented map equals the map a frame
//! with no kept plan presents.

use super::*;

use crate::core::{SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal};
use crate::grid::BidiDisplayMap;
use crate::settings::BidiDirection;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 4;

/// Project-authored mixed text: a Hebrew paragraph soft-wrapped over five
/// rows at 20 columns (so it reaches into history), then two short lines.
fn mixed_output() -> Vec<u8> {
    let mut text = String::new();
    for _ in 0..12 {
        text.push_str("\u{05D0}\u{05D1}\u{05D2}\u{05D3}\u{05D4}\u{05D5} ");
    }
    text.push_str("\r\nxy \u{05D3}\u{05D4}\r\nend");
    text.into_bytes()
}

fn app_with(settings: Settings) -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        settings,
    );
    terminal.lock().expect("terminal").advance(&mixed_output());
    app.set_test_cell_for_test(CELL);
    (app, terminal)
}

fn gated_app() -> (App, Arc<Mutex<Terminal>>) {
    app_with(Settings {
        bidi_reorder: true,
        ..Settings::default()
    })
}

fn presented(app: &mut App) -> BidiDisplayMap {
    let _ = app.present_bidi_frame_for_test(Instant::now());
    app.bidi_frame_map_for_test().expect("gate map").clone()
}

/// Present a frame after a change: it must plan once, and present the map a
/// frame with no kept plan presents. A second frame reuses that plan.
fn replans(app: &mut App, change: &str) -> BidiDisplayMap {
    let plans = app.bidi_plans_made_for_test();
    let cached = presented(app);
    assert_eq!(
        app.bidi_plans_made_for_test(),
        plans + 1,
        "{change}: the frame plans again"
    );
    let again = presented(app);
    assert_eq!(again, cached, "{change}: the next frame reuses the plan");
    assert_eq!(
        app.bidi_plans_made_for_test(),
        plans + 1,
        "{change}: reused"
    );
    app.clear_bidi_plan_cache_for_test();
    assert_eq!(
        presented(app),
        cached,
        "{change}: the reused plan is the plan a fresh frame makes"
    );
    cached
}

#[test]
fn an_unchanged_terminal_reuses_the_plan() {
    let (mut app, _terminal) = gated_app();
    let first = replans(&mut app, "first frame");
    assert!(!first.is_identity(), "the fixture reorders");
    let plans = app.bidi_plans_made_for_test();
    for _ in 0..3 {
        assert_eq!(presented(&mut app), first);
    }
    assert_eq!(app.bidi_plans_made_for_test(), plans);
    assert_eq!(app.bidi_plans_kept_for_test(), 1);
}

#[test]
fn output_replans() {
    let (mut app, terminal) = gated_app();
    let before = replans(&mut app, "first frame");
    terminal
        .lock()
        .expect("terminal")
        .advance("\r\n\u{05D0}\u{05D1} ab".as_bytes());
    let after = replans(&mut app, "output");
    assert_ne!(after, before, "the new line moved the rows");
}

#[test]
fn a_direction_change_on_an_existing_row_replans() {
    // SCP at the start of a paragraph rewrites rows that are already drawn
    // without printing a cell.
    let (mut app, terminal) = gated_app();
    terminal.lock().expect("terminal").advance(b"\r\nab cd\r");
    let before = replans(&mut app, "first frame");
    assert!(!before.row_is_reordered(ROWS - 1));
    terminal.lock().expect("terminal").advance(b"\x1b[2 k");
    let after = replans(&mut app, "SCP 2");
    assert!(after.row_is_reordered(ROWS - 1), "the row is right to left");
    terminal.lock().expect("terminal").advance(b"\x1b[8l");
    let explicit = replans(&mut app, "BDSM explicit");
    assert!(explicit.row_is_explicit(ROWS - 1));
}

#[test]
fn a_resize_replans() {
    let (mut app, _terminal) = gated_app();
    let before = replans(&mut app, "first frame");
    for columns in [12, 31, COLUMNS] {
        assert!(app.resize_grid(CELL, columns as u32 * CELL.width, ROWS as u32 * CELL.height));
        let map = replans(&mut app, &format!("{columns} columns"));
        if columns == COLUMNS {
            assert_eq!(map, before, "back at the first width");
        }
    }
}

#[test]
fn a_history_scroll_replans_at_every_offset() {
    let (mut app, _terminal) = gated_app();
    let live = replans(&mut app, "first frame");
    let scrollback = app.scrollback_len_for_test();
    assert!(scrollback >= 2, "the paragraph reaches into history");
    let mut maps = vec![live];
    for offset in 1..=scrollback {
        app.scroll_up_for_test(1);
        assert_eq!(app.viewport_offset_for_test(), offset);
        maps.push(replans(&mut app, &format!("offset {offset}")));
    }
    assert!(
        maps.windows(2).any(|pair| pair[0] != pair[1]),
        "not vacuous"
    );
}

#[test]
fn a_selection_plans_nothing_new() {
    let (mut app, _terminal) = gated_app();
    let _ = replans(&mut app, "first frame");
    let plans = app.bidi_plans_made_for_test();
    let y = f64::from(CELL.height) * 2.5;
    app.pointer_move_for_test(4.0, y);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(f64::from(CELL.width) * 10.0, y);
    app.mouse_left_release_for_test();
    assert!(
        app.selection_text_for_test().is_some(),
        "a selection exists"
    );
    let selected = presented(&mut app);
    assert_eq!(app.bidi_plans_made_for_test(), plans, "the plan is reused");
    app.clear_bidi_plan_cache_for_test();
    assert_eq!(presented(&mut app), selected, "and is the fresh plan");
}

#[test]
fn a_direction_setting_edit_replans() {
    let (mut app, _terminal) = gated_app();
    let before = replans(&mut app, "first frame");
    for direction in [
        BidiDirection::RightToLeft,
        BidiDirection::Auto,
        BidiDirection::LeftToRight,
    ] {
        let settings = Settings {
            bidi_reorder: true,
            bidi_direction: direction,
            ..Settings::default()
        };
        app.apply_overlay_settings_for_test(settings);
        let map = replans(&mut app, &format!("{direction:?}"));
        if direction == BidiDirection::RightToLeft {
            assert_ne!(map, before, "right to left moves the Latin rows");
        }
    }
}

#[test]
fn turning_reordering_off_drops_the_kept_plans() {
    let (mut app, terminal) = gated_app();
    let _ = replans(&mut app, "first frame");
    for on in [false, true] {
        let settings = Settings {
            bidi_reorder: on,
            ..Settings::default()
        };
        app.apply_overlay_settings_for_test(settings);
        if !on {
            let _ = app.present_bidi_frame_for_test(Instant::now());
            assert!(app.bidi_frame_map_for_test().is_none());
            assert_eq!(app.bidi_plans_kept_for_test(), 0, "off keeps no plan");
            terminal
                .lock()
                .expect("terminal")
                .advance("\r\n\u{05D0} off".as_bytes());
        }
    }
    let _ = replans(&mut app, "on again after output while off");
}

#[test]
fn a_screen_switch_replans() {
    let (mut app, terminal) = gated_app();
    let primary = replans(&mut app, "first frame");
    terminal
        .lock()
        .expect("terminal")
        .advance("\x1b[?1049h\x1b[H\u{05D0}\u{05D1} ab".as_bytes());
    let alternate = replans(&mut app, "alternate screen");
    assert_ne!(alternate, primary);
    terminal.lock().expect("terminal").advance(b"\x1b[?1049l");
    assert_eq!(replans(&mut app, "primary again"), primary);
}

#[test]
fn a_session_restore_replans() {
    let (mut app, terminal) = gated_app();
    let before = replans(&mut app, "first frame");
    let mut other = Terminal::new(COLUMNS, ROWS);
    other.advance("\x1b[2 kab \u{05D0}\u{05D1}\r\ncd".as_bytes());
    let envelope = SnapshotEnvelope::from_terminal(&other, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().expect("encode");
    let decoded =
        SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).expect("decode");
    terminal
        .lock()
        .expect("terminal")
        .restore_from_envelope(&decoded)
        .expect("restore");
    let restored = replans(&mut app, "restore");
    assert_ne!(restored, before);
    assert!(restored.row_is_reordered(0), "the restored SCP 2 row");
}

#[test]
fn split_panes_keep_their_own_plans() {
    let dims = Dimensions::new(COLUMNS, ROWS);
    let (mut app, first) = headless_app_with(
        NativeOptions::default(),
        dims,
        Settings {
            bidi_reorder: true,
            ..Settings::default()
        },
    );
    first.lock().expect("terminal").advance(&mixed_output());
    let second = Arc::new(Mutex::new(Terminal::new(COLUMNS, ROWS)));
    second
        .lock()
        .expect("terminal")
        .advance("ab \u{05D0}\u{05D1}".as_bytes());
    let writer = crate::native::test_support::headless_writer();
    app.seed_headless_split_pane_for_test(true, second.clone(), writer, dims);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        (COLUMNS * 2) as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    let _ = app.rebuild_multipane_probe_for_test();
    let plans = app.bidi_plans_made_for_test();
    assert_eq!(app.bidi_plans_kept_for_test(), 2, "one plan per pane");
    let maps = app.bidi_pane_maps_for_test();
    assert_eq!(maps.len(), 2);
    let _ = app.rebuild_multipane_probe_for_test();
    assert_eq!(app.bidi_plans_made_for_test(), plans, "both reused");
    assert_eq!(app.bidi_pane_maps_for_test(), maps);

    second.lock().expect("terminal").advance(b"\x1b[2 k\r\n");
    let _ = app.rebuild_multipane_probe_for_test();
    assert_eq!(
        app.bidi_plans_made_for_test(),
        plans + 1,
        "only the pane with output plans again"
    );
    let cached = app.bidi_pane_maps_for_test();
    app.clear_bidi_plan_cache_for_test();
    let _ = app.rebuild_multipane_probe_for_test();
    assert_eq!(
        app.bidi_pane_maps_for_test(),
        cached,
        "and both match a fresh plan"
    );
}
