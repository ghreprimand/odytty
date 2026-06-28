// SPDX-License-Identifier: GPL-3.0-only
//! SH2 native wiring tests: OSC 133 prompt-jump viewport navigation and the
//! command success/fail gutter's inverted gate.
//!
//! Headless (no GPU/window): the cell size the gutter geometry needs is injected
//! via the same test seam the scrollbar tests use, and the jump is driven
//! directly through the `App` handlers rather than synthesised key events.
//! Skipped when no PTY is available (CI sandboxes), mirroring the other App-level
//! suites.

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 10;

/// Build an `App` over a one-shot PTY with the given settings, feed `content`
/// into its terminal, and inject the cell size so the gutter geometry can run
/// without a GPU. Returns `None` when no PTY is available.
fn build_app(settings: Settings, content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
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
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    Some(app)
}

/// Output with two prompt marks: a prompt on the oldest physical row (row 0),
/// then enough lines to push it well into scrollback, then a second prompt near
/// the live tail. `\x1b]133;A\x07` is the OSC 133 prompt-start mark.
fn two_prompts_with_scrollback() -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(b"\x1b]133;A\x07first prompt\r\n");
    for i in 0..60 {
        content.extend_from_slice(format!("output line {i}\r\n").as_bytes());
    }
    content.extend_from_slice(b"\x1b]133;A\x07second prompt\r\n");
    content.extend_from_slice(b"done\r\n");
    content
}

#[test]
fn prompt_prev_from_live_tail_scrolls_to_the_earlier_prompt() {
    let Some(mut app) = build_app(Settings::default(), &two_prompts_with_scrollback()) else {
        return; // no PTY in this environment
    };
    // Enough output was fed to create scrollback with a prompt at row 0.
    if app.scrollback_len_for_test() == 0 {
        return;
    }
    assert_eq!(app.viewport_offset_for_test(), 0, "starts at the live tail");

    // Prev finds the prompt above the viewport top and scrolls up to it.
    assert!(
        app.jump_prompt_prev(),
        "an earlier prompt exists -> consumed"
    );
    assert!(
        app.viewport_offset_for_test() > 0,
        "scrolled back into history toward the earlier prompt"
    );

    // A second Prev sits at the oldest prompt (row 0): nothing earlier, so it
    // reports not-consumed and the chord would fall through to the PTY.
    assert!(
        !app.jump_prompt_prev(),
        "no prompt before the first -> falls through"
    );
}

/// A single prompt on the oldest row, then enough output to push it into
/// scrollback. From the live tail the only prompt is *above* the viewport top,
/// so a forward jump has nothing past it.
fn single_prompt_in_scrollback() -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(b"\x1b]133;A\x07only prompt\r\n");
    for i in 0..60 {
        content.extend_from_slice(format!("output line {i}\r\n").as_bytes());
    }
    content
}

#[test]
fn prompt_next_with_no_prompt_below_falls_through() {
    let Some(mut app) = build_app(Settings::default(), &single_prompt_in_scrollback()) else {
        return;
    };
    if app.scrollback_len_for_test() == 0 {
        return;
    }
    // The only prompt sits above the live-tail viewport top, so a forward jump
    // clamps (no wrap) and falls through to the PTY.
    assert!(
        !app.jump_prompt_next(),
        "no prompt past the viewport -> falls through"
    );

    // Prev still finds it (it is above), confirming the fixture has a reachable
    // prompt and the direction gating is correct.
    assert!(
        app.jump_prompt_prev(),
        "the scrollback prompt is reachable upward"
    );
}

#[test]
fn prompt_jump_no_marks_is_inert() {
    // No OSC 133 marks at all: both directions fall through, byte-identical to
    // an unbound chord.
    let Some(mut app) = build_app(Settings::default(), &b"plain output\r\n".repeat(40)) else {
        return;
    };
    assert!(!app.jump_prompt_prev());
    assert!(!app.jump_prompt_next());
}

/// A finished success command entirely on the live screen: prompt, output, and
/// an explicit `exit 0` close (`\x1b]133;D;0\x07`).
fn finished_success_command() -> &'static [u8] {
    b"\x1b]133;A\x07$ true\r\n\x1b]133;C\x07\x1b]133;D;0\x07"
}

#[test]
fn gutter_off_emits_no_overlays_even_with_marks() {
    let off = Settings {
        command_status_gutter: false,
        ..Settings::default()
    };
    let Some(app) = build_app(off, finished_success_command()) else {
        return;
    };
    let scrollback_len = app.scrollback_len_for_test();
    let overlays = app.command_status_gutter_overlays(
        scrollback_len,
        cell(CELL_W, CELL_H),
        WindowPadding::ZERO,
    );
    assert!(
        overlays.is_empty(),
        "the default-off gutter adds no overlay quads"
    );
}

#[test]
fn gutter_on_emits_a_bar_for_a_finished_command() {
    let on = Settings {
        command_status_gutter: true,
        ..Settings::default()
    };
    let Some(app) = build_app(on, finished_success_command()) else {
        return;
    };
    let scrollback_len = app.scrollback_len_for_test();
    let overlays = app.command_status_gutter_overlays(
        scrollback_len,
        cell(CELL_W, CELL_H),
        WindowPadding::ZERO,
    );
    assert_eq!(
        overlays.len(),
        1,
        "one finished command on screen draws one gutter bar"
    );
    // The bar hugs the left edge and is a thin column.
    let rect = overlays[0].rect;
    assert_eq!(rect[0], 0.0, "bar starts at the left edge");
    assert!(
        rect[2] > 0.0 && rect[2] < CELL_W as f32,
        "bar is a thin sliver"
    );
}
