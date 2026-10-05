// SPDX-License-Identifier: GPL-3.0-only
//! S3b-2: cursor effects and cell-anchored overlays under the test-only bidi
//! display gate. The cursor slide, trail, follower, and aura run in drawn
//! columns, so a move on a reordered line animates exactly as the same move
//! between the same screen cells on an unreordered line. The armed underline,
//! the click hint, and button chips are driven through the real pointer path
//! and the single-pane cell manifest. With the gate off every answer is the
//! shipping one.

use super::*;

use std::io::Write;
use std::time::{Duration, Instant};
use winit::event::MouseButton as WinitMouseButton;

use crate::core::Position;
use crate::grid::BidiDisplayMap;
use crate::native::gpu::{
    CursorGlowRequest, CursorStreakRequest, build_cursor_glow_instance_with_bidi,
    build_cursor_streak_instance_with_bidi,
};

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 4;

/// Logical "ab אבג xy": columns 3..=5 are Hebrew and draw reversed.
const LINE: &str = "ab \u{05D0}\u{05D1}\u{05D2} xy";
/// The same cell layout with no right-to-left text: identity placement.
const LATIN: &str = "ab cde xy";

/// "abc " then fourteen Hebrew letters at logical 4..=17, which draw reversed
/// at visual 17..=4: a one-cell logical move from 3 to 4 jumps 14 drawn cells.
fn long_line() -> String {
    let mut line = String::from("abc ");
    line.extend((0..14).map(|index| char::from_u32(0x05D0 + index).expect("Hebrew letter")));
    line
}

fn cup(row: usize, column: usize) -> Vec<u8> {
    format!("\x1b[{};{}H", row + 1, column + 1).into_bytes()
}

fn effects_app(text: &str, cursor: Position) -> (App, Arc<Mutex<crate::core::Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    {
        let mut terminal = terminal.lock().expect("terminal");
        terminal.advance(text.as_bytes());
        terminal.advance(&cup(cursor.row, cursor.column));
    }
    app.set_test_cell_for_test(CELL);
    app.enable_cursor_effects_for_test();
    (app, terminal)
}

fn move_cursor(terminal: &Arc<Mutex<crate::core::Terminal>>, to: Position) {
    terminal
        .lock()
        .expect("terminal")
        .advance(&cup(to.row, to.column));
}

/// Present a frame at `t0` with the cursor at `from`, move it to `to`, and
/// present at `t0 + 1ms` (the glide arms) and `t0 + 40ms` (mid-glide).
fn run_move(
    text: &str,
    gate: bool,
    from: Position,
    to: Position,
) -> (
    crate::native::app::BidiFrameProbe,
    crate::native::app::BidiFrameProbe,
) {
    let (mut app, terminal) = effects_app(text, from);
    app.set_bidi_display_for_test(gate);
    let t0 = Instant::now();
    let _ = app.present_bidi_frame_for_test(t0);
    move_cursor(&terminal, to);
    let armed = app.present_bidi_frame_for_test(t0 + Duration::from_millis(1));
    let mid = app.present_bidi_frame_for_test(t0 + Duration::from_millis(40));
    (armed, mid)
}

fn pos(row: usize, column: usize) -> Position {
    Position { row, column }
}

#[test]
fn a_reordered_move_glides_and_trails_between_its_drawn_cells() {
    // Logical 1 -> 3 on `LINE` draws from screen column 1 to screen column 5.
    let (armed, mid) = run_move(LINE, true, pos(0, 1), pos(0, 3));
    // The same screen move on an unreordered line, gate off.
    let (reference_armed, reference_mid) = run_move(LATIN, false, pos(0, 1), pos(0, 5));
    assert_eq!(armed.params, reference_armed.params, "glide start");
    assert_eq!(mid.params, reference_mid.params, "mid glide");
    assert_ne!(mid.params.offset, [0.0, 0.0], "the glide is in flight");
    assert!(!mid.trail.is_empty(), "a four-cell glide trails");
    assert_eq!(mid.trail, reference_mid.trail, "trail quads");
    // Not vacuous: the logical move (two cells right) glides differently.
    let (_, logical_mid) = run_move(LINE, false, pos(0, 1), pos(0, 3));
    assert_ne!(logical_mid.params, mid.params);
}

#[test]
fn a_move_inside_a_reversed_run_glides_against_the_logical_direction() {
    // Logical 3 -> 5 inside the Hebrew run draws from screen 5 to screen 3.
    let (armed, _) = run_move(LINE, true, pos(0, 3), pos(0, 5));
    // The block starts two cells right of its drawn destination; the logical
    // move would start it two cells left.
    assert_eq!(armed.params.offset, [2.0 * CELL.width as f32, 0.0]);
    let (logical, _) = run_move(LINE, false, pos(0, 3), pos(0, 5));
    assert_eq!(logical.params.offset, [-2.0 * CELL.width as f32, 0.0]);
}

#[test]
fn a_short_logical_move_that_jumps_on_screen_starts_the_follower() {
    let line = long_line();
    let (armed, _) = run_move(&line, true, pos(0, 3), pos(0, 4));
    assert!(
        armed.params.follower_active,
        "the block hands off to the follower"
    );
    let streak = armed.streak.expect("follower request");
    assert_eq!(streak.destination, pos(0, 17), "drawn destination");
    let (reference, _) = run_move(&line, false, pos(0, 3), pos(0, 17));
    assert_eq!(armed.params, reference.params);
    assert_eq!(armed.streak, reference.streak);
    // Gate off: a one-cell logical move glides and never starts the follower.
    let (logical, _) = run_move(&line, false, pos(0, 3), pos(0, 4));
    assert!(!logical.params.follower_active);
    assert!(logical.streak.is_none());
}

#[test]
fn aura_and_follower_place_at_the_drawn_cursor_column() {
    let mut terminal = crate::core::Terminal::new(COLUMNS, ROWS);
    terminal.advance(LINE.as_bytes());
    terminal.advance(&cup(0, 5));
    let snapshot = terminal.snapshot();
    let map = BidiDisplayMap::plan(&snapshot, &[false; ROWS]);
    assert_eq!(map.visual_column(0, 5), 3, "gimel draws at screen column 3");
    let origin = [11.0, 13.0];
    let request = CursorGlowRequest {
        clip_rect: [0.0, 0.0, 1000.0, 1000.0],
        intensity: crate::settings::DEFAULT_CURSOR_GLOW_INTENSITY,
    };
    let glow = |bidi: Option<&BidiDisplayMap>| {
        build_cursor_glow_instance_with_bidi(
            &snapshot,
            CELL,
            crate::core::CursorStyle::Block,
            origin,
            CursorRenderParams::default(),
            1.0,
            1.0,
            request,
            None,
            bidi,
        )
        .expect("aura")
        .source_rect
    };
    let x = |column: usize| origin[0] + column as f32 * CELL.width as f32;
    assert_eq!(glow(None)[0], x(5), "gate off: the logical column");
    assert_eq!(glow(Some(&map))[0], x(3), "the drawn column");
    // The follower's request is in drawn columns; its body lands exactly on
    // the requested rect plus the content origin, with no chrome shift.
    let rect = [24.0, 0.0, 32.0, 16.0];
    let streak = CursorStreakRequest {
        destination: pos(0, 3),
        rect,
        alpha: 1.0,
        clip_rect: [0.0, 0.0, 1000.0, 1000.0],
    };
    let body = build_cursor_streak_instance_with_bidi(&snapshot, CELL, origin, streak, Some(&map))
        .expect("follower")
        .source_rect;
    assert_eq!(body, [35.0, 13.0, 43.0, 29.0]);
    let shifted = build_cursor_streak_instance_with_bidi(&snapshot, CELL, origin, streak, None)
        .expect("follower")
        .source_rect;
    assert_ne!(shifted, body, "the logical column would shift the body");
}

#[test]
fn an_overlay_on_the_cursor_row_snaps_the_cursor_effects() {
    // The cursor glides inside the Hebrew run on the last row, then the IME
    // pre-edit writes text into that row: the row draws in logical order, so
    // the effects snap instead of gliding toward a column the block left.
    let last = ROWS - 1;
    let mut text = cup(last, 0);
    text.extend_from_slice(LINE.as_bytes());
    let text = String::from_utf8(text).expect("utf8");
    let (mut app, terminal) = effects_app(&text, pos(last, 1));
    app.set_bidi_display_for_test(true);
    let t0 = Instant::now();
    let _ = app.present_bidi_frame_for_test(t0);
    move_cursor(&terminal, pos(last, 3));
    app.set_ime_preedit_for_test("x");
    let probe = app.present_bidi_frame_for_test(t0 + Duration::from_millis(1));
    let map = app.bidi_frame_map_for_test().expect("gate map");
    assert!(
        !map.row_is_reordered(last),
        "the pre-edit row draws logically"
    );
    assert_eq!(probe.params.offset, [0.0, 0.0], "no glide");
    assert!(!probe.params.follower_active);
    assert!(probe.streak.is_none());
    assert!(probe.trail.is_empty());
}

/// A two-column split whose panes print `text`, with cursor effects on, the
/// gate as given, and one split frame built so each pane has its map.
fn split_effects_app(text: &str, gate: bool) -> App {
    let dims = Dimensions::new(COLUMNS, ROWS);
    let (mut app, first) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    first.lock().expect("terminal").advance(text.as_bytes());
    let second = Arc::new(Mutex::new(crate::core::Terminal::new(COLUMNS, ROWS)));
    second.lock().expect("terminal").advance(text.as_bytes());
    let writer = crate::native::test_support::headless_writer();
    app.seed_headless_split_pane_for_test(true, second, writer, dims);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        (COLUMNS * 2) as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.set_bidi_display_for_test(gate);
    let _ = app.rebuild_multipane_probe_for_test();
    app.enable_cursor_effects_for_test();
    app
}

/// Drive the focused split pane's cursor consumer through a move from `from`
/// to `to`; returns the mid-glide trail quads and cursor parameters.
fn split_move(
    text: &str,
    gate: bool,
    from: Position,
    to: Position,
) -> (Vec<SolidQuad>, CursorRenderParams) {
    let mut app = split_effects_app(text, gate);
    let mut terminal = crate::core::Terminal::new(COLUMNS, ROWS);
    terminal.advance(text.as_bytes());
    let mut snapshot = terminal.snapshot();
    let origin = [0.0, 0.0];
    let t0 = Instant::now();
    snapshot.cursor = from;
    let _ = app.advance_multipane_cursor_effects_for_test(t0, &mut snapshot, CELL, origin);
    snapshot.cursor = to;
    let _ = app.advance_multipane_cursor_effects_for_test(
        t0 + Duration::from_millis(1),
        &mut snapshot,
        CELL,
        origin,
    );
    let (trail, _, _, params) = app.advance_multipane_cursor_effects_for_test(
        t0 + Duration::from_millis(40),
        &mut snapshot,
        CELL,
        origin,
    );
    (trail, params)
}

#[test]
fn the_focused_split_pane_glides_and_trails_between_drawn_cells() {
    let (trail, params) = split_move(LINE, true, pos(0, 1), pos(0, 3));
    let (reference_trail, reference_params) = split_move(LATIN, false, pos(0, 1), pos(0, 5));
    assert_ne!(params.offset, [0.0, 0.0], "the glide is in flight");
    assert!(!trail.is_empty(), "a four-cell glide trails");
    // The blink-easing alpha follows each App's own clock; the geometry is
    // what the drawn columns decide.
    assert_eq!(params.offset, reference_params.offset);
    assert_eq!(params.follower_active, reference_params.follower_active);
    assert_eq!(trail, reference_trail);
    let (_, logical_params) = split_move(LINE, false, pos(0, 1), pos(0, 3));
    assert_ne!(logical_params.offset, params.offset, "not vacuous");
}

// Open-modifier armed underline and the mis-click hint.

/// "ab " then an absolute path whose file name is Hebrew: the name (logical
/// 9..=10) draws reversed inside the path span (logical 3..15).
const PATH_LINE: &str = "ab /proj/\u{05D0}\u{05D1}.txt";
const PATH: &str = "/proj/\u{05D0}\u{05D1}.txt";
const PATH_START: usize = 3;
const PATH_END: usize = 15;

fn path_app() -> (App, Arc<Mutex<crate::core::Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(PATH_LINE.as_bytes());
    app.set_test_cell_for_test(CELL);
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(crate::native::app::interactive_paths::MapProbe::new([(
        PATH,
        crate::paths::FsKind::File,
    )]));
    (app, terminal)
}

fn hold_open_modifier(app: &mut App, held: bool) {
    if cfg!(target_os = "macos") {
        app.set_super_key_for_test(held);
    } else {
        app.set_ctrl_modifier_for_test(held);
    }
}

fn hover_screen_cell(app: &mut App, row: usize, column: usize) {
    app.pointer_move_for_test(
        f64::from(CELL.width) * (column as f64 + 0.5),
        f64::from(CELL.height) * (row as f64 + 0.5),
    );
}

#[test]
fn armed_underline_follows_the_drawn_path_and_keeps_the_row_reordered() {
    let (mut app, _terminal) = path_app();
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(map.row_is_reordered(0));
    // Hover the screen cell alef (logical 9) draws at.
    let drawn = map.visual_column(0, 9);
    assert_eq!(drawn, 10, "the Hebrew name draws reversed");
    hover_screen_cell(&mut app, 0, drawn);
    assert!(
        app.hovered_path_for_test().is_some(),
        "the drawn path hovers"
    );
    hold_open_modifier(&mut app, true);
    assert_eq!(
        app.armed_underline_cells_for_test(),
        Some((0, PATH_START, PATH_END)),
        "the armed span is the logical path"
    );
    let probe = app.present_bidi_frame_for_test(Instant::now());
    let map = app.bidi_frame_map_for_test().expect("gate map");
    assert!(
        map.row_is_reordered(0),
        "the underline is attribute-only, so the row keeps its placement"
    );
    let underlined: Vec<usize> = (0..COLUMNS)
        .filter(|&column| probe.painted.cells[column].attrs.underline())
        .collect();
    assert_eq!(underlined, (PATH_START..PATH_END).collect::<Vec<_>>());
}

#[test]
fn the_click_hint_row_draws_in_logical_order() {
    let (mut app, terminal) = path_app();
    {
        // Right-to-left text on the hint's row as well.
        let mut terminal = terminal.lock().expect("terminal");
        terminal.advance(&cup(ROWS - 1, 0));
        terminal.advance(LINE.as_bytes());
    }
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(map.row_is_reordered(ROWS - 1));
    hover_screen_cell(&mut app, 0, map.visual_column(0, 9));
    for _ in 0..2 {
        app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
        app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    }
    assert!(
        app.click_hint_shown_for_test(),
        "two mis-clicks raise the hint"
    );
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let map = app.bidi_frame_map_for_test().expect("gate map");
    assert!(
        !map.row_is_reordered(ROWS - 1),
        "the hint writes text into its row, which then draws logically"
    );
    assert!(map.row_is_reordered(0), "other rows keep their placement");
}

// Button chips.

#[derive(Clone, Default)]
struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("bytes").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// "ab אב" then a `code=7` button labeled "גד", then "xy": the Hebrew run is
/// logical 3..=6 and the label (logical 5..=6) draws at screen 3..=4. Both
/// label neighbors hold text, so no pill cap writes into the row.
fn chip_line(open_cap: bool) -> Vec<u8> {
    let mut bytes = "ab \u{05D0}\u{05D1}".as_bytes().to_vec();
    bytes.extend_from_slice(b"\x1b]133;P;odytty-button;code=7\x07");
    bytes.extend_from_slice("\u{05D2}\u{05D3}".as_bytes());
    bytes.extend_from_slice(b"\x1b]133;P;odytty-button;end\x07");
    bytes.extend_from_slice(if open_cap { b" y" } else { b"xy" });
    bytes
}

fn chip_app(open_cap: bool) -> (App, Arc<Mutex<Vec<u8>>>) {
    let recorder = RecordingWriter::default();
    let bytes = recorder.0.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let settings = Settings {
        buttons: true,
        ..Settings::default()
    };
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        settings,
        writer,
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(&chip_line(open_cap));
    app.set_test_cell_for_test(CELL);
    // Spend the launch focus-click marker on an empty cell.
    hover_screen_cell(&mut app, ROWS - 1, COLUMNS - 1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    bytes.lock().expect("bytes").clear();
    (app, bytes)
}

fn click_screen_cell(app: &mut App, bytes: &Arc<Mutex<Vec<u8>>>, column: usize) -> Vec<u8> {
    hover_screen_cell(app, 0, column);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    std::mem::take(&mut *bytes.lock().expect("bytes"))
}

const ENVELOPE: &[u8] = b"\x1b[?1337;7~";

#[test]
fn a_chip_activates_from_the_screen_cell_its_label_draws_at() {
    let (mut app, bytes) = chip_app(false);
    // Gate off: screen column 3 is logical 3, outside the label.
    assert!(click_screen_cell(&mut app, &bytes, 3).is_empty());
    app.set_bidi_display_for_test(true);
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(
        map.row_is_reordered(0),
        "a capless chip keeps the placement"
    );
    assert_eq!(map.visual_column(0, 5), 4);
    assert_eq!(map.visual_column(0, 6), 3);
    assert_eq!(click_screen_cell(&mut app, &bytes, 3), ENVELOPE.to_vec());
    let _ = app.present_bidi_frame_for_test(Instant::now());
    assert!(
        click_screen_cell(&mut app, &bytes, 5).is_empty(),
        "screen column 5 draws logical 4, outside the label"
    );
}

#[test]
fn a_pill_cap_returns_the_chip_row_to_logical_order() {
    let (mut app, bytes) = chip_app(true);
    app.set_bidi_display_for_test(true);
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let map = app.bidi_frame_map_for_test().expect("gate map");
    assert!(
        !map.row_is_reordered(0),
        "the right cap writes into the blank neighbor, so the row draws logically"
    );
    assert_eq!(click_screen_cell(&mut app, &bytes, 5), ENVELOPE.to_vec());
}
