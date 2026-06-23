// SPDX-License-Identifier: GPL-3.0-only
//! CURSOR-ICON: the mouse-cursor shape the pointer path selects over the
//! terminal grid. Before this, OdyTTY never called `set_cursor`, so the pointer
//! stayed the OS default arrow everywhere; these tests pin the standard
//! affordance — an I-beam over selectable text, the arrow while a TUI owns the
//! mouse, and a hand over a hovered hyperlink.
//!
//! Headless (no GPU/window): the cell size is injected via a test seam and the
//! production `update_pointer_cell` handler is driven directly. Skipped when no
//! PTY is available (CI sandboxes).

use super::*;
use winit::window::CursorIcon;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 10;

/// Build an `App` over a one-shot PTY, feed `content` into its terminal, and
/// inject the cell size so the pointer hit-test runs without a GPU.
fn build_app(content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    let pty = Arc::new(Mutex::new(session));
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    Some(app)
}

/// Build a two-pane split `App` headlessly: spawn two one-shot PTYs, seed the
/// active tab into a `columns`/rows split, and inject the cell size + surface
/// geometry so `multipane_geometry()` (and the divider hover/drag cursor path)
/// resolves without a GPU. Surface is exactly `COLS×ROWS` cells, zero padding,
/// one tab (no tab bar), so the content rect is `(0,0, COLS·CELL_W, ROWS·CELL_H)`
/// and the lone even-ratio divider sits at its midpoint.
type PaneParts = (Arc<Mutex<Terminal>>, PtyWriter, Arc<Mutex<PtySession>>);

fn build_split_app(columns: bool) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let make = || -> Option<PaneParts> {
        let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
        let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(session));
        Some((terminal, writer, pty))
    };
    let (t1, w1, p1) = make()?;
    let mut app = App::new(
        NativeOptions::default(),
        t1,
        w1,
        p1,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let (t2, w2, p2) = make()?;
    app.seed_split_pane_for_test(columns, t2, w2, p2);
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    app.set_test_surface_for_test(
        COLS as u32 * CELL_W,
        ROWS as u32 * CELL_H,
        crate::native::WindowPadding::ZERO,
    );
    Some(app)
}

// Midpoint of the content rect — where the lone even-ratio divider sits.
const MID_X: f64 = (COLS as f64 * CELL_W as f64) / 2.0;
const MID_Y: f64 = (ROWS as f64 * CELL_H as f64) / 2.0;

#[test]
fn pointer_over_a_column_divider_is_a_col_resize() {
    // A column split (panes side-by-side) draws a vertical divider; hovering its
    // grab band shows the horizontal-resize cursor so drag-to-resize is
    // discoverable.
    let Some(mut app) = build_split_app(true) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(MID_X, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::ColResize);
}

#[test]
fn pointer_over_a_row_divider_is_a_row_resize() {
    // A row split (panes stacked) draws a horizontal divider; hovering its grab
    // band shows the vertical-resize cursor.
    let Some(mut app) = build_split_app(false) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(MID_X, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::RowResize);
}

#[test]
fn pointer_over_grid_in_a_split_is_still_an_i_beam() {
    // Off the divider grab band, a split pane is plain selectable text — the
    // I-beam, exactly as a single pane. The resize cursor is confined to the
    // divider.
    let Some(mut app) = build_split_app(true) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Far left of the vertical divider at the content midpoint.
    app.pointer_move_for_test(f64::from(CELL_W) * 2.5, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
}

#[test]
fn single_pane_never_shows_a_resize_cursor() {
    // BYTE-IDENTITY GUARD: a single-pane tab has no dividers, so the resize
    // branch never fires even at the exact pixel a split's divider would occupy
    // — the plain path stays an I-beam.
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(MID_X, MID_Y);
    let icon = app.cursor_icon_for_test();
    assert_ne!(icon, CursorIcon::ColResize);
    assert_ne!(icon, CursorIcon::RowResize);
    assert_eq!(icon, CursorIcon::Text);
}

#[test]
fn dragging_a_divider_keeps_the_resize_cursor() {
    // Pressing on the divider grabs it (the real press path), and subsequent
    // motion keeps the matching resize cursor for the whole gesture — even if
    // the pointer strays slightly off the hairline.
    use winit::event::MouseButton as WinitMouseButton;
    let Some(mut app) = build_split_app(true) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_px_for_test(MID_X, MID_Y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    // Drag a few pixels off the exact divider line; cursor stays ColResize.
    app.pointer_move_for_test(MID_X + 3.0, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::ColResize);
}

#[test]
fn pointer_over_plain_grid_is_an_i_beam() {
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Move into the middle of the grid: plain selectable text, no link, no TUI
    // mouse reporting -> the I-beam.
    app.pointer_move_for_test(f64::from(CELL_W) * 3.5, f64::from(CELL_H) * 2.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
}

#[test]
fn pointer_shows_arrow_while_tui_owns_the_mouse() {
    // A TUI enables mouse reporting (DECSET 1000): clicks belong to the app, so
    // the I-beam would mislead -> the arrow.
    let Some(mut app) = build_app(b"\x1b[?1000hhello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(f64::from(CELL_W) * 4.5, f64::from(CELL_H) * 2.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Default);
}

#[test]
fn pointer_over_hovered_hyperlink_is_a_hand() {
    // Print an OSC 8 hyperlink so the first cells carry a link id.
    let Some(mut app) = build_app(b"\x1b]8;;https://example.com\x07LINK\x1b]8;;\x07") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Hover the first cell of the link run (row 0, col 0) -> the hand.
    app.pointer_move_for_test(f64::from(CELL_W) * 0.5, f64::from(CELL_H) * 0.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Pointer);
    // Move off the link onto plain space -> back to the I-beam.
    app.pointer_move_for_test(f64::from(CELL_W) * 20.5, f64::from(CELL_H) * 5.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
}
