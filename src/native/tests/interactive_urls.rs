// SPDX-License-Identifier: GPL-3.0-only
//! INTERACTIVE-URLS: App-level wiring tests for bare-URL hover + Ctrl+click open.
//!
//! These drive the production pointer/hover path headlessly to pin the WIRING:
//! that an openable bare URL under the pointer latches `hovered_url` when the
//! feature is on, that the off path never scans (byte-identical), that a
//! non-openable scheme and an OSC 8 hyperlink cell never latch the bare-URL
//! decoration, and that Ctrl+hover arms the shared underline over the URL span.
//! The URL detection itself (scheme set, trailing-trim, spans) is unit-tested in
//! `crate::hints`; this only pins the App glue.
//!
//! Synthetic only: a bare URL is printed into the grid and resolved against the
//! in-memory snapshot — no network, no filesystem. Skipped when no PTY is
//! available (CI sandboxes).

use super::*;
use winit::window::CursorIcon;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 10;

// "see https://example.com here" — the URL occupies columns 4..=22.
const LINE: &[u8] = b"see https://example.com here";
const URL: &str = "https://example.com";
const URL_START_COL: usize = 4;
const URL_END_COL: usize = 22; // inclusive last cell of the 19-char URL

fn build_app(content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
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

/// Move the pointer onto the middle of the URL span at row 0.
fn hover_on_url(app: &mut App) {
    let col = (URL_START_COL + URL_END_COL) / 2;
    app.pointer_move_for_test(
        f64::from(CELL_W) * (col as f64 + 0.5),
        f64::from(CELL_H) * 0.5,
    );
}

#[test]
fn bare_url_latches_when_on_by_default() {
    let Some(mut app) = build_app(LINE) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // interactive_urls defaults on, so no explicit enable.
    hover_on_url(&mut app);
    assert_eq!(
        app.hovered_url_for_test(),
        Some(URL),
        "an openable bare URL under the pointer latches hovered_url"
    );
}

#[test]
fn bare_url_inert_when_off() {
    let Some(mut app) = build_app(LINE) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_urls_for_test(false);
    hover_on_url(&mut app);
    assert_eq!(
        app.hovered_url_for_test(),
        None,
        "with interactive_urls off the scan never runs and nothing latches"
    );
}

#[test]
fn non_openable_scheme_does_not_latch() {
    // ftp:// is detected by the hints scanner but is not on the open allowlist,
    // so it must not light the interactive-URL affordance.
    let Some(mut app) = build_app(b"get ftp://example.com/file now") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(f64::from(CELL_W) * 10.5, f64::from(CELL_H) * 0.5);
    assert_eq!(
        app.hovered_url_for_test(),
        None,
        "a non-openable scheme (ftp) never latches the interactive-URL affordance"
    );
}

#[test]
fn osc8_hyperlink_cell_wins_no_bare_url() {
    // An explicit OSC 8 hyperlink is handled by the OSC 8 path; the bare-URL
    // scan must not also latch on the same cell (no double-decoration).
    let Some(mut app) = build_app(b"\x1b]8;;https://example.com\x07LINK\x1b]8;;\x07") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Hover over the linked text (columns 0..=3 = "LINK").
    app.pointer_move_for_test(f64::from(CELL_W) * 1.5, f64::from(CELL_H) * 0.5);
    assert_eq!(
        app.cursor_icon_for_test(),
        CursorIcon::Pointer,
        "precondition: the OSC 8 hyperlink is hovered (hand cursor)"
    );
    assert_eq!(
        app.hovered_url_for_test(),
        None,
        "the OSC 8 path wins; the bare-URL scan does not double-decorate the cell"
    );
}

#[test]
fn ctrl_hover_arms_underline_over_url() {
    let Some(mut app) = build_app(LINE) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    hover_on_url(&mut app);

    // Plain hover: hand cursor only, no armed underline.
    assert_eq!(
        app.armed_underline_cells_for_test(),
        None,
        "plain hover does not arm the underline"
    );

    // Open-modifier + hover: the shared armed underline lights the exact URL
    // span. The open modifier is platform-aware (Cmd on macOS, Ctrl on Linux,
    // per `open_modifier_held`), so arm whichever the host actually checks.
    #[cfg(target_os = "macos")]
    app.set_super_key_for_test(true);
    #[cfg(not(target_os = "macos"))]
    app.set_ctrl_modifier_for_test(true);
    assert_eq!(
        app.armed_underline_cells_for_test(),
        Some((0, URL_START_COL, URL_END_COL + 1)),
        "open-modifier hover arms the underline over the bare-URL span"
    );
}
