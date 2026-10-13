// SPDX-License-Identifier: GPL-3.0-only
//! Pointer geometry in a split tab, driven through the real pointer path. A
//! split tab draws no scroll thumb and each pane is narrower than the window
//! grid, so the window-grid scroll-thumb hit test, the rail seam that yields to
//! a thumb, a thumb drag that outlives a split, and the hover cell bounds must
//! not act on the window grid's geometry.

use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 4;
const WINDOW_ROWS: usize = ROWS * 2 + 1;

fn history() -> String {
    (0..40)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn fit(app: &mut App) {
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        WINDOW_ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.resize_grid(
        CELL,
        COLUMNS as u32 * CELL.width,
        WINDOW_ROWS as u32 * CELL.height,
    );
}

fn single_pane_app() -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, WINDOW_ROWS),
        Settings::default(),
    );
    first.lock().expect("first").advance(history().as_bytes());
    fit(&mut app);
    (app, first)
}

/// Split the active tab and focus the new pane, whose terminal holds `text`.
fn split(app: &mut App, columns: bool, text: &str) {
    let dims = if columns {
        Dimensions::new(COLUMNS / 2, WINDOW_ROWS)
    } else {
        Dimensions::new(COLUMNS, ROWS)
    };
    let pane = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    pane.lock().expect("pane").advance(text.as_bytes());
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(std::io::sink())));
    app.seed_headless_split_pane_for_test(columns, pane, writer, dims);
    fit(app);
    app.reflow_active_panes_for_test();
}

/// Window-grid points whose press would grab a scroll thumb: the right-edge
/// band of the single-pane window grid.
fn thumb_band_hits(app: &mut App) -> Vec<(u32, u32)> {
    let mut hits = Vec::new();
    for y in 0..WINDOW_ROWS as u32 * CELL.height {
        for x in (COLUMNS as u32 * CELL.width - 14)..COLUMNS as u32 * CELL.width {
            app.set_pointer_px_for_test(f64::from(x), f64::from(y));
            if app.scrollbar_hit_for_test() {
                hits.push((x, y));
            }
        }
    }
    hits
}

#[test]
fn control_a_single_pane_tab_scrolled_back_has_a_grabbable_thumb() {
    let (mut app, _first) = single_pane_app();
    app.scroll_up_for_test(usize::MAX);
    assert!(app.viewport_offset_for_test() > 0);
    assert!(
        !thumb_band_hits(&mut app).is_empty(),
        "the single-pane thumb is hit-testable, so the split scan below is meaningful"
    );
}

#[test]
fn a_split_tab_has_no_scroll_thumb_to_hit_or_grab() {
    let (mut app, _first) = single_pane_app();
    split(&mut app, false, &history());
    app.scroll_up_for_test(usize::MAX);
    assert!(
        app.viewport_offset_for_test() > 0,
        "the focused pane is scrolled back"
    );
    let hits = thumb_band_hits(&mut app);
    assert!(
        hits.is_empty(),
        "no thumb is drawn in a split tab, so none can be hit; first hit {:?} of {}",
        hits.first(),
        hits.len()
    );
    app.set_pointer_px_for_test(f64::from(COLUMNS as u32 * CELL.width - 1), 4.0);
    app.mouse_left_press_for_test();
    assert!(!app.scrollbar_dragging_for_test());
    app.mouse_left_release_for_test();
}

#[test]
fn the_rail_seam_is_grabbable_in_a_split_tab_where_a_window_thumb_would_sit() {
    let (mut app, _first) = single_pane_app();
    split(&mut app, false, &history());
    app.set_workspace_rail_for_test("right");
    app.reflow_active_panes_for_test();
    app.scroll_up_for_test(usize::MAX);
    assert!(app.viewport_offset_for_test() > 0);
    // Below any window thumb the seam resolves as grabbable; find its span.
    let below = f64::from(WINDOW_ROWS as u32 * CELL.height - 2);
    let surface = 3 * COLUMNS as u32 * CELL.width;
    let mut seam_xs = Vec::new();
    for x in 0..surface {
        app.set_pointer_px_for_test(f64::from(x), below);
        if app.pointer_over_rail_seam_for_test(f64::from(x)) == Some(true) {
            seam_xs.push(x);
        }
    }
    assert!(!seam_xs.is_empty(), "a right rail has a grabbable seam");
    // The same x positions near the top, where a window thumb would sit.
    for x in seam_xs {
        app.set_pointer_px_for_test(f64::from(x), 4.0);
        assert_eq!(
            app.pointer_over_rail_seam_for_test(f64::from(x)),
            Some(true),
            "no thumb is drawn, so the seam at x={x} stays grabbable"
        );
    }
}

#[test]
fn control_a_split_made_while_the_thumb_is_held_drops_the_grab() {
    let (mut app, _first) = single_pane_app();
    app.scroll_up_for_test(usize::MAX);
    let top = thumb_band_hits(&mut app).first().copied().expect("a thumb");
    app.set_pointer_px_for_test(f64::from(top.0), f64::from(top.1));
    app.mouse_left_press_for_test();
    assert!(app.scrollbar_dragging_for_test(), "the thumb is grabbed");
    split(&mut app, false, &history());
    assert!(
        !app.scrollbar_dragging_for_test(),
        "a structural change ends the grab, so no drag can scrub with window geometry"
    );
    app.mouse_left_release_for_test();
}

#[test]
fn control_a_split_clears_the_pointer_cell_so_no_window_column_reaches_a_pane() {
    let (mut app, _first) = single_pane_app();
    app.pointer_move_for_test(15.5 * f64::from(CELL.width), 0.5 * f64::from(CELL.height));
    assert_eq!(
        app.pointer_cell_for_test(),
        Some(CellPoint { row: 0, column: 15 }),
        "window column 15 is hovered while the tab is one pane wide"
    );
    split(&mut app, true, "");
    assert_eq!(
        app.pointer_cell_for_test(),
        None,
        "the half-width pane has no column 15, so the hover cell does not survive"
    );
}
