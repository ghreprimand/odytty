// SPDX-License-Identifier: GPL-3.0-only
//! Button Protocol B3 native wiring tests: plain-click activation through the
//! production pointer ladder, the press+release same-span latch, the exact
//! report envelope on the wire, and every guard around it (master gate,
//! invalidation, focus-transfer exclusion, mouse-reporting precedence, the
//! sticky prompt-active suppression).
//!
//! Headless (no GPU/window): the App runs over a PTY whose writer is a
//! recording buffer, so the report the child WOULD receive is observable
//! byte-for-byte — an empty buffer proves a click was consumed or cancelled
//! without writing, and the envelope test pins the exact bytes the terminal
//! composes (`CSI ? 1337 ; code ~`, never emitter-supplied bytes). Driven
//! through `dispatch_mouse_button_for_test` so the ladder precedence (report
//! gate vs button arm vs selection) is exercised, not reimplemented.

use super::*;

const COLS: usize = 40;
const ROWS: usize = 6;
const CELL_W: u32 = 8;
const CELL_H: u32 = 16;

/// Tier 2 stream: `$ ` prompt text, then a `code=7` button labeled `Run!`
/// (span cells 2..6 on row 0).
const T2_BUTTON: &[u8] =
    b"$ \x1b]133;P;odytty-button;code=7\x07Run!\x1b]133;P;odytty-button;end\x07";
/// The exact envelope a `code=7` click must write.
const T2_ENVELOPE: &[u8] = b"\x1b[?1337;7~";

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

/// The shared terminal-model handle a headless App runs over.
type TerminalHandle = Arc<Mutex<crate::core::Terminal>>;
/// The recording writer's captured-bytes handle.
type CapturedBytes = Arc<Mutex<Vec<u8>>>;

/// Build a headless App over a recording writer, optionally arm ALL THREE
/// button gates (the fully-enabled configuration), and feed `content`.
/// Returns the app, the terminal handle, and the captured-bytes handle.
/// The harness seeds the terminal from `Settings` (all gates default-off), so
/// the armed fixture flips them explicitly; per-gate behavior is pinned by the
/// dedicated gate tests below and by the core suite.
fn build_app(content: &[u8], buttons_enabled: bool) -> (App, TerminalHandle, CapturedBytes) {
    let dims = Dimensions::new(COLS, ROWS);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (mut app, terminal) =
        headless_app_with_writer(NativeOptions::default(), dims, Settings::default(), writer);
    {
        let mut t = terminal.lock().expect("terminal");
        if buttons_enabled {
            t.set_buttons_enabled(true);
            t.set_buttons_iterm_compat(true);
            t.set_buttons_sticky(true);
        }
        t.advance(content);
    }
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    (app, terminal, bytes)
}

fn move_to_cell(app: &mut App, row: usize, col: usize) {
    app.pointer_move_for_test(
        f64::from(CELL_W) * (col as f64 + 0.5),
        f64::from(CELL_H) * (row as f64 + 0.5),
    );
}

fn press(app: &mut App) {
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
}

fn release(app: &mut App) {
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
}

/// Spend the startup focus-click marker (the app arms the #11167 exclusion at
/// launch, so the first content click never fires a button) with a click on
/// an empty cell far from any span.
fn settle_focus_click(app: &mut App, bytes: &CapturedBytes) {
    move_to_cell(app, 4, 30);
    press(app);
    release(app);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "the settle click must write nothing"
    );
}

fn drain(bytes: &CapturedBytes) -> Vec<u8> {
    std::mem::take(&mut *bytes.lock().expect("bytes"))
}

/// Build a headless App over a recording writer with the SUPPLIED settings and
/// NO manual gate mutation: the button gates arrive only through the shared
/// launch-session seeding (`seed_launch_session_model`) inside the harness,
/// exactly as production startup seeds the first pane.
fn build_app_seeded(
    content: &[u8],
    settings: crate::settings::Settings,
) -> (App, TerminalHandle, CapturedBytes) {
    let dims = Dimensions::new(COLS, ROWS);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (mut app, terminal) =
        headless_app_with_writer(NativeOptions::default(), dims, settings, writer);
    terminal.lock().expect("terminal").advance(content);
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    (app, terminal, bytes)
}

/// REGRESSION (launch-session gate seeding): with the `buttons` setting on,
/// the launch session must honor button sequences WITHOUT any manual
/// `set_buttons_enabled` — the gate arrives purely through the shared launch
/// seeding that `run_native` and this harness both call. The shipped bug:
/// new tabs/panes/attaches seeded the gate but the launch pane did not, and
/// no test noticed because every fixture set the gate by hand.
#[test]
fn launch_seeding_honors_the_buttons_setting_without_a_manual_gate() {
    let settings = crate::settings::Settings {
        buttons: true,
        ..crate::settings::Settings::default()
    };
    let (mut app, _terminal, bytes) = build_app_seeded(T2_BUTTON, settings);
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert_eq!(
        drain(&bytes),
        T2_ENVELOPE,
        "settings.buttons alone must arm the launch session's gate"
    );
}

/// The `buttons_iterm_compat` sub-gate rides the same launch seeding: a
/// Tier 1 definition is only accepted when settings carry BOTH the master and
/// the compat gate, with no manual setter anywhere.
#[test]
fn launch_seeding_carries_the_iterm_compat_sub_gate() {
    let settings = crate::settings::Settings {
        buttons: true,
        buttons_iterm_compat: true,
        ..crate::settings::Settings::default()
    };
    let (mut app, _terminal, bytes) = build_app_seeded(
        b"ab\x1b]1337;Button=type=custom;code=42;icon=star\x07",
        settings,
    );
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 2); // the anchor cell
    press(&mut app);
    release(&mut app);
    assert_eq!(
        drain(&bytes),
        [
            0x1b, 0x5b, 0x3f, 0x31, 0x33, 0x33, 0x37, 0x3b, 0x34, 0x32, 0x7e
        ],
        "settings-seeded sub-gate must accept the Tier 1 definition"
    );
}

/// Default settings through the same seeding leave the gate off: the label
/// prints as plain text and a click writes nothing. Pins default-off through
/// the REAL seeding path rather than through a hand-built terminal.
#[test]
fn launch_seeding_leaves_buttons_off_at_default_settings() {
    let (mut app, _terminal, bytes) =
        build_app_seeded(T2_BUTTON, crate::settings::Settings::default());
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(
        drain(&bytes).is_empty(),
        "default settings must leave the launch session's gate off"
    );
}

#[test]
fn plain_click_on_a_live_button_reports_the_exact_envelope() {
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 3); // inside the "Run!" span (cells 2..6)
    press(&mut app);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "the press alone writes nothing; the release fires"
    );
    release(&mut app);
    assert_eq!(
        drain(&bytes),
        T2_ENVELOPE,
        "release on the same span writes exactly CSI ? 1337 ; 7 ~"
    );
}

#[test]
fn tier1_point_button_click_matches_the_iterm2_example_bytes() {
    // The published example: code 42 must arrive as 1b 5b 3f 31 33 33 37 3b
    // 34 32 7e. The Tier 1 anchor's hit box is its single anchor cell.
    let (mut app, _terminal, bytes) = build_app(
        b"ab\x1b]1337;Button=type=custom;code=42;icon=star\x07",
        true,
    );
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 2); // the anchor cell
    press(&mut app);
    release(&mut app);
    assert_eq!(
        drain(&bytes),
        [
            0x1b, 0x5b, 0x3f, 0x31, 0x33, 0x33, 0x37, 0x3b, 0x34, 0x32, 0x7e
        ]
    );
}

#[test]
fn drag_off_the_span_cancels_without_writing() {
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    move_to_cell(&mut app, 2, 20); // drag off before releasing
    release(&mut app);
    assert!(
        drain(&bytes).is_empty(),
        "press+release must land on the same span"
    );
    // The cancelled gesture must not leave a stale latch: a later plain click
    // elsewhere still writes nothing.
    move_to_cell(&mut app, 3, 10);
    press(&mut app);
    release(&mut app);
    assert!(drain(&bytes).is_empty());
}

#[test]
fn gate_off_leaves_the_click_path_byte_identical() {
    // Same stream, master gate off (default): the label prints as plain text
    // and a click is an ordinary selection gesture — nothing on the wire.
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, false);
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(drain(&bytes).is_empty(), "gate off: no report, ever");
}

#[test]
fn turning_the_gate_off_kills_clickability_of_existing_spans() {
    // The partial-gate hole class: definitions accepted while the gate was on
    // must go inert the moment it turns off — enforced at the pointer arm's
    // hit-test, independent of the OSC arm.
    let (mut app, terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    terminal
        .lock()
        .expect("terminal")
        .set_buttons_enabled(false);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(drain(&bytes).is_empty(), "gate off kills existing buttons");
}

#[test]
fn settings_apply_pushes_the_button_gates_to_the_terminal_live() {
    // BUTTONS-SETTINGS: the user-facing knobs reach the terminal through the
    // same settings-apply seam the panel and config reload use. Start from
    // defaults (gate off), turn `buttons` on via a live apply, and the
    // previously-inert stream becomes clickable; apply defaults again and
    // clicks go inert without restarting the session.
    let (mut app, terminal, bytes) = build_app(b"", false);
    let on = crate::settings::Settings {
        buttons: true,
        ..crate::settings::Settings::default()
    };
    app.apply_saved_settings_live_for_test(on);
    terminal.lock().expect("terminal").advance(T2_BUTTON);
    settle_focus_click(&mut app, &bytes);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert_eq!(
        drain(&bytes),
        T2_ENVELOPE,
        "after a live settings apply the button must click"
    );

    app.apply_saved_settings_live_for_test(crate::settings::Settings::default());
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(
        drain(&bytes).is_empty(),
        "turning the setting off must kill existing buttons live"
    );
}

#[test]
fn an_invalidated_button_is_inert() {
    // A block-scoped button dies at the next OSC 133 A boundary; its dimmed
    // chip must swallow nothing and write nothing.
    let (mut app, terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]133;A\x07");
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(drain(&bytes).is_empty(), "invalidated: clicks are inert");
}

#[test]
fn a_mouse_reporting_tui_wins_the_plain_click() {
    // With DECSET 1000 active the report gate sits above the content ladder:
    // the TUI receives its mouse report and the button never fires.
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    app.enable_mouse_reporting_for_test();
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    let written = drain(&bytes);
    assert!(
        !written.is_empty(),
        "the TUI's mouse report reaches the PTY"
    );
    assert!(
        !written.windows(T2_ENVELOPE.len()).any(|w| w == T2_ENVELOPE),
        "no button envelope rides along: the app owns the click"
    );
}

#[test]
fn shift_click_reaches_the_button_under_mouse_reporting() {
    // Shift is the established local-content override while a TUI owns the
    // mouse (same convention as selection); over a chip it activates the
    // button — the explicit UI element wins the tie.
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    app.enable_mouse_reporting_for_test();
    app.set_shift_modifier_for_test(true);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert_eq!(drain(&bytes), T2_ENVELOPE);
}

#[test]
fn shift_click_without_reporting_keeps_its_selection_meaning() {
    // Outside mouse reporting Shift extends selections; a Shift+click over a
    // chip must not activate it.
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    app.set_shift_modifier_for_test(true);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(drain(&bytes).is_empty());
}

#[test]
fn the_window_activating_click_never_fires_a_button() {
    // #11167 class: a focus gain arms the exclusion; the first content click
    // is spent activating the window. The second, deliberate click fires.
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    app.on_window_focus_changed_for_test(false);
    app.on_window_focus_changed_for_test(true);
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(
        drain(&bytes).is_empty(),
        "the focus-transfer click is excluded"
    );
    press(&mut app);
    release(&mut app);
    assert_eq!(drain(&bytes), T2_ENVELOPE, "the second click fires");
}

#[test]
fn sticky_click_is_suppressed_while_the_prompt_is_active() {
    // The B3 suppression decision: at an active prompt (OSC 133 A, no C/D
    // yet) nothing can act on a sticky button's report — the shell's line
    // editor would just eat (or mangle) the bytes — so nothing is sent. Once
    // the prompt phase ends, the same button reports normally.
    let stream = b"$ \x1b]133;P;odytty-button;code=7;scope=sticky\x07Run!\
\x1b]133;P;odytty-button;end\x07";
    let (mut app, terminal, bytes) = build_app(stream, true);
    settle_focus_click(&mut app, &bytes);
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]133;A\x07");
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(
        drain(&bytes).is_empty(),
        "sticky + active prompt: suppressed"
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]133;C\x07");
    press(&mut app);
    release(&mut app);
    assert_eq!(
        drain(&bytes),
        T2_ENVELOPE,
        "prompt over: the sticky button reports"
    );
}

#[test]
fn open_modifier_click_skips_the_button_arm() {
    // Ctrl/Cmd is the open chord (OSC 8 / paths / URLs); over a chip it must
    // not activate the button.
    let (mut app, _terminal, bytes) = build_app(T2_BUTTON, true);
    settle_focus_click(&mut app, &bytes);
    if cfg!(target_os = "macos") {
        app.set_super_key_for_test(true);
    } else {
        app.set_ctrl_modifier_for_test(true);
    }
    move_to_cell(&mut app, 0, 3);
    press(&mut app);
    release(&mut app);
    assert!(drain(&bytes).is_empty());
}
