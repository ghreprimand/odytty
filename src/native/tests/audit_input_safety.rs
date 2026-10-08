// SPDX-License-Identifier: GPL-3.0-only
//! Synthetic end-to-end probes for control-bearing palette input and
//! read-only pane side effects. No payload is sent to a real shell.

use super::*;
use crate::native::layout::SplitAxis;
use crate::native::overlay::{OverlayInput, OverlayOutcome, OverlayUi};
use crate::native::session::{HeadlessSession, Session, SessionToken, WorkspaceSet};
use std::io::Write;

#[cfg(unix)]
struct BlockingHostWriter {
    started: std::sync::mpsc::Sender<()>,
    block: Arc<(Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(unix)]
impl Write for BlockingHostWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.started.send(());
        let (lock, ready) = &*self.block;
        let mut released = lock.lock().expect("host-writer test latch");
        while !*released {
            released = ready.wait(released).expect("host-writer test latch");
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
struct ReleaseHostWriterOnDrop(Arc<(Mutex<bool>, std::sync::Condvar)>);

#[cfg(unix)]
impl Drop for ReleaseHostWriterOnDrop {
    fn drop(&mut self) {
        let (lock, ready) = &*self.0;
        *lock.lock().expect("host-writer test latch") = true;
        ready.notify_all();
    }
}

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

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("synthetic writer failure"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("synthetic writer failure"))
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

/// PRIMARY exists only on Linux and the BSDs; on other targets the test is
/// absent rather than an empty pass.
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
#[test]
fn read_only_primary_selection_paste_is_refused_before_reading_primary() {
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

#[cfg(unix)]
#[test]
fn hosted_pty_queue_does_not_silently_discard_attached_paste_bytes() {
    use crate::session_host::HostPtyWriter;
    use std::sync::mpsc;
    use std::time::Duration;

    let (started_tx, started_rx) = mpsc::channel();
    let block = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let _release = ReleaseHostWriterOnDrop(Arc::clone(&block));
    let writer = HostPtyWriter::spawn(Box::new(BlockingHostWriter {
        started: started_tx,
        block,
    }))
    .expect("spawn bounded host writer");

    writer.write(b"prime the blocked PTY write");
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("writer thread enters its blocked write");
    let paste = vec![b'p'; 6 * 1024 * 1024];
    writer.write(&paste);

    assert_eq!(
        writer.dropped_bytes(),
        0,
        "accepted attached paste bytes must not be discarded silently"
    );
}

#[test]
fn bracketed_paste_cap_matches_attach_frame_and_refuses_before_writing() {
    use crate::native::clipboard::encode_paste_chunks;
    use crate::native::clipboard::{MAX_BRACKETED_PASTE_BYTES, PasteError, write_paste_text};
    use crate::native::pty::PASTE_CHUNK_SIZE;
    use crate::session_host::protocol::MAX_CLIENT_INPUT_LEN;

    const { assert!(MAX_BRACKETED_PASTE_BYTES <= MAX_CLIENT_INPUT_LEN) };

    let body = "x".repeat(MAX_BRACKETED_PASTE_BYTES - 12);
    let chunks = encode_paste_chunks(&body, true, PASTE_CHUNK_SIZE);

    assert_eq!(chunks.len(), 1, "bracketed paste framing stays atomic");
    assert!(chunks[0].len() <= MAX_CLIENT_INPUT_LEN);

    let (terminal, writer, bytes) = bracketed_paste_sink();
    write_paste_text(&terminal, &writer, &body).expect("paste at the limit is delivered");
    assert_eq!(recorded(&bytes).len(), chunks[0].len());

    let over_limit = "x".repeat(MAX_BRACKETED_PASTE_BYTES - 11);
    let (terminal, writer, bytes) = bracketed_paste_sink();
    assert!(matches!(
        write_paste_text(&terminal, &writer, &over_limit),
        Err(PasteError::TooLarge { .. })
    ));
    assert!(
        recorded(&bytes).is_empty(),
        "refusal writes no prefix or body"
    );
}

#[test]
fn app_surfaces_bracketed_paste_refusal_without_writing() {
    use crate::native::clipboard::MAX_BRACKETED_PASTE_BYTES;

    let (mut app, terminal, bytes) = input_app();
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
    let clipboard = "x".repeat(MAX_BRACKETED_PASTE_BYTES - 11);
    app.enable_osc52_read_for_test(&clipboard);

    app.handle_paste_shortcut_for_test();

    assert!(recorded(&bytes).is_empty(), "refused paste writes no bytes");
    assert!(
        app.open_notice_message_for_test()
            .as_deref()
            .is_some_and(|notice| notice.starts_with("Paste refused:"))
    );
}

#[test]
fn app_surfaces_writer_failure_for_keyboard_input() {
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(FailingWriter)));
    let (mut app, _terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );

    app.drive_char_with_mods_for_test('k', false, false);

    assert!(
        app.open_notice_message_for_test()
            .as_deref()
            .is_some_and(|notice| notice.starts_with("Input not delivered"))
    );
}

#[test]
fn app_surfaces_oversized_osc52_read_refusal_without_reply() {
    use crate::core::OSC52_CLIPBOARD_MAX_BYTES;

    let (mut app, _terminal, bytes) = input_app();
    let clipboard = "x".repeat(OSC52_CLIPBOARD_MAX_BYTES + 1);
    app.enable_osc52_read_for_test(&clipboard);
    app.set_window_focus_for_test(true);
    app.advance_primary_terminal_for_test(b"\x1b]52;c;?\x07");

    app.drain_clipboard_requests_for_test();

    assert!(
        recorded(&bytes).is_empty(),
        "refused read queues no host reply"
    );
    assert!(
        app.open_notice_message_for_test()
            .as_deref()
            .is_some_and(|notice| notice.starts_with("Clipboard read refused:"))
    );
}

type PasteSink = (Arc<Mutex<Terminal>>, PtyWriter, Arc<Mutex<Vec<u8>>>);

fn bracketed_paste_sink() -> PasteSink {
    let terminal = Arc::new(Mutex::new(Terminal::new(80, 24)));
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
    let recorder = RecordingWriter::default();
    let bytes = Arc::clone(&recorder.0);
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    (terminal, writer, bytes)
}

#[test]
fn osc52_reply_obeys_inbound_cap_and_refuses_oversize_whole() {
    use crate::core::{ClipboardSelection, OSC52_CLIPBOARD_MAX_BYTES};

    let mut terminal = Terminal::new(80, 24);
    let at_limit = "x".repeat(OSC52_CLIPBOARD_MAX_BYTES);
    assert!(terminal.answer_clipboard_read(ClipboardSelection::Clipboard, &at_limit));
    assert_eq!(
        terminal.take_host_output(),
        format!("\x1b]52;c;{}\x1b\\", base64_for_test(at_limit.as_bytes())).as_bytes(),
        "reply at the cap decodes to the entire clipboard"
    );

    for oversized in [
        "x".repeat(OSC52_CLIPBOARD_MAX_BYTES + 1),
        "x".repeat(256 * 1024),
    ] {
        assert!(!terminal.answer_clipboard_read(ClipboardSelection::Clipboard, &oversized));
        assert!(
            terminal.take_host_output().is_empty(),
            "oversized OSC 52 read is refused whole without a reply"
        );
    }
}

fn base64_for_test(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}
