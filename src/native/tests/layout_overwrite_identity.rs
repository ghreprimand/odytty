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

#[test]
fn legacy_and_invisible_name_collisions_prompt_before_replacement() {
    for legacy in [true, false] {
        let scratch = crate::test_dirs::ScratchDir::new("layout-name-prompt-");
        super::config_env::with_config_base(scratch.path(), true, || {
            let mut app = three_workspaces();
            let old = if legacy { "COM0" } else { "work" };
            app.rename_workspace_for_test(1, old);
            save_through_prompt(&mut app, 1);
            if legacy {
                let dir = crate::native::persistence::layouts_dir();
                std::fs::rename(dir.join("_COM0.json"), dir.join("COM0.json")).unwrap();
            }
            let new = if legacy { "COM0" } else { "work\u{200d}" };
            app.rename_workspace_for_test(1, new);
            save_through_prompt(&mut app, 1);
            assert!(
                app.overlay_open_for_test(),
                "same visible name must ask first"
            );
            app.drive_named_key_for_test(NamedKey::Enter);
            assert_eq!(
                crate::native::persistence::list_layout_names(),
                vec![if legacy { "_COM0" } else { new }]
            );
        });
    }
}
