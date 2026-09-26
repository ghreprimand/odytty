// SPDX-License-Identifier: GPL-3.0-only
//! Read-only pane disclosure and toggle paths: the in-pane `READ-ONLY` label,
//! its render-cache fragment, and the palette and context-menu toggles driven
//! through the production entry points. Input enforcement is covered by
//! `read_only_pane`.

use super::super::app::read_only::{READ_ONLY_LABEL, paint_read_only_label};
use super::super::overlay::OverlayOutcome;
use super::super::render_helpers::OverlayFragment;
use super::*;

fn row_text(snapshot: &Snapshot, row: usize) -> String {
    let columns = snapshot.dimensions.columns;
    snapshot.cells[row * columns..(row + 1) * columns]
        .iter()
        .map(|cell| cell.ch)
        .collect()
}

#[test]
fn label_paints_at_top_right_leaving_the_attention_column() {
    let mut snap = snapshot(&["hello world                   ", "second"], 30);
    paint_read_only_label(&mut snap, true);
    let top = row_text(&snap, 0);
    let expected_start = 30 - 1 - READ_ONLY_LABEL.len();
    assert_eq!(&top[expected_start..29], READ_ONLY_LABEL);
    assert_eq!(
        &top[..11],
        "hello world",
        "content left of the label is kept"
    );
    assert_eq!(top.chars().last(), Some(' '), "attention column untouched");
    let cell = &snap.cells[expected_start + 1];
    assert!(cell.attrs.inverse() && cell.attrs.bold());
    assert_eq!(
        row_text(&snap, 1).trim_end(),
        "second",
        "only row 0 changes"
    );
}

#[test]
fn label_is_a_no_op_for_a_writable_pane() {
    let original = snapshot(&["abc", "def"], 20);
    let mut snap = original.clone();
    paint_read_only_label(&mut snap, false);
    assert_eq!(snap, original);
}

#[test]
fn label_shortens_on_a_narrow_pane() {
    let mut snap = snapshot(&["abcdefghijk"], 11);
    paint_read_only_label(&mut snap, true);
    assert_eq!(row_text(&snap, 0), "aREAD-ONLYk", "unpadded word fits");
    let mut snap = snapshot(&["abcdef"], 6);
    paint_read_only_label(&mut snap, true);
    assert_eq!(row_text(&snap, 0), "abcROf", "never a misleading fragment");
    let mut tiny = snapshot(&["ab"], 2);
    paint_read_only_label(&mut tiny, true);
    assert_eq!(
        row_text(&tiny, 0),
        "ab",
        "no room left of the attention column"
    );
}

#[test]
fn label_blanks_a_wide_glyph_it_would_split() {
    let columns = 20;
    let mut snap = snapshot(&[""], columns);
    let start = columns - 1 - READ_ONLY_LABEL.len();
    snap.cells[start - 1] = Cell::new('\u{4e2d}', Attrs::default());
    snap.cells[start].wide_continuation = true;
    paint_read_only_label(&mut snap, true);
    assert_eq!(
        snap.cells[start - 1].ch,
        ' ',
        "orphaned wide lead is blanked"
    );
    assert!(!snap.cells[start].wide_continuation);
}

#[test]
fn palette_toggle_flips_the_flag_and_rekeys_the_frame() {
    let (mut app, _terminal) = headless_app_for_test();
    assert!(!app.active_pane_read_only());
    assert_eq!(app.read_only_overlay_signature(), OverlayFragment::Inert);
    app.needs_rebuild = false;

    app.handle_palette_action_for_test("toggle-read-only");
    assert!(app.active_pane_read_only());
    assert!(!app.active_pane_accepts_input());
    assert_eq!(app.read_only_overlay_signature(), OverlayFragment::ReadOnly);
    assert!(app.needs_rebuild, "the pane repaints so the label appears");

    app.needs_rebuild = false;
    app.handle_palette_action_for_test("toggle-read-only");
    assert!(!app.active_pane_read_only());
    assert!(app.active_pane_accepts_input());
    assert_eq!(app.read_only_overlay_signature(), OverlayFragment::Inert);
    assert!(app.needs_rebuild, "the pane repaints so the label clears");
}

#[test]
fn context_menu_offers_exactly_one_read_only_row_and_toggles() {
    let (mut app, _terminal) = headless_app_for_test();
    app.set_pointer_cell_for_test(5, 10);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    let rows = app.render_overlay_rows_for_test(80, 40).join("\n");
    assert!(rows.contains("Make Pane Read-Only"));
    assert!(!rows.contains("Make Pane Writable"));

    // No selection and no shell integration: Paste, Select All, the four tab
    // rows, the two splits, then Make Pane Read-Only at visible index 8.
    for _ in 0..8 {
        app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::ArrowDown), false, false);
    }
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(!app.context_menu_open_for_test());
    assert!(app.active_pane_read_only(), "the menu row turned input off");

    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    let rows = app.render_overlay_rows_for_test(80, 40).join("\n");
    assert!(rows.contains("Make Pane Writable"));
    assert!(!rows.contains("Make Pane Read-Only"));
    assert!(
        rows.contains("Paste Text"),
        "Paste stays listed (disabled) on a read-only pane"
    );

    app.apply_overlay_outcome_for_test(OverlayOutcome::ContextMenuToggleReadOnly);
    assert!(!app.active_pane_read_only());
}

#[test]
fn read_only_flag_is_captured_into_the_shape() {
    let (mut app, _terminal) = headless_app_for_test();
    let leaf_read_only =
        |app: &App| match &app.capture_shape_for_test().workspaces[0].tabs[0].layout {
            crate::native::persistence::PaneShape::Leaf { read_only, .. } => *read_only,
            other => panic!("expected a single leaf, got {other:?}"),
        };
    assert!(!leaf_read_only(&app));
    app.toggle_active_pane_read_only();
    assert!(leaf_read_only(&app));
}

#[test]
fn focus_reports_still_reach_a_read_only_pane() {
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("bytes").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let recorder = Recorder::default();
    let bytes = recorder.0.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (mut app, _terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );
    app.enable_focus_reporting_for_test();
    app.toggle_active_pane_read_only();
    app.drive_text_key_for_test("x");
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "typed input is dropped"
    );
    app.send_focus_report_for_test(false);
    assert_eq!(bytes.lock().expect("bytes").as_slice(), b"\x1b[O");
}

#[test]
fn duplicates_inherit_the_source_panes_read_only_mode() {
    let (mut app, _terminal) = headless_app_for_test();
    let source = app.active_session_token_for_test();
    // A failed duplicate spawn leaves the source active: nothing changes.
    app.carry_read_only_to_duplicate(source);
    assert!(app.active_pane_accepts_input());

    app.toggle_active_pane_read_only();
    let dims = Dimensions::new(80, 24);
    let position = app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        crate::native::test_support::headless_writer(),
        dims,
    );
    assert!(app.switch_to_session_for_test(position));
    let duplicate = app.active_session_token_for_test();
    assert_ne!(duplicate, source, "the duplicate is the new active pane");
    assert!(
        app.active_pane_accepts_input(),
        "fresh panes start writable"
    );
    app.carry_read_only_to_duplicate(source);
    assert!(app.active_pane_read_only(), "the duplicate keeps read-only");

    // A writable source leaves its duplicate writable.
    app.toggle_active_pane_read_only();
    let writable_source = app.active_session_token_for_test();
    let position = app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        crate::native::test_support::headless_writer(),
        dims,
    );
    assert!(app.switch_to_session_for_test(position));
    app.carry_read_only_to_duplicate(writable_source);
    assert!(app.active_pane_accepts_input());
}
