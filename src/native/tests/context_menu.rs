// SPDX-License-Identifier: GPL-3.0-only
//! IN2 App-level right-click context-menu tests. These drive a real `App` (with
//! a one-shot PTY, skipped when none is available) through the production
//! `handle_mouse_input` routing, pinning the load-bearing traps from the
//! ratified design: the TUI passthrough gate (T1), the Shift override (T2),
//! main-overlay precedence (T3), off-path identity (T4), Copy gating (T5), and
//! the full activation → action path. Exhaustive `match` arms (T6) are enforced
//! by the compiler; the per-item gating/focus logic is unit-tested in
//! `context_menu_ui`.

use super::*;

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

fn enable_tui_mouse_reporting(terminal: &Arc<Mutex<Terminal>>) {
    let mut t = terminal.lock().expect("terminal");
    // DECSET 1000 + SGR 1006 — the mode where a no-overlay press reports to PTY.
    t.advance(b"\x1b[?1000h\x1b[?1006h");
}

#[test]
fn right_click_opens_menu_in_a_plain_shell() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(
        app.context_menu_open_for_test(),
        "right-click in a plain shell opens the context menu"
    );
    let sig = app.overlay_signature_for_test();
    assert_eq!(sig.mode, OverlayMode::ContextMenu);
    assert_eq!(
        sig.context_menu.spawn,
        (5, 10),
        "menu spawns at the click cell"
    );
}

#[test]
fn right_click_with_no_pointer_cell_does_not_open() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // No pointer cell injected: open is a no-op.
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(!app.context_menu_open_for_test());
}

/// T1 — in a TUI with mouse reporting active, right-click is reported to the
/// PTY (report gate, step 6) and the menu never opens (step 7 unreached).
#[test]
fn tui_right_click_reports_and_does_not_open_menu() {
    let Some((mut app, terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    enable_tui_mouse_reporting(&terminal);
    assert!(
        app.would_report_mouse_to_pty_for_test(),
        "precondition: reporting armed (TUI mouse mode, no Shift)"
    );
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(
        !app.context_menu_open_for_test(),
        "the report gate runs before the menu-open step"
    );
    assert!(
        app.report_button_for_test().is_some(),
        "the right-click was reported to the PTY instead"
    );
}

/// T2 — Shift+right-click bypasses the report gate (Shift is excluded from
/// `should_report_mouse_to_pty`), so the menu opens even inside a TUI.
#[test]
fn shift_right_click_opens_menu_even_in_a_tui() {
    let Some((mut app, terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    enable_tui_mouse_reporting(&terminal);
    app.set_shift_modifier_for_test(true);
    assert!(
        !app.would_report_mouse_to_pty_for_test(),
        "precondition: Shift suppresses reporting"
    );
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(
        app.context_menu_open_for_test(),
        "Shift+right-click opens the menu in a TUI"
    );
    assert!(
        app.report_button_for_test().is_none(),
        "and nothing was reported to the PTY"
    );
}

/// T3 — with the settings overlay already open, a right-click is routed to the
/// overlay handler (step 1), never reaching the context-menu open step.
#[test]
fn right_click_while_settings_overlay_open_does_not_open_context_menu() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.open_settings_overlay_for_test();
    let rect = app.overlay_rect_for_test().expect("overlay rect");
    // The top-left corner is inside the panel box but on the border (inert), so
    // the overlay consumes the press and stays open in Settings mode.
    app.set_pointer_cell_for_test(rect.top, rect.left);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    let sig = app.overlay_signature_for_test();
    assert_eq!(
        sig.mode,
        OverlayMode::Settings,
        "the right-click went to the open overlay, not the context menu"
    );
    assert!(!app.context_menu_open_for_test());
}

/// T4 — off-path identity: a fresh App has the menu closed and its sub-signature
/// at the default, so the closed-overlay render is byte-identical to today.
#[test]
fn off_path_signature_is_default_and_closed() {
    let Some((app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let sig = app.overlay_signature_for_test();
    assert_ne!(sig.mode, OverlayMode::ContextMenu);
    assert_eq!(
        sig.context_menu,
        ContextMenuSignature::default(),
        "no right-click ⇒ default (inert) context-menu signature"
    );
}

/// T5 — Copy is gated on a live selection: disabled with none, enabled with one.
#[test]
fn copy_gating_reflects_live_selection() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // No selection: Copy disabled.
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(
        !app.overlay_signature_for_test().context_menu.copy_enabled,
        "Copy is disabled with no selection"
    );
    app.close_overlay_for_test();

    // With a selection: Copy enabled. Opening the menu must NOT clear it.
    app.force_selection_for_test(0, 0, 2, 4);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(
        app.overlay_signature_for_test().context_menu.copy_enabled,
        "Copy is enabled when a selection exists"
    );
    assert!(
        app.selection_range_for_test().is_some(),
        "opening the menu preserves the selection Copy needs"
    );
}

/// Full activation path: clicking Select All closes the menu and selects the
/// whole buffer (the absolute range from row 0 to the last visible row).
#[test]
fn clicking_select_all_selects_whole_buffer_and_closes() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    // Menu spawns at (5,10): width 14 fits, so left=10, top=5, items at rows
    // 6 (Copy), 7 (Paste), 8 (Select All). Click Select All.
    app.set_pointer_cell_for_test(8, 12);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);

    assert!(
        !app.context_menu_open_for_test(),
        "activating an item closes the menu"
    );
    let range = app
        .selection_range_for_test()
        .expect("Select All set a selection");
    assert_eq!(
        (range.0, range.1),
        (0, 0),
        "selection starts at the buffer top"
    );
    // The end column is the last grid column (80-wide grid ⇒ column 79).
    assert_eq!(range.3, 79, "selection spans to the last column");
}

/// Clicking outside the open menu dismisses it (existing out-of-rect path).
#[test]
fn clicking_outside_the_menu_dismisses_it() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    // Far from the menu box (which occupies rows 5..10, cols 10..24).
    app.set_pointer_cell_for_test(0, 0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    assert!(
        !app.context_menu_open_for_test(),
        "a click outside the menu dismisses it"
    );
}
