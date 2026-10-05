// SPDX-License-Identifier: GPL-3.0-only
//! S3b: input-facing surfaces under the test-only bidi display gate, through
//! the real pointer path of a headless App. Mouse reports, hyperlink hover,
//! and split-pane hit testing address the logical cell drawn under the
//! pointer; the gate off keeps every answer unchanged.

use super::*;

use std::io::Write;
use winit::window::CursorIcon;

use crate::native::session::SessionToken;
use crate::selection::CellPoint;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 4;

/// Logical "ab אבג xy": columns 3..=5 are Hebrew and draw reversed.
const LINE: &str = "ab \u{05D0}\u{05D1}\u{05D2} xy";

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

fn recording_app(output: &[u8]) -> (App, Recorded) {
    let recorded = Recorded::default();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter(recorded.clone()))));
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
        writer,
    );
    terminal.lock().expect("terminal").advance(output);
    app.set_test_cell_for_test(CELL);
    (app, recorded)
}

fn pixel(visual_column: usize, row: usize) -> (f64, f64) {
    (
        f64::from(CELL.width) * visual_column as f64 + 4.0,
        f64::from(CELL.height) * row as f64 + 8.0,
    )
}

/// The bytes one left click at `(x, y)` reports to the application.
fn click_report(app: &mut App, recorded: &Recorded, (x, y): (f64, f64)) -> Vec<u8> {
    recorded.lock().expect("bytes").clear();
    app.pointer_move_for_test(x, y);
    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();
    std::mem::take(&mut *recorded.lock().expect("bytes"))
}

#[test]
fn cell_mouse_reports_name_the_logical_cell_in_every_encoding() {
    // X10-compatible normal tracking, SGR (1006), and URXVT (1015).
    for modes in [
        &b"\x1b[?1000h"[..],
        &b"\x1b[?1000h\x1b[?1006h"[..],
        &b"\x1b[?1000h\x1b[?1015h"[..],
    ] {
        let mut output = LINE.as_bytes().to_vec();
        output.extend_from_slice(modes);
        let (mut app, recorded) = recording_app(&output);
        // Gate off: the report for each logical column at its own screen column.
        let plain: Vec<Vec<u8>> = (0..COLUMNS)
            .map(|column| click_report(&mut app, &recorded, pixel(column, 0)))
            .collect();
        assert!(plain.iter().all(|bytes| !bytes.is_empty()), "{modes:?}");
        app.present_bidi_frame_map_for_test();
        let map = app.bidi_frame_map_for_test().expect("gate map").clone();
        assert!(map.row_is_reordered(0));
        for (column, expected) in plain.iter().enumerate() {
            let visual = map.visual_column(0, column);
            assert_eq!(
                &click_report(&mut app, &recorded, pixel(visual, 0)),
                expected,
                "{modes:?}: the click over visual {visual} reports logical {column}"
            );
        }
    }
}

#[test]
fn reordered_mouse_reports_differ_from_the_screen_column() {
    // Guard against a vacuous pass: on the reordered span the logical report
    // differs from the report for the screen column the pointer is over.
    let mut output = LINE.as_bytes().to_vec();
    output.extend_from_slice(b"\x1b[?1000h\x1b[?1006h");
    let (mut app, recorded) = recording_app(&output);
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert_eq!(map.visual_column(0, 3), 5);
    assert_eq!(
        click_report(&mut app, &recorded, pixel(5, 0)),
        b"\x1b[<0;4;1M\x1b[<0;4;1m".to_vec(),
        "the click over visual column 5 reports logical column 3 (1-based 4)"
    );
}

#[test]
fn sgr_pixel_reports_move_by_whole_cells_onto_the_logical_cell() {
    let (mut app, _recorded) = recording_app(LINE.as_bytes());
    // Gate off: identity.
    assert_eq!(
        app.bidi_logical_report_px_for_test((5 * 8 + 3 + 1, 9), CELL),
        (5 * 8 + 3 + 1, 9)
    );
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    for visual in 0..COLUMNS {
        let logical = map.logical_column(0, visual);
        for inner in [0, 3, 7] {
            assert_eq!(
                app.bidi_logical_report_px_for_test((visual * 8 + inner + 1, 9), CELL),
                (logical * 8 + inner + 1, 9),
                "visual {visual} offset {inner}"
            );
        }
    }
    // Rows below the map's reordered row keep their pixels.
    assert_eq!(
        app.bidi_logical_report_px_for_test((5 * 8 + 1, 3 * 16 + 1), CELL),
        (5 * 8 + 1, 3 * 16 + 1)
    );
}

#[test]
fn hyperlink_hover_follows_the_logical_cell_under_the_pointer() {
    // `LINE` with its first two Hebrew letters (logical 3..=4) linked; the
    // Hebrew run reverses, so the link draws at visual 4..=5 instead.
    let output = "ab \x1b]8;;https://example.com\x07\u{05D0}\u{05D1}\x1b]8;;\x07\u{05D2} xy";
    let output = output.as_bytes();
    let (mut app, _recorded) = recording_app(output);
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(map.row_is_reordered(0));
    let mut moved = false;
    for visual in 0..COLUMNS {
        let logical = map.logical_column(0, visual);
        moved |= logical != visual && (3..=4).contains(&logical);
        let (x, y) = pixel(visual, 0);
        app.pointer_move_for_test(x, y);
        let expected = if (3..=4).contains(&logical) {
            CursorIcon::Pointer
        } else {
            CursorIcon::Text
        };
        assert_eq!(
            app.cursor_icon_for_test(),
            expected,
            "visual {visual} shows logical {logical}"
        );
    }
    assert!(moved, "the link draws away from its logical columns");
}

/// A two-column split whose panes both print `LINE`, with the gate on and one
/// split frame built. Returns the focused pane's token.
fn split_app() -> (App, SessionToken) {
    let dims = Dimensions::new(COLUMNS, ROWS);
    let (mut app, first) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    first.lock().expect("terminal").advance(LINE.as_bytes());
    let second = Arc::new(Mutex::new(Terminal::new(COLUMNS, ROWS)));
    second.lock().expect("terminal").advance(LINE.as_bytes());
    let writer = crate::native::test_support::headless_writer();
    app.seed_headless_split_pane_for_test(true, second, writer, dims);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        (COLUMNS * 2) as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    let token = app.active_session_token_for_test();
    (app, token)
}

#[test]
fn split_panes_plan_their_own_maps_and_the_pointer_maps_the_focused_pane() {
    let (mut app, focused) = split_app();
    // Gate off: the pane-relative cell under each pixel of the window.
    let _ = app.rebuild_multipane_probe_for_test();
    assert!(app.bidi_pane_map_for_test(focused).is_none());
    let width = (COLUMNS * 2) * CELL.width as usize;
    let plain: Vec<Option<CellPoint>> = (0..width)
        .step_by(CELL.width as usize)
        .map(|x| {
            app.pointer_move_for_test(x as f64 + 4.0, 8.0);
            app.pointer_cell_for_test()
        })
        .collect();
    assert!(plain.iter().any(Option::is_some));

    app.set_bidi_display_for_test(true);
    let _ = app.rebuild_multipane_probe_for_test();
    let tokens = app.active_tab_pane_tokens_for_test();
    assert_eq!(tokens.len(), 2);
    for token in &tokens {
        let map = app.bidi_pane_map_for_test(*token).expect("each pane plans");
        assert!(map.row_is_reordered(0), "pane {token:?} reorders its row");
    }
    let map = app
        .bidi_pane_map_for_test(focused)
        .expect("focused map")
        .clone();
    let mut moved = false;
    for (index, x) in (0..width).step_by(CELL.width as usize).enumerate() {
        app.pointer_move_for_test(x as f64 + 4.0, 8.0);
        let expected = plain[index].map(|point| CellPoint {
            row: point.row,
            column: map.logical_column(point.row, point.column),
        });
        moved |= expected != plain[index];
        assert_eq!(app.pointer_cell_for_test(), expected, "pixel column {x}");
    }
    assert!(
        moved,
        "the focused pane's reordered span maps away from identity"
    );
}

#[test]
fn split_frame_drops_maps_when_the_gate_turns_off() {
    let (mut app, focused) = split_app();
    app.set_bidi_display_for_test(true);
    let _ = app.rebuild_multipane_probe_for_test();
    assert!(app.bidi_pane_map_for_test(focused).is_some());
    app.set_bidi_display_for_test(false);
    let _ = app.rebuild_multipane_probe_for_test();
    assert!(app.bidi_pane_map_for_test(focused).is_none());
}

#[test]
fn ime_candidate_window_anchors_at_the_cursor_cell_drawn_on_screen() {
    // The cursor sits on logical column 4 (bet) after "ab א" is printed and the
    // cursor moves back over the remaining Hebrew.
    let mut output = LINE.as_bytes().to_vec();
    output.extend_from_slice(b"\x1b[1;5H");
    let (mut app, _recorded) = recording_app(&output);
    let cursor = crate::core::Position { row: 0, column: 4 };
    assert_eq!(app.ime_anchor_column_for_test(cursor), 4, "gate off");
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert_eq!(map.visual_column(0, 4), 4, "bet is the middle of the run");
    let cursor = crate::core::Position { row: 0, column: 3 };
    assert_eq!(app.ime_anchor_column_for_test(cursor), 5, "alef draws at 5");
    let cursor = crate::core::Position { row: 0, column: 1 };
    assert_eq!(app.ime_anchor_column_for_test(cursor), 1, "Latin stays put");
}
