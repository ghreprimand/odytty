// SPDX-License-Identifier: GPL-3.0-only
//! MOUSE-SCROLLBAR App-level press-routing precedence tests. They prove that a
//! left press on the *visible* scroll thumb grabs it to scrub scrollback and
//! wins over TUI mouse reporting, while every other press — including in a
//! mouse-reporting app, and at the live tail — routes exactly as before (the
//! byte-identical guarantee for the default/off paths).
//!
//! Headless (no GPU/window): the cell size the hit-test needs is injected via a
//! test seam, and the press is driven through the real `handle_mouse_input`
//! routing so the precedence is pinned, not reimplemented. Skipped when no PTY
//! is available (CI sandboxes).

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 10;

/// Build an `App` over a one-shot PTY, feed `content` into its terminal, and
/// inject the cell size so the pointer hit-test can run without a GPU. Mirrors
/// the `selection_extend` harness. Returns `None` when no PTY is available.
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

/// Build an app with enough output to create scrollback, then scroll up to the
/// oldest row so the thumb is visible at the track top. Returns the app plus the
/// rendered thumb quad (computed from the same geometry the press hit-tests),
/// or `None` when no PTY / no scrollback materialized.
fn app_scrolled_back() -> Option<(App, SolidQuad)> {
    let mut app = build_app(&b"scrollback line\r\n".repeat(80))?;
    app.scroll_up_for_test(usize::MAX); // clamps to the oldest row
    let len = app.scrollback_len_for_test();
    if len == 0 {
        return None;
    }
    let offset = app.viewport_offset_for_test();
    let thumb = scroll_indicator_quad(
        offset,
        len,
        Dimensions::new(COLS, ROWS),
        cell(CELL_W, CELL_H),
        [1.0, 1.0, 1.0, 0.62],
    )
    .expect("thumb visible while scrolled back");
    Some((app, thumb))
}

#[test]
fn press_on_thumb_grabs_and_beats_mouse_reporting() {
    let Some((mut app, thumb)) = app_scrolled_back() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // A mouse-reporting app is active...
    app.enable_mouse_reporting_for_test();
    assert!(app.would_report_mouse_to_pty_for_test());

    // ...but a press on the visible thumb grabs it locally. Precedence: the
    // scroll-thumb grab sits before the report branch, gated on a thumb hit.
    let cx = ((thumb.rect[0] + thumb.rect[2]) / 2.0) as f64;
    let cy = ((thumb.rect[1] + thumb.rect[3]) / 2.0) as f64;
    app.set_pointer_px_for_test(cx, cy);
    assert_eq!(app.left_button_outcome_for_test(true), "grab");
    assert_eq!(
        app.report_button_for_test(),
        None,
        "a thumb grab must not leak a PTY report"
    );

    // Releasing ends the grab cleanly without leaking a report.
    assert_eq!(app.left_button_outcome_for_test(false), "idle");
}

#[test]
fn press_off_thumb_still_reports_in_a_mouse_reporting_app() {
    let Some((mut app, thumb)) = app_scrolled_back() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.enable_mouse_reporting_for_test();

    // A press well left of the thumb's grab band (the text area) is not a thumb
    // grab, so it routes to the PTY report exactly as before — even though the
    // thumb is visible and scrollbar_drag is on.
    let off_x = (thumb.rect[0] / 2.0) as f64;
    let cy = ((thumb.rect[1] + thumb.rect[3]) / 2.0) as f64;
    app.set_pointer_px_for_test(off_x, cy);
    assert_eq!(app.left_button_outcome_for_test(true), "report");
}

#[test]
fn off_switch_does_not_grab_even_on_the_visible_thumb() {
    // Inverted-gate parity: with `scrollbar_drag = false`, a press on the
    // visible thumb does NOT grab — it falls through to a local selection,
    // byte-identical to before the feature existed.
    let Some((mut app, thumb)) = app_scrolled_back() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_scrollbar_drag_for_test(false);
    let cx = ((thumb.rect[0] + thumb.rect[2]) / 2.0) as f64;
    let cy = ((thumb.rect[1] + thumb.rect[3]) / 2.0) as f64;
    app.set_pointer_px_for_test(cx, cy);
    app.set_pointer_cell_for_test(0, COLS - 1);
    assert_eq!(
        app.left_button_outcome_for_test(true),
        "select",
        "off switch: a thumb press starts a local selection, not a grab"
    );
}

#[test]
fn press_at_live_tail_never_grabs() {
    // Default offset (live tail): the thumb is hidden, so a right-edge press
    // does not grab — keeping the plain press path byte-identical. With no thumb
    // to grab, the press starts a local selection (the historical behavior).
    let Some(mut app) = build_app(b"") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.viewport_offset_for_test(), 0, "starts at the live tail");
    // Press at the far right edge, where the thumb would be if it were visible.
    app.set_pointer_px_for_test((COLS as u32 * CELL_W) as f64 - 1.0, 5.0);
    app.set_pointer_cell_for_test(0, COLS - 1);
    assert_eq!(
        app.left_button_outcome_for_test(true),
        "select",
        "live tail: a right-edge press starts a local selection (no thumb to grab)"
    );
}
