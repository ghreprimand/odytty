// SPDX-License-Identifier: GPL-3.0-only
//! Paragraph direction and explicit mode under the bidi display gate, through
//! the real pointer path of a headless single-pane App. A right-to-left
//! paragraph (SCP `CSI 2 SP k`, or the `bidi_direction` setting) starts at
//! the right edge; the pointer, selection, and input-method anchor address
//! the logical cell drawn on screen, and copied text stays logical.

use super::*;

use crate::selection::CellPoint;
use crate::settings::BidiDirection;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;

fn app_with(output: &str, settings: Settings) -> App {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, 4),
        settings,
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(output.as_bytes());
    app.set_test_cell_for_test(CELL);
    app
}

fn app(output: &str) -> App {
    app_with(output, Settings::default())
}

fn pixel(visual_column: usize) -> (f64, f64) {
    (
        f64::from(CELL.width) * visual_column as f64 + 4.0,
        f64::from(CELL.height) / 2.0,
    )
}

fn drag(app: &mut App, from_visual: usize, to_visual: usize) {
    let (x0, y) = pixel(from_visual);
    let (x1, _) = pixel(to_visual);
    app.pointer_move_for_test(x0, y);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(x1, y);
    app.mouse_left_release_for_test();
}

/// Logical "אב xy" in a right-to-left paragraph: alef at the right edge,
/// then bet, the space, and "xy" as a left-to-right run to their left.
const RTL_LINE: &str = "\x1b[2 k\u{05D0}\u{05D1} xy";

#[test]
fn rtl_paragraph_places_logical_column_zero_at_the_right_edge() {
    let mut app = app(RTL_LINE);
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert_eq!(
        (0..5).map(|c| map.visual_column(0, c)).collect::<Vec<_>>(),
        [19, 18, 17, 15, 16]
    );
    assert_eq!(map.visual_column(0, 5), 14, "trailing blanks fill the left");
    assert_eq!(map.visual_column(0, 19), 0);
}

#[test]
fn rtl_paragraph_pointer_selects_logical_cell() {
    let mut app = app(RTL_LINE);
    app.present_bidi_frame_map_for_test();
    for (visual, column) in [(19, 0), (18, 1), (15, 3), (16, 4), (0, 19)] {
        let (x, y) = pixel(visual);
        app.pointer_move_for_test(x, y);
        assert_eq!(
            app.pointer_cell_for_test(),
            Some(CellPoint { row: 0, column }),
            "visual {visual}"
        );
    }
    // Drag from the right edge (alef) to the screen column showing x.
    drag(&mut app, 19, 15);
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("\u{05D0}\u{05D1} x"),
        "the selection spans logical columns 0..=3 and copies in logical order"
    );
}

#[test]
fn rtl_paragraph_ime_anchor_at_visual_column() {
    let mut app = app(&format!("{RTL_LINE}\x1b[1;1H"));
    let cursor = crate::core::Position { row: 0, column: 0 };
    assert_eq!(app.ime_anchor_column_for_test(cursor), 0, "gate off");
    app.present_bidi_frame_map_for_test();
    assert_eq!(app.ime_anchor_column_for_test(cursor), 19);
    let cursor = crate::core::Position { row: 0, column: 3 };
    assert_eq!(app.ime_anchor_column_for_test(cursor), 15);
}

#[test]
fn the_direction_setting_applies_to_paragraphs_without_their_own() {
    let settings = Settings {
        bidi_direction: BidiDirection::RightToLeft,
        ..Settings::default()
    };
    // Row 0 has no SCP of its own and follows the setting; row 1 selected
    // left to right with SCP 1 and keeps the left edge.
    let mut app = app_with("ab\r\n\x1b[1 kcd", settings);
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert_eq!(map.visual_column(0, 0), 18);
    assert_eq!(map.visual_column(0, 1), 19);
    assert!(!map.row_is_reordered(1), "SCP 1 overrides the setting");
    // Auto: the first strong character decides.
    let settings = Settings {
        bidi_direction: BidiDirection::Auto,
        ..Settings::default()
    };
    let mut app = app_with("\u{05D0}b\r\nab \u{05D0}", settings);
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert_eq!(map.visual_column(0, 0), 19, "alef first: right to left");
    assert_eq!(map.visual_column(1, 0), 0, "Latin first: left to right");
}

#[test]
fn explicit_rtl_paragraph_reverses_columns_and_keeps_pointer_logical() {
    let mut app = app("\x1b[8l\x1b[2 k(ab");
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(map.row_is_explicit(0));
    assert_eq!(
        (0..3).map(|c| map.visual_column(0, c)).collect::<Vec<_>>(),
        [19, 18, 17]
    );
    assert_eq!(map.mirrored_char(0, 0), Some(')'));
    drag(&mut app, 19, 17);
    assert_eq!(app.selection_text_for_test().as_deref(), Some("(ab"));
}

#[test]
fn explicit_ltr_paragraph_never_reorders_right_to_left_text() {
    let mut app = app("\x1b[8l\u{05D0}\u{05D1} xy");
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(map.is_identity());
}

#[test]
fn with_reordering_off_every_paragraph_keeps_logical_columns() {
    let mut app = app("\x1b[8l\x1b[2 k(ab\r\n\x1b[8h\x1b[2 k\u{05D0}");
    for column in 0..COLUMNS {
        let (x, y) = pixel(column);
        app.pointer_move_for_test(x, y);
        assert_eq!(
            app.pointer_cell_for_test(),
            Some(CellPoint { row: 0, column })
        );
    }
    assert!(app.bidi_frame_map_for_test().is_none());
}

#[test]
fn a_settings_edit_of_the_direction_replans_the_next_frame() {
    let mut app = app("ab");
    app.present_bidi_frame_map_for_test();
    let before = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(before.is_identity());
    let settings = Settings {
        bidi_direction: BidiDirection::RightToLeft,
        ..Settings::default()
    };
    app.apply_overlay_settings_for_test(settings);
    app.present_bidi_frame_map_for_test();
    let after = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert_eq!(after.visual_column(0, 0), 18);
    assert_ne!(before.placement_hash(), after.placement_hash());
}
