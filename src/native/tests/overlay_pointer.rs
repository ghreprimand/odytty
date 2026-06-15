// SPDX-License-Identifier: GPL-3.0-only
//! UX4-P1 App-level pointer-precedence tests: an open overlay captures the
//! mouse before selection / PTY mouse-reporting, the exact mouse analogue of
//! the keyboard overlay guard. These drive a real `App` (with a one-shot PTY,
//! skipped when none is available) through the same handlers the `window_event`
//! arms call.

use super::*;
use winit::event::ElementState;

/// Build an `App` over a one-shot PTY, returning the App plus a clone of the
/// shared terminal handle (so a test can drive it into a mouse-reporting mode),
/// or `None` when no PTY is available (CI sandboxes); callers then skip.
fn app_for_test() -> Option<(App, Arc<Mutex<Terminal>>)> {
    let dims = Dimensions::new(80, 24);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let app = App::new(
        NativeOptions::default(),
        terminal.clone(),
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, terminal))
}

#[test]
fn overlay_open_press_is_captured_and_neither_selects_nor_reports() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.open_settings_overlay_for_test();
    let rect = app.overlay_rect_for_test().expect("overlay rect");
    // Body row 0 is the first group header; row 1 is the theme value line.
    app.set_pointer_cell_for_test(rect.body_top + 1, rect.body_left);

    app.handle_overlay_pointer_button(ElementState::Pressed, WinitMouseButton::Left);

    // The press was captured by the overlay (theme click opens the picker)...
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::ThemePicker,
        "overlay consumed the click"
    );
    // ...and it neither started a local selection nor armed PTY mouse-reporting.
    assert!(
        !app.selecting_for_test(),
        "no selection while overlay is open"
    );
    assert!(
        app.report_button_for_test().is_none(),
        "no PTY mouse report while overlay is open"
    );
}

#[test]
fn overlay_press_does_not_leak_to_pty_even_with_mouse_reporting_enabled() {
    let Some((mut app, terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Drive the terminal into a TUI mouse-reporting mode (DECSET 1000 + SGR
    // 1006), the case where a no-overlay press WOULD be reported to the PTY.
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(b"\x1b[?1000h\x1b[?1006h");
    }
    assert!(
        app.would_report_mouse_to_pty_for_test(),
        "precondition: reporting is armed (TUI mouse mode, no Shift)"
    );

    // Open the overlay and click a value row inside it.
    app.open_settings_overlay_for_test();
    let rect = app.overlay_rect_for_test().expect("overlay rect");
    app.set_pointer_cell_for_test(rect.body_top + 1, rect.body_left);
    app.handle_overlay_pointer_button(ElementState::Pressed, WinitMouseButton::Left);

    // Reporting was armed, yet the overlay captured the press: no report button
    // was held and no selection began — the click never reached the PTY path.
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::ThemePicker,
        "overlay captured the press ahead of the report decision"
    );
    assert!(
        app.report_button_for_test().is_none(),
        "no PTY mouse report despite reporting being enabled"
    );
    assert!(!app.selecting_for_test(), "no local selection either");
}

#[test]
fn overlay_open_wheel_scrolls_the_panel_not_the_scrollback() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.open_settings_overlay_for_test();
    let before = app.overlay_signature_for_test().panel.scroll;

    // Wheel down (negative line delta) advances the settings list.
    app.handle_overlay_pointer_wheel(MouseScrollDelta::LineDelta(0.0, -1.0));

    let after = app.overlay_signature_for_test().panel.scroll;
    assert!(after > before, "wheel scrolled the overlay list");
    assert!(!app.selecting_for_test(), "wheel did not start a selection");
}

#[test]
fn overlay_slider_drag_routes_through_cursor_move_and_release() {
    // UX4-P2: an open-overlay slider drag is driven by the same window-event
    // arms as P1 — MouseInput (press/release) and CursorMoved (move) — and the
    // move only advances an armed drag. Drive the real App handlers end to end.
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.open_settings_overlay_for_test();
    // Scroll a numeric (slider) row into the capped panel window via the wheel.
    for _ in 0..8 {
        app.handle_overlay_pointer_wheel(MouseScrollDelta::LineDelta(0.0, -1.0));
    }
    let Some((track_left, track_right)) = app.overlay_first_slider_track_cells_for_test() else {
        eprintln!("skipping: no slider row visible at this size");
        return;
    };

    let value_of = |app: &App, key: &str| -> f32 {
        app.overlay_signature_for_test()
            .panel
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.value.parse::<f32>().ok())
            .expect("numeric value parses")
    };

    // A hover move before any press is a no-op (not dragging).
    app.set_pointer_cell_for_test(track_left.row, track_left.column);
    app.handle_overlay_pointer_move();
    assert!(!app.overlay_is_dragging_for_test(), "hover does not drag");

    // Press the far-right of the track → arms the drag and pushes the value up.
    app.set_pointer_cell_for_test(track_right.row, track_right.column);
    app.handle_overlay_pointer_button(ElementState::Pressed, WinitMouseButton::Left);
    assert!(
        app.overlay_is_dragging_for_test(),
        "track press arms the drag"
    );
    let high = value_of(&app, "font_size");

    // CursorMoved to the far-left of the track → drives the value down.
    app.set_pointer_cell_for_test(track_left.row, track_left.column);
    app.handle_overlay_pointer_move();
    let low = value_of(&app, "font_size");
    assert!(low < high, "drag left lowered the value ({low} < {high})");

    // Release ends the drag; a subsequent move no longer changes the value.
    app.handle_overlay_pointer_button(ElementState::Released, WinitMouseButton::Left);
    assert!(!app.overlay_is_dragging_for_test(), "release ends the drag");
    app.set_pointer_cell_for_test(track_right.row, track_right.column);
    app.handle_overlay_pointer_move();
    assert_eq!(
        value_of(&app, "font_size"),
        low,
        "no value change after release"
    );
    // The drag never armed a PTY report or a local selection.
    assert!(app.report_button_for_test().is_none());
    assert!(!app.selecting_for_test());
}

#[test]
fn focus_loss_cancels_an_armed_overlay_slider_drag() {
    // Regression (UX4-P2): focus loss mid-drag while the overlay stays open
    // (e.g. alt-tab while pressing a slider) must abandon the drag through the
    // real `WindowEvent::Focused(false)` seam, or the next bare CursorMoved on
    // focus regain commits a phantom value — the overlay-stays-open analogue of
    // the lost-release close/reopen case. Drives the production helper end to
    // end (not a parallel reimplementation).
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.open_settings_overlay_for_test();
    // Scroll the settings list one notch at a time until a numeric (slider) row
    // enters the capped panel window, stopping at the FIRST one. Scrolling a
    // fixed large amount overshoots past every slider into the non-numeric tail
    // (the sliders sit near the top), which is why the blanket-scroll sibling
    // test skips at this size.
    let mut tracks = app.overlay_first_slider_track_cells_for_test();
    for _ in 0..12 {
        if tracks.is_some() {
            break;
        }
        app.handle_overlay_pointer_wheel(MouseScrollDelta::LineDelta(0.0, -1.0));
        tracks = app.overlay_first_slider_track_cells_for_test();
    }
    let Some((track_left, track_right)) = tracks else {
        eprintln!("skipping: no slider row visible at this size");
        return;
    };

    // Press the far-right of the track → arms the drag.
    app.set_pointer_cell_for_test(track_right.row, track_right.column);
    app.handle_overlay_pointer_button(ElementState::Pressed, WinitMouseButton::Left);
    assert!(
        app.overlay_is_dragging_for_test(),
        "track press arms the drag"
    );

    // Focus loss WITHOUT a release (the lost-release case): the overlay stays
    // open, but the drag must be cancelled via the production focus handler.
    app.cancel_overlay_drag_on_focus_loss_for_test();
    assert!(
        !app.overlay_is_dragging_for_test(),
        "focus loss cancels the drag"
    );

    // Snapshot the full overlay signature, then on focus regain drive a bare
    // CursorMoved to the far-left of the track: it must be inert — no phantom
    // value commit (signature unchanged), no re-armed drag/selection. The
    // snapshot is key-agnostic, so it holds for whichever slider was first.
    let before = app.overlay_signature_for_test();
    app.set_pointer_cell_for_test(track_left.row, track_left.column);
    app.handle_overlay_pointer_move();
    assert!(
        !app.overlay_is_dragging_for_test(),
        "hover after focus regain does not re-arm the drag"
    );
    assert_eq!(
        app.overlay_signature_for_test(),
        before,
        "no phantom value change after focus loss"
    );
    assert!(app.report_button_for_test().is_none());
    assert!(!app.selecting_for_test());
}

#[test]
fn opening_overlay_clears_a_held_tui_report_button() {
    // Regression (UX4-P1 review): a TUI mouse press arms `report_button`; the
    // user can then open an overlay from the keyboard while the physical button
    // is still held. Overlay presses short-circuit before the reported-input
    // path and overlay releases are inert, so without clearing on entry the
    // held button would survive the overlay and re-enter the stale-button
    // motion path after it closes.
    let Some((mut app, terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Arm TUI reporting (DECSET 1000 + SGR 1006) and simulate a held press.
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(b"\x1b[?1000h\x1b[?1006h");
    }
    app.set_pointer_cell_for_test(5, 5);
    app.arm_reported_mouse_press_for_test(CoreMouseButton::Left);
    assert_eq!(
        app.report_button_for_test(),
        Some(CoreMouseButton::Left),
        "precondition: a TUI press holds the report button"
    );

    // Open the overlay from the keyboard while the button is still held.
    app.open_settings_overlay_for_test();
    assert!(
        app.report_button_for_test().is_none(),
        "overlay entry clears the stale held report button"
    );

    // A release routed through the overlay path stays inert and cannot re-arm.
    app.handle_overlay_pointer_button(ElementState::Released, WinitMouseButton::Left);
    assert!(
        app.report_button_for_test().is_none(),
        "overlay release does not re-arm the report button"
    );

    // After the overlay closes the button is still clear, so a later pointer
    // motion cannot re-enter the held-button motion-report path.
    app.close_overlay_for_test();
    assert!(
        app.report_button_for_test().is_none(),
        "no stale report button survives the overlay"
    );
}
