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

    app.handle_overlay_pointer_press(ElementState::Pressed, WinitMouseButton::Left);

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
    app.handle_overlay_pointer_press(ElementState::Pressed, WinitMouseButton::Left);

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
    app.handle_overlay_pointer_press(ElementState::Released, WinitMouseButton::Left);
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
