// SPDX-License-Identifier: GPL-3.0-only
//! Read-only pane enforcement through the App input routes.

use super::*;
use crate::native::session::SessionToken;
use std::io::Write;

#[derive(Clone, Default)]
struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

type RecordedApp = (App, Arc<Mutex<Terminal>>, Arc<Mutex<Vec<u8>>>);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("recorded bytes")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn app_with_writer() -> RecordedApp {
    let recorder = RecordingWriter::default();
    let bytes = recorder.0.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let dimensions = Dimensions::new(80, 24);
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        dimensions,
        Settings::default(),
        writer,
    );
    (app, terminal, bytes)
}

fn make_read_only(app: &mut App) -> SessionToken {
    let token = app.active_session_token_for_test();
    assert!(app.set_pane_read_only(token, true));
    assert!(!app.pane_accepts_input(token));
    token
}

fn bytes(recorded: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    recorded.lock().expect("recorded bytes").clone()
}

#[test]
fn key_press_kitty_release_and_ime_commit_are_dropped_without_replay() {
    let (mut app, terminal, recorded) = app_with_writer();
    make_read_only(&mut app);

    // Kitty reports both key edges. The release must be dropped independently;
    // it must not leak even when the press already was refused.
    terminal.lock().expect("terminal").advance(b"\x1b[=1;2u");
    let key = WinitKey::Character("x".into());
    for event_type in [KeyEventType::Press, KeyEventType::Release] {
        app.drive_raw_key_event_for_test(
            key.clone(),
            key.clone(),
            PhysicalKey::Code(KeyCode::KeyX),
            Modifiers::NONE,
            event_type,
        );
    }
    app.handle_ime(winit::event::Ime::Commit("ime-text".to_owned()));
    assert!(
        bytes(&recorded).is_empty(),
        "read-only user input is dropped"
    );

    app.set_pane_read_only(app.active_session_token_for_test(), false);
    app.drive_raw_key_event_for_test(
        WinitKey::Character("z".into()),
        WinitKey::Character("z".into()),
        PhysicalKey::Code(KeyCode::KeyZ),
        Modifiers::NONE,
        KeyEventType::Press,
    );
    assert_eq!(bytes(&recorded), b"z", "blocked bytes are never replayed");
}

#[test]
fn clipboard_and_bracketed_paste_are_dropped_then_fresh_paste_is_sent() {
    let (mut app, terminal, recorded) = app_with_writer();
    app.enable_osc52_read_for_test("clipboard text");
    make_read_only(&mut app);

    app.handle_paste_shortcut_for_test();
    assert!(bytes(&recorded).is_empty());
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some("Pane is read-only: input not sent")
    );

    // Bracketed paste takes the same policy path and must not leak either
    // wrapper or payload bytes.
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
    app.handle_paste_shortcut_for_test();
    assert!(bytes(&recorded).is_empty());

    app.set_pane_read_only(app.active_session_token_for_test(), false);
    app.handle_paste_shortcut_for_test();
    assert_eq!(
        bytes(&recorded),
        b"\x1b[200~clipboard text\x1b[201~",
        "only the fresh post-toggle paste is sent"
    );
}

#[test]
fn primary_middle_click_paste_is_dropped() {
    let (mut app, _terminal, recorded) = app_with_writer();
    app.enable_osc52_read_for_test("primary text");
    make_read_only(&mut app);

    app.handle_primary_paste_for_test();

    assert!(bytes(&recorded).is_empty());
}

#[test]
fn image_paste_confirmation_is_refused_for_a_read_only_pane() {
    let (mut app, _terminal, recorded) = app_with_writer();
    app.set_active_remote_upload_for_test("deploy@host.example.invalid");
    app.set_remote_image_paste_enabled_for_test(true);
    app.set_clipboard_image_for_test(Some(vec![0x89, b'P', b'N', b'G']));
    make_read_only(&mut app);

    app.handle_paste_shortcut_for_test();

    assert!(!app.image_paste_pending_for_test());
    assert_eq!(app.confirm_image_paste_for_test(), None);
    assert!(bytes(&recorded).is_empty());
}

#[cfg(unix)]
#[test]
fn file_drop_text_is_refused_for_a_read_only_pane() {
    use crate::pty::ForegroundJob;
    use crate::shell_integration::ShellKind;
    use std::path::PathBuf;

    let (mut app, _terminal, recorded) = app_with_writer();
    app.headless_session()
        .expect("headless session")
        .set_foreground_job(ForegroundJob::None);
    app.set_file_drop_shell_for_test(Some(ShellKind::Bash));
    app.set_window_focus_for_test(true);
    make_read_only(&mut app);

    app.queue_file_drop_for_test(PathBuf::from("/tmp/ordinary-file"));

    assert!(app.pending_file_drop_len_for_test().is_none());
    assert!(bytes(&recorded).is_empty());
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some("Pane is read-only: input not sent")
    );
}

#[test]
fn mouse_button_and_wheel_reports_are_dropped() {
    let (mut app, terminal, recorded) = app_with_writer();
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b[?1002h\x1b[?1006h");
    app.set_pointer_cell_for_test(2, 3);
    make_read_only(&mut app);

    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_wheel_for_test(1.0);

    assert!(bytes(&recorded).is_empty());
}

#[test]
fn copy_search_selection_scroll_resize_and_focus_reports_remain_available() {
    let (mut app, terminal, recorded) = app_with_writer();
    let mut output = String::from("copyable text\r\n");
    for index in 0..32 {
        output.push_str(&format!("line {index}\r\n"));
    }
    terminal
        .lock()
        .expect("terminal")
        .advance(output.as_bytes());
    app.set_selection_range_for_test(0, 0, 0, 7);
    app.enable_focus_reporting_for_test();
    make_read_only(&mut app);

    assert_eq!(
        app.copy_shortcut_text_for_test().as_deref(),
        Some("copyable")
    );
    app.drive_search_for_test("line");
    assert!(app.search_match_count_for_test() >= 1);
    app.scroll_up_for_test(2);
    assert!(app.viewport_offset_for_test() > 0);
    let before = app.session_dimensions_for_test(0).expect("dimensions");
    assert!(app.resize_grid_with_padding_for_test(cell(8, 16), WindowPadding::ZERO, 400, 160,));
    assert_ne!(app.session_dimensions_for_test(0), Some(before));

    app.on_window_focus_changed_for_test(false);
    assert!(
        bytes(&recorded).ends_with(b"\x1b[O"),
        "focus reports are terminal state, not typed input"
    );
}

#[test]
fn read_only_policy_is_per_pane_and_does_not_block_a_writable_sibling() {
    let (mut app, _terminal, first_bytes) = app_with_writer();
    let first = app.active_session_token_for_test();

    let dimensions = NativeOptions::default().initial_grid;
    let sibling_bytes = Arc::new(Mutex::new(Vec::new()));
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter(sibling_bytes.clone()))));
    let sibling_terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    app.seed_headless_split_pane_for_test(true, sibling_terminal, writer, dimensions);
    let sibling = app
        .active_tab_pane_tokens_for_test()
        .into_iter()
        .find(|token| *token != first)
        .expect("sibling pane token");

    assert!(app.set_pane_read_only(first, true));
    app.focus_session_token_for_test(sibling);
    assert!(app.active_pane_accepts_input());
    app.drive_raw_key_event_for_test(
        WinitKey::Character("s".into()),
        WinitKey::Character("s".into()),
        PhysicalKey::Code(KeyCode::KeyS),
        Modifiers::NONE,
        KeyEventType::Press,
    );
    assert_eq!(bytes(&sibling_bytes), b"s");
    assert!(bytes(&first_bytes).is_empty());
}

#[test]
fn splitting_a_read_only_pane_creates_a_writable_sibling() {
    let (mut app, _terminal, _recorded) = app_with_writer();
    let original = make_read_only(&mut app);
    let dimensions = NativeOptions::default().initial_grid;
    let sibling_writer = crate::native::test_support::headless_writer();
    let sibling_terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));

    app.seed_headless_split_pane_for_test(true, sibling_terminal, sibling_writer, dimensions);

    let tokens = app.active_tab_pane_tokens_for_test();
    assert_eq!(tokens.len(), 2);
    assert!(!app.pane_accepts_input(original));
    let sibling = tokens.into_iter().find(|token| *token != original).unwrap();
    assert!(app.pane_accepts_input(sibling), "split starts writable");
}

#[test]
fn duplicate_carries_the_source_read_only_flag_to_its_fresh_session() {
    let (mut app, _terminal, _recorded) = app_with_writer();
    let source = make_read_only(&mut app);
    let dimensions = NativeOptions::default().initial_grid;
    let duplicate_position = app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(
            dimensions.columns,
            dimensions.rows,
        ))),
        crate::native::test_support::headless_writer(),
        dimensions,
    );
    assert!(app.switch_to_session_for_test(duplicate_position));
    let duplicate = app.active_session_token_for_test();
    assert_ne!(duplicate, source);
    assert!(
        app.active_pane_accepts_input(),
        "fresh session starts writable"
    );

    app.carry_read_only_to_duplicate(source);

    assert!(!app.active_pane_accepts_input());
}
