// SPDX-License-Identifier: GPL-3.0-only
//! CURSOR-ICON: the mouse-cursor shape the pointer path selects over the
//! terminal grid. Before this, OdyTTY never called `set_cursor`, so the pointer
//! stayed the OS default arrow everywhere; these tests pin the standard
//! affordance — an I-beam over selectable text, the arrow while a TUI owns the
//! mouse, and a hand over a hovered hyperlink.
//!
//! Headless (no GPU/window): the cell size is injected via a test seam and the
//! production `update_pointer_cell` handler is driven directly. Skipped when no
//! PTY is available (CI sandboxes).

use super::*;
use winit::window::CursorIcon;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 10;

/// Build an `App` over a one-shot PTY, feed `content` into its terminal, and
/// inject the cell size so the pointer hit-test runs without a GPU.
fn build_app(content: &[u8]) -> Option<App> {
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
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    Some(app)
}

/// Build a two-pane split `App` headlessly: spawn two one-shot PTYs, seed the
/// active tab into a `columns`/rows split, and inject the cell size + surface
/// geometry so `multipane_geometry()` (and the divider hover/drag cursor path)
/// resolves without a GPU. Surface is exactly `COLS×ROWS` cells, zero padding,
/// one tab (no tab bar), so the content rect is `(0,0, COLS·CELL_W, ROWS·CELL_H)`
/// and the lone even-ratio divider sits at its midpoint.
type PaneParts = (Arc<Mutex<Terminal>>, PtyWriter, Arc<Mutex<PtySession>>);

fn build_split_app(columns: bool) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let make = || -> Option<PaneParts> {
        let session = spawn_test_pause_shell(dims).ok()?;
        let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(session));
        Some((terminal, writer, pty))
    };
    let (t1, w1, p1) = make()?;
    let mut app = App::new(
        NativeOptions::default(),
        t1,
        w1,
        p1,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let (t2, w2, p2) = make()?;
    app.seed_split_pane_for_test(columns, t2, w2, p2);
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    app.set_test_surface_for_test(
        COLS as u32 * CELL_W,
        ROWS as u32 * CELL_H,
        crate::native::WindowPadding::ZERO,
    );
    Some(app)
}

// Midpoint of the content rect — where the lone even-ratio divider sits.
const MID_X: f64 = (COLS as f64 * CELL_W as f64) / 2.0;
const MID_Y: f64 = (ROWS as f64 * CELL_H as f64) / 2.0;

#[test]
fn pointer_over_a_column_divider_is_a_col_resize() {
    // A column split (panes side-by-side) draws a vertical divider; hovering its
    // grab band shows the horizontal-resize cursor so drag-to-resize is
    // discoverable.
    let Some(mut app) = build_split_app(true) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(MID_X, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::ColResize);
}

#[test]
fn pointer_over_a_row_divider_is_a_row_resize() {
    // A row split (panes stacked) draws a horizontal divider; hovering its grab
    // band shows the vertical-resize cursor.
    let Some(mut app) = build_split_app(false) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(MID_X, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::RowResize);
}

#[test]
fn pointer_over_grid_in_a_split_is_still_an_i_beam() {
    // Off the divider grab band, a split pane is plain selectable text — the
    // I-beam, exactly as a single pane. The resize cursor is confined to the
    // divider.
    let Some(mut app) = build_split_app(true) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Far left of the vertical divider at the content midpoint.
    app.pointer_move_for_test(f64::from(CELL_W) * 2.5, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
}

#[test]
fn single_pane_never_shows_a_resize_cursor() {
    // BYTE-IDENTITY GUARD: a single-pane tab has no dividers, so the resize
    // branch never fires even at the exact pixel a split's divider would occupy
    // — the plain path stays an I-beam.
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(MID_X, MID_Y);
    let icon = app.cursor_icon_for_test();
    assert_ne!(icon, CursorIcon::ColResize);
    assert_ne!(icon, CursorIcon::RowResize);
    assert_eq!(icon, CursorIcon::Text);
}

#[test]
fn dragging_a_divider_keeps_the_resize_cursor() {
    // Pressing on the divider grabs it (the real press path), and subsequent
    // motion keeps the matching resize cursor for the whole gesture — even if
    // the pointer strays slightly off the hairline.
    use winit::event::MouseButton as WinitMouseButton;
    let Some(mut app) = build_split_app(true) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_px_for_test(MID_X, MID_Y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    // Drag a few pixels off the exact divider line; cursor stays ColResize.
    app.pointer_move_for_test(MID_X + 3.0, MID_Y);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::ColResize);
}

#[test]
fn pointer_over_plain_grid_is_an_i_beam() {
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Move into the middle of the grid: plain selectable text, no link, no TUI
    // mouse reporting -> the I-beam.
    app.pointer_move_for_test(f64::from(CELL_W) * 3.5, f64::from(CELL_H) * 2.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
}

#[test]
fn pointer_shows_arrow_while_tui_owns_the_mouse() {
    // A TUI enables mouse reporting (DECSET 1000): clicks belong to the app, so
    // the I-beam would mislead -> the arrow.
    let Some(mut app) = build_app(b"\x1b[?1000hhello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.pointer_move_for_test(f64::from(CELL_W) * 4.5, f64::from(CELL_H) * 2.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Default);
}

#[test]
fn pointer_over_hovered_hyperlink_is_a_hand() {
    // Print an OSC 8 hyperlink so the first cells carry a link id.
    let Some(mut app) = build_app(b"\x1b]8;;https://example.com\x07LINK\x1b]8;;\x07") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Hover the first cell of the link run (row 0, col 0) -> the hand.
    app.pointer_move_for_test(f64::from(CELL_W) * 0.5, f64::from(CELL_H) * 0.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Pointer);
    // Move off the link onto plain space -> back to the I-beam.
    app.pointer_move_for_test(f64::from(CELL_W) * 20.5, f64::from(CELL_H) * 5.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
}

// ── INTERACTIVE-PATHS (Phase 7): hover affordance over resolved path spans ──
//
// Synthetic only: an absolute path is printed into the grid and the stat-gate is
// a `MapProbe` over a fixed in-memory map, so no test reaches the real
// filesystem. The path is absolute, so neither OSC 7 cwd nor `$HOME` is
// consulted — the resolution is fully synthetic. `/proj/...` is a fabricated
// path, never a real machine path.
use crate::native::app::interactive_paths::MapProbe;
use crate::paths::FsKind;

/// Print an absolute path at row 0 col 0; with `interactive_paths` on and the
/// probe classifying it as a real file, hovering inside the span shows the hand
/// and hovering off it falls back to the I-beam.
#[test]
fn pointer_over_resolved_path_is_a_hand() {
    let Some(mut app) = build_app(b"/proj/src/main.rs") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));

    // Hover within the path span (col 5 of "/proj/src/main.rs") -> the hand.
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Pointer);

    // Move onto a blank row far from the path -> back to the I-beam.
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 6.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
}

/// With the setting OFF (the default), the scanner never runs: hovering the same
/// path text yields the plain I-beam, so the default hover path is unchanged.
#[test]
fn path_hover_is_inert_when_setting_off() {
    let Some(mut app) = build_app(b"/proj/src/main.rs") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Even with a probe that *would* resolve the span, the gate keeps the
    // scanner from ever running while the setting is off.
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));

    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
    assert!(app.hovered_path_for_test().is_none());
}

/// A span that does not resolve (probe reports it absent) gets no affordance:
/// detection is syntactic, but the stat-gate keeps a dead path inert.
#[test]
fn unresolved_path_span_gets_no_hand() {
    let Some(mut app) = build_app(b"/proj/src/main.rs") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    // Empty synthetic fs -> the span is syntactically a path but resolves to
    // nothing, so no hand.
    app.set_test_path_probe_for_test(MapProbe::default());

    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
    assert!(app.hovered_path_for_test().is_none());
}

// ── INTERACTIVE-PATHS: spaced-filename hover span expansion ─────────────────
//
// A filename containing a space is split by the whitespace tokenizer, so neither
// half resolves alone. With stat-guided span expansion the hover offers the
// joined name; these pin that the hand + full-name extent resolve when the file
// exists, that hovering either half works, that the longest-EXISTING name wins
// over a shorter one, and that a spaceless filename is byte-identical. The cwd
// is seeded via OSC 7 and the fs is the synthetic MapProbe — no real filesystem.

/// Seed cwd `/proj` via OSC 7, then print `row0` at row 0. Barewords + paths on.
fn build_app_with_cwd(row0: &str) -> Option<App> {
    let mut content = b"\x1b]7;file://localhost/proj\x1b\\".to_vec();
    content.extend_from_slice(row0.as_bytes());
    let mut app = build_app(&content)?;
    app.set_interactive_paths_for_test(true);
    app.set_interactive_paths_barewords_for_test(true);
    Some(app)
}

#[test]
fn spaced_filename_resolves_when_hovering_either_half() {
    // The real file is `my notes.txt`; hovering the `notes.txt` half OR the `my`
    // half must resolve the whole joined name, and the hand-cursor extent must
    // cover the entire name (cols 0..12), not just one split token.
    let Some(mut app) = build_app_with_cwd("my notes.txt") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/my notes.txt", FsKind::File)]));

    // Hover the `notes.txt` half (col 5).
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Pointer);
    assert_eq!(
        app.hovered_path_for_test().map(|r| r.abs.as_str()),
        Some("/proj/my notes.txt")
    );
    assert_eq!(
        app.hovered_path_cells_for_test(),
        Some((0, 0, 12)),
        "the hand/underline covers the whole spaced name"
    );

    // Hover the `my` half (col 0).
    app.pointer_move_for_test(f64::from(CELL_W) * 0.5, f64::from(CELL_H) * 0.5);
    assert_eq!(
        app.hovered_path_for_test().map(|r| r.abs.as_str()),
        Some("/proj/my notes.txt"),
        "hovering the leading half resolves the joined name too"
    );
}

#[test]
fn longest_existing_spaced_name_wins_over_a_shorter_one() {
    // Guard (b): in `see my file.txt here`, only `file.txt` exists — the joined
    // `my file.txt` must NOT be chosen; the shorter existing name wins.
    let Some(mut app) = build_app_with_cwd("see my file.txt here") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/file.txt", FsKind::File)]));

    // `file.txt` begins at col 7 (`see my ` = 7 chars); hover col 9.
    app.pointer_move_for_test(f64::from(CELL_W) * 9.5, f64::from(CELL_H) * 0.5);
    assert_eq!(
        app.hovered_path_for_test().map(|r| r.abs.as_str()),
        Some("/proj/file.txt"),
        "the shorter existing name wins; prose is not swept in"
    );
}

#[test]
fn spaceless_filename_resolves_byte_identically() {
    // Rule D: a spaceless filename resolves to exactly the single token, with the
    // single-token cell extent — the expansion never changes the no-space case.
    let Some(mut app) = build_app_with_cwd("open notes.txt now") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/notes.txt", FsKind::File)]));

    // `notes.txt` begins at col 5 (`open ` = 5 chars); hover col 8.
    app.pointer_move_for_test(f64::from(CELL_W) * 8.5, f64::from(CELL_H) * 0.5);
    assert_eq!(
        app.hovered_path_for_test().map(|r| r.abs.as_str()),
        Some("/proj/notes.txt")
    );
    assert_eq!(
        app.hovered_path_cells_for_test(),
        Some((0, 5, 14)),
        "single-token extent: cols 5..14 (the 9-char name)"
    );
}

#[test]
fn spaced_filename_inert_when_feature_off() {
    // Guard (c): with interactive_paths off the candidate scan never runs.
    let Some(mut app) = build_app_with_cwd("my notes.txt") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(false);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/my notes.txt", FsKind::File)]));
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    assert_eq!(app.cursor_icon_for_test(), CursorIcon::Text);
    assert!(app.hovered_path_for_test().is_none());
}

// ── INTERACTIVE-PATHS (Phase 8 / C3): Ctrl+click open gate + dispatch argv ──
//
// The success branch spawns a process, so these tests only exercise the gate's
// FALSE branches (no open, selection path untouched) and verify the dispatch
// argv via the pure `path_open_argv_for_test` seam (reads $EDITOR, never spawns).

/// Without Ctrl held, the Ctrl+click open gate never fires even over a resolved
/// path — the press would fall through to selection (byte-identical).
#[test]
fn ctrl_click_open_requires_ctrl() {
    let Some(mut app) = build_app(b"/proj/src/main.rs") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    assert!(app.hovered_path_for_test().is_some(), "path is hovered");
    // No Ctrl → gate returns false, nothing opens.
    assert!(!app.try_open_hovered_path_for_test());
}

/// With the feature off, the open gate returns false immediately even if a Ctrl
/// click lands where a path would be — the default click path is unchanged.
#[test]
fn ctrl_click_open_is_inert_when_setting_off() {
    let Some(mut app) = build_app(b"/proj/src/main.rs") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    app.set_ctrl_modifier_for_test(true);
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    // Feature off → no hovered path and the gate short-circuits to false.
    assert!(!app.try_open_hovered_path_for_test());
    assert!(app.hovered_path_for_test().is_none());
}

/// With no path under the pointer, the gate returns false even with Ctrl held
/// and the feature on.
#[test]
fn ctrl_click_open_is_inert_with_no_path() {
    let Some(mut app) = build_app(b"plain text") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::default());
    app.set_ctrl_modifier_for_test(true);
    app.pointer_move_for_test(f64::from(CELL_W) * 2.5, f64::from(CELL_H) * 0.5);
    assert!(!app.try_open_hovered_path_for_test());
}

/// A `path:line:col` span under the pointer builds the editor-matrix argv from
/// the configured override (`interactive_paths_editor`), tokenized and never
/// shell-evaluated. Asserts the vector; never spawns.
#[test]
fn ctrl_click_dispatch_builds_editor_argv_for_path_line_col() {
    let Some(mut app) = build_app(b"/proj/src/main.rs:42:7") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_interactive_paths_editor_for_test("vim");
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    assert_eq!(
        app.path_open_argv_for_test(),
        Some(vec![
            "vim".to_owned(),
            "+call cursor(42,7)".to_owned(),
            "/proj/src/main.rs".to_owned()
        ])
    );
}

/// A plain file span (no line suffix) dispatches to the platform opener --
/// `xdg-open` on Linux, `open` on macOS, `cmd /C start` on Windows -- with the
/// path passed through to the opener.
#[test]
fn ctrl_click_dispatch_builds_platform_open_for_plain_file() {
    let Some(mut app) = build_app(b"/proj/src/main.rs") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
    // The opener argv is platform-specific: macOS `open <path>`, Windows
    // `cmd /C start "" <path>` (the empty arg is `start`'s window-title slot),
    // Linux `xdg-open <path>`.
    #[cfg(target_os = "macos")]
    let expected = vec!["open".to_owned(), "/proj/src/main.rs".to_owned()];
    #[cfg(windows)]
    let expected = vec![
        "cmd".to_owned(),
        "/C".to_owned(),
        "start".to_owned(),
        String::new(),
        "/proj/src/main.rs".to_owned(),
    ];
    #[cfg(not(any(target_os = "macos", windows)))]
    let expected = vec!["xdg-open".to_owned(), "/proj/src/main.rs".to_owned()];
    assert_eq!(app.path_open_argv_for_test(), Some(expected));
}

// ── OPEN-NOTICE (P0-2): a failed open spawn surfaces a visible notice; a
// successful spawn does NOT ──
//
// The whole point of P0-2 is that a missing/broken opener is no longer an
// indistinguishable silent no-op. These drive the production
// `spawn_open_or_notice` path with a deliberately-nonexistent program (the
// missing-opener case) and a real one, asserting the notice is set only on
// failure.

/// A spawn of a program that does not exist (the missing-`xdg-open`/`open` case)
/// raises a visible, non-blocking notice naming the program.
#[test]
fn failed_open_spawn_raises_visible_notice() {
    let Some(mut app) = build_app(b"") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert!(app.open_notice_message_for_test().is_none(), "clean start");
    let argv = vec![
        "odytty-nonexistent-opener-xyz".to_owned(),
        "/proj/a.png".to_owned(),
    ];
    app.spawn_open_or_notice_for_test(&argv);
    let message = app
        .open_notice_message_for_test()
        .expect("a failed open must surface a notice");
    assert!(
        message.contains("odytty-nonexistent-opener-xyz"),
        "notice names the missing opener: {message}"
    );
    assert!(message.contains("Couldn't open"), "user-facing phrasing");
}

/// A spawn of a real program (`true`, which exists on every unix CI host and
/// exits immediately) does NOT raise a notice — the success path is silent.
#[test]
fn successful_open_spawn_raises_no_notice() {
    let Some(mut app) = build_app(b"") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let argv = vec!["true".to_owned()];
    app.spawn_open_or_notice_for_test(&argv);
    assert!(
        app.open_notice_message_for_test().is_none(),
        "the success path must NOT fire a notice"
    );
}
