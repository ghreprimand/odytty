// SPDX-License-Identifier: GPL-3.0-only
//! IN2 App-level right-click context-menu tests. These drive a real `App` (with
//! a one-shot PTY, skipped when none is available) through the production
//! `handle_mouse_input` routing, pinning the load-bearing traps from the
//! ratified design: the TUI passthrough gate (T1), the Shift override (T2),
//! main-overlay precedence (T3), off-path identity (T4), Copy gating (T5), and
//! the full activation → action path. Exhaustive `match` arms (T6) are enforced
//! by the compiler; the per-item gating/focus logic is unit-tested in
//! `context_menu_ui`.

use super::super::context_menu_ui::{
    CONTEXT_MENU_BODY_ROWS, CONTEXT_MENU_FOURTH_SEPARATOR_ROW, CONTEXT_MENU_SECOND_SEPARATOR_ROW,
    CONTEXT_MENU_SEPARATOR_ROW, CONTEXT_MENU_THIRD_SEPARATOR_ROW, ContextMenuRow, ContextMenuUi,
};
use super::super::pty::UserEvent;
use super::super::session::{Session, SessionToken, TabSet};
use super::*;
use winit::event_loop::EventLoop;
#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

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

fn app_for_test_with_proxy() -> Option<(App, EventLoop<UserEvent>)> {
    let dims = Dimensions::new(80, 24);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
    }
    let event_loop = builder.build().ok()?;
    let proxy = event_loop.create_proxy();
    let sessions = TabSet::new(
        Session::new(SessionToken(0), terminal, writer, pty, None),
        Some(proxy),
    );
    let app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, event_loop))
}

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn app_with_recording_writer(content: &[u8]) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(80, 24);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let _ = session.take_writer().ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    terminal.lock().expect("terminal").advance(content);
    let pty = Arc::new(Mutex::new(session));
    let app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, bytes))
}

/// Click the open context-menu item whose rendered row contains `needle`,
/// resolving its grid row from the live composited menu at the App's real grid
/// dims (so the rect/edge-clamp matches the click path exactly) and clicking at
/// the rect-derived body column. Robust to layout shifts — the v0.3.1 launcher
/// section changed item rows and the edge-clamped top, so hardcoded grid rows
/// would silently drift onto the wrong item.
fn click_menu_item(app: &mut App, needle: &str) {
    let (cols, rows) = app.grid_dims_for_test();
    let rendered = app.render_overlay_rows_for_test(cols, rows);
    let grid_row = rendered
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("menu item {needle:?} not visible in {rendered:?}"));
    let rect = app.overlay_rect_for_test().expect("context menu open");
    app.set_pointer_cell_for_test(grid_row, rect.body_left);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
}

/// The grid row of the `nth` (0-based) separator line in the live composited
/// menu, for the separator-inert test. Uses the box-drawing glyph the renderer
/// paints for separators.
fn separator_grid_row(app: &mut App, nth: usize) -> usize {
    let (cols, rows) = app.grid_dims_for_test();
    let rendered = app.render_overlay_rows_for_test(cols, rows);
    rendered
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains('\u{2500}'))
        .map(|(i, _)| i)
        .nth(nth)
        .unwrap_or_else(|| panic!("separator #{nth} not found in {rendered:?}"))
}

fn enable_tui_mouse_reporting(terminal: &Arc<Mutex<Terminal>>) {
    let mut t = terminal.lock().expect("terminal");
    // DECSET 1000 + SGR 1006 — the mode where a no-overlay press reports to PTY.
    t.advance(b"\x1b[?1000h\x1b[?1006h");
}

#[test]
fn context_menu_rows_include_tab_split_items_and_three_separators() {
    let mut menu = ContextMenuUi::new();
    menu.open(
        CellPoint { row: 5, column: 10 },
        false,
        false,
        false,
        false,
        None,
        false,
    );

    let rows = menu.rows();
    assert_eq!(rows.len(), CONTEXT_MENU_BODY_ROWS);
    assert!(matches!(
        rows[CONTEXT_MENU_SEPARATOR_ROW],
        ContextMenuRow::Separator
    ));
    assert!(matches!(
        rows[CONTEXT_MENU_SECOND_SEPARATOR_ROW],
        ContextMenuRow::Separator
    ));
    assert!(matches!(
        rows[CONTEXT_MENU_THIRD_SEPARATOR_ROW],
        ContextMenuRow::Separator
    ));
    assert!(matches!(
        rows[6],
        ContextMenuRow::Item {
            label: "New Tab",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[7],
        ContextMenuRow::Item {
            label: "Rename Tab",
            enabled: false,
            ..
        }
    ));
    assert!(matches!(
        rows[8],
        ContextMenuRow::Item {
            label: "Close Tab",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[10],
        ContextMenuRow::Item {
            label: "Split Right",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[11],
        ContextMenuRow::Item {
            label: "Split Down",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[13],
        ContextMenuRow::Item {
            label: "Settings",
            enabled: true,
            ..
        }
    ));
    // v0.3.1 launcher section: a fourth separator, then the three always-enabled
    // launcher items below Settings.
    assert!(matches!(
        rows[CONTEXT_MENU_FOURTH_SEPARATOR_ROW],
        ContextMenuRow::Separator
    ));
    assert!(matches!(
        rows[15],
        ContextMenuRow::Item {
            label: "Connection Manager",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[16],
        ContextMenuRow::Item {
            label: "Command Palette",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[17],
        ContextMenuRow::Item {
            label: "Session Replay",
            enabled: true,
            ..
        }
    ));
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
fn right_clicking_tab_enables_rename_for_that_tab() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let dims = Dimensions::new(80, 24);
    let Some(session) = PtySession::spawn_shell_command(dims, "sleep 1").ok() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().expect("writer")));
    let terminal2 = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty2 = Arc::new(Mutex::new(session));
    app.push_session_for_test(terminal2, writer, pty2);
    app.set_session_tab_title_for_test(0, "first-tab");
    app.set_session_tab_title_for_test(1, "second-tab");
    app.set_test_cell_for_test(cell(8, 16));

    app.set_pointer_px_for_test(12.0, 8.0);
    app.set_pointer_cell_for_test(0, 0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(app.context_menu_open_for_test());
    assert!(
        app.overlay_signature_for_test().context_menu.rename_enabled,
        "right-clicking a tab body enables Rename Tab"
    );

    // Menu spawned at top-left: body row 7 = Rename Tab => grid row 8.
    app.set_pointer_cell_for_test(8, 2);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);

    assert!(app.rename_active_for_test());
    assert_eq!(app.rename_text_for_test().as_deref(), Some("first-tab"));
}

#[test]
fn plain_context_menu_open_disables_rename_tab() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let dims = Dimensions::new(80, 24);
    let Some(session) = PtySession::spawn_shell_command(dims, "sleep 1").ok() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().expect("writer")));
    let terminal2 = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty2 = Arc::new(Mutex::new(session));
    app.push_session_for_test(terminal2, writer, pty2);
    app.set_test_cell_for_test(cell(8, 16));

    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(app.context_menu_open_for_test());
    assert!(
        !app.overlay_signature_for_test().context_menu.rename_enabled,
        "terminal-area right-click has no tab target"
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

#[test]
fn cut_delete_gating_requires_editable_prompt_input() {
    let Some((mut app, _bytes)) = app_with_recording_writer(b"\x1b]133;A\x07$ \x1b]133;B\x07abc")
    else {
        return;
    };
    app.set_pointer_cell_for_test(5, 10);

    app.force_selection_for_test(0, 0, 0, 4);
    assert_eq!(
        app.editable_input_selection_text_for_test().as_deref(),
        Some("abc"),
        "selection spanning prompt text clips to editable input"
    );
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    let sig = app.overlay_signature_for_test().context_menu;
    assert!(sig.cut_enabled, "Cut is enabled for clipped input");
    assert!(sig.delete_enabled, "Delete is enabled for clipped input");
    app.close_overlay_for_test();

    app.force_selection_for_test(0, 0, 0, 1);
    assert_eq!(
        app.editable_input_selection_text_for_test(),
        None,
        "prompt-only selection has no editable clipped text"
    );
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    let sig = app.overlay_signature_for_test().context_menu;
    assert!(!sig.cut_enabled, "Cut is disabled for prompt-only text");
    assert!(
        !sig.delete_enabled,
        "Delete is disabled for prompt-only text"
    );
}

#[test]
fn delete_selected_input_sends_shell_edit_bytes_without_copying() {
    let Some((mut app, bytes)) = app_with_recording_writer(b"\x1b]133;A\x07$ \x1b]133;B\x07abc")
    else {
        return;
    };
    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    click_menu_item(&mut app, "Delete");

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "Delete moves to the clipped input start, then deletes only the clipped text"
    );
    assert!(!app.context_menu_open_for_test());
    assert!(
        app.selection_range_for_test().is_none(),
        "editing clears the stale visual selection"
    );
}

/// D-IN2-CUT-SAFE: if `write_text` returns `None`, Cut must NOT delete the
/// editable input and must NOT clear the selection. The menu closes (item was
/// activated) but the text and selection survive intact.
#[test]
fn cut_clipboard_failure_leaves_input_and_selection_intact() {
    let Some((mut app, bytes)) = app_with_recording_writer(b"\x1b]133;A\x07$ \x1b]133;B\x07abc")
    else {
        return;
    };
    // Force clipboard writes to fail so we exercise the fail-safe path.
    app.force_clipboard_write_fail_for_test();

    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    // Click Cut.
    click_menu_item(&mut app, "Cut");

    // Menu closes when any item is activated.
    assert!(
        !app.context_menu_open_for_test(),
        "menu closes on Cut click"
    );

    // No PTY bytes written — the delete step was skipped.
    let written = bytes.lock().expect("bytes").clone();
    assert!(
        written.is_empty(),
        "clipboard failure: no PTY bytes should be written"
    );

    // Selection preserved — not cleared by the failed Cut.
    assert!(
        app.selection_range_for_test().is_some(),
        "clipboard failure: selection must remain intact"
    );
}

/// Cut happy-path (when clipboard is available): same PTY bytes as Delete, plus
/// the selection is cleared. This test is skipped gracefully when no clipboard
/// or PTY is available in the test environment.
#[test]
fn cut_selected_input_sends_edit_bytes_when_clipboard_succeeds() {
    let Some((mut app, bytes)) = app_with_recording_writer(b"\x1b]133;A\x07$ \x1b]133;B\x07abc")
    else {
        return;
    };

    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    // Click Cut.
    click_menu_item(&mut app, "Cut");
    assert!(!app.context_menu_open_for_test(), "menu closes on Cut");

    let written = bytes.lock().expect("bytes").clone();
    if written.is_empty() {
        // Clipboard unavailable in this environment — skip gracefully.
        eprintln!("skipping: clipboard write failed (no display/clipboard daemon)");
        return;
    }

    // Same edit bytes as Delete: move to start, then delete characters.
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "Cut moves to the clipped input start, then deletes only the clipped text"
    );
    assert!(
        app.selection_range_for_test().is_none(),
        "successful Cut clears the stale visual selection"
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

    click_menu_item(&mut app, "Select All");

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

// Linux-only at runtime: the proxy harness (`app_for_test_with_proxy`) builds a
// real winit `EventLoop` off the test thread. Linux permits that via the
// `with_any_thread` builder shim; macOS has no equivalent because AppKit must
// own the main thread, so constructing/using the loop off-main-thread aborts
// (SIGSEGV). The new-tab path itself runs on the main thread in production and
// is exercised on Linux here; the test stays compiled on macOS (so the helper
// and its imports are still used) but is skipped at runtime.
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS (AppKit main-thread requirement)"
)]
#[test]
fn clicking_new_tab_spawns_session_and_closes_menu() {
    let Some((mut app, _event_loop)) = app_for_test_with_proxy() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.session_count_for_test(), 1);
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    click_menu_item(&mut app, "New Tab");

    assert!(!app.context_menu_open_for_test());
    assert_eq!(app.session_count_for_test(), 2);
    assert_eq!(app.active_session_id_for_test(), 1);
}

#[test]
fn clicking_close_tab_closes_active_session_and_keeps_neighbor_active() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let dims = Dimensions::new(80, 24);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok();
    let Some(session) = session else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().expect("writer")));
    let terminal2 = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty2 = Arc::new(Mutex::new(session));
    app.push_session_for_test(terminal2, writer, pty2);
    assert!(app.switch_to_session_for_test(1));
    assert_eq!(app.session_count_for_test(), 2);
    assert_eq!(app.active_session_id_for_test(), 1);

    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    click_menu_item(&mut app, "Close Tab");

    assert!(!app.context_menu_open_for_test());
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_session_id_for_test(), 0);
}

/// D-IN2-SETTINGS: clicking the Settings item closes the context menu and
/// opens the settings panel. Settings is below the split section; the item is
/// always enabled. The exact grid row is resolved from the live menu so the
/// v0.3.1 launcher section (which sits below Settings) cannot shift this test
/// onto the wrong row.
#[test]
fn clicking_settings_opens_settings_panel_and_closes_menu() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(
        app.context_menu_open_for_test(),
        "right-click opens the context menu"
    );

    click_menu_item(&mut app, "Settings");

    assert!(
        !app.context_menu_open_for_test(),
        "activating Settings closes the context menu"
    );
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::Settings,
        "Settings item opens the settings panel"
    );
}

/// D-IN2-SETTINGS: clicking either separator row is inert — the separator is
/// inside the rect so the press is consumed and the menu stays open; no action
/// is taken.
#[test]
fn clicking_separator_rows_is_inert() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    // Click the first separator (resolved live from the rendered menu).
    let sep0 = separator_grid_row(&mut app, 0);
    let rect = app.overlay_rect_for_test().expect("menu open");
    app.set_pointer_cell_for_test(sep0, rect.body_left);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);

    assert!(
        app.context_menu_open_for_test(),
        "first separator click is inert: context menu stays open"
    );
    assert_ne!(
        app.overlay_signature_for_test().mode,
        OverlayMode::Settings,
        "first separator click must not open the settings panel"
    );

    // Click the second separator (resolved live from the rendered menu).
    let sep1 = separator_grid_row(&mut app, 1);
    let rect = app.overlay_rect_for_test().expect("menu open");
    app.set_pointer_cell_for_test(sep1, rect.body_left);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);

    assert!(
        app.context_menu_open_for_test(),
        "second separator click is inert: context menu stays open"
    );
    assert_ne!(
        app.overlay_signature_for_test().mode,
        OverlayMode::Settings,
        "second separator click must not open the settings panel"
    );
}

/// v0.3.1 launcher section: the three new items render with the live accelerator
/// labels from the default bindings (Ctrl+Shift+S / P / R), proving the
/// `set_accelerators` path covers them and they auto-track rebinds.
#[test]
fn launcher_items_show_live_accelerator_labels() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    let (cols, rows) = app.grid_dims_for_test();
    let rendered = app.render_overlay_rows_for_test(cols, rows);
    let row_with = |needle: &str| -> String {
        rendered
            .iter()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not rendered in {rendered:?}"))
            .clone()
    };
    assert!(
        row_with("Connection Manager").contains("Ctrl+Shift+S"),
        "Connection Manager shows its bound chord"
    );
    assert!(
        row_with("Command Palette").contains("Ctrl+Shift+P"),
        "Command Palette shows its bound chord"
    );
    assert!(
        row_with("Session Replay").contains("Ctrl+Shift+R"),
        "Session Replay shows its bound chord"
    );
    assert!(
        row_with("Attach Session").contains("Ctrl+Shift+A"),
        "Attach Session shows its bound chord (Phase 5 / B2)"
    );
}

/// v0.3.1 launcher section: clicking each launcher item closes the menu and
/// opens the matching overlay through the production outcome→App apply path.
#[test]
fn clicking_connection_manager_opens_the_connection_overlay() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    click_menu_item(&mut app, "Connection Manager");
    assert!(!app.context_menu_open_for_test());
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::Connections,
        "Connection Manager item opens the connection overlay"
    );
}

#[test]
fn clicking_command_palette_opens_the_palette_overlay() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    click_menu_item(&mut app, "Command Palette");
    assert!(!app.context_menu_open_for_test());
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::CommandPalette,
        "Command Palette item opens the palette overlay"
    );
}

#[test]
fn clicking_session_replay_opens_the_replay_overlay() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    click_menu_item(&mut app, "Session Replay");
    assert!(!app.context_menu_open_for_test());
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::Replay,
        "Session Replay item opens the replay overlay"
    );
}

/// Phase 5 / B2: clicking the Attach Session launcher item closes the menu and
/// opens the session-attach overlay through the production outcome→App apply
/// path. The test runtime has no live sessions, so the overlay opens in its
/// empty-list hint state — the assertion is on the overlay mode switch.
#[test]
fn clicking_attach_session_opens_the_session_attach_overlay() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    click_menu_item(&mut app, "Attach Session");
    assert!(!app.context_menu_open_for_test());
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::SessionAttach,
        "Attach Session item opens the session-attach overlay"
    );
}
