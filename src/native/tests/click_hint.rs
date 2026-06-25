// SPDX-License-Identifier: GPL-3.0-only
//! UX-A (Phase 11): click-to-open discoverability — App-level wiring tests for
//! the Ctrl+hover armed underline and the bottom-left mis-click hint.
//!
//! The timing rules of the mis-click trigger (the ≥2-in-window, cooldown, and
//! expiry logic) are unit-tested purely against `ClickHintState` in
//! `app::click_hint`; these drive the production `App` pointer path headlessly to
//! pin the WIRING: that a Ctrl+hover over a resolved path arms the underline, a
//! plain hover does not, and two plain mis-clicks raise the hint while the gates
//! (`interactive_paths`, `interactive_paths_click_hint`) silence it correctly.
//!
//! Synthetic only: an absolute path is printed into the grid and the stat-gate is
//! a `MapProbe` over a fixed in-memory map, so no test reaches the real
//! filesystem. `/proj/...` is fabricated, never a real machine path. Skipped when
//! no PTY is available (CI sandboxes).

use super::*;
use crate::native::app::interactive_paths::MapProbe;
use crate::paths::FsKind;
use winit::event::MouseButton as WinitMouseButton;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 10;

const PATH: &[u8] = b"/proj/src/main.rs";
const PATH_LEN: usize = 17; // "/proj/src/main.rs"

fn build_app(content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    let pty = Arc::new(Mutex::new(session));
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    Some(app)
}

/// Position the pointer inside the path span at row 0 (col ~5 of the 17-char
/// path) so `hovered_path` latches.
fn hover_on_path(app: &mut App) {
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
}

/// One plain (no-modifier) left click at the cached pointer position.
fn plain_left_click(app: &mut App) {
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
}

// ── Armed underline (Ctrl+hover) ───────────────────────────────────────────

#[test]
fn ctrl_hover_over_resolved_path_arms_the_underline() {
    let Some(mut app) = build_app(PATH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    hover_on_path(&mut app);
    assert!(app.hovered_path_for_test().is_some(), "path is hovered");

    // Plain hover: no underline armed (hand cursor only).
    assert_eq!(
        app.armed_underline_cells_for_test(),
        None,
        "plain hover does not arm the underline"
    );

    // Ctrl held: the span is armed across its full cell range.
    app.set_ctrl_modifier_for_test(true);
    assert_eq!(
        app.armed_underline_cells_for_test(),
        Some((0, 0, PATH_LEN)),
        "ctrl+hover arms the underline over the whole path span"
    );

    // Releasing Ctrl disarms it again.
    app.set_ctrl_modifier_for_test(false);
    assert_eq!(app.armed_underline_cells_for_test(), None);
}

#[test]
fn armed_underline_is_inert_when_feature_off() {
    let Some(mut app) = build_app(PATH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Feature OFF (default): even with Ctrl held over the path text, nothing is
    // hovered (the scanner never runs) so nothing is armed — byte-identical.
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    app.set_ctrl_modifier_for_test(true);
    hover_on_path(&mut app);
    assert!(app.hovered_path_for_test().is_none());
    assert_eq!(app.armed_underline_cells_for_test(), None);
}

#[test]
fn armed_underline_absent_over_unresolved_span() {
    let Some(mut app) = build_app(PATH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    // Empty fs map: the span is syntactically a path but resolves to nothing.
    app.set_test_path_probe_for_test(MapProbe::default());
    app.set_ctrl_modifier_for_test(true);
    hover_on_path(&mut app);
    assert_eq!(app.armed_underline_cells_for_test(), None);
}

// ── Mis-click hint ─────────────────────────────────────────────────────────

#[test]
fn two_plain_misclicks_on_a_path_raise_the_hint() {
    let Some(mut app) = build_app(PATH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    hover_on_path(&mut app);
    assert!(app.hovered_path_for_test().is_some());

    assert!(!app.click_hint_shown_for_test(), "clean start");
    plain_left_click(&mut app);
    assert!(
        !app.click_hint_shown_for_test(),
        "a single mis-click does not raise the hint"
    );
    plain_left_click(&mut app);
    assert!(
        app.click_hint_shown_for_test(),
        "two mis-clicks within the window raise the hint"
    );
}

#[test]
fn hint_silenced_when_click_hint_knob_off() {
    let Some(mut app) = build_app(PATH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_interactive_paths_click_hint_for_test(false);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    hover_on_path(&mut app);
    assert!(app.hovered_path_for_test().is_some());

    plain_left_click(&mut app);
    plain_left_click(&mut app);
    assert!(
        !app.click_hint_shown_for_test(),
        "the knob off silences the hint even after two mis-clicks"
    );
}

#[test]
fn hint_inert_when_interactive_paths_off() {
    let Some(mut app) = build_app(PATH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Master gate OFF (default): no hover, no mis-click tracking, no hint.
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    hover_on_path(&mut app);
    plain_left_click(&mut app);
    plain_left_click(&mut app);
    assert!(
        !app.click_hint_shown_for_test(),
        "feature off: nothing tracks or shows"
    );
}

#[test]
fn misclick_off_a_path_does_not_raise_the_hint() {
    let Some(mut app) = build_app(b"plain text with no path") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::default());
    // Hover over plain text — no resolved path.
    app.pointer_move_for_test(f64::from(CELL_W) * 2.5, f64::from(CELL_H) * 0.5);
    assert!(app.hovered_path_for_test().is_none());
    plain_left_click(&mut app);
    plain_left_click(&mut app);
    assert!(
        !app.click_hint_shown_for_test(),
        "clicks off a path are never mis-clicks"
    );
}
