// SPDX-License-Identifier: GPL-3.0-only
//! SH-CLICK native wiring tests: OSC 133 click-to-position-cursor.
//!
//! Headless (no GPU/window): the App is driven over a one-shot PTY whose writer
//! is a recording buffer, so the cursor-positioning arrow burst it emits can be
//! asserted byte-for-byte. The click is driven through the production
//! `handle_mouse_input` press/release routing (via `left_button_outcome_for_test`)
//! so the full precedence — overlay → selection → TUI report → local — is
//! exercised, not reimplemented. Skipped when no PTY is available (CI sandboxes),
//! mirroring the other App-level suites.

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;

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

/// Build an `App` over a one-shot PTY whose writer is a recording buffer, feed
/// `content` into its terminal, and turn `sh_click` on. Returns the app plus the
/// captured-bytes handle, or `None` when no PTY is available.
fn build_app(content: &[u8]) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    build_app_with(content, true)
}

fn build_app_with(content: &[u8], sh_click: bool) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
    // Spawn provides the `pty` field; the writer is the recorder so the emitted
    // arrows are observable (the real PTY writer would swallow them into a shell).
    let _ = session.take_writer().ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
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
    app.set_sh_click_for_test(sh_click);
    Some((app, bytes))
}

/// A live prompt awaiting input, advertising click-events: an OSC 133 `A` with a
/// `click_events=1` attribute, the `$ ` prompt, the OSC 133 `B` input-start
/// mark (F2: the input-region model requires it, exactly as every bundled
/// snippet emits it), then `hello` so the input spans columns 2–6 and the
/// cursor sits at column 7.
fn live_prompt_click_enabled() -> &'static [u8] {
    b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07hello"
}

/// Drive a bare left click at `(row, column)` through the production press/release
/// routing and return the bytes the PTY writer received.
fn click_at(app: &mut App, bytes: &Arc<Mutex<Vec<u8>>>, row: usize, column: usize) -> Vec<u8> {
    app.set_pointer_cell_for_test(row, column);
    let _ = app.left_button_outcome_for_test(true); // press
    let _ = app.left_button_outcome_for_test(false); // release
    bytes.lock().expect("bytes").clone()
}

/// Like [`click_at`], but drives a sub-cell pointer PIXEL as well, so the
/// nearest-boundary (half-cell) targeting is exercised end to end (the plain
/// `click_at` leaves `pointer_px` unset and rounds down to the cell's left
/// edge). `frac` is the horizontal position within the clicked cell in
/// `[0.0, 1.0)`: `< 0.5` is the left half (caret before the glyph), `>= 0.5`
/// the right half (caret after it). A known test cell size makes the sub-cell
/// pixel math deterministic without a GPU.
fn click_at_frac(
    app: &mut App,
    bytes: &Arc<Mutex<Vec<u8>>>,
    row: usize,
    column: usize,
    frac: f64,
) -> Vec<u8> {
    app.set_test_cell_for_test(cell(8, 16));
    app.set_pointer_cell_for_test(row, column);
    let x_px = (column as f64 + frac) * 8.0;
    let y_px = (row as f64 + 0.5) * 16.0;
    app.set_pointer_px_for_test(x_px, y_px);
    let _ = app.left_button_outcome_for_test(true); // press
    let _ = app.left_button_outcome_for_test(false); // release
    bytes.lock().expect("bytes").clone()
}

/// The prompt-start (OSC 133 `A`) bytes the bash shell-integration snippet
/// emits, translated from the snippet's `printf` form into real PTY bytes,
/// followed by the prompt, the `B` input-start mark the snippet also emits
/// (asserted, so this fixture stays bound to the bundled snippet's contract),
/// and `hello` so the cursor lands at column 7. Binds the click test below to
/// the ACTUAL bundled snippet rather than a hand-written sequence.
fn bash_snippet_live_prompt() -> Vec<u8> {
    let snippet = crate::shell_integration::snippet(crate::shell_integration::ShellKind::Bash);
    let start = snippet
        .find(r"\e]133;A")
        .expect("snippet emits an OSC 133 prompt-start");
    let rest = &snippet[start..];
    let end = rest.find(r"\a").expect("prompt-start is BEL-terminated") + r"\a".len();
    let mut bytes = rest[..end]
        .replace(r"\e", "\x1b")
        .replace(r"\a", "\x07")
        .into_bytes();
    // F2: the input-region model needs the `B` input-start mark; the snippet
    // emits one at the end of PS1 (asserted here so a snippet regression that
    // drops `B` — silently disabling click-to-position — fails this test).
    assert!(
        snippet.contains(r"133;B"),
        "bash snippet must emit the OSC 133 B input-start mark"
    );
    bytes.extend_from_slice(b"$ \x1b]133;B\x07hello");
    bytes
}

#[test]
fn bundled_snippet_repositions_only_when_setting_on() {
    // The bundled snippet now advertises click_events=1, so its prompt-start
    // enables the core flag. With sh_click ON, a click on the live prompt
    // repositions (the feature can finally turn on). With sh_click OFF (the
    // default), the SAME snippet output is inert — proving the producer change
    // does NOT alter default behavior: the consumer stays gated on the setting.
    let content = bash_snippet_live_prompt();

    // sh_click ON: cursor at column 7 ("$ hello"); click column 2 -> 5x Left.
    let Some((mut app_on, bytes_on)) = build_app_with(&content, true) else {
        return; // no PTY in this environment
    };
    let written_on = click_at(&mut app_on, &bytes_on, 0, 2);
    // Under ConPTY the terminal cursor is reported one cell right of the
    // shell's true edit caret on the RightEdgeUnknown path, so the emitted
    // travel is one shorter toward the caret (Windows-only correction);
    // other platforms report an accurate cursor and are unchanged.
    let lefts = if cfg!(windows) { 4 } else { 5 };
    assert_eq!(
        written_on,
        b"\x1b[D".repeat(lefts),
        "with sh_click on, the bundled snippet's click_events=1 lets a click reposition"
    );

    // sh_click OFF (default): the same snippet output emits nothing.
    let Some((mut app_off, bytes_off)) = build_app_with(&content, false) else {
        return;
    };
    let written_off = click_at(&mut app_off, &bytes_off, 0, 2);
    assert!(
        written_off.is_empty(),
        "with sh_click off (default), the snippet's click_events=1 must not change behavior"
    );
}

#[test]
fn click_left_of_cursor_emits_left_arrows_on_live_prompt() {
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return; // no PTY in this environment
    };
    // Cursor at column 7 ("$ hello"); click column 2 -> delta -5 -> 5x Left
    // (4x on Windows/ConPTY, see below).
    let written = click_at(&mut app, &bytes, 0, 2);
    // Under ConPTY the terminal cursor is reported one cell right of the
    // shell's true edit caret on the RightEdgeUnknown path, so the emitted
    // travel is one shorter toward the caret (Windows-only correction);
    // other platforms report an accurate cursor and are unchanged.
    let lefts = if cfg!(windows) { 4 } else { 5 };
    assert_eq!(
        written,
        b"\x1b[D".repeat(lefts),
        "a bare click left of the cursor moves the shell cursor left"
    );
}

#[test]
fn click_right_of_cursor_emits_right_arrows() {
    // "$ hello" with the cursor moved back to column 4 (CUB 3): a click right
    // of the cursor, inside the typed input, yields Right arrows — one per
    // glyph between the cursor and the clicked cell.
    let Some((mut app, bytes)) =
        build_app(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07hello\x1b[3D")
    else {
        return;
    };
    // Cursor at column 4; click column 6 -> 2 glyphs ("ll") -> 2x Right
    // (3x on Windows/ConPTY, see below).
    let written = click_at(&mut app, &bytes, 0, 6);
    // Under ConPTY the terminal cursor is reported one cell right of the
    // shell's true edit caret on the RightEdgeUnknown path, so the emitted
    // travel is one shorter toward the caret (Windows-only correction);
    // other platforms report an accurate cursor and are unchanged.
    let rights = if cfg!(windows) { 3 } else { 2 };
    assert_eq!(written, b"\x1b[C".repeat(rights));
}

#[test]
fn click_with_no_typed_input_is_inert() {
    // F2 G1: a bare prompt with the `B` mark but nothing typed has no input
    // region (core returns None for empty input) — a click emits nothing. The
    // pre-F2 code sent Right arrows the shell then ignored (caret clamped at
    // an empty buffer); the new no-op is the same user-visible outcome with
    // zero bytes on the wire.
    let Some((mut app, bytes)) = build_app(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07") else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 6);
    assert!(written.is_empty(), "no editable input -> no bytes");
}

#[test]
fn click_honors_application_cursor_mode() {
    // Finding A regression guard: with DECCKM (application cursor) on, the burst
    // is the SS3 form, byte-identical to a real arrow keypress in that mode.
    let Some((mut app, bytes)) =
        build_app(b"\x1b]133;A;click_events=1\x07\x1b[?1h$ \x1b]133;B\x07hello")
    else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 2);
    // Under ConPTY the terminal cursor is reported one cell right of the
    // shell's true edit caret on the RightEdgeUnknown path, so the emitted
    // travel is one shorter toward the caret (Windows-only correction);
    // other platforms report an accurate cursor and are unchanged.
    let lefts = if cfg!(windows) { 4 } else { 5 };
    assert_eq!(written, b"\x1bOD".repeat(lefts));
}

#[test]
fn click_on_reported_cursor_cell() {
    // T4 same-cell: a click on the reported cursor's own column. On an accurate
    // cursor (Unix/macOS shells, and fish's Exact path) delta is 0 -> a no-op.
    // Under ConPTY the reported cursor sits one cell right of the true edit
    // caret on the RightEdgeUnknown path, so a click on the *reported* cursor
    // cell lands one cell right of the true caret and steps it a single Right.
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 7);
    let expected: &[u8] = if cfg!(windows) { b"\x1b[C" } else { b"" };
    assert_eq!(
        written, expected,
        "same-cell click is inert with an accurate cursor, one Right under the \
         one-cell-right ConPTY report"
    );
}

#[test]
fn click_off_path_is_inert_when_setting_off() {
    // T1: with sh_click off (the default), the click path is byte-identical to
    // today — no arrows, even though the shell advertised click-events.
    let Some((mut app, bytes)) = build_app_with(live_prompt_click_enabled(), false) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 2);
    assert!(written.is_empty(), "feature off -> no bytes emitted");
}

#[test]
fn click_without_advertised_click_events_is_inert() {
    // A plain prompt with NO click_events attribute: the core flag stays off, so
    // even with sh_click on the feature does nothing (shell-gated by construction).
    let Some((mut app, bytes)) = build_app(b"\x1b]133;A\x07$ hello") else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 2);
    assert!(written.is_empty());
}

#[test]
fn shift_click_does_not_reposition() {
    // T2: Shift is the selection/passthrough seam; SH-CLICK never reads it and
    // never fires under Shift.
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    app.set_shift_modifier_for_test(true);
    let written = click_at(&mut app, &bytes, 0, 2);
    assert!(written.is_empty(), "shift+click stays a selection gesture");
}

#[test]
fn tui_mouse_reporting_wins_over_click_to_position() {
    // T3 (highest risk): a TUI with mouse reporting active owns the click — the
    // report gate returns before the local click-to-position path is reached.
    let Some((mut app, _bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    app.enable_mouse_reporting_for_test(); // DECSET 1000
    app.set_pointer_cell_for_test(0, 2);
    app.set_pointer_px_for_test(16.0, 0.0);
    let outcome = app.left_button_outcome_for_test(true);
    assert_eq!(
        outcome, "report",
        "an active mouse-reporting mode routes the press to the app, not click-to-position"
    );
}

#[test]
fn click_during_running_command_does_not_reposition() {
    // T4 prompt-context gate: the prompt has executed (an OutputStart mark
    // exists), so there is no live prompt even though click_events is still
    // enabled. A click on the cursor row must NOT emit arrows into the program.
    let Some((mut app, bytes)) =
        build_app(b"\x1b]133;A;click_events=1\x07$ cmd\r\n\x1b]133;C\x07output")
    else {
        return;
    };
    // The cursor now sits on the output row (row 1) after "output" (column 6);
    // click left of it on the same row.
    let written = click_at(&mut app, &bytes, 1, 2);
    assert!(
        written.is_empty(),
        "a running command (no live prompt) is not click-to-position territory"
    );
}

#[test]
fn click_on_a_different_row_than_the_cursor_is_inert() {
    // F2 G2: a click off the input region's rows (here: an output row far below
    // a single-row prompt) never repositions — no wrong jump.
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 3, 2);
    assert!(written.is_empty(), "off-region click never repositions");
}

// --- F2: InputRegion-gated click-to-place (multi-row, rune accuracy, tiers) ---

#[test]
fn click_on_prompt_text_is_a_noop() {
    // F2 G2 (fails-before): the shipped code sent Left×(cursor−click) for a
    // click on the prompt text left of the input start, walking the caret to
    // buffer position 0 (benign only because the shell clamps). F2 makes the
    // prompt-side click a proper no-op.
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    // Input starts at column 2 ("$ " is prompt); click column 0.
    let written = click_at(&mut app, &bytes, 0, 0);
    assert!(
        written.is_empty(),
        "a click on the prompt (left of the input start) must not move the caret"
    );
}

#[test]
fn wide_glyph_line_counts_glyphs_not_cells() {
    // F2-NF1 (fails-before): "漢字" occupies four cells (two wide glyphs with
    // continuation spacers) but is two caret steps. The shipped raw-cell delta
    // sent 4 Lefts (caret overshoots); the glyph count sends 2.
    let Some((mut app, bytes)) =
        build_app("\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07漢字".as_bytes())
    else {
        return;
    };
    // Cursor at column 6 (after both wide glyphs); click column 2 (the first
    // glyph's lead cell) -> 2 glyphs -> 2x Left (1x on Windows/ConPTY, see below).
    let written = click_at(&mut app, &bytes, 0, 2);
    // Under ConPTY the terminal cursor is reported one cell right of the
    // shell's true edit caret on the RightEdgeUnknown path, so the emitted
    // travel is one shorter toward the caret (Windows-only correction);
    // other platforms report an accurate cursor and are unchanged.
    let lefts = if cfg!(windows) { 1 } else { 2 };
    assert_eq!(
        written,
        b"\x1b[D".repeat(lefts),
        "one wide glyph is one caret step, not two"
    );
}

#[test]
fn exact_signal_clamps_decoration_click_to_input_end() {
    // F2 Exact tier (fails-before): with a fresh private edit-region signal the
    // input's right edge is authoritative. A click on a right-aligned
    // decoration ("23.1s"-style) clamps the target to the true input end — the
    // shipped code sent Right×(click−cursor), overshooting by the decoration
    // distance and relying on the shell's clamp.
    let mut content = Vec::new();
    content.extend_from_slice(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07abc");
    // Right-aligned decoration at columns 15..19, then the cursor moved back
    // into the input (column 3, i.e. after one rune) via CHA.
    content.extend_from_slice(b"\x1b[16G23.1s\x1b[4G");
    // The shell reports its buffer: len=3 ("abc"), cur=1 -> grid cursor col 3.
    content.extend_from_slice(b"\x1b]133;P;odytty-edit;len=3;cur=1\x07");
    let Some((mut app, bytes)) = build_app(&content) else {
        return;
    };
    // Click the decoration (column 17): target clamps to the input end (rune
    // 3), cursor is at rune 1 -> exactly 2x Right, not 14.
    let written = click_at(&mut app, &bytes, 0, 17);
    assert_eq!(
        written,
        b"\x1b[C".repeat(2),
        "a decoration click moves the caret to the true input end, never past it"
    );
}

#[test]
fn exact_soft_wrap_multi_row_click_travels_horizontally() {
    // F2 multi-row Exact (fails-before): the shipped code bailed whenever the
    // click was not on the cursor's own row. A soft-wrapped logical line is one
    // buffer; a click on the FIRST row with the cursor on the SECOND travels
    // horizontally (Left only — never Up, which would recall history).
    let mut content = Vec::new();
    content.extend_from_slice(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07");
    content.extend_from_slice(&[b'a'; 78]); // fills row 0 columns 2..79
    // "bcdef" wraps onto row 1, columns 0..4. The shell then reports the full
    // buffer: 78 + 5 = 83 runes, cursor at the end.
    content.extend_from_slice(b"bcdef");
    content.extend_from_slice(b"\x1b]133;P;odytty-edit;len=83;cur=83\x07");
    let Some((mut app, bytes)) = build_app(&content) else {
        return;
    };
    // Click row 0 column 10: 8 glyphs into the input; cursor is at rune 83.
    let written = click_at(&mut app, &bytes, 0, 10);
    assert_eq!(
        written,
        b"\x1b[D".repeat(75),
        "soft-wrap travel is horizontal-only across the wrap"
    );
}

#[test]
fn heuristic_soft_wrap_multi_row_click_travels() {
    // F2 multi-row heuristic (fails-before): same geometry WITHOUT the private
    // signal (bash/PowerShell tier). The wrapped rows are known from the core
    // wrap flags; the grapheme-cell heuristic still travels across the wrap
    // (operator-approved: motion is non-destructive, a mis-land is click-again).
    let mut content = Vec::new();
    content.extend_from_slice(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07");
    content.extend_from_slice(&[b'a'; 78]);
    content.extend_from_slice(b"bcdef");
    let Some((mut app, bytes)) = build_app(&content) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 10);
    // Under ConPTY the terminal cursor is reported one cell right of the
    // shell's true edit caret on the RightEdgeUnknown path, so the emitted
    // travel is one shorter toward the caret (Windows-only correction);
    // other platforms report an accurate cursor and are unchanged.
    let lefts = if cfg!(windows) { 74 } else { 75 };
    assert_eq!(
        written,
        b"\x1b[D".repeat(lefts),
        "heuristic tier travels across soft wraps too"
    );
}

#[test]
fn hard_newline_buffer_click_is_a_noop() {
    // F2 R-None (fails-before): a multi-logical-line buffer (the signal carries
    // an `nl=` offset — begin…end / PS2 continuation) has unmodeled
    // continuation-prompt geometry, so v1 never synthesizes travel — even for a
    // same-row click the shipped code would have acted on.
    let mut content = Vec::new();
    content.extend_from_slice(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07for i in 1");
    content.extend_from_slice(b"\x1b]133;P;odytty-edit;len=10;cur=10;nl=3\x07");
    let Some((mut app, bytes)) = build_app(&content) else {
        return;
    };
    // Cursor at column 12; click column 4 on the SAME row: still a no-op.
    let written = click_at(&mut app, &bytes, 0, 4);
    assert!(
        written.is_empty(),
        "hard-newline buffers are a clean no-op in v1"
    );
}

#[test]
fn alt_screen_click_is_inert() {
    // F11: a full-screen app on the alternate screen owns its layout; the
    // explicit alt-screen gate keeps click-to-position from ever firing there.
    let mut content = Vec::new();
    content.extend_from_slice(live_prompt_click_enabled());
    content.extend_from_slice(b"\x1b[?1049hfullscreen app");
    let Some((mut app, bytes)) = build_app(&content) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 2);
    assert!(written.is_empty(), "alt screen -> no click-to-position");
}

// --- HALF-CELL (nearest-boundary) click-to-position targeting (all platforms) ---
//
// Click-to-place snaps the caret target to the nearest column BOUNDARY, so a
// click that falls in a glyph's right half places the caret AFTER it rather than
// flooring to the cell's left edge (the reported "clicking between two chars
// sometimes lands one cell left" symptom on Linux). These drive the full
// press/release routing plus the sub-cell pixel recovery. The `cfg!(windows)`
// branches account for the orthogonal ConPTY cursor-report correction (the
// heuristic path reports the cursor one cell right on native Windows).

#[test]
fn half_cell_left_and_right_half_straddle_a_glyph_boundary() {
    // "$ hello", cursor at col 7 (5 glyphs in). A click on the 'l' at col 4:
    // the LEFT half lands the caret before it (delta -3), the RIGHT half after
    // it (delta -2) — exactly one column apart.
    let lefts = if cfg!(windows) { 2 } else { 3 };
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return; // no PTY in this environment
    };
    assert_eq!(
        click_at_frac(&mut app, &bytes, 0, 4, 0.2),
        b"\x1b[D".repeat(lefts),
        "a left-half click targets the boundary before the glyph"
    );

    let rights = if cfg!(windows) { 1 } else { 2 };
    let Some((mut app2, bytes2)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    assert_eq!(
        click_at_frac(&mut app2, &bytes2, 0, 4, 0.8),
        b"\x1b[D".repeat(rights),
        "a right-half click targets the boundary after the glyph"
    );
}

#[test]
fn half_cell_right_half_of_a_wide_glyph_lands_after_the_whole_glyph() {
    // "漢字ab": 漢 spans cols 2-3, 字 cols 4-5, then a, b; cursor after b (col 8,
    // 4 glyphs in). A right-half click on 漢's lead cell (col 2) rounds across
    // its continuation cell and lands the caret AFTER the whole glyph (1 glyph
    // in -> 3 Left), never between the glyph's two cells.
    let Some((mut app, bytes)) =
        build_app("\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07漢字ab".as_bytes())
    else {
        return;
    };
    let lefts = if cfg!(windows) { 2 } else { 3 };
    assert_eq!(
        click_at_frac(&mut app, &bytes, 0, 2, 0.8),
        b"\x1b[D".repeat(lefts),
        "a right-half click on a 2-cell glyph places the caret after the whole glyph"
    );
}

#[test]
fn half_cell_last_glyph_right_half_clamps_to_the_append_origin() {
    // "$ hello" with the cursor pulled back to col 4 (2 glyphs in). A right-half
    // click on the LAST glyph 'o' (col 6) targets the append origin (5 glyphs),
    // never past it -> 3 Right.
    let rights = if cfg!(windows) { 4 } else { 3 };
    let Some((mut app, bytes)) =
        build_app(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07hello\x1b[3D")
    else {
        return;
    };
    assert_eq!(
        click_at_frac(&mut app, &bytes, 0, 6, 0.8),
        b"\x1b[C".repeat(rights),
        "a right-half click on the last glyph clamps to the append origin"
    );

    // A click well past the input's right edge clamps to the SAME append origin.
    let Some((mut app2, bytes2)) =
        build_app(b"\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07hello\x1b[3D")
    else {
        return;
    };
    assert_eq!(
        click_at_frac(&mut app2, &bytes2, 0, 10, 0.8),
        b"\x1b[C".repeat(rights),
        "a click past the input end also clamps to the append origin"
    );
}

#[test]
fn half_cell_prompt_side_right_half_is_a_noop() {
    // "$ " is the prompt (cols 0-1); the input starts at col 2. A right-half
    // click on the last prompt cell (col 1) must NOT round up across the input
    // start into a bogus travel — the guard tests the floored cell.
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    assert!(
        click_at_frac(&mut app, &bytes, 0, 1, 0.8).is_empty(),
        "a right-half click on the prompt stays a no-op"
    );
}
