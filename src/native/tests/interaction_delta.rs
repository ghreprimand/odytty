// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::native::session::SessionToken;

fn picker_app() -> App {
    let (mut app, _) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    for _ in 0..2 {
        app.push_headless_workspace_for_test(
            Arc::new(Mutex::new(Terminal::new(80, 24))),
            crate::native::test_support::headless_writer(),
            Dimensions::new(80, 24),
        );
    }
    app.rename_workspace_for_test(0, "Source");
    app.rename_workspace_for_test(1, "Same");
    app.rename_workspace_for_test(2, "Same");
    app
}

#[test]
fn delta_new_window_held_chord_creates_only_one_request() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, _) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.drive_char_with_mods_typed_for_test('n', true, true, KeyEventType::Press);
    assert!(app.take_new_window_request_for_test().is_some());
    for _ in 0..3 {
        app.drive_char_with_mods_typed_for_test('n', true, true, KeyEventType::Repeat);
        assert!(
            app.take_new_window_request_for_test().is_none(),
            "held chord must not queue another window"
        );
    }
}

#[test]
fn delta_workspace_picker_follows_destination_after_rail_shift() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = picker_app();
    let token = SessionToken(0);
    let destination = app
        .workspace_set()
        .workspace_identity(1)
        .expect("destination");
    app.open_move_tab_workspace_picker_for_test(token);
    app.move_workspace_at_for_test(1, false);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    let index = app
        .workspace_set()
        .workspace_index_of(destination)
        .expect("destination survives");
    app.handle_palette_action_for_test(&format!("workspace-switch-{index}"));
    assert!(
        app.tab_tokens_for_test().contains(&token),
        "picked destination follows identity"
    );
}

#[test]
fn delta_workspace_picker_drops_closed_destination() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = picker_app();
    let token = SessionToken(0);
    app.open_move_tab_workspace_picker_for_test(token);
    app.close_workspace_at_for_test(1);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    app.handle_palette_action_for_test("workspace-switch-1");
    assert!(
        !app.tab_tokens_for_test().contains(&token),
        "replacement destination must not receive tab"
    );
    app.handle_palette_action_for_test("workspace-switch-0");
    assert!(
        app.tab_tokens_for_test().contains(&token),
        "source tab remains in source"
    );
}

#[test]
fn delta_workspace_navigation_keeps_repeating() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = picker_app();
    let key = WinitKey::Named(NamedKey::PageDown);
    let physical = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::PageDown);
    let mods = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    };
    app.drive_raw_key_event_for_test(
        key.clone(),
        key.clone(),
        physical,
        mods,
        KeyEventType::Press,
    );
    let first = app.active_workspace_index_for_test();
    app.drive_raw_key_event_for_test(key.clone(), key, physical, mods, KeyEventType::Repeat);
    assert_ne!(
        app.active_workspace_index_for_test(),
        first,
        "held navigation continues cycling"
    );
}
