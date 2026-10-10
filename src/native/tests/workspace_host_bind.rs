// SPDX-License-Identifier: GPL-3.0-only
//! A rail host picker binds the workspace that was right-clicked, by its
//! creation identity, even when the rail shifts while the picker is open.
use super::super::session::SessionToken;
use super::*;

struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn rail_host_picker_binds_the_clicked_workspace_after_a_rail_shift() {
    let root = crate::test_dirs::fresh_temp_dir("odytty-rail-bind-");
    let _cleanup = Cleanup(root.clone());
    std::fs::write(
        crate::connection_hosts::hosts_file_path(&root),
        "Host picked\n    HostName picked.example.invalid\n",
    )
    .expect("write the synthetic hosts file");
    let (mut app, _first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_config_path_for_test(root.join("odytty.conf"));
    app.set_test_cell_for_test(cell(10, 20));
    app.set_test_surface_for_test(800, 480, WindowPadding::ZERO);
    for _ in 0..2 {
        app.push_headless_workspace_for_test(
            Arc::new(Mutex::new(Terminal::new(80, 24))),
            crate::native::test_support::headless_writer(),
            Dimensions::new(80, 24),
        );
    }
    app.rename_workspace_for_test(0, "A");
    app.rename_workspace_for_test(1, "X");
    app.rename_workspace_for_test(2, "X");

    app.set_pointer_cell_for_test(5, 10);
    app.open_workspace_rail_menu_for_test(1);
    app.apply_overlay_outcome_for_test(
        crate::native::overlay::OverlayOutcome::ContextMenuBindWorkspaceAt(1),
    );
    // A background shell exit closes workspace A while the picker is open;
    // the second X slides into the clicked slot.
    app.dispatch_user_event_for_test(crate::native::pty::UserEvent::ShellExited {
        session: SessionToken(0),
    });
    assert_eq!(app.workspace_names_for_test(), vec!["X", "X"]);

    app.drive_named_key_for_test(winit::keyboard::NamedKey::Enter);

    let set = app.workspace_set();
    assert_eq!(
        set.workspace_default_profile_at(0),
        Some("picked"),
        "the clicked workspace, now first, is bound"
    );
    assert_eq!(
        set.workspace_default_profile_at(1),
        None,
        "the workspace that slid into the clicked slot stays local"
    );
}

#[test]
fn rail_host_picker_binds_nothing_once_the_clicked_workspace_closed() {
    let root = crate::test_dirs::fresh_temp_dir("odytty-rail-bind-gone-");
    let _cleanup = Cleanup(root.clone());
    std::fs::write(
        crate::connection_hosts::hosts_file_path(&root),
        "Host picked\n    HostName picked.example.invalid\n",
    )
    .expect("write the synthetic hosts file");
    let (mut app, _first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_config_path_for_test(root.join("odytty.conf"));
    app.set_test_cell_for_test(cell(10, 20));
    app.set_test_surface_for_test(800, 480, WindowPadding::ZERO);
    for _ in 0..2 {
        app.push_headless_workspace_for_test(
            Arc::new(Mutex::new(Terminal::new(80, 24))),
            crate::native::test_support::headless_writer(),
            Dimensions::new(80, 24),
        );
    }
    app.set_pointer_cell_for_test(5, 10);
    app.open_workspace_rail_menu_for_test(1);
    app.apply_overlay_outcome_for_test(
        crate::native::overlay::OverlayOutcome::ContextMenuBindWorkspaceAt(1),
    );
    app.dispatch_user_event_for_test(crate::native::pty::UserEvent::ShellExited {
        session: SessionToken(1),
    });

    app.drive_named_key_for_test(winit::keyboard::NamedKey::Enter);

    let set = app.workspace_set();
    for idx in 0..2 {
        assert_eq!(set.workspace_default_profile_at(idx), None, "slot {idx}");
    }
}
