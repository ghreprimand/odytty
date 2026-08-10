// SPDX-License-Identifier: GPL-3.0-only
//! CTRL-WHEEL-ZOOM App-level wheel-routing precedence tests. They prove that
//! Ctrl+wheel adjusts the font size only while mouse reporting is off, that a
//! reporting app's Ctrl+wheel passes through to the PTY (the report gate wins),
//! that the off switch returns Ctrl+wheel to plain scrollback movement, and
//! that the zoom clamps at the supported font-size bounds. A plain wheel (no
//! Ctrl) never zooms — the byte-identical guarantee for the default path.
//!
//! Headless (no GPU/window): the wheel is driven through the real
//! `handle_mouse_wheel` routing so the precedence is pinned, not reimplemented.
//! Skipped when no PTY is available (CI sandboxes).

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;

/// Build an `App` over a one-shot PTY, feed `content` into its terminal, and
/// return it. Mirrors the `scrollbar` harness. Returns `None` when no PTY is
/// available.
fn build_app(content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let (app, terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    Some(app)
}

/// A PTY writer that records every byte written to it, so a test can assert the
/// exact mouse report a routing decision emits. (`flush` is a no-op — the byte
/// stream is what we check.)
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

/// Like [`build_app`], but routes the App's PTY writes to a recording writer so
/// a test can assert the bytes a Ctrl+wheel emits while a TUI has mouse
/// reporting enabled. A real `PtySession` is still spawned for the session
/// handle; only the write side is swapped. Returns `None` when no PTY is
/// available.
fn build_recording_app(content: &[u8]) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(COLS, ROWS);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) =
        headless_app_with_writer(NativeOptions::default(), dims, Settings::default(), writer);
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    Some((app, bytes))
}

#[test]
fn ctrl_wheel_zooms_font_when_reporting_off() {
    let Some(mut app) = build_app(b"") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let base = app.font_size_px_for_test();
    assert_eq!(base, DEFAULT_FONT_SIZE_PX);

    // Ctrl held, reporting off (default): wheel up grows the font by one step,
    // wheel down shrinks it back.
    app.set_ctrl_modifier_for_test(true);
    app.dispatch_wheel_for_test(1.0);
    assert_eq!(app.font_size_px_for_test(), base + 1.0);
    let expected_hud = format!("Font {:.0} px", base + 1.0);
    assert_eq!(
        app.transient_hud_text_for_test(),
        Some(expected_hud.as_str()),
        "a successful zoom step raises the centered font-size HUD"
    );
    app.dispatch_wheel_for_test(-1.0);
    assert_eq!(app.font_size_px_for_test(), base);
}

#[test]
fn plain_wheel_without_ctrl_never_zooms() {
    // The byte-identical default path: a wheel with no Ctrl scrolls scrollback
    // and never touches the font size, even with `wheel_zoom` on.
    let Some(mut app) = build_app(&b"scrollback line\r\n".repeat(80)) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let base = app.font_size_px_for_test();
    // Ctrl is never set in this test, so the zoom branch cannot fire.
    app.dispatch_wheel_for_test(1.0);
    assert_eq!(
        app.font_size_px_for_test(),
        base,
        "a plain wheel must not change the font size"
    );
    assert_eq!(app.transient_hud_text_for_test(), None);
}

#[test]
fn ctrl_wheel_does_not_zoom_in_a_mouse_reporting_app() {
    // The report gate sits before the zoom branch: when a TUI has enabled mouse
    // reporting, Ctrl+wheel must report to the PTY exactly as a normal wheel
    // notch does, and never zoom. Both halves are pinned — the font size stays
    // put AND the emitted bytes match the production wheel-up report at the
    // pointer cell — so this proves the report path actually ran, not merely
    // that the zoom branch was skipped.
    let Some((mut app, pty_bytes)) = build_recording_app(b"") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let base = app.font_size_px_for_test();
    app.enable_mouse_reporting_for_test();
    assert!(app.would_report_mouse_to_pty_for_test());

    // A cached pointer cell is required for the legacy (non-pixel) report
    // encoding; without it `send_mouse_report` would emit nothing, and the test
    // could not tell a routed-but-empty report from a swallowed event.
    let (row, column) = (4, 9);
    app.set_pointer_cell_for_test(row, column);

    app.set_ctrl_modifier_for_test(true);
    app.dispatch_wheel_for_test(1.0);

    assert_eq!(
        app.font_size_px_for_test(),
        base,
        "Ctrl+wheel in a mouse-reporting app must not change the font size"
    );
    assert_eq!(app.transient_hud_text_for_test(), None);

    // DECSET 1000 selects Normal tracking with the legacy (Default) encoding.
    // Re-derive the expected report through the production encoder so this pins
    // routing (correct cell, wheel-up button, Ctrl modifier carried through)
    // without duplicating the encoder's modifier math.
    let protocol = MouseProtocol {
        tracking: MouseTracking::Normal,
        encoding: crate::core::MouseEncoding::Default,
    };
    let expected = encode_native_mouse_report(
        protocol,
        CellPoint { row, column },
        CoreMouseButton::WheelUp,
        MouseEventKind::Press,
        Modifiers {
            shift: false,
            ctrl: true,
            alt: false,
        },
    )
    .expect("a wheel-up report at a valid cell");
    assert!(
        !expected.is_empty(),
        "the production encoder must emit a wheel-up report"
    );
    assert_eq!(
        &*pty_bytes.lock().expect("pty bytes"),
        expected.as_slice(),
        "Ctrl+wheel under mouse reporting must emit the normal wheel-up report"
    );
}

#[test]
fn ctrl_wheel_over_open_overlay_does_not_zoom() {
    // Overlay precedence (UX4-P1): an open settings panel captures the wheel
    // first to scroll its own list, before the report gate or the zoom branch.
    // So Ctrl+wheel over an open overlay must not change the font size — this is
    // intentional, not an oversight.
    let Some(mut app) = build_app(b"") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.open_settings_overlay_for_test();
    let base = app.font_size_px_for_test();
    app.set_ctrl_modifier_for_test(true);
    app.dispatch_wheel_for_test(1.0);
    assert_eq!(
        app.font_size_px_for_test(),
        base,
        "an open overlay captures the wheel; Ctrl+wheel must not zoom"
    );
    assert_eq!(app.transient_hud_text_for_test(), None);
}

#[test]
fn wheel_zoom_off_switch_routes_ctrl_wheel_as_plain_scroll() {
    // Inverted-gate parity: with `wheel_zoom = false`, Ctrl+wheel falls through
    // to plain scrollback movement — the font is untouched and the viewport
    // scrolls into history exactly as a plain wheel would.
    let Some(mut app) = build_app(&b"scrollback line\r\n".repeat(80)) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    if app.scrollback_len_for_test() == 0 {
        eprintln!("skipping: no scrollback materialized");
        return;
    }
    let base = app.font_size_px_for_test();
    assert_eq!(app.viewport_offset_for_test(), 0, "starts at the live tail");

    app.set_wheel_zoom_for_test(false);
    app.set_ctrl_modifier_for_test(true);
    app.dispatch_wheel_for_test(1.0); // wheel up = into history

    assert_eq!(
        app.font_size_px_for_test(),
        base,
        "off switch: Ctrl+wheel must not zoom"
    );
    assert!(
        app.viewport_offset_for_test() > 0,
        "off switch: Ctrl+wheel scrolls scrollback like a plain wheel"
    );
}

#[test]
fn ctrl_wheel_clamps_at_font_size_bounds() {
    // Zooming past either bound is a clean no-op (the clamp leaves the size
    // unchanged, which `apply_reloadable_values` treats as no change).
    //
    // WHEEL-SENS: Ctrl+wheel is now debounced to at most ONE font step per
    // physical notch (the cap that ends runaway zoom), so reaching a bound takes
    // one notch per pixel of headroom rather than a single giant event. Each
    // `dispatch_wheel_for_test(1.0)` is one clean discrete notch == one step.
    let Some(mut app) = build_app(b"") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_ctrl_modifier_for_test(true);

    // Step up one notch at a time until pinned at the maximum. The font range is
    // bounded, so enough single notches always reach the ceiling; a further
    // notch up then stays clamped.
    let steps_to_span =
        (crate::settings::MAX_FONT_SIZE_PX - crate::settings::MIN_FONT_SIZE_PX).ceil() as usize + 2;
    for _ in 0..steps_to_span {
        app.dispatch_wheel_for_test(1.0); // one notch == one +1px step
    }
    assert_eq!(
        app.font_size_px_for_test(),
        crate::settings::MAX_FONT_SIZE_PX,
        "single notches accumulate to the maximum"
    );
    app.dispatch_wheel_for_test(1.0);
    assert_eq!(
        app.font_size_px_for_test(),
        crate::settings::MAX_FONT_SIZE_PX,
        "already at max: a further zoom-in is a no-op"
    );

    // ...and symmetrically at the minimum.
    for _ in 0..steps_to_span {
        app.dispatch_wheel_for_test(-1.0); // one notch == one -1px step
    }
    assert_eq!(
        app.font_size_px_for_test(),
        crate::settings::MIN_FONT_SIZE_PX,
        "single notches accumulate to the minimum"
    );
    app.dispatch_wheel_for_test(-1.0);
    assert_eq!(
        app.font_size_px_for_test(),
        crate::settings::MIN_FONT_SIZE_PX,
        "already at min: a further zoom-out is a no-op"
    );
}
