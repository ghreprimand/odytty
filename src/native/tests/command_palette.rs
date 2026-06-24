// SPDX-License-Identifier: GPL-3.0-only
use super::*;

use std::io::{self, Write};

#[derive(Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn app_with_recording_writer() -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(80, 24);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let _ = session.take_writer().ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
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

#[test]
fn accepted_palette_text_writes_to_pty_without_newline() {
    let Some((mut app, bytes)) = app_with_recording_writer() else {
        return;
    };

    app.handle_palette_type_text_for_test("cargo test".to_owned());

    assert_eq!(&*bytes.lock().expect("bytes"), b"cargo test");
}

/// v0.3.1 discoverability E2E: driving the default `Ctrl+Shift+P` chord through
/// the production key path opens the command palette overlay (no overlay was
/// open, so this exercises the global dispatch, not the overlay-key path).
#[test]
fn ctrl_shift_p_chord_opens_command_palette_overlay() {
    let Some((mut app, _bytes)) = app_with_recording_writer() else {
        return;
    };
    assert_ne!(
        app.overlay_signature_for_test().mode,
        OverlayMode::CommandPalette,
        "palette is closed before the chord"
    );
    app.drive_char_with_mods_for_test('p', true, true);
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::CommandPalette,
        "Ctrl+Shift+P opens the command palette via the production key path"
    );
}

/// Companion representative: `Ctrl+Shift+S` opens the connection manager.
#[test]
fn ctrl_shift_s_chord_opens_connection_overlay() {
    let Some((mut app, _bytes)) = app_with_recording_writer() else {
        return;
    };
    app.drive_char_with_mods_for_test('s', true, true);
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::Connections,
        "Ctrl+Shift+S opens the connection manager via the production key path"
    );
}

/// Phase 5 / B2 stale-session robustness: accepting a session id that is not in
/// the live set (it ended between the overlay listing and the attach) must fail
/// gracefully — the production attach path returns Err, which the App swallows
/// like the connect arm. This drives the same `attach_session_in_new_tab` the
/// `AttachSession` outcome calls with a synthetic, non-existent id and asserts
/// it errors instead of panicking.
#[test]
fn attaching_a_stale_session_id_errors_without_panic() {
    let Some((mut app, _bytes)) = app_with_recording_writer() else {
        return;
    };
    let result = app.attach_session_in_new_tab(None, "s-9999-stale-synthetic");
    assert!(
        result.is_err(),
        "a non-existent session id must error, not attach"
    );
    // The overlay/App is still usable afterwards — no panic, no poisoned state.
    assert_ne!(
        app.overlay_signature_for_test().mode,
        OverlayMode::SessionAttach,
        "a failed attach does not leave a phantom overlay open"
    );
}

/// Phase 5 / B2: `Ctrl+Shift+A` opens the in-window session-attach summon
/// overlay via the production key path. No live sessions exist in the test
/// runtime, so the overlay opens in its empty-list hint state — the point is the
/// chord reaches the open path and switches the overlay mode.
#[test]
fn ctrl_shift_a_chord_opens_session_attach_overlay() {
    let Some((mut app, _bytes)) = app_with_recording_writer() else {
        return;
    };
    assert_ne!(
        app.overlay_signature_for_test().mode,
        OverlayMode::SessionAttach,
        "session-attach overlay is closed before the chord"
    );
    app.drive_char_with_mods_for_test('a', true, true);
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::SessionAttach,
        "Ctrl+Shift+A opens the session-attach overlay via the production key path"
    );
}

/// `Ctrl+Shift+B` opens the theme builder (the action that gained both a chord
/// and a settings entry in v0.3.1).
#[test]
fn ctrl_shift_b_chord_opens_theme_builder_overlay() {
    let Some((mut app, _bytes)) = app_with_recording_writer() else {
        return;
    };
    app.drive_char_with_mods_for_test('b', true, true);
    assert_eq!(
        app.overlay_signature_for_test().mode,
        OverlayMode::ThemeBuilder,
        "Ctrl+Shift+B opens the theme builder via the production key path"
    );
}
