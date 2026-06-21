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
