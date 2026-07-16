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
    CONTEXT_MENU_BODY_ROWS, CONTEXT_MENU_FIFTH_SEPARATOR_ROW, CONTEXT_MENU_FOURTH_SEPARATOR_ROW,
    CONTEXT_MENU_SECOND_SEPARATOR_ROW, CONTEXT_MENU_SEPARATOR_ROW,
    CONTEXT_MENU_THIRD_SEPARATOR_ROW, ContextMenuRow, ContextMenuUi,
};
use super::super::pty::UserEvent;
use super::super::session::{Session, SessionToken, WorkspaceSet};
use super::*;
use winit::event_loop::EventLoop;
#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

fn app_for_test() -> Option<(App, Arc<Mutex<Terminal>>)> {
    let dims = Dimensions::new(80, 24);
    Some(headless_app_with(
        NativeOptions::default(),
        dims,
        Settings::default(),
    ))
}

fn app_for_test_with_proxy() -> Option<(App, EventLoop<UserEvent>)> {
    let dims = Dimensions::new(80, 24);
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let headless = Arc::new(crate::native::session::HeadlessSession::new(dims));
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
    }
    #[cfg(target_os = "windows")]
    {
        EventLoopBuilderExtWindows::with_any_thread(&mut builder, true);
    }
    let event_loop = builder.build().ok()?;
    let proxy = event_loop.create_proxy();
    let sessions = WorkspaceSet::new(
        Session::new_headless(SessionToken(0), terminal, writer, headless),
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
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) =
        headless_app_with_writer(NativeOptions::default(), dims, Settings::default(), writer);
    terminal.lock().expect("terminal").advance(content);
    Some((app, bytes))
}

/// RC-19b: same as [`app_with_recording_writer`], but also hands back the
/// `Terminal` handle so a test can read the live absolute cursor row
/// (`scrollback_len() + cursor().row`) before and after a resize — needed to
/// re-point a screen-space selection at the prompt's new row across a
/// width-change reflow.
#[allow(clippy::type_complexity)]
fn app_with_recording_writer_and_terminal(
    content: &[u8],
) -> Option<(App, Arc<Mutex<Vec<u8>>>, Arc<Mutex<Terminal>>)> {
    let dims = Dimensions::new(80, 24);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) =
        headless_app_with_writer(NativeOptions::default(), dims, Settings::default(), writer);
    terminal.lock().expect("terminal").advance(content);
    Some((app, bytes, terminal))
}

/// Click the open context-menu item whose rendered row contains `needle`,
/// resolving its grid row from the live composited menu at the App's real grid
/// dims (so the rect/edge-clamp matches the click path exactly) and clicking at
/// the rect-derived body column. Robust to layout shifts — the v0.3.1 launcher
/// section changed item rows and the edge-clamped top, so hardcoded grid rows
/// would silently drift onto the wrong item.
fn click_menu_item(app: &mut App, needle: &str) {
    let (cols, rows) = app.grid_dims_for_test();
    // On a short grid the menu is taller than the viewport and scrolls, so a
    // bottom item (e.g. Detach & switch) is not rendered at the initial scroll
    // offset. Drive focus down until the needle scrolls into view, bounded by
    // the visible item count so a genuinely-absent item still panics.
    let mut grid_row = None;
    // Bounded by the total body-row count — a genuinely-absent needle still
    // panics after exhausting every scroll position.
    for _ in 0..=CONTEXT_MENU_BODY_ROWS {
        let rendered = app.render_overlay_rows_for_test(cols, rows);
        if let Some(row) = rendered.iter().position(|line| line.contains(needle)) {
            grid_row = Some(row);
            break;
        }
        app.drive_overlay_key_for_test(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
            false,
            false,
        );
    }
    let grid_row = grid_row
        .unwrap_or_else(|| panic!("menu item {needle:?} not visible after scrolling the menu"));
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
    // Open with a selection so the reference layout (Copy/Cut/Paste/Delete/
    // Select All present) is exercised; F7 drops the tab-only Rename Tab row.
    menu.open(
        CellPoint { row: 5, column: 10 },
        true,
        true,
        true,
        true,
        None,
        false,
        None,
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
    // F1: New Window follows New Tab in the tab-actions section.
    assert!(matches!(
        rows[7],
        ContextMenuRow::Item {
            label: "New Window",
            enabled: true,
            ..
        }
    ));
    // F7: Rename Tab dropped from the content menu; Close Tab follows New Window.
    assert!(!rows.iter().any(|r| matches!(
        r,
        ContextMenuRow::Item {
            label: "Rename Tab",
            ..
        }
    )));
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
    // Workspace section: after Split, before Settings, bracketed by the third
    // (split|workspace) and fourth (workspace|Settings) separators.
    assert!(matches!(
        rows[13],
        ContextMenuRow::Item {
            label: "New Workspace",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[14],
        ContextMenuRow::Item {
            label: "Rename Workspace",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[15],
        ContextMenuRow::Item {
            label: "Close Workspace",
            enabled: true,
            ..
        }
    ));
    // ODP-6B: an unbound workspace shows Bind to Host as the workspace section's
    // last row, right before the workspace|Settings separator.
    assert!(matches!(
        rows[16],
        ContextMenuRow::Item {
            label: "Bind to Host\u{2026}",
            enabled: true,
            ..
        }
    ));
    // LAYOUT-SURFACE + SAVE-ALL-LAYOUT: the whole-app Save as Layout, the single-
    // workspace Save Workspace as Layout, and Open Layout round out the workspace
    // section, right before the workspace|Settings separator.
    assert!(matches!(
        rows[17],
        ContextMenuRow::Item {
            label: "Save as Layout\u{2026}",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[18],
        ContextMenuRow::Item {
            label: "Save Workspace as Layout\u{2026}",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[19],
        ContextMenuRow::Item {
            label: "Open Layout\u{2026}",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[CONTEXT_MENU_FOURTH_SEPARATOR_ROW],
        ContextMenuRow::Separator
    ));
    assert!(matches!(
        rows[21],
        ContextMenuRow::Item {
            label: "Settings",
            enabled: true,
            ..
        }
    ));
    // v0.3.1 launcher section: a fifth separator, then the always-enabled
    // launcher items below Settings.
    assert!(matches!(
        rows[CONTEXT_MENU_FIFTH_SEPARATOR_ROW],
        ContextMenuRow::Separator
    ));
    // F3: Keyboard Shortcuts is the first launcher item, right below Settings.
    assert!(matches!(
        rows[23],
        ContextMenuRow::Item {
            label: "Keyboard Shortcuts",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[24],
        ContextMenuRow::Item {
            label: "Connection Manager",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[25],
        ContextMenuRow::Item {
            label: "Command Palette",
            enabled: true,
            ..
        }
    ));
    assert!(matches!(
        rows[26],
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
fn context_menu_open_replaces_the_grid_i_beam_immediately() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 10));
    app.pointer_move_for_test(28.0, 25.0);
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Text,
        "precondition: the stationary pointer starts over terminal content"
    );

    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(app.context_menu_open_for_test());
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Default,
        "opening the menu in place must replace the terminal I-beam"
    );
}

#[test]
fn right_click_over_resolved_path_shows_file_section() {
    // C3: right-clicking over a resolved interactive path (feature on) re-detects
    // the path at the click cell and surfaces the file section. Synthetic only:
    // the stat-gate is a MapProbe over a fixed map; `/proj/...` is fabricated.
    let Some((mut app, terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    terminal
        .lock()
        .expect("terminal")
        .advance(b"/proj/src/main.rs:42:7");
    app.set_test_cell_for_test(cell(8, 16));
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(crate::native::app::interactive_paths::MapProbe::new([(
        "/proj/src/main.rs",
        crate::paths::FsKind::File,
    )]));
    // Pointer at row 0, col 5 — inside the path span.
    app.set_pointer_cell_for_test(0, 5);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(app.context_menu_open_for_test());
    assert!(
        app.overlay_signature_for_test()
            .context_menu
            .has_path_target,
        "the file section is present over a resolved path"
    );
    // The full file section (Open / Open With… / Copy Path / Copy File /
    // Reveal) is unit-tested in `context_menu_ui`; here the menu can exceed the
    // grid and scroll, so assert the section surfaced via its top rows.
    // `has_path_target` above is the structural guarantee that every item is
    // present.
    let (cols, rows) = app.grid_dims_for_test();
    let rendered = app.render_overlay_rows_for_test(cols, rows);
    for label in ["Open", "Open With\u{2026}"] {
        assert!(
            rendered.iter().any(|line| line.contains(label)),
            "file item {label:?} must render in {rendered:?}"
        );
    }
}

#[test]
fn right_click_over_path_with_feature_off_has_no_file_section() {
    // BYTE-IDENTITY GUARD: with the feature off (default), the same right-click
    // over path-looking text never scans and the menu carries no file section.
    let Some((mut app, terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    terminal
        .lock()
        .expect("terminal")
        .advance(b"/proj/src/main.rs:42:7");
    app.set_test_cell_for_test(cell(8, 16));
    // Even with a probe that WOULD resolve, the off gate keeps it inert.
    app.set_test_path_probe_for_test(crate::native::app::interactive_paths::MapProbe::new([(
        "/proj/src/main.rs",
        crate::paths::FsKind::File,
    )]));
    app.set_pointer_cell_for_test(0, 5);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    assert!(app.context_menu_open_for_test());
    assert!(
        !app.overlay_signature_for_test()
            .context_menu
            .has_path_target,
        "no file section when the feature is off"
    );
    let (cols, rows) = app.grid_dims_for_test();
    let rendered = app.render_overlay_rows_for_test(cols, rows);
    assert!(
        !rendered
            .iter()
            .any(|line| line.contains("Reveal in File Manager")),
        "file items absent with feature off: {rendered:?}"
    );
}

#[test]
fn right_clicking_tab_enables_rename_for_that_tab() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let dims = Dimensions::new(80, 24);
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal2 = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_session_for_test(terminal2, writer, dims);
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

    // F7: a tab right-click opens the tight TabSlot menu (New Tab / Duplicate
    // Tab / Rename Tab · Close Tab / Close Other Tabs · New Window). Rename Tab
    // is body row 2 => grid row 3 (menu spawned at top-left, body starts one row
    // below the top border; Duplicate Tab now sits between New Tab and Rename).
    app.set_pointer_cell_for_test(3, 2);
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
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal2 = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_session_for_test(terminal2, writer, dims);
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
    let Some((mut app, _bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
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
fn cut_delete_disabled_hint_points_to_shell_integration_when_prompt_mark_missing() {
    let Some((mut app, _bytes)) = app_with_recording_writer(b"$ abc") else {
        return;
    };
    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    let sig = app.overlay_signature_for_test().context_menu;
    assert!(!sig.cut_enabled);
    assert!(!sig.delete_enabled);
    assert!(sig.prompt_editing_hint);

    let (cols, rows) = app.grid_dims_for_test();
    let rendered = app.render_overlay_rows_for_test(cols, rows);
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Enable shell integration in Settings")),
        "disabled Cut/Delete hint must render: {rendered:?}"
    );
}

#[test]
fn cut_delete_enabled_when_prompt_mark_is_present() {
    let Some((mut app, _bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
        return;
    };
    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);

    let sig = app.overlay_signature_for_test().context_menu;
    assert!(sig.cut_enabled);
    assert!(sig.delete_enabled);
    assert!(!sig.prompt_editing_hint);
}

#[test]
fn delete_selected_input_sends_shell_edit_bytes_without_copying() {
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
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

/// SELDEL-KEY: pressing Delete with a selection on the editable prompt input
/// deletes that selection through the same shell-edit-byte path as the menu's
/// Delete item, and clears the stale visual selection.
#[test]
fn delete_key_deletes_editable_selection_like_menu() {
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
        return;
    };
    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "Delete key sends the same clipped editable-input edit bytes as the menu Delete"
    );
    assert!(
        app.selection_range_for_test().is_none(),
        "the Delete key clears the stale visual selection after editing"
    );
    assert!(
        !app.click_hint_shown_for_test(),
        "successful prompt-aware delete does not show the disabled hint"
    );
}

/// SELDEL-KEY: Backspace behaves the same as Delete for an editable selection
/// (the universal GUI convention — either key removes the selection).
#[test]
fn backspace_key_deletes_editable_selection() {
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
        return;
    };
    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Backspace);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "Backspace deletes the editable selection identically to Delete"
    );
    assert!(app.selection_range_for_test().is_none());
}

#[test]
fn delete_key_with_missing_prompt_mark_clears_selection_and_hints() {
    let Some((mut app, bytes)) = app_with_recording_writer(b"$ abc") else {
        return;
    };
    app.force_selection_for_test(0, 0, 0, 4);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert!(
        written.is_empty(),
        "selection-delete without a prompt boundary must not send blind edit bytes"
    );
    assert!(
        app.selection_range_for_test().is_none(),
        "unavailable prompt-aware editing clears the stale visual selection"
    );
    assert!(
        app.click_hint_shown_for_test(),
        "unavailable prompt-aware editing surfaces the shell-integration hint"
    );
    assert_eq!(
        app.click_hint_text_for_test(),
        Some("Enable shell integration in Settings")
    );
}

/// SELDEL-KEY off path: a Delete with a selection that is NOT on editable input
/// (prompt-only) falls through to the normal key encode — byte-identical to
/// before the feature — and never touches the selection via the editable path.
#[test]
fn delete_key_on_input_row_outside_input_no_ops_with_hint() {
    // B-DESIGN ladder default: a selection ON the input row but entirely over
    // non-input cells (here the prompt "$"; same rung as a right-aligned
    // decoration) must consume the key as a hinted no-op — never forward a
    // blind Delete byte that would eat a character at the cursor.
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
        return;
    };
    // Prompt-only selection (column 0..1 covers "$"): no editable clipped text.
    app.force_selection_for_test(0, 0, 0, 1);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert!(
        written.is_empty(),
        "a non-input selection on the input row must not send any bytes, got {written:?}"
    );
    assert!(
        app.selection_range_for_test().is_none(),
        "the hinted no-op clears the stale visual selection"
    );
    assert!(
        app.click_hint_shown_for_test(),
        "the hinted no-op surfaces the shell-integration hint"
    );
}

/// SELDEL-KEY off path: a Delete with a selection that does not touch the
/// input region at all (here: a scrolled-out output row) falls through to the
/// normal key encode — byte-identical to before the feature.
#[test]
fn delete_key_falls_through_when_selection_not_on_input_region() {
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"output line\r\n\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
        return;
    };
    // Select on row 0 ("output line"), while the prompt input lives on row 1.
    app.force_selection_for_test(0, 0, 0, 5);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written, b"\x1b[3~",
        "a selection off the input region lets Delete encode normally to the shell"
    );
}

#[test]
fn delete_key_without_selection_still_falls_through() {
    let Some((mut app, bytes)) = app_with_recording_writer(b"$ abc") else {
        return;
    };

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written, b"\x1b[3~",
        "plain Delete with no selection still reaches the shell"
    );
    assert!(!app.click_hint_shown_for_test());
}

/// B2/T3/T5: a right-aligned decoration on the input row (RPROMPT, fish
/// duration, git status) renders as ordinary non-blank cells, indistinguishable
/// from input on the wire. With the shell's edit-region report bounding the
/// buffer exactly, selecting the decoration and pressing Delete must be a
/// hinted no-op — before this slice the last-non-blank heuristic claimed the
/// decoration as deletable input and sent motion+delete bytes for it.
#[test]
fn right_aligned_decoration_is_not_deletable_as_input() {
    // Prompt + "abc", then a decoration painted at column 15 with the cursor
    // saved/restored back to the input, then the edit-region report (len=3).
    let content = b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b7\x1b[1;16H23.1s\x1b8\x1b]133;P;odytty-edit;len=3;cur=3\x07";
    let Some((mut app, bytes)) = app_with_recording_writer(content) else {
        return;
    };
    // Select the decoration cells only (columns 15..19).
    app.force_selection_for_test(0, 15, 0, 19);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert!(
        written.is_empty(),
        "decoration cells must never synthesize edit bytes, got {written:?}"
    );
    assert!(app.selection_range_for_test().is_none());
    assert!(app.click_hint_shown_for_test());
}

/// B2/T2: a whole-row selection over input + decoration deletes ONLY the real
/// input span reported by the shell — the exact right edge clamps the
/// selection, so the decoration cells contribute no motion and no deletes.
#[test]
fn whole_row_selection_deletes_only_the_reported_input() {
    let content = b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b7\x1b[1;16H23.1s\x1b8\x1b]133;P;odytty-edit;len=3;cur=3\x07";
    let Some((mut app, bytes)) = app_with_recording_writer(content) else {
        return;
    };
    // Select the entire row: prompt, input, gap, decoration.
    app.force_selection_for_test(0, 0, 0, 19);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "only the reported 3-cell input is moved-to and deleted"
    );
    assert!(app.selection_range_for_test().is_none());
}

/// B2/T2: cursor mid-input — the reported cursor offset must reconcile against
/// the real grid cursor for the region to be Exact, and the synthesized motion
/// starts from the true cursor cell.
#[test]
fn mid_input_cursor_reconciles_and_deletes_exactly() {
    // Type "abcd", move the cursor left twice (to column 4, between b and c),
    // then report len=4 cur=2.
    let content =
        b"\x1b]133;A\x07$ \x1b]133;B\x07abcd\x1b[2D\x1b]133;P;odytty-edit;len=4;cur=2\x07";
    let Some((mut app, bytes)) = app_with_recording_writer(content) else {
        return;
    };
    // Select all four input cells (columns 2..5).
    app.force_selection_for_test(0, 2, 0, 5);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(2), b"\x1b[3~".repeat(4)].concat(),
        "motion runs from the true cursor (column 4) to the selection start, then deletes 4"
    );
}

/// NF14-R (Option R, replacing the B2/T14 ODP-3 pin): input
/// with a `133;B` mark but NO edit-region report — bash always, PowerShell,
/// zsh/fish before their snippets update — is a single-row `RightEdgeUnknown`
/// region, and Delete falls back to the pre-B2 heuristic delete (last
/// non-blank right edge) instead of the strict no-op that had turned
/// select+Delete off for every shell without the private OSC.
#[test]
fn input_without_edit_region_report_deletes_single_row_heuristically() {
    let Some((mut app, bytes)) = app_with_recording_writer(b"\x1b]133;A\x07$ \x1b]133;B\x07abc")
    else {
        return;
    };
    app.force_selection_for_test(0, 2, 0, 4);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "single-row RightEdgeUnknown falls back to the heuristic delete (Option R)"
    );
    assert!(
        app.selection_range_for_test().is_none(),
        "editing clears the stale visual selection"
    );
    assert!(
        !app.click_hint_shown_for_test(),
        "a successful heuristic delete does not show the disabled hint"
    );
}

/// NF14-R: a PowerShell-shaped prompt (the Windows default shell; its snippet
/// emits OSC 133 marks but no `odytty-edit` report) gets the same single-row
/// heuristic delete — this is the case that made select+Delete a complete
/// no-op on Windows under the strict ODP-3 gate.
#[test]
fn powershell_prompt_without_report_deletes_single_row_heuristically() {
    let Some((mut app, bytes)) =
        app_with_recording_writer(b"\x1b]133;A\x07PS C:\\> \x1b]133;B\x07dir")
    else {
        return;
    };
    // Prompt "PS C:\> " spans columns 0..7; input "dir" sits at columns 8..10.
    app.force_selection_for_test(0, 8, 0, 10);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "PowerShell-style no-signal input deletes via the single-row heuristic"
    );
    assert!(app.selection_range_for_test().is_none());
}

/// NF14-R boundary pin (the ODP-3 remainder): a MULTI-ROW region without an
/// edit-region report stays a hinted no-op — without a trustworthy right
/// edge, row joins cannot anchor a synthesized multi-row edit. Option R
/// restores only the single-row heuristic.
#[test]
fn multi_row_input_without_report_stays_a_hinted_no_op() {
    // 78 bytes fill row 0 from column 2 to the 80-column right edge, so the
    // input soft-wraps onto row 1 — a two-row region with no signal.
    let content = wrapped_input_content(&[b'a'; 78], b"bbb", b"");
    let Some((mut app, bytes)) = app_with_recording_writer(&content) else {
        return;
    };
    app.force_selection_for_test(1, 0, 1, 2);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert!(
        written.is_empty(),
        "multi-row RightEdgeUnknown must stay a no-op, got {written:?}"
    );
    assert!(app.selection_range_for_test().is_none());
    assert!(
        app.click_hint_shown_for_test(),
        "the multi-row degradation still surfaces a no-op hint"
    );
    // NF17: integration IS on here (the input mark is present, region computed
    // to multi-row RightEdgeUnknown) — the hint must NOT tell the user to
    // enable something already enabled. It reports unavailable geometry.
    assert_eq!(
        app.click_hint_text_for_test(),
        Some("Selection can't be edited here"),
        "the geometry-unavailable no-op must not reuse the enable-integration text"
    );
}

/// NF14-R accepted-risk documentation: WITHOUT an edit-region
/// report, a right-aligned decoration on the input row is indistinguishable
/// from input, so the restored heuristic treats it as deletable — selecting
/// the decoration and pressing Delete sends motion+delete bytes for it. This
/// was shipped behavior for the feature's whole pre-B2 life and is the
/// bounded risk Option R deliberately re-accepts in exchange for the feature
/// working at all on no-signal shells. Shells WITH the report keep the exact
/// boundary (see `right_aligned_decoration_is_not_deletable_as_input`).
#[test]
fn no_signal_decoration_is_heuristically_deletable_by_design() {
    // Prompt + "abc" (cursor restored to column 5), decoration at columns
    // 15..19 — and NO edit-region report.
    let content = b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b7\x1b[1;16H23.1s\x1b8";
    let Some((mut app, bytes)) = app_with_recording_writer(content) else {
        return;
    };
    // Select the decoration cells only (columns 15..19).
    app.force_selection_for_test(0, 15, 0, 19);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[C".repeat(10), b"\x1b[3~".repeat(5)].concat(),
        "the heuristic right edge claims the decoration: motion right from the \
         cursor (column 5) to the selection start (column 15), then 5 deletes"
    );
}

/// B2/T18 (consumer side), updated for NF14-R: a malformed edit-region report
/// is ignored — the region falls back to the heuristic path, which under
/// Option R means the single-row heuristic DELETE (identical to the no-signal
/// case), not a panic and not a bogus Exact-tier edit.
#[test]
fn malformed_edit_region_report_degrades_to_heuristic_delete() {
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=;cur=zzz\x07",
    ) else {
        return;
    };
    app.force_selection_for_test(0, 2, 0, 4);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "a malformed report degrades to the same heuristic delete as no signal"
    );
}

/// Wrapped-input content for the B1 (R5 soft-wrap) tests: prompt "$ " with the
/// `133;B` mark at column 2, then `first` bytes filling row 0 from column 2 and
/// `second` bytes on row 1, followed by the given edit-region report. At the
/// 80-column test grid, 78 bytes of `first` exactly fill row 0.
fn wrapped_input_content(first: &[u8], second: &[u8], report: &[u8]) -> Vec<u8> {
    let mut content = b"\x1b]133;A\x07$ \x1b]133;B\x07".to_vec();
    content.extend_from_slice(first);
    content.extend_from_slice(second);
    content.extend_from_slice(report);
    content
}

/// B1/T8 (R5): a soft-wrapped command is ONE logical line; a selection
/// crossing the wrap boundary flattens to horizontal-only motion — Left to the
/// selection start (summed across the wrap), then Delete per selected glyph.
#[test]
fn wrapped_selection_crossing_wrap_deletes_exactly() {
    // 78 'a' (row 0, cols 2..79) + 5 'b' (row 1, cols 0..4) = 83 runes,
    // cursor at (1, 5).
    let content = wrapped_input_content(
        &[b'a'; 78],
        &[b'b'; 5],
        b"\x1b]133;P;odytty-edit;len=83;cur=83\x07",
    );
    let Some((mut app, bytes)) = app_with_recording_writer(&content) else {
        return;
    };
    // Select row 0 cols 70..79 (10 glyphs) through row 1 cols 0..2 (3 glyphs).
    app.force_selection_for_test(0, 70, 1, 2);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(15), b"\x1b[3~".repeat(13)].concat(),
        "cursor (offset 83) moves Left 15 to the selection start (offset 68), \
         then deletes the 13 selected glyphs across the wrap"
    );
    assert!(
        app.selection_range_for_test().is_none(),
        "editing clears the stale visual selection"
    );
    assert!(
        !app.click_hint_shown_for_test(),
        "successful wrapped delete does not show the disabled hint"
    );
}

/// B1 (R5): cursor repositioned mid-input BEFORE the wrap with the selection
/// after it — motion must run Right across the wrap boundary, never a line
/// motion.
#[test]
fn wrapped_cursor_before_wrap_synthesizes_right_motion() {
    // Same 83-rune buffer, but the cursor was moved to offset 73 (row 0,
    // col 75) and the shell reported cur=73.
    let mut content = wrapped_input_content(
        &[b'a'; 78],
        &[b'b'; 5],
        b"\x1b]133;P;odytty-edit;len=83;cur=73\x07",
    );
    content.extend_from_slice(b"\x1b[1;76H"); // cursor to (0, 75), 0-based
    let Some((mut app, bytes)) = app_with_recording_writer(&content) else {
        return;
    };
    // Select row 1 cols 1..3 (glyph offsets 79..81).
    app.force_selection_for_test(1, 1, 1, 3);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[C".repeat(6), b"\x1b[3~".repeat(3)].concat(),
        "cursor (offset 73) moves Right 6 across the wrap to offset 79, then deletes 3"
    );
}

/// B1/T9 (A4): a wide glyph that did not fit at the right edge leaves a
/// display-only wrap-filler blank. The filler is NOT a buffer character: the
/// motion/delete counts must skip it exactly.
#[test]
fn wide_glyph_straddling_wrap_counts_exactly() {
    // 77 'a' (cols 2..78), then 漢 does not fit at col 79: filler blank at
    // col 79, 漢 wraps to row 1 cols 0..1, then "xy". Buffer = 80 runes.
    let mut second = "漢".as_bytes().to_vec();
    second.extend_from_slice(b"xy");
    let content = wrapped_input_content(
        &[b'a'; 77],
        &second,
        b"\x1b]133;P;odytty-edit;len=80;cur=80\x07",
    );
    let Some((mut app, bytes)) = app_with_recording_writer(&content) else {
        return;
    };
    // Select row 0 cols 75..79 (4 glyphs + the filler) through row 1 cols
    // 0..1 (the wide glyph, 1 glyph): 5 buffer glyphs, filler excluded.
    app.force_selection_for_test(0, 75, 1, 1);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert_eq!(
        written,
        [b"\x1b[D".repeat(7), b"\x1b[3~".repeat(5)].concat(),
        "filler cell contributes no motion and no delete: Left 7 (80-73), Delete 5"
    );
}

/// B1/T10 (ODP-2 default): a multi-line buffer (hard newlines, e.g. fish
/// `begin…end` or a zsh continuation) reports `nl=` offsets; the region is
/// Unknown and Delete over a selection touching it is a hinted no-op — no
/// vertical motion is ever synthesized.
#[test]
fn hard_newline_report_no_ops_with_hint() {
    let content =
        b"\x1b]133;A\x07$ \x1b]133;B\x07for x in 1\r\necho\x1b]133;P;odytty-edit;len=15;cur=15;nl=10\x07";
    let Some((mut app, bytes)) = app_with_recording_writer(content) else {
        return;
    };
    app.force_selection_for_test(0, 2, 0, 8);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert!(
        written.is_empty(),
        "hard newlines => Unknown => no bytes, got {written:?}"
    );
    assert!(app.selection_range_for_test().is_none());
    assert!(app.click_hint_shown_for_test());
}

/// B1 (ladder default on the flattened axis): a selection over only non-input
/// cells of a wrapped region's last row (right of the reported input end) is a
/// hinted no-op, exactly like the single-row decoration case.
#[test]
fn wrapped_selection_beyond_input_end_no_ops() {
    let content = wrapped_input_content(
        &[b'a'; 78],
        &[b'b'; 5],
        b"\x1b]133;P;odytty-edit;len=83;cur=83\x07",
    );
    let Some((mut app, bytes)) = app_with_recording_writer(&content) else {
        return;
    };
    // Row 1 input ends at col 5 (exclusive); select cols 10..15.
    app.force_selection_for_test(1, 10, 1, 15);
    app.set_pointer_cell_for_test(5, 10);

    app.drive_named_key_for_test(NamedKey::Delete);

    let written = bytes.lock().expect("bytes").clone();
    assert!(
        written.is_empty(),
        "selection over non-input cells must not synthesize, got {written:?}"
    );
    assert!(app.click_hint_shown_for_test());
}

/// D-IN2-CUT-SAFE: if `write_text` returns `None`, Cut must NOT delete the
/// editable input and must NOT clear the selection. The menu closes (item was
/// activated) but the text and selection survive intact.
#[test]
fn cut_clipboard_failure_leaves_input_and_selection_intact() {
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
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
    let Some((mut app, bytes)) = app_with_recording_writer(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    ) else {
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

// The proxy harness (`app_for_test_with_proxy`) builds a real winit `EventLoop`
// off the test thread. Linux and Windows permit that via `with_any_thread`;
// macOS has no equivalent because AppKit must own the main thread, so
// constructing/using the loop off-main-thread aborts (SIGSEGV). The new-tab path
// itself runs on the main thread in production and is exercised on Linux here
// and on Windows once Phase 4 CI is unblocked; the test stays compiled on macOS
// (so the helper and its imports are still used) but is skipped at runtime.
#[cfg_attr(
    target_os = "macos",
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
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal2 = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_session_for_test(terminal2, writer, dims);
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
    assert_eq!(
        app.settings_active_section_for_test(),
        None,
        "content-menu Settings keeps the generic section list"
    );
}

#[test]
fn tab_strip_menu_settings_opens_tabs_and_panes() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.open_empty_tab_strip_menu_for_test();
    click_menu_item(&mut app, "Settings");
    assert_eq!(app.settings_active_section_for_test(), Some("Layout"));
}

#[test]
fn workspace_menu_settings_opens_tabs_and_panes() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.open_workspace_rail_menu_for_test(0);
    // Settings is the eighth selectable row: New, Duplicate, Rename, Close,
    // Bind, the two layout rows, then Settings in its trailing group.
    for _ in 0..7 {
        app.drive_overlay_key_for_test(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
            false,
            false,
        );
    }
    app.drive_overlay_key_for_test(
        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        false,
        false,
    );
    assert_eq!(app.overlay_signature_for_test().mode, OverlayMode::Settings);
    assert_eq!(app.settings_active_section_for_test(), Some("Layout"));
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
    // On the short 80x24 test grid the menu is taller than the viewport and
    // scrolls, so the bottom launcher rows only render once focus walks down to
    // them (LAYOUT-SURFACE added two workspace rows). Drive focus down until each
    // needle scrolls into view, bounded so a genuinely-absent item still panics.
    let find_row = |app: &mut App, needle: &str| -> String {
        for _ in 0..=CONTEXT_MENU_BODY_ROWS {
            let rendered = app.render_overlay_rows_for_test(cols, rows);
            if let Some(line) = rendered.iter().find(|line| line.contains(needle)) {
                return line.clone();
            }
            app.drive_overlay_key_for_test(
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
                false,
                false,
            );
        }
        panic!("{needle} not rendered after scrolling the menu");
    };
    assert!(
        find_row(&mut app, "Connection Manager").contains("Ctrl+Shift+S"),
        "Connection Manager shows its bound chord"
    );
    assert!(
        find_row(&mut app, "Command Palette").contains("Ctrl+Shift+P"),
        "Command Palette shows its bound chord"
    );
    assert!(
        find_row(&mut app, "Session Replay").contains("Ctrl+Shift+R"),
        "Session Replay shows its bound chord"
    );
    assert!(
        find_row(&mut app, "Manage Sessions").contains("Ctrl+Shift+A"),
        "Manage Sessions shows its bound chord (Phase 5 / B2)"
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

/// Phase 5 / B2: clicking the Manage Sessions launcher item closes the menu and
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
    click_menu_item(&mut app, "Manage Sessions");
    assert!(!app.context_menu_open_for_test());
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::SessionAttach,
        "Manage Sessions item opens the session-attach overlay"
    );
}

/// Clicking the "Detach & switch" menu item closes the menu and opens
/// the 3-way choice dialog. The test terminal has no OSC 7 cwd, so the dialog
/// names the default directory — the assertion is on the overlay mode switch.
#[test]
fn clicking_detach_switch_opens_the_choice_dialog() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    click_menu_item(&mut app, "Detach & switch");
    assert!(!app.context_menu_open_for_test());
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::DetachSwitchChoice,
        "Detach & switch item opens the choice dialog"
    );
}

/// Failure guard: a spawn failure during Detach & switch surfaces a
/// transient notice and leaves the original pane untouched — never close the
/// original before the new managed session is confirmed live.
#[test]
#[cfg(unix)]
fn detach_switch_spawn_failure_raises_notice_and_keeps_panes() {
    let Some((mut app, _terminal)) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let before = app.session_count_for_test();
    assert!(
        app.open_notice_message_for_test().is_none(),
        "clean start: no notice"
    );
    // Drive the full orchestration with a spawner that always fails (swap=true,
    // the destructive path — proves the original is NOT closed on failure).
    app.detach_switch_spawn_failure_for_test(true);
    assert!(
        app.open_notice_message_for_test().is_some(),
        "a spawn failure raises a transient notice"
    );
    assert_eq!(
        app.session_count_for_test(),
        before,
        "a spawn failure adds no session and closes none — original untouched"
    );
}

/// RC-19b: app-level regression lock for RC-19 (commit 6d2baaf). The CORE-
/// level test (`input_start_is_reanchored_through_a_width_change_resize` in
/// `src/core/tests/osc_prompt.rs`) pins the screen invariant in isolation;
/// this test pins the actual consumer the bug bit — the right-click/Delete
/// gate `editable_input_selection_for_context_menu` — through a real
/// width-change resize driven the same way a side-by-side split drives it
/// (`App::resize_grid` -> `resize_all_panes` -> `Terminal::resize`).
#[test]
fn editable_input_selection_survives_a_width_change_resize() {
    // Build enough scrollback that a width SHRINK forces a rewrap (so
    // `scrollback_len()` grows and the cached prompt-input anchor goes stale
    // without RC-19's re-anchor). Each pushed line is 70 cols: under the
    // initial 80-col width it is exactly one physical row, but over the
    // 40-col width resized to below, so it wraps into 2 rows post-resize and
    // scrollback_len() is guaranteed to change.
    let long_line = "0123456789".repeat(7); // 70 chars
    let mut content = Vec::new();
    for _ in 0..40 {
        content.extend_from_slice(long_line.as_bytes());
        content.extend_from_slice(b"\r\n");
    }
    // A fresh prompt with an input mark on the live input row — same `$ ` /
    // `abc` shape as the other single-pane gate tests above.
    content.extend_from_slice(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    );

    let Some((mut app, _bytes, terminal)) = app_with_recording_writer_and_terminal(&content) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // PRE: locate the live input row in absolute (scrollback + cursor) terms
    // and select across the prompt text into the input, exactly like
    // `cut_delete_enabled_when_prompt_mark_is_present` does.
    let pre_row = {
        let terminal = terminal.lock().expect("terminal");
        terminal.screen().scrollback_len() + terminal.screen().cursor().row
    };
    app.force_selection_for_test(pre_row, 0, pre_row, 4);
    assert_eq!(
        app.editable_input_selection_text_for_test().as_deref(),
        Some("abc"),
        "precondition: the gate passes before any resize (single-pane today)"
    );

    // Apply a width-change resize (narrower) — the side-by-side-split
    // scenario — through the exact same model path a real split drives.
    let resized = app.resize_grid(cell(8, 16), 40 * 8, 24 * 16);
    assert!(resized, "narrower width must be a genuine grid change");

    // The selection is screen-space (an absolute row), and the resize above
    // rewrapped scrollback, so the prompt's absolute row moved. Re-point the
    // SAME logical selection ("abc") at the NEW live input row — this
    // isolates exactly what RC-19 fixed: whether the cached prompt-input
    // anchor (`active_prompt_input_start`) was re-anchored to track that
    // move. If it was not (RC-19 reverted), the gate's `input_row !=
    // cursor_row` check fails here and the POST assertion below sees `None`.
    let post_row = {
        let terminal = terminal.lock().expect("terminal");
        terminal.screen().scrollback_len() + terminal.screen().cursor().row
    };
    app.force_selection_for_test(post_row, 0, post_row, 4);

    assert_eq!(
        app.editable_input_selection_text_for_test().as_deref(),
        Some("abc"),
        "select+Delete must still engage on the prompt input after a width-change resize"
    );
}

// ── FIX-A: right-click freeze (no blocking clipboard/stat on the loop thread) ──

/// FIX-A helper: a left-rail App with two workspaces, so a right-press on rail
/// slot 0 opens the `WorkspaceSlot` context menu through the real
/// `handle_mouse_input` route. `None` when no PTY fixture is available.
fn rail_two_workspace_app() -> Option<App> {
    let (mut app, _terminal) = app_for_test()?;
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    // A second workspace so the rail has a real slot 0 to right-click.
    let dims = Dimensions::new(80, 24);
    let recorder = RecordingWriter::default();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_workspace_for_test(terminal, writer, dims);
    Some(app)
}

#[test]
fn opening_a_rail_context_menu_never_probes_the_clipboard() {
    // The ~12s Wayland right-click freeze: `open_context_menu` synchronously read
    // the clipboard (`get_text`, a no-timeout pipe read on Wayland) on EVERY menu
    // open, blocking the winit event-loop thread. The Paste item is now shown
    // optimistically, so the open path must not touch the clipboard at all. Drive
    // the real right-press route on a rail slot and assert zero clipboard reads.
    let Some(mut app) = rail_two_workspace_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(
        app.clipboard_read_text_calls_for_test(),
        0,
        "no clipboard read before the menu opens"
    );
    // Right-press on rail slot 0 (a workspace surface) via the real route.
    app.pointer_move_for_test(12.0, 24.0);
    assert_eq!(
        app.chrome_hit_band_for_test(),
        Some("workspace"),
        "the pointer is over a rail slot"
    );
    app.mouse_right_press_for_test();
    assert!(
        app.context_menu_open_for_test(),
        "the rail right-press opened the context menu"
    );
    assert_eq!(
        app.clipboard_read_text_calls_for_test(),
        0,
        "opening the menu must not synchronously probe the clipboard"
    );
}

#[test]
fn a_chrome_right_click_skips_the_interactive_path_scan() {
    // The second blocking site: `resolved_hovered_path` stat-probes path spans
    // and can hang on a stalled mount. A right-click on chrome (a rail slot) can
    // never sit over a content path, so the scan is skipped there even with
    // `interactive_paths` on -- while a content right-click still runs it.
    let Some(mut app) = rail_two_workspace_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);

    // Rail slot right-click: chrome surface, scan skipped.
    app.pointer_move_for_test(12.0, 24.0);
    app.mouse_right_press_for_test();
    assert!(app.context_menu_open_for_test());
    assert!(
        !app.last_menu_path_scan_for_test(),
        "a rail (chrome) right-click must skip the interactive-path scan"
    );
    app.close_overlay_for_test();

    // Content right-click (well past the rail band): the scan runs as before.
    app.pointer_move_for_test(400.0, 200.0);
    app.mouse_right_press_for_test();
    assert!(app.context_menu_open_for_test());
    assert!(
        app.last_menu_path_scan_for_test(),
        "a content right-click still runs the interactive-path scan"
    );
}

#[test]
fn a_stale_press_burst_into_a_fresh_workspace_menu_activates_nothing() {
    // The "phantom New Workspace": while the loop was frozen, queued clicks piled
    // up and replayed into the just-opened WorkspaceSlot menu, activating an
    // unintended item. A press landing on a workspace-rail menu within its open
    // debounce window is now swallowed. Open the rail menu, then fire a burst of
    // presses onto the menu body: the workspace count must not change.
    let Some(mut app) = rail_two_workspace_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(12.0, 24.0);
    app.mouse_right_press_for_test();
    assert!(app.context_menu_open_for_test());
    let before = app.workspace_count_for_test();

    // A stale burst: several press/release pairs onto the menu body (where the
    // "New Workspace" row could sit), all within the debounce window (elapsed
    // ~0 in a test). Every one is swallowed, so no item activates.
    let rect = app.overlay_rect_for_test().expect("rail menu open");
    app.set_pointer_cell_for_test(rect.body_top, rect.body_left);
    for _ in 0..4 {
        app.mouse_left_press_for_test();
        app.mouse_left_release_for_test();
    }
    assert_eq!(
        app.workspace_count_for_test(),
        before,
        "a stale press burst into a fresh menu must not mutate workspaces"
    );
    assert!(
        app.context_menu_open_for_test(),
        "the swallowed presses leave the menu open"
    );

    // Once the debounce window has elapsed, presses route to the menu again: a
    // click-away press dismisses it (routing is only deferred, not broken).
    app.expire_context_menu_debounce_for_test();
    app.pointer_move_for_test(600.0, 360.0);
    app.mouse_left_press_for_test();
    assert!(
        !app.context_menu_open_for_test(),
        "after the debounce elapses, a routed click-away dismisses the menu"
    );
}
