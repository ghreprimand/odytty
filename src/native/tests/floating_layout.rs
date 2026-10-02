// SPDX-License-Identifier: GPL-3.0-only
//! Stacked and floating pane layouts through the App input routes: palette
//! actions, the keyboard arrange mode, pane focus order, and the label.

use super::*;
use crate::native::app::floating_ui::{
    ARRANGE_NEEDS_FLOAT_NOTICE, LAYOUT_NEEDS_SPLIT_NOTICE, arrange_label_text, paint_arrange_label,
};
use crate::native::session::SessionToken;
use std::io::Write;
use winit::keyboard::NamedKey;

type Recorded = Arc<Mutex<Vec<u8>>>;

#[derive(Clone, Default)]
struct RecordingWriter(Recorded);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("recorded bytes")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn recording() -> (PtyWriter, Recorded) {
    let recorded = Recorded::default();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter(recorded.clone()))));
    (writer, recorded)
}

/// A headless App with `panes` panes (the first records its writes), a fixed
/// cell size and surface, and the first pane focused.
fn app_with_panes(panes: usize) -> (App, Recorded) {
    let (writer, recorded) = recording();
    let (mut app, _terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    let first = app.active_session_token_for_test();
    for _ in 1..panes {
        let (writer, _) = recording();
        app.seed_headless_split_pane_for_test(
            true,
            Arc::new(Mutex::new(Terminal::new(40, 24))),
            writer,
            Dimensions::new(40, 24),
        );
    }
    // The test geometry seams are per session, so seed every pane's.
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(cell(8, 16));
        app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    }
    app.focus_session_token_for_test(first);
    app.reflow_active_panes_for_test();
    (app, recorded)
}

fn press(app: &mut App, key: NamedKey, code: KeyCode, shift: bool) {
    app.drive_raw_key_event_for_test(
        WinitKey::Named(key),
        WinitKey::Named(key),
        PhysicalKey::Code(code),
        Modifiers {
            ctrl: false,
            alt: false,
            shift,
        },
        KeyEventType::Press,
    );
}

fn arrow(app: &mut App, key: NamedKey, code: KeyCode, shift: bool) {
    press(app, key, code, shift);
}

fn type_char(app: &mut App, ch: char) {
    let text = ch.to_string();
    app.drive_raw_key_event_for_test(
        WinitKey::Character(text.clone().into()),
        WinitKey::Character(text.into()),
        PhysicalKey::Code(KeyCode::KeyA),
        Modifiers::NONE,
        KeyEventType::Press,
    );
}

fn bytes(recorded: &Recorded) -> Vec<u8> {
    recorded.lock().expect("recorded bytes").clone()
}

fn top_rect(app: &App) -> [f32; 4] {
    app.active_pane_rects_for_test().last().expect("a pane").1
}

fn focused_dimensions(app: &App) -> (usize, usize) {
    let token = app.active_session_token_for_test();
    let d = app
        .workspace_set()
        .get(token)
        .expect("pane")
        .terminal
        .lock()
        .expect("terminal")
        .screen()
        .dimensions();
    (d.columns, d.rows)
}

#[test]
fn the_palette_switches_a_split_tab_between_tiled_stacked_and_floating() {
    let (mut app, _bytes) = app_with_panes(2);
    let tiled = app.active_pane_rects_for_test();
    assert_eq!(tiled.len(), 2);
    assert!(app.workspace_set().active_arrangement_is_tiled());

    app.handle_palette_action_for_test("stack-panes");
    assert!(app.workspace_set().active_shows_only_focused());
    assert_eq!(app.active_pane_rects_for_test().len(), 1);

    app.handle_palette_action_for_test("float-panes");
    assert!(app.workspace_set().active_is_floating());
    assert_eq!(app.active_pane_rects_for_test().len(), 2);

    app.handle_palette_action_for_test("tile-panes");
    assert!(app.workspace_set().active_arrangement_is_tiled());
    assert_eq!(
        app.active_pane_rects_for_test(),
        tiled,
        "tiling again restores the exact geometry"
    );
}

#[test]
fn a_layout_on_a_single_pane_tab_raises_a_notice_and_changes_nothing() {
    let (mut app, _bytes) = app_with_panes(1);
    for id in ["stack-panes", "float-panes"] {
        app.handle_palette_action_for_test(id);
        assert_eq!(
            app.open_notice_message_for_test().as_deref(),
            Some(LAYOUT_NEEDS_SPLIT_NOTICE),
            "{id}"
        );
        assert!(app.workspace_set().active_arrangement_is_tiled());
    }
}

#[test]
fn arrange_needs_a_floating_tab() {
    let (mut app, _bytes) = app_with_panes(2);
    app.handle_palette_action_for_test("arrange-floating-pane");
    assert!(!app.float_arrange_active_for_test());
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some(ARRANGE_NEEDS_FLOAT_NOTICE)
    );
}

#[test]
fn arrange_mode_moves_and_resizes_from_the_keyboard_and_swallows_other_keys() {
    let (mut app, first_bytes) = app_with_panes(2);
    app.handle_palette_action_for_test("float-panes");
    let first = app.active_session_token_for_test();
    let start = top_rect(&app);
    let start_dims = focused_dimensions(&app);

    // Bare arrows reach the shell until the mode is armed.
    arrow(&mut app, NamedKey::ArrowRight, KeyCode::ArrowRight, false);
    assert!(
        !bytes(&first_bytes).is_empty(),
        "bare arrows go to the pane"
    );
    assert_eq!(top_rect(&app), start);
    let written = bytes(&first_bytes).len();

    app.handle_palette_action_for_test("arrange-floating-pane");
    assert!(app.float_arrange_active_for_test());

    // Shrink first (the tiled-to-floating conversion fills the height), then
    // move, so both directions have room.
    arrow(&mut app, NamedKey::ArrowUp, KeyCode::ArrowUp, true);
    arrow(&mut app, NamedKey::ArrowUp, KeyCode::ArrowUp, true);
    let shrunk = top_rect(&app);
    assert_eq!(
        shrunk[3],
        start[3] - 32.0,
        "Shift+Up shrinks by a cell each"
    );
    assert_eq!(shrunk[2], start[2], "a height change keeps the width");
    assert_eq!(
        focused_dimensions(&app).1,
        start_dims.1 - 2,
        "the pane's terminal follows its rectangle"
    );

    arrow(&mut app, NamedKey::ArrowRight, KeyCode::ArrowRight, false);
    arrow(&mut app, NamedKey::ArrowDown, KeyCode::ArrowDown, false);
    let moved = top_rect(&app);
    assert_eq!(moved[0], shrunk[0] + 8.0, "one cell right");
    assert_eq!(moved[1], shrunk[1] + 16.0, "one cell down");
    assert_eq!(
        (moved[2], moved[3]),
        (shrunk[2], shrunk[3]),
        "a move keeps the size"
    );

    arrow(&mut app, NamedKey::ArrowRight, KeyCode::ArrowRight, true);
    let resized = top_rect(&app);
    assert_eq!(resized[2], moved[2] + 8.0, "Shift+Right grows by a cell");

    // Other keys are swallowed, never typed into the shell.
    type_char(&mut app, 'q');
    assert_eq!(
        bytes(&first_bytes).len(),
        written,
        "no key leaks to the pty"
    );
    assert_eq!(app.active_session_token_for_test(), first);

    // Escape ends the mode, and arrows reach the shell again.
    press(&mut app, NamedKey::Escape, KeyCode::Escape, false);
    assert!(!app.float_arrange_active_for_test());
    arrow(&mut app, NamedKey::ArrowLeft, KeyCode::ArrowLeft, false);
    assert!(bytes(&first_bytes).len() > written);
    assert_eq!(top_rect(&app), resized, "the rectangle no longer moves");
}

#[test]
fn arrange_tab_cycles_every_pane_and_enter_leaves_the_mode() {
    let (mut app, _bytes) = app_with_panes(3);
    app.handle_palette_action_for_test("float-panes");
    app.handle_palette_action_for_test("arrange-floating-pane");
    let mut visited = vec![app.active_session_token_for_test()];
    for _ in 0..3 {
        press(&mut app, NamedKey::Tab, KeyCode::Tab, false);
        let token = app.active_session_token_for_test();
        if !visited.contains(&token) {
            visited.push(token);
        }
        assert_eq!(
            app.active_pane_rects_for_test().last().expect("top").0,
            token,
            "the focused pane is painted on top"
        );
    }
    assert_eq!(
        visited.len(),
        3,
        "every pane, including a buried one, is reachable"
    );
    press(&mut app, NamedKey::Enter, KeyCode::Enter, false);
    assert!(!app.float_arrange_active_for_test());
}

#[test]
fn the_arrange_mode_belongs_to_its_tab() {
    let (mut app, bytes_first) = app_with_panes(2);
    app.handle_palette_action_for_test("float-panes");
    app.handle_palette_action_for_test("arrange-floating-pane");
    assert!(app.float_arrange_active_for_test());
    let (writer, _) = recording();
    let position = app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        writer,
        Dimensions::new(80, 24),
    );
    assert!(app.switch_to_session_for_test(position));
    assert!(
        !app.float_arrange_active_for_test(),
        "a tab switch ends the mode"
    );
    assert!(app.switch_to_session_for_test(0));
    assert!(
        !app.float_arrange_active_for_test(),
        "returning to the tab does not silently re-arm it"
    );
    let before = bytes(&bytes_first).len();
    arrow(&mut app, NamedKey::ArrowLeft, KeyCode::ArrowLeft, false);
    assert!(
        bytes(&bytes_first).len() > before,
        "keys reach the shell again"
    );
}

#[test]
fn tiling_or_closing_down_to_one_pane_ends_the_mode() {
    let (mut app, _bytes) = app_with_panes(2);
    app.handle_palette_action_for_test("float-panes");
    app.handle_palette_action_for_test("arrange-floating-pane");
    app.handle_palette_action_for_test("tile-panes");
    assert!(!app.float_arrange_active_for_test());

    app.handle_palette_action_for_test("float-panes");
    app.handle_palette_action_for_test("arrange-floating-pane");
    assert!(app.float_arrange_active_for_test());
    app.close_focused_pane_for_test();
    assert!(
        !app.float_arrange_active_for_test(),
        "a single pane is not a floating tab"
    );
}

#[test]
fn stacked_focus_cycling_resizes_the_newly_shown_pane_to_the_content() {
    let (mut app, _bytes) = app_with_panes(2);
    app.handle_palette_action_for_test("stack-panes");
    let (content, cell_px) = app.pane_geometry_for_test().expect("geometry");
    let full = (
        (content.w / cell_px.0 as f32) as usize,
        (content.h / cell_px.1 as f32) as usize,
    );
    assert_eq!(focused_dimensions(&app), full);
    // Ctrl-b o: the tmux "next pane" chord through the real prefix path.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_eq!(app.active_pane_rects_for_test().len(), 1);
    assert_eq!(
        focused_dimensions(&app),
        full,
        "the pane that came to the front fills the content"
    );
}

#[test]
fn the_palette_lists_every_pane_in_the_stable_order_with_the_front_one_marked() {
    let (mut app, _bytes) = app_with_panes(3);
    assert!(
        app.pane_focus_row_labels().is_empty(),
        "a tiled tab needs no list"
    );
    app.handle_palette_action_for_test("stack-panes");
    let labels = app.pane_focus_row_labels();
    assert_eq!(labels.len(), 3);
    assert_eq!(labels[0], "Focus Pane 1 of 3 (shown)");
    assert_eq!(labels[1], "Focus Pane 2 of 3");

    app.handle_palette_action_for_test("pane-focus-2");
    let labels = app.pane_focus_row_labels();
    assert_eq!(labels[2], "Focus Pane 3 of 3 (shown)");
    let front = app.active_session_token_for_test();
    assert_eq!(app.active_pane_rects_for_test()[0].0, front);

    app.handle_palette_action_for_test("float-panes");
    let labels = app.pane_focus_row_labels();
    assert!(labels[2].ends_with("(front)"), "{labels:?}");
}

#[test]
fn clicking_the_visible_part_of_a_buried_floating_pane_focuses_and_raises_it() {
    let (mut app, _bytes) = app_with_panes(2);
    app.handle_palette_action_for_test("float-panes");
    let buried = app.active_pane_rects_for_test()[0].0;
    let front = app.active_session_token_for_test();
    assert_ne!(buried, front);
    // Pane order is tree order [0, 1]; focus is on pane 0, so pane 1 is the
    // buried one here.
    let rects = app.active_pane_rects_for_test();
    let (buried_token, buried_rect) = rects[0];
    let (x, y) = (
        f64::from(buried_rect[0] + buried_rect[2] / 2.0),
        f64::from(buried_rect[1] + 8.0),
    );
    app.set_pointer_px_for_test(x, y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert_eq!(app.active_session_token_for_test(), buried_token);
    assert_eq!(
        app.active_pane_rects_for_test().last().expect("top").0,
        buried_token,
        "the clicked pane is now on top"
    );
}

#[test]
fn a_shrinking_window_keeps_every_floating_pane_reachable() {
    let (mut app, _bytes) = app_with_panes(3);
    app.handle_palette_action_for_test("float-panes");
    app.set_test_surface_for_test(240, 96, WindowPadding::ZERO);
    app.reflow_active_panes_for_test();
    let (content, _) = app.pane_geometry_for_test().expect("geometry");
    let rects = app.active_pane_rects_for_test();
    assert_eq!(rects.len(), 3, "no pane vanishes");
    for (_, r) in rects {
        assert!(r[0] >= content.x && r[1] >= content.y);
        assert!(r[0] + r[2] <= content.x + content.w + 0.01);
        assert!(r[1] + r[3] <= content.y + content.h + 0.01);
    }
}

#[test]
fn the_arrange_label_paints_only_while_active_and_falls_back_on_narrow_panes() {
    let blank = |columns: usize| crate::core::Snapshot {
        dimensions: Dimensions::new(columns, 3),
        cursor: crate::core::Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: crate::core::DynamicColors::default(),
        cells: vec![crate::core::Cell::default(); columns * 3],
    };
    let row = |snapshot: &crate::core::Snapshot| -> String {
        snapshot.cells[..snapshot.dimensions.columns]
            .iter()
            .map(|c| c.ch)
            .collect()
    };
    let mut off = blank(80);
    let untouched = off.clone();
    paint_arrange_label(&mut off, false);
    assert_eq!(off, untouched, "an inactive mode leaves the frame alone");

    let mut wide = blank(80);
    paint_arrange_label(&mut wide, true);
    assert!(
        row(&wide).starts_with(" ARRANGE  arrows move"),
        "{}",
        row(&wide)
    );
    let mut mid = blank(24);
    paint_arrange_label(&mut mid, true);
    assert!(
        row(&mid).starts_with(" ARRANGE  Esc done "),
        "{}",
        row(&mid)
    );
    let mut tiny = blank(11);
    paint_arrange_label(&mut tiny, true);
    assert!(row(&tiny).starts_with(" ARRANGE "));
    let mut none = blank(5);
    let before = none.clone();
    paint_arrange_label(&mut none, true);
    assert_eq!(none, before, "too narrow for any form paints nothing");
    assert_eq!(arrange_label_text(5), None);
}

#[test]
fn moving_a_pane_between_windows_into_a_floating_tab_keeps_the_layout_mode() {
    // The model-level join is covered in the session tests; here the App path
    // confirms a floating tab still reports its mode after a reflow.
    let (mut app, _bytes) = app_with_panes(2);
    app.handle_palette_action_for_test("float-panes");
    app.reflow_active_panes_for_test();
    assert!(app.workspace_set().active_is_floating());
    let token: SessionToken = app.active_session_token_for_test();
    assert_eq!(
        app.active_pane_rects_for_test().last().expect("top").0,
        token
    );
}
