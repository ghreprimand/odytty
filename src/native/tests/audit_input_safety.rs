// SPDX-License-Identifier: GPL-3.0-only
//! Synthetic end-to-end probes for control-bearing palette input and
//! read-only pane side effects. No payload is sent to a real shell.

use super::*;
use crate::native::layout::SplitAxis;
use crate::native::overlay::{OverlayInput, OverlayOutcome, OverlayUi};
use crate::native::session::{HeadlessSession, Session, SessionToken, WorkspaceSet};
use std::io::Write;

#[derive(Clone, Default)]
struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

type InputAppParts = (App, Arc<Mutex<Terminal>>, Arc<Mutex<Vec<u8>>>);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("recorded PTY bytes")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn input_app() -> InputAppParts {
    let recorder = RecordingWriter::default();
    let bytes = Arc::clone(&recorder.0);
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );
    (app, terminal, bytes)
}

fn split_input_app() -> InputAppParts {
    let dimensions = Dimensions::new(80, 24);
    let recorder = RecordingWriter::default();
    let bytes = Arc::clone(&recorder.0);
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let first_terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    let first = Session::new_headless(
        SessionToken(0),
        first_terminal,
        crate::native::test_support::headless_writer(),
        Arc::new(HeadlessSession::new(dimensions)),
    );
    let second_terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    let second = Session::new_headless(
        SessionToken(1),
        Arc::clone(&second_terminal),
        writer,
        Arc::new(HeadlessSession::new(dimensions)),
    );
    let mut sessions = WorkspaceSet::new(first, None);
    sessions.split_active_for_test(SplitAxis::Columns, second);
    let app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    (app, second_terminal, bytes)
}

fn make_read_only(app: &mut App) -> SessionToken {
    let token = app.active_session_token_for_test();
    assert!(app.set_pane_read_only(token, true));
    token
}

fn recorded(bytes: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    bytes.lock().expect("recorded PTY bytes").clone()
}

fn seed_scrollback(app: &mut App) {
    let mut output = String::new();
    for row in 0..48 {
        output.push_str(&format!("synthetic-row-{row:02}\r\n"));
    }
    app.advance_primary_terminal_for_test(output.as_bytes());
    app.scroll_up_for_test(8);
    assert!(app.viewport_offset_for_test() > 0);
}

fn palette_acceptance(
    history: &[&str],
    cwd: Option<&str>,
    query: &str,
) -> (OverlayUi, OverlayOutcome) {
    let mut overlay = OverlayUi::new(&Settings::default());
    overlay.open_command_palette_for_test(history.iter().copied(), cwd);
    for ch in query.chars() {
        overlay.handle_input(OverlayInput::Char(ch));
    }
    let outcome = overlay.handle_input(OverlayInput::Activate);
    (overlay, outcome)
}

#[test]
fn palette_refuses_control_bearing_history_payloads_and_stays_open() {
    // Candidate labels are deliberately recognizable and queryable. Their raw
    // payloads contain controls that must not become shell text on acceptance.
    for payload in [
        "synthetic-control-newline\necho second",
        "synthetic-control-return\recho second",
        "synthetic-control-tab\techo second",
        "synthetic-control-escape\x1b[31mecho second",
    ] {
        let query = payload
            .split(['\n', '\r', '\t', '\x1b'])
            .next()
            .expect("payload has a queryable label");
        let (overlay, outcome) = palette_acceptance(&[payload], None, query);

        assert!(
            !matches!(outcome, OverlayOutcome::PaletteTypeText(_)),
            "control-bearing palette payload must be refused: {payload:?}"
        );
        assert!(
            overlay.is_open(),
            "refusal must keep the actual palette open for correction: {payload:?}"
        );
    }
}

#[test]
fn palette_refuses_control_bearing_directory_payload_and_stays_open() {
    let unsafe_cwd = "/synthetic/control-dir\necho second";
    let (overlay, outcome) = palette_acceptance(&[], Some(unsafe_cwd), "control-dir");
    assert!(
        !matches!(outcome, OverlayOutcome::PaletteTypeText(_)),
        "a newline-bearing cwd candidate must not be sent to the shell"
    );
    assert!(
        overlay.is_open(),
        "directory refusal keeps the palette open"
    );
}

#[test]
fn palette_accepts_control_free_directory_payload() {
    let safe_cwd = "/synthetic/control-free-dir";
    let (_overlay, outcome) = palette_acceptance(&[], Some(safe_cwd), "control-free-dir");
    assert_eq!(
        outcome,
        OverlayOutcome::PaletteTypeText(safe_cwd.to_owned()),
        "ordinary one-line directory candidates retain their existing behavior"
    );
}

#[test]
fn palette_accepts_single_line_escaped_shell_text_without_controls() {
    let payload = "printf 'first\\nsecond'";
    let (_overlay, outcome) = palette_acceptance(&[payload], None, "printf");
    assert_eq!(
        outcome,
        OverlayOutcome::PaletteTypeText(payload.to_owned()),
        "printable backslash escapes remain eligible as literal text"
    );
}

#[test]
fn direct_palette_text_dispatch_does_not_send_control_bytes() {
    let (mut app, _terminal, bytes) = input_app();
    app.open_palette_with_synthetic_history_for_test(0);

    app.apply_overlay_outcome_for_test(OverlayOutcome::PaletteTypeText(
        "synthetic-command\nsecond-command".to_owned(),
    ));

    assert!(
        recorded(&bytes).is_empty(),
        "raw palette controls never reach the PTY"
    );
    assert!(
        app.overlay_open_for_test(),
        "refusal leaves the palette available"
    );
}

#[test]
fn read_only_prefix_passthrough_preserves_scrollback_and_sends_no_bytes() {
    let (mut app, _terminal, bytes) = split_input_app();
    assert!(app.active_pane_count_for_test() > 1);
    seed_scrollback(&mut app);
    let offset = app.viewport_offset_for_test();
    make_read_only(&mut app);

    // The default multiplexer prefix is Ctrl+B; repeating it passes one literal
    // prefix byte to the child unless read-only policy refuses user input.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('b', true, false);

    assert_eq!(
        recorded(&bytes),
        b"",
        "prefix passthrough sends no PTY bytes"
    );
    assert_eq!(
        app.viewport_offset_for_test(),
        offset,
        "a refused prefix passthrough must not return the pane to live"
    );
}

#[test]
fn read_only_clear_input_palette_action_preserves_scrollback_and_sends_no_bytes() {
    let (mut app, _terminal, bytes) = input_app();
    seed_scrollback(&mut app);
    let offset = app.viewport_offset_for_test();
    make_read_only(&mut app);

    app.handle_palette_action_for_test("clear-input");

    assert_eq!(app.viewport_offset_for_test(), offset);
    assert!(recorded(&bytes).is_empty());
}

#[test]
fn read_only_palette_text_preserves_scrollback_viewport_and_sends_no_bytes() {
    let (mut app, _terminal, bytes) = input_app();
    seed_scrollback(&mut app);
    let offset = app.viewport_offset_for_test();
    make_read_only(&mut app);

    app.handle_palette_type_text_for_test("synthetic command".to_owned());

    assert_eq!(
        app.viewport_offset_for_test(),
        offset,
        "refused palette text must not jump to the live tail"
    );
    assert!(recorded(&bytes).is_empty());
}

#[test]
fn read_only_editable_selection_delete_preserves_selection_and_sends_no_bytes() {
    let (mut app, _terminal, bytes) = input_app();
    app.advance_primary_terminal_for_test(
        b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b]133;P;odytty-edit;len=3;cur=3\x07",
    );
    app.force_selection_for_test(0, 0, 0, 4);
    assert_eq!(
        app.editable_input_selection_text_for_test().as_deref(),
        Some("abc")
    );
    let selection = app.selection_range_for_test();
    make_read_only(&mut app);

    app.drive_named_key_for_test(NamedKey::Delete);

    assert!(
        recorded(&bytes).is_empty(),
        "read-only Delete sends no edit bytes"
    );
    assert_eq!(
        app.selection_range_for_test(),
        selection,
        "refusing Delete must leave the user's selection intact"
    );
}

#[test]
fn read_only_click_to_position_sends_no_shell_edit_bytes() {
    let (mut writable, _terminal, writable_bytes) = input_app();
    writable
        .advance_primary_terminal_for_test(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07hello");
    writable.set_pointer_cell_for_test(0, 3);
    writable.left_button_outcome_for_test(true);
    writable.left_button_outcome_for_test(false);
    assert!(
        !recorded(&writable_bytes).is_empty(),
        "the synthetic prompt/click fixture exercises click-to-position"
    );

    let (mut app, _terminal, bytes) = input_app();
    app.advance_primary_terminal_for_test(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07hello");
    app.set_pointer_cell_for_test(0, 3);
    make_read_only(&mut app);

    app.left_button_outcome_for_test(true);
    app.left_button_outcome_for_test(false);

    assert!(
        recorded(&bytes).is_empty(),
        "read-only click-to-position must not send cursor movement to the shell"
    );
}

#[test]
fn read_only_primary_selection_paste_is_refused_before_reading_primary() {
    #[cfg(not(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    )))]
    return;

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    {
        let (mut app, _terminal, bytes) = input_app();
        app.enable_osc52_read_for_test("synthetic primary selection");
        make_read_only(&mut app);

        app.handle_primary_paste_for_test();

        assert_eq!(
            app.clipboard_read_text_calls_for_test(),
            0,
            "read-only refusal happens before PRIMARY clipboard access"
        );
        assert!(recorded(&bytes).is_empty());
    }
}

#[test]
fn read_only_clipboard_paste_is_refused_before_clipboard_read() {
    let (mut app, _terminal, bytes) = input_app();
    app.enable_osc52_read_for_test("synthetic clipboard text");
    make_read_only(&mut app);

    app.handle_paste_shortcut_for_test();

    assert_eq!(
        app.clipboard_read_text_calls_for_test(),
        0,
        "do not access clipboard for refused input"
    );
    assert!(recorded(&bytes).is_empty());
}
