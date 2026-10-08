// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored output-driven scrollback coordinate regressions.
use super::*;
use crate::native::test_support::headless_app_with;
use std::sync::{Arc, Mutex};

fn history() -> (App, Arc<Mutex<Terminal>>) {
    history_with_urls(false)
}

fn history_with_urls(urls: bool) -> (App, Arc<Mutex<Terminal>>) {
    let dims = Dimensions::new(40, 6);
    let settings = Settings {
        scroll_glide: false,
        ..Settings::default()
    };
    let (mut app, terminal) = headless_app_with(NativeOptions::default(), dims, settings);
    app.set_test_cell_for_test(crate::text::CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    {
        let mut terminal = terminal.lock().expect("terminal");
        terminal.set_scrollback_limit(16);
        for row in 0..40 {
            if urls && (row == 19 || row == 21) {
                let suffix = if row == 19 { "a" } else { "b" };
                terminal.advance(format!("https://example.com/{suffix}\r\n").as_bytes());
            } else {
                terminal.advance(format!("row {row:02}\r\n").as_bytes());
            }
        }
    }
    app.sessions.reconcile_scrollback_trims();
    app.anchor_viewport_for_render_frame_for_test();
    (app, terminal)
}

#[test]
fn alternate_frame_then_exit_and_wheel_does_not_consume_primary_lifetime_growth() {
    let (mut app, terminal) = history();
    terminal.lock().expect("terminal").advance(b"\x1b[?1049h");
    assert_eq!(app.anchor_viewport_for_render_frame_for_test(), 0);
    terminal.lock().expect("terminal").advance(b"\x1b[?1049l");
    app.handle_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
    let offset = app.viewport.offset();
    assert!(offset > 0 && offset < 16);
    assert_eq!(app.anchor_viewport_for_render_frame_for_test(), offset);
}

#[test]
fn alternate_frames_preserve_primary_search_return_offset() {
    let (mut app, terminal) = history();
    app.viewport.scroll_up(5, 16);
    app.toggle_search();
    assert_eq!(app.search_restore_viewport, Some(5));
    terminal.lock().expect("terminal").advance(b"\x1b[?1049h");
    assert_eq!(app.anchor_viewport_for_render_frame_for_test(), 0);
    assert_eq!(app.search_restore_viewport, Some(5));
    terminal.lock().expect("terminal").advance(b"\x1b[?1049l");
    app.anchor_viewport_for_render_frame_for_test();
    app.close_search(true);
    assert_eq!(app.viewport.offset(), 5);
}

#[test]
fn eviction_preserves_mouse_selection_and_held_drag_for_retained_text() {
    let (mut app, terminal) = history();
    app.set_pointer_px_for_test(8.0, 33.0);
    app.pointer_move_for_test(8.0, 33.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(40.0, 49.0);
    let before = app.current_selection_text().expect("mouse selection");
    let range = app.selection.range().expect("selection range");
    assert!(app.pointer_drag.is_selecting());
    terminal.lock().expect("terminal").advance(b"new row\r\n");
    assert_eq!(app.copy_shortcut_text_for_test(), Some(before));
    assert_eq!(
        app.selection
            .range()
            .expect("surviving selection")
            .start
            .row,
        range.start.row - 1
    );
    assert!(app.pointer_drag.is_selecting());
    app.pointer_move_for_test(48.0, 49.0);
    assert!(app.selection.range().is_some());
    app.mouse_left_release_for_test();
    assert!(!app.pointer_drag.is_selecting());
}

#[test]
fn eviction_preserves_keyboard_copy_mode_and_yanks_the_same_retained_text() {
    let (mut app, terminal) = history();
    enter_copy(&mut app);
    assert!(app.copy_mode.is_some());
    text_key(&mut app, "k");
    text_key(&mut app, "0");
    text_key(&mut app, "v");
    text_key(&mut app, "l");
    let before = app.copy_mode.expect("copy mode");
    terminal.lock().expect("terminal").advance(b"new row\r\n");
    app.sessions.reconcile_scrollback_trims();
    let after = app.copy_mode.expect("surviving copy mode");
    assert_eq!(after.cursor().row, before.cursor().row - 1);
    assert_eq!(
        after.anchor().expect("anchor").row,
        before.anchor().expect("anchor").row - 1
    );
    text_key(&mut app, "y");
    assert!(app.copy_mode.is_none());
    assert_eq!(app.last_clipboard_write_for_test().as_deref(), Some("ro"));
}

#[test]
fn eviction_preserves_hint_labels_and_repaints_their_rebased_rows() {
    let (mut app, terminal) = history();
    terminal
        .lock()
        .expect("terminal")
        .advance(b"https://example.com\r\n");
    app.sessions.reconcile_scrollback_trims();
    enter_hints(&mut app);
    assert!(app.hints_selecting());
    let signature = app.hints_overlay_signature();
    terminal.lock().expect("terminal").advance(b"new row\r\n");
    app.sessions.reconcile_scrollback_trims();
    assert!(app.hints_selecting());
    assert_ne!(app.hints_overlay_signature(), signature);
    text_key(&mut app, "a");
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some("https://example.com")
    );
}

fn select_visible(app: &mut App) -> String {
    app.set_pointer_px_for_test(8.0, 33.0);
    app.pointer_move_for_test(8.0, 33.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(40.0, 49.0);
    app.current_selection_text()
        .expect("visible mouse selection")
}

#[test]
fn a_render_before_reconciliation_does_not_consume_the_coordinate_shift() {
    let (mut app, terminal) = history();
    let before = select_visible(&mut app);
    terminal.lock().expect("terminal").advance(b"new row\r\n");
    app.anchor_viewport_for_render_frame_for_test();
    assert_eq!(app.copy_shortcut_text_for_test(), Some(before));
    let range = app.selection.range();
    app.sessions.reconcile_scrollback_trims();
    assert_eq!(app.selection.range(), range, "an eviction is rebased once");
}

#[test]
fn history_shrink_keeps_live_selection_and_full_clear_resets_it() {
    let (mut app, terminal) = history();
    let before = select_visible(&mut app);
    terminal.lock().expect("terminal").set_scrollback_limit(8);
    assert_eq!(app.copy_shortcut_text_for_test(), Some(before.clone()));
    terminal.lock().expect("terminal").advance(b"\x1b[3J");
    assert_eq!(app.copy_shortcut_text_for_test(), None);
    assert!(!app.pointer_drag.is_selecting());
}

#[test]
fn a_second_eviction_before_extraction_refuses_stale_coordinates() {
    let (mut app, terminal) = history();
    let before = select_visible(&mut app);
    terminal.lock().expect("terminal").advance(b"new row\r\n");
    app.sessions.reconcile_scrollback_trims();
    terminal
        .lock()
        .expect("terminal")
        .advance(b"another row\r\n");
    assert_eq!(
        app.current_selection_text(),
        None,
        "no later-row bytes escape"
    );
    assert_eq!(app.copy_shortcut_text_for_test(), Some(before));
}

#[test]
fn a_lost_copy_anchor_clears_only_the_selection_and_keeps_the_caret() {
    let (mut app, terminal) = history();
    enter_copy(&mut app);
    text_key(&mut app, "g");
    text_key(&mut app, "g");
    text_key(&mut app, "v");
    text_key(&mut app, "j");
    let before = app.copy_mode.expect("copy mode");
    assert_eq!(before.anchor().expect("anchor").row, 0);
    assert_eq!(before.cursor().row, 1);
    let signature = app.copy_mode_overlay_signature();
    terminal.lock().expect("terminal").advance(b"new row\r\n");
    app.sessions.reconcile_scrollback_trims();
    let after = app.copy_mode.expect("surviving caret");
    assert_eq!(after.cursor().row, 0);
    assert_eq!(after.anchor(), None);
    assert!(!after.is_selecting());
    assert_ne!(app.copy_mode_overlay_signature(), signature);
}

#[test]
fn hints_drop_evicted_matches_without_renaming_survivors() {
    let (mut app, terminal) = history_with_urls(true);
    app.viewport.scroll_up(16, 16);
    enter_hints(&mut app);
    assert!(app.hints_selecting());
    terminal.lock().expect("terminal").advance(b"new row\r\n");
    app.sessions.reconcile_scrollback_trims();
    assert!(app.hints_selecting());
    text_key(&mut app, "a");
    assert_eq!(
        app.last_clipboard_write_for_test(),
        None,
        "the evicted label no longer resolves"
    );
    text_key(&mut app, "s");
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some("https://example.com/b")
    );
}

#[test]
fn evicting_all_hint_matches_closes_the_modal() {
    let (mut app, terminal) = history_with_urls(true);
    app.viewport.scroll_up(16, 16);
    enter_hints(&mut app);
    assert!(app.hints_selecting());
    terminal
        .lock()
        .expect("terminal")
        .advance(&b"new row\r\n".repeat(20));
    app.sessions.reconcile_scrollback_trims();
    assert!(!app.hints_selecting());
}

#[test]
fn eviction_of_a_wrapped_logical_line_uses_physical_rows() {
    let dims = Dimensions::new(10, 4);
    let (mut app, terminal) =
        headless_app_with(NativeOptions::default(), dims, Settings::default());
    app.set_test_cell_for_test(crate::text::CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    {
        let mut terminal = terminal.lock().expect("terminal");
        terminal.set_scrollback_limit(2);
        for ch in *b"abcd" {
            terminal.advance(&[ch; 26]);
            terminal.advance(b"\r\n");
        }
    }
    app.sessions.reconcile_scrollback_trims();
    app.anchor_viewport_for_render_frame_for_test();
    app.set_pointer_px_for_test(8.0, 17.0);
    app.pointer_move_for_test(8.0, 17.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(32.0, 33.0);
    let before = app.current_selection_text().expect("wrapped visible text");
    let old_row = app.selection.range().expect("selection").start.row;
    terminal
        .lock()
        .expect("terminal")
        .advance(b"eeeeeeeeeeeeeeeeeeeeeeeeee\r\n");
    assert_eq!(app.copy_shortcut_text_for_test(), Some(before));
    let new_row = app
        .selection
        .range()
        .expect("surviving wrapped text")
        .start
        .row;
    assert_eq!(
        old_row - new_row,
        3,
        "one logical line removes three physical rows"
    );
}

fn text_key(app: &mut App, text: &str) {
    use winit::keyboard::{KeyCode, PhysicalKey};
    let code = match text {
        "a" => KeyCode::KeyA,
        "s" => KeyCode::KeyS,
        "g" => KeyCode::KeyG,
        "v" => KeyCode::KeyV,
        "j" => KeyCode::KeyJ,
        "k" => KeyCode::KeyK,
        "l" => KeyCode::KeyL,
        "y" => KeyCode::KeyY,
        "0" => KeyCode::Digit0,
        _ => panic!("unmapped project-authored fixture key"),
    };
    let logical = WinitKey::Character(text.into());
    for kind in [
        crate::input::KeyEventType::Press,
        crate::input::KeyEventType::Release,
    ] {
        app.drive_raw_key_event_for_test(
            logical.clone(),
            logical.clone(),
            PhysicalKey::Code(code),
            crate::input::Modifiers::default(),
            kind,
        );
    }
}

fn enter_copy(app: &mut App) {
    chord(
        app,
        WinitKey::Named(NamedKey::Space),
        winit::keyboard::KeyCode::Space,
    );
}

fn enter_hints(app: &mut App) {
    chord(
        app,
        WinitKey::Character("l".into()),
        winit::keyboard::KeyCode::KeyL,
    );
}

fn chord(app: &mut App, logical: WinitKey, code: winit::keyboard::KeyCode) {
    for kind in [
        crate::input::KeyEventType::Press,
        crate::input::KeyEventType::Release,
    ] {
        app.drive_raw_key_event_for_test(
            logical.clone(),
            logical.clone(),
            winit::keyboard::PhysicalKey::Code(code),
            crate::input::Modifiers {
                ctrl: true,
                shift: true,
                ..crate::input::Modifiers::default()
            },
            kind,
        );
    }
}

#[test]
fn a_session_created_on_the_alternate_screen_seeds_the_primary_baseline() {
    let dims = Dimensions::new(40, 6);
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut terminal = terminal.lock().expect("terminal");
        terminal.set_scrollback_limit(16);
        for row in 0..40 {
            terminal.advance(format!("row {row:02}\r\n").as_bytes());
        }
        terminal.advance(b"\x1b[?1049h");
    }
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal.clone(),
        crate::native::test_support::headless_writer(),
        Arc::new(crate::native::session::HeadlessSession::new(dims)),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(std::time::Instant::now()),
    );
    app.set_test_cell_for_test(crate::text::CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    assert_eq!(app.anchor_viewport_for_render_frame_for_test(), 0);
    terminal.lock().expect("terminal").advance(b"\x1b[?1049l");
    app.handle_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
    let offset = app.viewport.offset();
    assert!(offset > 0 && offset < 16);
    assert_eq!(app.anchor_viewport_for_render_frame_for_test(), offset);
}

/// Pointer motion, buttons, wheel, and keys reconcile only the active tab's
/// panes. A background tab whose terminal lock is held (a pump parsing a
/// flood) must not stall them; it is reconciled by the next redraw.
#[test]
fn input_events_do_not_wait_on_a_background_tab_terminal() {
    use std::sync::mpsc;
    use std::time::Duration;
    use winit::keyboard::{KeyCode, PhysicalKey};
    let dims = Dimensions::new(40, 6);
    let (mut app, first) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    app.set_test_cell_for_test(crate::text::CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    let second = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_session_for_test(
        second.clone(),
        crate::native::test_support::headless_writer(),
        dims,
    );
    let background = if Arc::ptr_eq(&app.terminal, &first) {
        second
    } else {
        first
    };
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = background.lock().expect("background terminal");
        locked_tx.send(()).expect("signal");
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });
    locked_rx.recv().expect("the background lock is held");
    let start = Instant::now();
    app.update_pointer_cell(20.5, 20.5);
    app.handle_mouse_input(ElementState::Pressed, WinitMouseButton::Left);
    app.handle_mouse_input(ElementState::Released, WinitMouseButton::Left);
    app.handle_mouse_wheel(MouseScrollDelta::LineDelta(0.0, -1.0));
    let key = WinitKey::Named(NamedKey::ArrowLeft);
    app.handle_key_event(
        key.clone(),
        key,
        PhysicalKey::Code(KeyCode::ArrowLeft),
        KeyEventType::Press,
    );
    let elapsed = start.elapsed();
    let _ = release_tx.send(());
    holder.join().expect("holder");
    assert!(
        elapsed < Duration::from_secs(5),
        "input waited {elapsed:?} on a background tab's terminal"
    );
}

/// A command whose rows survive while older history is evicted at the
/// limit. Filler rows come first, so the trim takes only them.
fn command_under_trimming() -> (App, Arc<Mutex<Terminal>>) {
    let dims = Dimensions::new(40, 6);
    let (app, terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    {
        let mut terminal = terminal.lock().expect("terminal");
        terminal.set_scrollback_limit(20);
        for row in 0..20 {
            terminal.advance(format!("filler {row:02}\r\n").as_bytes());
        }
        terminal.advance(
            b"\x1b]133;A\x07$ show\r\n\x1b]133;C\x07kept output\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ",
        );
    }
    (app, terminal)
}

/// Evict filler rows without any key, pointer, or redraw reconcile.
fn evict_filler(terminal: &Arc<Mutex<Terminal>>) {
    let mut terminal = terminal.lock().expect("terminal");
    let epoch = terminal.scrollback_trim_epoch();
    for row in 0..12 {
        terminal.advance(format!("more {row:02}\r\n").as_bytes());
    }
    assert_ne!(terminal.scrollback_trim_epoch(), epoch, "rows were evicted");
}

/// The save dialog's result arrives as a user event. Rows evicted after the
/// last reconcile, before the handle was taken, must not make a command that
/// still resolves read as unavailable. (Output after the handle was taken
/// changes its generation and is refused by design.)
#[test]
fn a_command_export_dialog_result_reconciles_eviction_before_reading() {
    let (mut app, terminal) = command_under_trimming();
    app.sessions.reconcile_scrollback_trims();
    evict_filler(&terminal);
    let handle = app.command_handle_for_test().expect("complete command");
    app.pending_command_exports.insert(
        7,
        super::command_output::PendingCommandExport {
            session: app.sessions.active_id(),
            handle,
        },
    );
    let target = std::env::temp_dir().join("odytty-unwritten-export.txt");
    app.finish_command_export_dialog(
        7,
        crate::native::save_dialog::SaveDialogSelection::Selected(target),
    );
    // The headless App has no event proxy, so the export stops at the
    // writer hand-off with its own notice; the command itself resolved.
    let notice = app.open_notice_message_for_test().unwrap_or_default();
    assert!(
        !notice.contains("Command action unavailable"),
        "the surviving command is not reported unavailable: {notice:?}"
    );
}

#[test]
fn a_command_copy_reconciles_eviction_before_reading() {
    let (mut app, terminal) = command_under_trimming();
    app.sessions.reconcile_scrollback_trims();
    evict_filler(&terminal);
    let handle = app.command_handle_for_test().expect("complete command");
    app.copy_command_range_from_handle(handle, crate::core::CommandRangePart::Output);
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some("kept output")
    );
}
