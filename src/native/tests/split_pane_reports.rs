// SPDX-License-Identifier: GPL-3.0-only
//! A split pane's Select All and SGR-pixel mouse reports address that pane.
//!
//! The App's `grid` is the whole-window content grid. Select All and the
//! SGR-pixel (1016) clamp must use the focused pane's own size instead, so a
//! smaller pane selects exactly its rows and never reports a pixel outside
//! its screen.

use std::io::Write;

use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 40;
const ROWS: usize = 8;

type Recorded = Arc<Mutex<Vec<u8>>>;

struct RecordingWriter(Recorded);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("bytes").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A window of `COLUMNS` x `ROWS` cells split once along `columns`; the new
/// pane is focused, prints `output`, and records its PTY bytes.
fn split_app(columns: bool, output: &[u8]) -> (App, Recorded) {
    let (mut app, _first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    let pane_dims = if columns {
        Dimensions::new(COLUMNS / 2 - 1, ROWS)
    } else {
        Dimensions::new(COLUMNS, ROWS / 2 - 1)
    };
    let pane = Arc::new(Mutex::new(Terminal::new(pane_dims.columns, pane_dims.rows)));
    pane.lock().expect("terminal").advance(output);
    let recorded = Recorded::default();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter(recorded.clone()))));
    app.seed_headless_split_pane_for_test(columns, pane, writer, pane_dims);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    (app, recorded)
}

#[test]
fn select_all_in_a_rows_split_selects_only_the_focused_pane() {
    let (mut app, _recorded) = split_app(false, b"top\r\nmiddle\r\nend");
    let dims = app.active_terminal_dimensions_for_test();
    assert!(
        dims.rows < ROWS,
        "the focused pane is shorter than the window"
    );

    app.select_all_for_test();

    assert_eq!(
        app.selection_range_for_test(),
        Some((0, 0, dims.rows - 1, dims.columns - 1)),
        "Select All spans the focused pane's own rows and columns"
    );
    let text = app.selection_text_for_test().expect("selected text");
    assert!(text.starts_with("top\nmiddle\nend"), "{text:?}");
    assert_eq!(text.lines().count(), dims.rows.min(3));
}

#[test]
fn copying_a_range_past_the_live_rows_stops_at_the_buffer_end() {
    let (mut app, _recorded) = split_app(false, b"one\r\ntwo");
    let dims = app.active_terminal_dimensions_for_test();
    // A range built against the window grid reaches past the pane's rows.
    app.force_selection_for_test(0, 0, dims.rows - 1, dims.columns - 1);
    let whole = app.selection_text_for_test().expect("pane text");
    assert!(whole.starts_with("one\ntwo"), "{whole:?}");
    app.force_selection_for_test(0, 0, ROWS * 4, COLUMNS - 1);
    assert_eq!(
        app.selection_text_for_test(),
        Some(whole),
        "the copy walk ends at the last live row of a {} row pane",
        dims.rows
    );
}

/// Every SGR-pixel report in `bytes`, as `(x, y)`.
fn pixel_reports(bytes: &[u8]) -> Vec<(usize, usize)> {
    let text = String::from_utf8_lossy(bytes);
    text.split("\x1b[<")
        .skip(1)
        .filter_map(|report| {
            let body = report.trim_end_matches(['M', 'm']);
            let mut fields = body.split(';').skip(1);
            let x = fields.next()?.parse().ok()?;
            let y = fields.next()?.trim_end_matches(['M', 'm']).parse().ok()?;
            Some((x, y))
        })
        .collect()
}

/// Press inside the focused pane, drag beyond each window edge, release.
/// Every report stays inside the pane's own pixel extent. The middle button
/// drives the gesture: a left press in a split begins a local selection.
fn drag_reports_stay_inside_the_pane(columns: bool) {
    let (mut app, recorded) = split_app(columns, b"\x1b[?1002h\x1b[?1016h");
    let dims = app.active_terminal_dimensions_for_test();
    let max_x = dims.columns * CELL.width as usize;
    let max_y = dims.rows * CELL.height as usize;
    let window_w = (COLUMNS * CELL.width as usize) as f64;
    let window_h = (ROWS * CELL.height as usize) as f64;
    // The focused pane is the second one: right of a column split, below a
    // row split.
    let inside = if columns {
        (window_w - 12.0, 10.0)
    } else {
        (10.0, window_h - 10.0)
    };
    app.pointer_move_for_test(inside.0, inside.1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Middle);
    for (x, y) in [
        (window_w + 400.0, inside.1),
        (inside.0, window_h + 400.0),
        (-400.0, inside.1),
        (inside.0, -400.0),
        (window_w + 400.0, window_h + 400.0),
    ] {
        app.pointer_move_for_test(x, y);
    }
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Middle);

    let reports = pixel_reports(&recorded.lock().expect("bytes"));
    // Motion outside the window is not reported; the press and the release
    // beyond the corner are.
    assert!(reports.len() >= 2, "press and release report: {reports:?}");
    for (x, y) in &reports {
        assert!(
            (1..=max_x).contains(x) && (1..=max_y).contains(y),
            "report ({x}, {y}) is outside the {max_x}x{max_y} pane"
        );
    }
    let (x, y) = *reports.last().expect("release report");
    assert_eq!(
        (x, y),
        (max_x, max_y),
        "a release beyond the bottom-right corner clamps to the pane's last pixel"
    );
}

#[test]
fn sgr_pixel_drag_in_a_column_split_stays_inside_the_pane() {
    drag_reports_stay_inside_the_pane(true);
}

#[test]
fn sgr_pixel_drag_in_a_rows_split_stays_inside_the_pane() {
    drag_reports_stay_inside_the_pane(false);
}
