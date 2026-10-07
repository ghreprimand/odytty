// SPDX-License-Identifier: GPL-3.0-only
//! App-level context-menu tests through the production pointer and opener
//! paths: which button activates a row, the workspace accelerators, and the
//! render signature for accelerator text.

use super::*;
use crate::native::bindings::KeyBindings;
use crate::native::context_menu_ui::humanize_chord;
use crate::settings::{BindableAction, KeyBindingOverride, KeyChord, format_key_chord};
use winit::event::MouseButton as WinitMouseButton;

fn app_with(settings: Settings) -> App {
    let (app, _terminal) =
        headless_app_with(NativeOptions::default(), Dimensions::new(100, 48), settings);
    app
}

fn open_content_menu(app: &mut App) {
    app.set_pointer_cell_for_test(1, 1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test(), "context menu opened");
}

fn rendered_rows(app: &mut App) -> Vec<String> {
    let (columns, rows) = app.grid_dims_for_test();
    app.render_overlay_rows_for_test(columns, rows)
}

/// The painted grid row whose label is `label`, skipping the longer
/// "... with Profile" row that starts with the same words.
fn row_with_label(app: &mut App, label: &str) -> (usize, String) {
    let longer = format!("{label} with");
    rendered_rows(app)
        .into_iter()
        .enumerate()
        .find(|(_, line)| line.contains(label) && !line.contains(&longer))
        .unwrap_or_else(|| panic!("no painted row labelled {label:?}"))
}

fn chord_text(chord: KeyChord) -> String {
    humanize_chord(format_key_chord(chord))
}

#[test]
fn right_press_on_a_destructive_menu_row_does_not_activate_it() {
    let mut app = app_with(Settings::default());
    open_content_menu(&mut app);
    let (row, _) = row_with_label(&mut app, "Close Tab");
    let rect = app.overlay_rect_for_test().expect("context menu open");
    let focus_before = app.overlay_signature_for_test().context_menu.focused;

    app.set_pointer_cell_for_test(row, rect.body_left);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Right);
    assert!(
        !app.pending_exit_for_test(),
        "a right press on Close Tab must not close the last tab"
    );
    assert!(app.context_menu_open_for_test(), "the menu stays open");
    assert_eq!(
        app.overlay_signature_for_test().context_menu.focused,
        focus_before,
        "a right press does not move focus"
    );

    // Left-press parity: the same row activates with the left button.
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert!(
        !app.context_menu_open_for_test(),
        "the left press activates"
    );
    assert!(
        app.pending_exit_for_test(),
        "Close Tab on the last tab exits"
    );
}

#[test]
fn new_workspace_shows_its_default_chord_on_the_content_and_strip_menus() {
    let chord = KeyBindings::default()
        .chord_for_action(BindableAction::NewWorkspace)
        .expect("New Workspace has a default chord");
    let text = chord_text(chord);

    let mut app = app_with(Settings::default());
    open_content_menu(&mut app);
    let (_, line) = row_with_label(&mut app, "New Workspace");
    assert!(
        line.contains(&text),
        "content menu row {line:?} lacks {text}"
    );
    app.drive_named_key_for_test(NamedKey::Escape);

    app.open_empty_tab_strip_menu_for_test();
    let (_, line) = row_with_label(&mut app, "New Workspace");
    assert!(line.contains(&text), "strip menu row {line:?} lacks {text}");
}

#[test]
fn remapped_rename_and_close_workspace_show_their_chords_on_the_rail_menu() {
    let defaults = KeyBindings::default();
    let rename = defaults
        .chord_for_action(BindableAction::ThemePicker)
        .expect("Theme Picker has a default chord");
    let close = defaults
        .chord_for_action(BindableAction::SessionReplay)
        .expect("Session Replay has a default chord");
    let settings = Settings {
        key_bindings: vec![
            KeyBindingOverride {
                chord: rename,
                action: BindableAction::RenameWorkspace,
            },
            KeyBindingOverride {
                chord: close,
                action: BindableAction::CloseWorkspace,
            },
        ],
        ..Settings::default()
    };
    let mut app = app_with(settings);
    app.set_pointer_cell_for_test(5, 10);
    app.open_workspace_rail_menu_for_test(0);
    assert!(app.context_menu_open_for_test());
    let (_, line) = row_with_label(&mut app, "Rename Workspace");
    assert!(line.contains(&chord_text(rename)), "{line:?}");
    let (_, line) = row_with_label(&mut app, "Close Workspace");
    assert!(line.contains(&chord_text(close)), "{line:?}");
}

#[test]
fn a_rebound_accelerator_changes_the_menu_render_signature() {
    let mut plain = app_with(Settings::default());
    open_content_menu(&mut plain);
    let chord = KeyBindings::default()
        .chord_for_action(BindableAction::ThemePicker)
        .expect("Theme Picker has a default chord");
    let mut rebound = app_with(Settings {
        key_bindings: vec![KeyBindingOverride {
            chord,
            action: BindableAction::RenameWorkspace,
        }],
        ..Settings::default()
    });
    open_content_menu(&mut rebound);
    assert_ne!(
        rendered_rows(&mut plain),
        rendered_rows(&mut rebound),
        "the painted Rename Workspace accelerator differs"
    );
    assert_ne!(
        plain.overlay_signature_for_test().context_menu,
        rebound.overlay_signature_for_test().context_menu,
        "different painted accelerators must not share a frame-cache key"
    );
}
