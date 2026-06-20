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
