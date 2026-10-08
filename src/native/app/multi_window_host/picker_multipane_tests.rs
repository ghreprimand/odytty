// SPDX-License-Identifier: GPL-3.0-only
//! The move and merge pickers through the real request path with split
//! windows: a palette action raises the request, the host opens the picker, and
//! the origin banner and candidate numeral reach every visible pane of every
//! multi-pane window. Cancel clears them all. A winit key event cannot be
//! built outside winit, so keypresses enter either as the decoded `PickerKey`
//! or through the host's key routing below the event conversion.

use super::tests::{headless, host_of};
use super::*;
use crate::core::{Dimensions, Terminal};
use crate::native::app::PanePaintProbe;
use crate::native::options::NativeOptions;
use crate::native::pty::PtyWriter;
use crate::native::test_support::headless_app_with_writer;
use crate::native::tests::cell;
use crate::settings::Settings;
use std::io::Write;

struct SinkWriter;

impl Write for SinkWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn sink() -> PtyWriter {
    Arc::new(Mutex::new(Box::new(SinkWriter)))
}

/// A headless window whose active tab holds `panes` side-by-side panes, with a
/// fixed cell size and surface so the multi-pane rebuild runs.
fn split_window(panes: usize) -> App {
    let (mut app, _terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        sink(),
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, crate::native::WindowPadding::ZERO);
    let first = app.active_session_token_for_test();
    for _ in 1..panes {
        app.seed_headless_split_pane_for_test(
            true,
            Arc::new(Mutex::new(Terminal::new(40, 24))),
            sink(),
            Dimensions::new(40, 24),
        );
    }
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(cell(8, 16));
        app.set_test_surface_for_test(800, 416, crate::native::WindowPadding::ZERO);
    }
    app.focus_session_token_for_test(first);
    app.reflow_active_panes_for_test();
    app
}

/// The picker chrome row (row 1 on a pane of three or more rows) of every
/// visible pane.
fn chrome_rows(app: &mut App) -> Vec<String> {
    app.rebuild_multipane_probe_for_test()
        .iter()
        .map(|probe: &PanePaintProbe| probe.rows.get(1).cloned().unwrap_or_default())
        .collect()
}

fn every_pane_shows(rows: &[String], needle: &str) -> bool {
    !rows.is_empty() && rows.iter().all(|row| row.contains(needle))
}

fn no_pane_shows(rows: &[String], needle: &str) -> bool {
    rows.iter().all(|row| !row.contains(needle))
}

/// Open the picker the way a user does: the palette action raises the request
/// on the invoking window, and the host's service pass turns it into an open
/// picker over the other windows.
fn open_through_the_palette(host: &mut MultiWindowHost, origin: usize, action: &str) {
    host.sync_sibling_counts();
    host.windows[origin].handle_palette_action_for_test(action);
    host.service_merge_requests();
}

fn clean_baseline(host: &mut MultiWindowHost) {
    for app in &mut host.windows {
        if !app.active_is_single_pane_for_test() {
            app.rebuild_multipane_probe_for_test();
            app.clear_visible_pane_rebuild_flags_for_test();
            assert!(!app.should_rebuild_frame_for_test(), "idle baseline");
        }
    }
}

/// A split source and a split destination: the banner is on every pane of the
/// source, the numeral on every pane of the destination, a single-pane
/// destination still badges, and opening marks both multi-pane windows for a
/// rebuild (the multi-pane path has no frame cache to re-key).
#[test]
fn a_split_source_opens_the_move_picker_through_the_palette_and_paints_every_pane() {
    let mut host = host_of(vec![split_window(2), split_window(2), headless()]);
    clean_baseline(&mut host);

    open_through_the_palette(&mut host, 0, "move-pane-to-window");
    assert!(host.picker.is_some(), "the picker opened");

    assert!(host.windows[0].should_rebuild_frame_for_test());
    assert!(host.windows[1].should_rebuild_frame_for_test());
    let source = chrome_rows(&mut host.windows[0]);
    assert!(source.len() >= 2, "a split source: {source:?}");
    assert!(
        every_pane_shows(&source, "Move picker"),
        "the banner is on every visible source pane: {source:?}"
    );
    assert!(no_pane_shows(&source, "Press"), "no numeral on the origin");

    let destination = chrome_rows(&mut host.windows[1]);
    assert!(
        destination.len() >= 2,
        "a split destination: {destination:?}"
    );
    assert!(
        every_pane_shows(&destination, "Press 1 to move here"),
        "the first candidate's numeral is on every visible pane: {destination:?}"
    );
    assert!(no_pane_shows(&destination, "Move picker"));

    assert_eq!(host.windows[2].merge_numeral(), Some(2));
    let mut single = crate::native::tests::snapshot(&[""; 6], 60);
    host.windows[2].paint_merge_numeral_cells(&mut single);
    let single_text: String = single.cells.iter().map(|cell| cell.ch).collect();
    assert!(
        single_text.contains("Press 2 to move here"),
        "a single-pane candidate paints through the single-pane path"
    );
}

/// Escape closes the picker, and every pane of every window is clean on the
/// next rebuild, which the close itself marks.
#[test]
fn cancel_clears_the_banner_and_numerals_from_every_pane_of_both_windows() {
    let mut host = host_of(vec![split_window(2), split_window(3)]);
    open_through_the_palette(&mut host, 0, "move-pane-to-window");
    assert!(host.picker.is_some());
    assert!(every_pane_shows(
        &chrome_rows(&mut host.windows[0]),
        "Move picker"
    ));
    assert!(every_pane_shows(
        &chrome_rows(&mut host.windows[1]),
        "Press 1 to move here"
    ));

    clean_baseline(&mut host);
    host.handle_picker_key(PickerKey::Cancel);
    assert!(host.picker.is_none());
    assert!(host.windows[0].should_rebuild_frame_for_test());
    assert!(host.windows[1].should_rebuild_frame_for_test());
    for window in 0..2 {
        let rows = chrome_rows(&mut host.windows[window]);
        assert!(rows.len() >= 2, "window {window} still split: {rows:?}");
        assert!(
            no_pane_shows(&rows, "Move picker") && no_pane_shows(&rows, "Press"),
            "window {window} is clean after cancel: {rows:?}"
        );
    }
    assert_eq!(host.windows.len(), 2, "cancel moves nothing");
}

/// The merge pickers (not the moves) badge a split origin and candidate the
/// same way, with the merge wording.
#[test]
fn the_merge_picker_paints_split_windows_with_the_merge_wording() {
    let mut host = host_of(vec![split_window(2), split_window(2)]);
    open_through_the_palette(&mut host, 0, "merge-window-into");
    assert!(host.picker.is_some());
    assert!(every_pane_shows(
        &chrome_rows(&mut host.windows[0]),
        "Merge picker"
    ));
    assert!(every_pane_shows(
        &chrome_rows(&mut host.windows[1]),
        "Press 1 to merge here"
    ));
    host.handle_picker_key(PickerKey::Cancel);
    for window in 0..2 {
        let rows = chrome_rows(&mut host.windows[window]);
        assert!(no_pane_shows(&rows, "Merge picker") && no_pane_shows(&rows, "Press"));
    }
}

/// A non-cancel outcome clears the badges too: selecting the candidate runs
/// the move and leaves no picker chrome on the surviving split window.
#[test]
fn selecting_a_candidate_leaves_no_picker_chrome_on_the_split_windows() {
    let mut host = host_of(vec![split_window(2), split_window(2)]);
    open_through_the_palette(&mut host, 0, "move-pane-to-window");
    host.handle_picker_key(PickerKey::Select(1));
    assert!(host.picker.is_none());
    for window in 0..host.windows.len() {
        if host.windows[window].active_is_single_pane_for_test() {
            continue;
        }
        let rows = chrome_rows(&mut host.windows[window]);
        assert!(
            no_pane_shows(&rows, "Move picker") && no_pane_shows(&rows, "Press"),
            "window {window}: {rows:?}"
        );
    }
}

fn key_input(
    code: winit::keyboard::KeyCode,
    pressed: bool,
    repeat: bool,
    logical: &winit::keyboard::Key,
) -> picker_keys::PickerKeyInput<'_> {
    picker_keys::PickerKeyInput {
        physical: winit::keyboard::PhysicalKey::Code(code),
        pressed,
        repeat,
        logical,
    }
}

/// The key that closed the picker keeps its repeats and release out of the
/// windows afterwards; a later press of the same key routes normally.
#[test]
fn the_key_that_closed_the_picker_keeps_its_release_out_of_the_windows() {
    use winit::keyboard::{Key, KeyCode, NamedKey};
    let mut host = host_of(vec![split_window(2), split_window(2)]);
    open_through_the_palette(&mut host, 0, "move-pane-to-window");
    let escape = Key::Named(NamedKey::Escape);
    assert!(host.route_picker_input(key_input(KeyCode::Escape, true, false, &escape)));
    assert!(host.picker.is_none(), "Escape cancelled the picker");
    assert!(
        host.route_picker_input(key_input(KeyCode::Escape, true, true, &escape)),
        "a repeat of the consumed key is dropped"
    );
    assert!(
        host.route_picker_input(key_input(KeyCode::Escape, false, false, &escape)),
        "its release is dropped"
    );
    assert!(
        !host.route_picker_input(key_input(KeyCode::Escape, false, false, &escape)),
        "a second release is not the consumed one"
    );
    assert!(
        !host.route_picker_input(key_input(KeyCode::Escape, true, false, &escape)),
        "a new press with no picker routes to the window"
    );
}

/// A text of more than one character is not a picker numeral, even when it
/// starts with one.
#[test]
fn a_multi_character_text_is_not_a_picker_numeral() {
    use winit::keyboard::{Key, KeyCode};
    let mut host = host_of(vec![split_window(2), split_window(2)]);
    open_through_the_palette(&mut host, 0, "move-pane-to-window");
    let text = Key::Character("12".into());
    assert!(!host.route_picker_input(key_input(KeyCode::Digit1, true, false, &text)));
    assert!(host.picker.is_some(), "the picker stays open");
    assert_eq!(host.windows.len(), 2, "nothing moved");
    let one = Key::Character("1".into());
    assert!(host.route_picker_input(key_input(KeyCode::Digit1, true, false, &one)));
    assert!(host.picker.is_none(), "a single numeral selects");
}
