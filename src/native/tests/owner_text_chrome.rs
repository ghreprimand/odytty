// SPDX-License-Identifier: GPL-3.0-only
//! The search bar, the rename prompt and the tab labels measure and paint by
//! terminal owners, like overlay rows: a combining mark stays on its base and
//! an emoji ZWJ sequence is one wide glyph. Each case drives the real input
//! path (key chords, an IME commit, PTY title bytes, pointer presses), reads
//! the painted frame, and checks that the frame's render signature changes
//! with the typed text.

use winit::event::Ime;

use super::*;

const MARKED: &str = "e\u{301}";
const ZWJ: &str = "\u{1f469}\u{200d}\u{1f4bb}";

fn headless() -> App {
    let (mut app, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    app
}

fn painted_row(app: &mut App, row: usize) -> Vec<crate::core::Cell> {
    let painted = app.present_bidi_frame_for_test(Instant::now()).painted;
    let columns = painted.dimensions.columns;
    painted.cells[row * columns..(row + 1) * columns].to_vec()
}

fn signature(app: &mut App) -> crate::native::render_helpers::RenderSignature {
    app.redraw_single_pane_probe_for_test()
        .expect("a dirty single-pane frame is generated")
        .0
}

/// The column of the cell holding `base` with exactly `marks` retained.
fn owner_column(row: &[crate::core::Cell], base: char, marks: &[char]) -> Option<usize> {
    row.iter()
        .position(|cell| cell.ch == base && cell.combining() == marks)
}

#[test]
fn search_bar_paints_the_query_by_owner() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = headless();
    app.drive_char_with_mods_for_test('f', true, true);
    assert!(app.search_open_for_test(), "Ctrl+Shift+F opens search");
    let before = signature(&mut app);
    app.handle_ime(Ime::Commit(MARKED.to_owned()));
    app.drive_text_key_for_test(ZWJ);
    assert_eq!(app.search_query_for_test(), format!("{MARKED}{ZWJ}"));
    assert_ne!(
        signature(&mut app),
        before,
        "the typed query re-keys the frame"
    );

    let rows = app.grid_dims_for_test().1;
    let bar = painted_row(&mut app, rows - 1);
    let mark = owner_column(&bar, 'e', &['\u{301}']).expect("the mark stays on its base");
    assert!(
        !bar.iter().any(|cell| cell.ch == '\u{301}'),
        "no cell of its own for the mark"
    );
    let emoji = mark + 1;
    assert_eq!(bar[emoji].ch, '\u{1f469}');
    assert_eq!(bar[emoji].combining(), ['\u{200d}', '\u{1f4bb}']);
    assert!(
        bar[emoji + 1].wide_continuation,
        "the sequence is one wide glyph"
    );
}

#[test]
fn rename_prompt_edits_and_paints_by_owner() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = headless();
    app.set_session_title_override_for_test(0, Some("ab"));
    assert!(app.begin_rename_tab_for_test(0));
    let before = signature(&mut app);
    app.handle_ime(Ime::Commit(MARKED.to_owned()));
    app.drive_text_key_for_test(ZWJ);
    let text = format!("ab{MARKED}{ZWJ}");
    assert_eq!(app.rename_text_for_test().as_deref(), Some(text.as_str()));
    assert_eq!(app.rename_cursor_for_test(), Some(text.chars().count()));
    assert_ne!(
        signature(&mut app),
        before,
        "the typed name re-keys the frame"
    );

    // One Left steps over the whole emoji sequence, a second over the mark
    // and its base.
    app.drive_named_key_for_test(NamedKey::ArrowLeft);
    assert_eq!(app.rename_cursor_for_test(), Some(4));
    let at_emoji = signature(&mut app);
    app.drive_named_key_for_test(NamedKey::ArrowLeft);
    assert_eq!(app.rename_cursor_for_test(), Some(2));
    assert_ne!(
        signature(&mut app),
        at_emoji,
        "the caret move re-keys the frame"
    );

    let (columns, rows) = app.grid_dims_for_test();
    let width = columns.clamp(8, 48);
    let input_row = (rows - 3) / 2 + 1;
    let input_left = (columns - width) / 2 + 2 + "Tab name: ".len();
    let field = painted_row(&mut app, input_row);
    assert_eq!(field[input_left].ch, 'a');
    assert_eq!(field[input_left + 1].ch, 'b');
    let mark = &field[input_left + 2];
    assert_eq!((mark.ch, mark.combining()), ('e', &['\u{301}'][..]));
    let emoji = &field[input_left + 3];
    assert_eq!(emoji.ch, '\u{1f469}');
    assert_eq!(emoji.combining(), ['\u{200d}', '\u{1f4bb}']);
    assert!(field[input_left + 4].wide_continuation);

    // A click on the emoji's tail places the caret before the sequence.
    app.rename_pointer_press_for_test(input_row, input_left + 4);
    assert_eq!(app.rename_cursor_for_test(), Some(4));
    app.rename_pointer_release_for_test();
    // Backspace removes the marked owner whole; Delete removes the emoji.
    app.drive_named_key_for_test(NamedKey::Backspace);
    assert_eq!(app.rename_text_for_test(), Some(format!("ab{ZWJ}")));
    assert_eq!(app.rename_cursor_for_test(), Some(2));
    app.drive_named_key_for_test(NamedKey::Delete);
    assert_eq!(app.rename_text_for_test().as_deref(), Some("ab"));
}

#[test]
fn tab_labels_paint_program_titles_by_owner() {
    let _guard = crate::test_lock::render_globals_lock();
    let settings = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    let (mut app, terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        settings,
    );
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    let before = signature(&mut app);
    terminal
        .lock()
        .expect("terminal")
        .advance(format!("\x1b]2;{MARKED}x{ZWJ}\x07").as_bytes());
    // The output wake the PTY reader sends after those bytes.
    let session = app
        .session_token_at_position_for_test(0)
        .expect("session token");
    app.dispatch_user_event_for_test(crate::native::pty::UserEvent::Redraw { session });
    assert_ne!(
        signature(&mut app),
        before,
        "the new title re-keys the frame"
    );

    let row = app
        .tab_bar_label_cells_for_test()
        .expect("the tab bar shows");
    let mark = owner_column(&row, 'e', &['\u{301}']).expect("the mark stays on its base");
    assert!(!row.iter().any(|cell| cell.ch == '\u{301}'));
    assert_eq!(row[mark + 1].ch, 'x');
    let emoji = &row[mark + 2];
    assert_eq!(emoji.ch, '\u{1f469}');
    assert_eq!(emoji.combining(), ['\u{200d}', '\u{1f4bb}']);
    assert!(row[mark + 3].wide_continuation, "a real wide tail");
}

#[test]
fn notice_banner_paints_by_owner() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = headless();
    let before = signature(&mut app);
    app.raise_open_notice(format!("x{MARKED}{ZWJ}y"));
    assert_ne!(signature(&mut app), before, "the notice re-keys the frame");
    let banner = painted_row(&mut app, 0);
    let mark = owner_column(&banner, 'e', &['\u{301}']).expect("the mark stays on its base");
    assert_eq!(banner[mark - 1].ch, 'x');
    assert!(
        !banner
            .iter()
            .any(|cell| matches!(cell.ch, '\u{301}' | '\u{200d}' | '\u{1f4bb}')),
        "no cell of its own for a retained scalar"
    );
    assert_eq!(banner[mark + 1].ch, '\u{1f469}');
    assert_eq!(banner[mark + 1].combining(), ['\u{200d}', '\u{1f4bb}']);
    assert!(banner[mark + 2].wide_continuation, "a real wide tail");
    assert_eq!(banner[mark + 3].ch, 'y');
}

#[test]
fn rail_labels_paint_and_size_by_owner() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = headless();
    app.set_workspace_rail_for_test("left");
    app.rename_workspace_for_test(0, "ab");
    let narrow = app.rail_auto_want_cols_for_test();
    let before = signature(&mut app);
    app.rename_workspace_for_test(0, &format!("x{MARKED}{ZWJ}y"));
    assert_eq!(
        app.rail_auto_want_cols_for_test(),
        narrow + 3,
        "x, the marked e, the two-column sequence and y take five columns"
    );
    assert_ne!(
        signature(&mut app),
        before,
        "the new name re-keys the frame"
    );
    let label = app
        .decorated_rows_for_test()
        .expect("a decorated frame")
        .into_iter()
        .find(|row| owner_column(row, 'e', &['\u{301}']).is_some())
        .expect("the rail paints the mark on its base");
    let mark = owner_column(&label, 'e', &['\u{301}']).expect("marked base");
    assert_eq!(label[mark - 1].ch, 'x');
    assert_eq!(label[mark + 1].ch, '\u{1f469}');
    assert_eq!(label[mark + 1].combining(), ['\u{200d}', '\u{1f4bb}']);
    assert!(label[mark + 2].wide_continuation, "a real wide tail");
    assert_eq!(label[mark + 3].ch, 'y');
}
