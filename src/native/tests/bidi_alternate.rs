// SPDX-License-Identifier: GPL-3.0-only
//! The alternate screen under the bidi display gate, with project-authored
//! full-screen escape streams: an implicit program frame reorders its
//! right-to-left rows, an explicit-mode program (`CSI 8 l`) keeps the text it
//! arranged itself, and leaving the alternate screen restores the primary
//! layout with the primary rows' own properties.

use super::*;

use crate::selection::CellPoint;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 5;

fn app(output: &str) -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(output.as_bytes());
    app.set_test_cell_for_test(CELL);
    (app, terminal)
}

fn present(app: &mut App) -> crate::grid::BidiDisplayMap {
    app.present_bidi_frame_map_for_test();
    app.bidi_frame_map_for_test().expect("gate map").clone()
}

/// A full-screen frame: enter the alternate screen, clear, draw a title row
/// and a status row holding Hebrew, and park the cursor on row 2.
const TUI_FRAME: &str =
    "\x1b[?1049h\x1b[H\x1b[2Jtitle\x1b[5;1Hstatus \u{05D0}\u{05D1}\u{05D2} ok\x1b[3;1H";

#[test]
fn alt_screen_implicit_row_reorders() {
    let (mut app, _terminal) = app(TUI_FRAME);
    let map = present(&mut app);
    assert!(!map.row_is_reordered(0), "Latin title row");
    assert!(map.row_is_reordered(4), "the Hebrew status row reorders");
    // "status " is columns 0..=6; alef, bet, gimel at 7..=9 draw reversed.
    assert_eq!(
        (7..10).map(|c| map.visual_column(4, c)).collect::<Vec<_>>(),
        [9, 8, 7]
    );
    let (x, y) = (f64::from(CELL.width) * 9.5, f64::from(CELL.height) * 4.5);
    app.pointer_move_for_test(x, y);
    assert_eq!(
        app.pointer_cell_for_test(),
        Some(CellPoint { row: 4, column: 7 }),
        "the pointer over visual 9 addresses alef"
    );
}

#[test]
fn alt_screen_explicit_row_is_identity() {
    // Explicit mode sent before entering: every fresh row takes it. The
    // program prints Hebrew already in visual order, which must stay as sent.
    let before = "\x1b[8l\x1b[?1049h\x1b[H\u{05D2}\u{05D1}\u{05D0} xy";
    // Explicit mode sent after entering, then a clear stamps every row.
    let after = "\x1b[?1049h\x1b[8l\x1b[2J\x1b[H\u{05D2}\u{05D1}\u{05D0} xy";
    for stream in [before, after] {
        let (mut app, _terminal) = app(stream);
        let map = present(&mut app);
        assert!(map.is_identity(), "{stream:?}");
    }
}

#[test]
fn alt_screen_explicit_rtl_reverses_columns_exactly() {
    let (mut app, _terminal) = app("\x1b[8l\x1b[2 k\x1b[?1049h\x1b[H(ab");
    let map = present(&mut app);
    assert!(map.row_is_explicit(0));
    assert_eq!(
        (0..3).map(|c| map.visual_column(0, c)).collect::<Vec<_>>(),
        [19, 18, 17]
    );
    assert_eq!(map.mirrored_char(0, 0), Some(')'));
}

#[test]
fn leaving_alt_screen_restores_primary_layout() {
    // A right-to-left primary paragraph, then a program that enters the
    // alternate screen in explicit mode and restores implicit mode as it
    // leaves.
    let (mut app, terminal) = app("\x1b[2 k\u{05D0}\u{05D1} ab");
    let primary = present(&mut app);
    assert_eq!(primary.visual_column(0, 0), 19);
    terminal
        .lock()
        .expect("terminal")
        .advance("\x1b[8l\x1b[?1049h\x1b[H\u{05D2}\u{05D1}\u{05D0}".as_bytes());
    // The current direction is still right to left, so the program's rows
    // are explicit right to left: reversed column for column, not resolved.
    let alternate = present(&mut app);
    assert!(alternate.row_is_explicit(0), "the program's own order");
    assert_eq!(alternate.visual_column(0, 0), 19);
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b[?1049l\x1b[8h");
    let restored = present(&mut app);
    assert_eq!(restored.placement_hash(), primary.placement_hash());
}

#[test]
fn split_panes_reorder_their_alternate_screens() {
    let dims = Dimensions::new(COLUMNS, ROWS);
    let (mut app, first) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    first
        .lock()
        .expect("terminal")
        .advance(TUI_FRAME.as_bytes());
    let second = Arc::new(Mutex::new(Terminal::new(COLUMNS, ROWS)));
    second
        .lock()
        .expect("terminal")
        .advance(format!("\x1b[8l{TUI_FRAME}").as_bytes());
    let writer = crate::native::test_support::headless_writer();
    app.seed_headless_split_pane_for_test(true, second, writer, dims);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        (COLUMNS * 2) as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.set_bidi_display_for_test(true);
    let _ = app.rebuild_multipane_probe_for_test();
    let tokens = app.active_tab_pane_tokens_for_test();
    assert_eq!(tokens.len(), 2);
    let reordered: Vec<bool> = tokens
        .iter()
        .map(|token| {
            app.bidi_pane_map_for_test(*token)
                .expect("pane map")
                .row_is_reordered(4)
        })
        .collect();
    assert!(reordered.contains(&true), "the implicit pane reorders");
    assert!(
        reordered.contains(&false),
        "the explicit pane keeps its order"
    );
}
