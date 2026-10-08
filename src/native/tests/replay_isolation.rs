// SPDX-License-Identifier: GPL-3.0-only
//! Phase 2 replay isolation: output recording and the replay overlay never
//! mutate live core terminal state (the Phase 2 tests-box third part).
//!
//! The recorder lives off the render path (the PTY pump clones the live
//! snapshot it just produced), and the overlay scrubs a *decoupled clone* of
//! the ring, so the live terminal frame is byte-identical whether or not replay
//! is active. These tests pin that contract.

use crate::core::Terminal;
use crate::native::output_recorder::RecorderHandle;
use crate::native::overlay::OverlayInput;
use crate::native::replay_overlay::ReplayOverlay;

#[test]
fn replay_overlay_never_mutates_live_terminal_state() {
    let mut term = Terminal::new(24, 6);
    term.advance(b"first\r\n");
    let recorder = RecorderHandle::new();
    recorder.set_enabled(true);
    recorder.record(term.snapshot());
    term.advance(b"second\r\n");
    recorder.record(term.snapshot());

    // The authoritative live frame BEFORE the overlay opens.
    let live_before = term.snapshot();

    // Open replay over a decoupled clone and scrub all over it.
    let mut overlay = ReplayOverlay::new();
    overlay.open(recorder.frames_clone());
    overlay.handle_input(OverlayInput::Home);
    overlay.handle_input(OverlayInput::End);
    overlay.handle_input(OverlayInput::Left);
    overlay.handle_input(OverlayInput::Right);

    // The live terminal frame is byte-identical whether or not replay is active.
    let live_after = term.snapshot();
    assert_eq!(
        live_before.cells, live_after.cells,
        "replay scrubbing must not mutate live terminal state"
    );
    assert_eq!(live_before.cursor, live_after.cursor);

    // Recording keeps working independently while the overlay is open, and the
    // overlay's frozen view does not change underneath the user.
    term.advance(b"third\r\n");
    recorder.record(term.snapshot());
    assert_eq!(recorder.len(), 3);
    assert_eq!(
        overlay.frame_count(),
        2,
        "the overlay holds a frozen clone, decoupled from the live ring"
    );
}

#[test]
fn disabled_recorder_keeps_plain_path_state_free() {
    // RECORDING-OFF: with recording disabled the pump-equivalent path records
    // nothing, so the ring stays empty and opening replay shows no frames.
    let mut term = Terminal::new(20, 4);
    let recorder = RecorderHandle::new();
    // Mirror the pump's gate exactly: only record when enabled.
    term.advance(b"hello\r\n");
    if recorder.is_enabled() {
        recorder.record(term.snapshot());
    }
    assert_eq!(recorder.len(), 0);

    let mut overlay = ReplayOverlay::new();
    overlay.open(recorder.frames_clone());
    assert_eq!(overlay.frame_count(), 0);
}

#[test]
fn reopening_replay_with_changed_marks_invalidates_the_app_frame() {
    use super::*;

    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = headless_app_for_test();
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    let recorder = app.sessions_mut_for_test().active().recorder.clone();
    // Equal frame counts and cursors isolate the frozen payload's identity.
    let mut fingerprints = Vec::new();
    let mut previous = None;
    for text in ["a\u{301}", "a\u{302}", "a\u{301}\u{302}"] {
        let mut recorded = Terminal::new(80, 24);
        recorded.advance(text.as_bytes());
        recorder.set_enabled(false);
        recorder.set_enabled(true);
        recorder.record(recorded.snapshot());
        let live_before = terminal.lock().unwrap().snapshot();
        app.drive_char_with_mods_for_test('r', true, true);
        assert_eq!(app.overlay_signature_for_test().mode, OverlayMode::Replay);
        let (signature, _) = app
            .redraw_single_pane_probe_for_test()
            .expect("headless frame probe");
        let replay = &signature.content.overlay.replay;
        assert_eq!((replay.frames_len, replay.cursor), (1, 0));
        if let Some(previous) = &previous {
            assert_eq!(
                RenderSignature::update_from(Some(previous), &signature),
                GeometryUpdate::Full
            );
            assert_ne!(
                previous.content.overlay.replay.frame_fingerprint,
                replay.frame_fingerprint
            );
        }
        fingerprints.push(replay.frame_fingerprint);
        previous = Some(signature);
        app.drive_named_key_for_test(NamedKey::Escape);
        assert!(!app.overlay_signature_for_test().open);
        assert_eq!(terminal.lock().unwrap().snapshot().cells, live_before.cells);
    }
    assert!(fingerprints.windows(2).all(|pair| pair[0] != pair[1]));
}
