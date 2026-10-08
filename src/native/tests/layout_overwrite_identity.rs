// SPDX-License-Identifier: GPL-3.0-only
//! The overwrite confirmation of "Save Workspace as Layout" names a workspace,
//! not a rail position, through the real prompt and dialog keys: a workspace
//! before it that closes while the dialog is open does not make the confirm
//! save its neighbor, and a confirm after the named workspace closed saves
//! nothing.

use std::sync::{Arc, Mutex};

use winit::keyboard::NamedKey;

use super::*;
use crate::native::persistence::{LoadOutcome, load_layout};

fn three_workspaces() -> App {
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
    for idx in 0..3 {
        app.rename_workspace_for_test(idx, &format!("w{idx}"));
    }
    app
}

/// Save workspace `idx` through the name prompt under its own name, the
/// prompt's seed.
fn save_through_prompt(app: &mut App, idx: usize) {
    app.enter_save_layout_prompt_for_test(idx);
    app.drive_named_key_for_test(NamedKey::Enter);
}

fn saved_workspace_name(name: &str) -> Option<String> {
    match load_layout(name) {
        LoadOutcome::Loaded(snapshot) => snapshot.workspaces.first().map(|ws| ws.name.clone()),
        _ => None,
    }
}

#[test]
fn a_preceding_workspace_closing_does_not_retarget_the_overwrite() {
    let base = crate::test_dirs::fresh_temp_dir("odytty-layout-identity-");
    super::config_env::with_config_base(&base, true, || {
        let mut app = three_workspaces();
        save_through_prompt(&mut app, 1);
        assert_eq!(saved_workspace_name("w1").as_deref(), Some("w1"));
        save_through_prompt(&mut app, 1);
        assert!(app.overlay_open_for_test(), "the collision asks first");
        // A workspace before it closes while the dialog is open.
        app.close_workspace_at_for_test(0);
        app.drive_named_key_for_test(NamedKey::Enter);
        assert_eq!(
            saved_workspace_name("w1").as_deref(),
            Some("w1"),
            "the named workspace is saved, not the one now at its old position"
        );
    });
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn an_overwrite_confirmed_after_its_workspace_closed_saves_nothing() {
    let base = crate::test_dirs::fresh_temp_dir("odytty-layout-identity-");
    super::config_env::with_config_base(&base, true, || {
        let mut app = three_workspaces();
        save_through_prompt(&mut app, 1);
        save_through_prompt(&mut app, 1);
        assert!(app.overlay_open_for_test(), "the collision asks first");
        app.close_workspace_at_for_test(1);
        app.drive_named_key_for_test(NamedKey::Enter);
        assert_eq!(
            saved_workspace_name("w1").as_deref(),
            Some("w1"),
            "the earlier save is left as it was"
        );
    });
    let _ = std::fs::remove_dir_all(base);
}
