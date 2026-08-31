// SPDX-License-Identifier: GPL-3.0-only
//! Clipboard slot, paste-chunk encoding, and PTY-writer tests. (M6 mechanical split from native/tests.rs).

use super::*;

#[test]
fn clipboard_slot_retains_initialized_handle() {
    let mut slot = ClipboardSlot::default();
    let mut created = 0;

    *slot
        .get_or_try_init(|| {
            created += 1;
            Ok::<_, ()>(41)
        })
        .expect("first handle") += 1;
    let retained = *slot
        .get_or_try_init(|| {
            created += 1;
            Ok::<_, ()>(0)
        })
        .expect("retained handle");

    assert_eq!(created, 1);
    assert_eq!(retained, 42);
    assert!(slot.is_retaining_handle());
}

#[test]
fn clipboard_slot_can_drop_failed_or_stale_handle() {
    let mut slot = ClipboardSlot::default();

    let _ = slot
        .get_or_try_init(|| Ok::<_, ()>("first"))
        .expect("first handle");
    assert!(slot.is_retaining_handle());

    slot.clear();
    assert!(!slot.is_retaining_handle());

    let retained = *slot
        .get_or_try_init(|| Ok::<_, ()>("replacement"))
        .expect("replacement handle");
    assert_eq!(retained, "replacement");
}

#[test]
fn selected_text_extracts_plain_terminal_text() {
    let snapshot = snapshot(&["copy me   ", "not image "], 10);
    let range = selection::SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 6 },
    };

    assert_eq!(selection::selected_text(&snapshot, range), "copy me");
}

#[test]
fn selected_text_trims_an_all_blank_selection_to_empty() {
    let snapshot = snapshot(&["          "], 10);
    let range = selection::SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 9 },
    };

    assert!(selection::selected_text(&snapshot, range).is_empty());
}

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    flushes: Arc<Mutex<usize>>,
}

type RecordingWriterParts = (PtyWriter, Arc<Mutex<Vec<u8>>>, Arc<Mutex<usize>>);
type PasteAppParts = (App, Arc<Mutex<Vec<u8>>>, Arc<Mutex<Terminal>>);

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        *self.flushes.lock().expect("flushes") += 1;
        Ok(())
    }
}

fn recording_writer() -> RecordingWriterParts {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let flushes = recorder.flushes.clone();
    (Arc::new(Mutex::new(Box::new(recorder))), bytes, flushes)
}

#[test]
fn plain_paste_chunks_normalize_line_endings_to_carriage_return() {
    for fixture in crate::core::v013_fixtures::PLAIN_PASTE_FIXTURES {
        let chunks = encode_paste_chunks(fixture.source, false, PASTE_CHUNK_SIZE);

        assert_eq!(
            flatten_chunks(&chunks),
            fixture.expected,
            "plain-paste fixture: {}",
            fixture.label
        );
    }
}

#[test]
fn shortcut_route_reads_original_fixture_text_then_uses_plain_encoder() {
    let dimensions = Dimensions::new(80, 24);
    for fixture in crate::core::v013_fixtures::PLAIN_PASTE_FIXTURES {
        let recorder = RecordingWriter::default();
        let bytes = recorder.bytes.clone();
        let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
        let (mut app, _terminal) = headless_app_with_writer(
            NativeOptions::default(),
            dimensions,
            Settings::default(),
            writer,
        );
        app.inject_paste_text_for_test(fixture.source);

        app.handle_paste_shortcut_for_test();

        if crate::native::paste_policy::assess(fixture.source).risky {
            assert!(
                bytes.lock().expect("paste bytes before confirm").is_empty(),
                "risky source is held before confirmation: {}",
                fixture.label
            );
            assert!(app.risky_paste_pending_for_test());
            app.confirm_risky_paste_for_test(false);
        }

        assert_eq!(
            &*bytes.lock().expect("paste bytes"),
            fixture.expected,
            "shortcut paste fixture: {}",
            fixture.label
        );
        assert_eq!(
            app.clipboard_read_text_calls_for_test(),
            1,
            "shortcut reads the fixture once: {}",
            fixture.label
        );
    }
}

fn paste_app() -> PasteAppParts {
    let dimensions = Dimensions::new(80, 24);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        dimensions,
        Settings::default(),
        writer,
    );
    (app, bytes, terminal)
}

#[test]
fn risky_paste_cancel_and_focus_loss_write_nothing() {
    let (mut app, bytes, _) = paste_app();
    app.inject_paste_text_for_test("first\nsecond");
    app.handle_paste_shortcut_for_test();
    assert!(app.risky_paste_pending_for_test());
    app.cancel_risky_paste_for_test();
    assert!(bytes.lock().expect("cancel bytes").is_empty());

    app.inject_paste_text_for_test("again\rnext");
    app.handle_paste_shortcut_for_test();
    app.on_window_focus_changed_for_test(false);
    assert!(!app.risky_paste_pending_for_test());
    assert!(bytes.lock().expect("focus bytes").is_empty());
}

#[test]
fn safe_single_line_and_bracketed_multiline_keep_historical_bytes() {
    let (mut app, bytes, terminal) = paste_app();
    app.inject_paste_text_for_test("safe\ttext");
    app.handle_paste_shortcut_for_test();
    assert_eq!(&*bytes.lock().expect("safe bytes"), b"safe\ttext");

    bytes.lock().expect("clear bytes").clear();
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
    app.inject_paste_text_for_test("one\r\ntwo\n");
    app.handle_paste_shortcut_for_test();
    assert!(!app.risky_paste_pending_for_test());
    assert_eq!(
        &*bytes.lock().expect("bracketed bytes"),
        b"\x1b[200~one\r\ntwo\n\x1b[201~"
    );
}

#[test]
fn opt_out_and_lossless_one_line_are_explicit() {
    let (mut app, bytes, _) = paste_app();
    app.set_warn_on_risky_paste_for_test(false);
    app.inject_paste_text_for_test("one\r\ntwo");
    app.handle_paste_shortcut_for_test();
    assert_eq!(&*bytes.lock().expect("opt-out bytes"), b"one\rtwo");

    bytes.lock().expect("clear bytes").clear();
    app.set_warn_on_risky_paste_for_test(true);
    app.inject_paste_text_for_test("one\\path\r\ntwo");
    app.handle_paste_shortcut_for_test();
    app.confirm_risky_paste_for_test(true);
    assert_eq!(
        &*bytes.lock().expect("one-line bytes"),
        b"one\\\\path\\r\\ntwo"
    );
}

#[test]
fn all_text_source_entry_points_share_the_hold_policy() {
    let (mut app, bytes, _) = paste_app();
    for route in 0..6 {
        app.inject_paste_text_for_test("a\nb");
        match route {
            0 => app.handle_paste_shortcut_for_test(),
            1 => app.handle_palette_action_for_test("paste"),
            2 => app.route_context_menu_paste_for_test(),
            3 => app.handle_primary_paste_for_test(),
            4 => app.route_external_text_drop_for_test("a\nb"),
            _ => app.route_automation_paste_for_test("a\nb"),
        }
        assert!(app.risky_paste_pending_for_test());
        assert!(bytes.lock().expect("held bytes").is_empty());
        app.cancel_risky_paste_for_test();
    }
}

#[test]
fn large_risky_payload_keeps_only_a_bounded_escaped_preview() {
    let text = format!("{}\nend", "x".repeat(2 * 1024 * 1024));
    let assessment = crate::native::paste_policy::assess(&text);
    assert!(assessment.risky);
    assert!(assessment.preview_truncated);
    assert!(
        assessment.escaped_preview.len() <= crate::native::paste_policy::MAX_ESCAPED_PREVIEW_BYTES
    );
    assert_eq!(assessment.byte_count, text.len());
    assert_eq!(assessment.line_count, 2);
}

#[test]
fn alternate_screen_does_not_bypass_confirmation() {
    let (mut app, bytes, terminal) = paste_app();
    terminal.lock().expect("terminal").advance(b"\x1b[?1049h");
    app.inject_paste_text_for_test("a\nb");
    app.handle_paste_shortcut_for_test();
    assert!(bytes.lock().expect("held bytes").is_empty());
    app.confirm_risky_paste_for_test(false);
    assert_eq!(&*bytes.lock().expect("confirmed bytes"), b"a\rb");
}

#[test]
fn stale_bracketed_mode_and_pane_owner_cancel_without_writing() {
    let (mut app, bytes, terminal) = paste_app();
    app.inject_paste_text_for_test("a\nb");
    app.handle_paste_shortcut_for_test();
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
    app.confirm_risky_paste_for_test(false);
    assert!(bytes.lock().expect("stale mode bytes").is_empty());

    terminal.lock().expect("terminal").advance(b"\x1b[?2004l");
    app.inject_paste_text_for_test("c\nd");
    app.handle_paste_shortcut_for_test();
    let dimensions = Dimensions::new(80, 24);
    let other_terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    let other = app.push_headless_session_for_test(
        other_terminal,
        crate::native::test_support::headless_writer(),
        dimensions,
    );
    assert!(app.switch_to_session_for_test(other));
    assert!(!app.risky_paste_pending_for_test());
    assert!(bytes.lock().expect("owner change bytes").is_empty());
}

/// Matrix section H: the middle-click PRIMARY route classifies per platform
/// with no cross-leg inference. Linux X11/Wayland has a PRIMARY selection, so a
/// risky middle-click paste is held for confirmation exactly like the clipboard
/// route. macOS and Windows have no PRIMARY selection (`read_primary_text`
/// returns `None` at compile time), so the identical gesture writes nothing and
/// opens no dialog. Each leg asserts only its own behavior; neither is inferred
/// from the other.
#[test]
fn primary_paste_route_matches_platform_primary_availability() {
    let (mut app, bytes, _terminal) = paste_app();
    app.inject_paste_text_for_test("primary\nrisky");
    app.handle_primary_paste_for_test();

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    {
        // PRIMARY present: risky text is held before any PTY write, then the
        // confirmation is cancelled so nothing reaches the child.
        assert!(
            app.risky_paste_pending_for_test(),
            "PRIMARY-capable platform holds a risky middle-click paste"
        );
        assert!(bytes.lock().expect("primary held bytes").is_empty());
        app.cancel_risky_paste_for_test();
        assert!(bytes.lock().expect("primary cancelled bytes").is_empty());
    }

    #[cfg(not(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    )))]
    {
        // No PRIMARY selection (macOS, Windows, and other non-X11/Wayland
        // targets): the gesture is a silent no-op with no dialog and no bytes.
        assert!(
            !app.risky_paste_pending_for_test(),
            "platform without PRIMARY opens no confirmation dialog"
        );
        assert!(bytes.lock().expect("primary absent bytes").is_empty());
    }
}

#[test]
fn paste_chunks_split_large_plain_payload_without_data_loss() {
    let chunks = encode_paste_chunks("abcdefghi", false, 3);

    assert_eq!(
        chunks,
        vec![b"abc".to_vec(), b"def".to_vec(), b"ghi".to_vec()]
    );
    assert_eq!(&flatten_chunks(&chunks), b"abcdefghi");
}

#[test]
fn bracketed_paste_is_one_indivisible_framed_chunk() {
    // Framing atomicity: the start marker, body, and end marker travel as ONE
    // chunk regardless of the chunk-size hint, so no downstream drop-whole-
    // chunk policy (outbound queue overflow, attached-session frame drop) can
    // ever separate a marker from the body.
    let chunks = encode_paste_chunks("abcdefghi", true, 3);

    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks.first().map(Vec::as_slice),
        Some(b"\x1b[200~abcdefghi\x1b[201~".as_slice())
    );
}

#[test]
fn bracketed_paste_chunks_strip_embedded_end_marker_only_from_payload() {
    let chunks = encode_paste_chunks("safe\x1b[201~tail\r\nkept", true, 4);

    assert_eq!(chunks.len(), 1);
    assert_eq!(
        &flatten_chunks(&chunks),
        b"\x1b[200~safetail\r\nkept\x1b[201~"
    );
}

#[test]
fn write_chunks_blocking_writes_all_chunks_and_flushes_once() {
    let (writer, bytes, flushes) = recording_writer();
    let chunks = vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()];

    write_chunks_blocking(&writer, &chunks).expect("chunk write");

    assert_eq!(&*bytes.lock().expect("bytes"), b"onetwothree");
    assert_eq!(*flushes.lock().expect("flushes"), 1);
}

/// End-to-end PTY → core check: spawn a one-shot command on a real PTY,
/// pump its bytes into a `Terminal` exactly as the native pump thread does,
/// and assert the rendered snapshot contains the command's output.
///
/// `#[ignore]`d like the other live-PTY smoke test: it needs a real shell
/// and a PTY, so it is opt-in (`cargo test -- --ignored`).
#[test]
#[cfg(unix)]
#[ignore = "spawns a real shell on a PTY"]
fn pty_output_pumps_into_terminal_snapshot() {
    use std::io::Read;

    let dims = Dimensions::new(40, 10);
    let session = PtySession::spawn_shell_command(dims, "printf 'HELLO_ODYTTY'")
        .expect("spawn one-shot pty command");
    let mut reader = session.try_clone_reader().expect("clone reader");
    let mut terminal = Terminal::new(dims.columns, dims.rows);

    // Pump to EOF, mirroring the pump thread's read/advance loop.
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(len) => terminal.advance(&buffer[..len]),
            Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    assert!(
        terminal.screen().plain_text().contains("HELLO_ODYTTY"),
        "snapshot should contain the command output"
    );
}
