// SPDX-License-Identifier: GPL-3.0-only
use super::*;

#[test]
fn profile_ui_profile_catalog_real_wheel_survives_render() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, _) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 18),
        Settings::default(),
    );
    app.set_test_cell_for_test(cell(10, 20));
    app.set_test_surface_for_test(800, 360, WindowPadding::ZERO);
    let mut catalog = crate::profiles::ProfileCatalog::default();
    for i in 0..20 {
        let name = format!("p{i:02}");
        catalog.profiles.insert(
            name.clone(),
            crate::profiles::LaunchProfile::new(&name).expect("profile"),
        );
    }
    app.open_profile_catalog_for_test(catalog);
    let before = app.render_overlay_rows_for_test(80, 18);
    assert!(
        before.iter().any(|row| row.contains("p00")),
        "first row initially visible"
    );
    let rect = app.overlay_rect_for_test().expect("profile overlay");
    app.set_pointer_cell_for_test(rect.body_top + 1, rect.body_left + 1);
    app.dispatch_wheel_for_test(-1.0);
    let after = app.render_overlay_rows_for_test(80, 18);
    assert!(
        !after.iter().any(|row| row.contains("p00")),
        "wheel position persists through production render"
    );
    assert_ne!(after, before);
}

#[test]
fn no_op_backspace_preserves_profile_launch_choice_through_app_save() {
    let base = std::env::temp_dir().join(format!(
        "odytty-noop-launch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    super::config_env::with_config_base(&base, true, || {
        let _guard = crate::test_lock::render_globals_lock();
        for command in [true, false] {
            let mut profile = crate::profiles::LaunchProfile::new("launch-choice").unwrap();
            if command {
                profile.launch.command = Some(crate::profiles::ProfileCommand {
                    program: "fixture-runner".to_owned(),
                    args: vec!["retained argument".to_owned()],
                    preserved: Default::default(),
                });
            } else {
                profile.launch.shell = Some("fixture-shell".to_owned());
            }
            let dir = crate::profiles::profiles_dir_path().unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("launch-choice.profile.json");
            crate::profiles::write_profile_file(&path, &profile).unwrap();
            let before = std::fs::read(&path).unwrap();
            let mut catalog = crate::profiles::ProfileCatalog::default();
            catalog.profiles.insert(profile.name.clone(), profile);
            let (mut app, _) = headless_app_with(
                NativeOptions::default(),
                Dimensions::new(100, 80),
                Settings::default(),
            );
            app.set_test_cell_for_test(cell(10, 20));
            app.set_test_surface_for_test(1000, 1600, WindowPadding::ZERO);
            app.resize_grid_with_padding_for_test(cell(10, 20), WindowPadding::ZERO, 1000, 1600);
            let (grid_columns, grid_rows) = app.grid_dims_for_test();
            app.open_profile_catalog_for_test(catalog);
            app.drive_named_key_for_test(NamedKey::Enter);
            // Name, display name and platforms precede Shell; working
            // directory then precedes Command program in the launch group.
            for _ in 0..if command { 3 } else { 5 } {
                app.drive_named_key_for_test(NamedKey::ArrowDown);
            }
            app.drive_named_key_for_test(NamedKey::Backspace);
            // Down clamps at Cancel, whose preceding field is Save.
            for _ in 0..100 {
                app.drive_named_key_for_test(NamedKey::ArrowDown);
            }
            app.drive_named_key_for_test(NamedKey::ArrowUp);
            app.drive_named_key_for_test(NamedKey::Enter);
            assert!(
                app.render_overlay_rows_for_test(grid_columns, grid_rows)
                    .iter()
                    .any(|row| row.contains("Saved profile launch-choice")),
                "the real input route completed a save: {:?}",
                app.render_overlay_rows_for_test(grid_columns, grid_rows)
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "no-op editing keeps exact launch data"
            );
        }
    });
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn settings_path_picker_real_navigation_repaints_and_keeps_wheel_scroll() {
    let base = std::env::temp_dir().join(format!("odytty-picker-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    for index in 0..20 {
        std::fs::write(base.join(format!(".face-{index:02}.ttf")), b"fixture").unwrap();
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::config_env::with_config_base(&base, true, || {
            let _guard = crate::test_lock::render_globals_lock();
            let settings = Settings {
                explicit_font_path: Some(base.clone()),
                ..Settings::default()
            };
            let (mut app, _) =
                headless_app_with(NativeOptions::default(), Dimensions::new(80, 18), settings);
            app.set_test_cell_for_test(cell(10, 20));
            app.set_test_surface_for_test(800, 360, WindowPadding::ZERO);
            app.open_settings_section_for_test("Fonts");
            // Font file is the first row of the Fonts section.
            app.drive_named_key_for_test(winit::keyboard::NamedKey::Enter);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let rows = app.render_overlay_rows_for_test(80, 18);
                if rows.iter().any(|row| row.contains(".face-")) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "picker listing: {rows:?}"
                );
                std::thread::yield_now();
            }
            let before = app.overlay_signature_for_test();
            app.drive_named_key_for_test(winit::keyboard::NamedKey::End);
            let rows = app.render_overlay_rows_for_test(80, 18);
            assert!(
                rows.iter().any(|row| row.contains(".face-19.ttf")),
                "last selected row: {rows:?}"
            );
            assert_ne!(
                app.overlay_signature_for_test(),
                before,
                "picker input reaches render signature"
            );
            let rect = app.overlay_rect_for_test().unwrap();
            app.set_pointer_cell_for_test(rect.body_top + 1, rect.body_left + 1);
            app.dispatch_wheel_for_test(3.0);
            let wheel = app.overlay_signature_for_test();
            let _ = app.render_overlay_rows_for_test(80, 18);
            assert_eq!(
                app.overlay_signature_for_test(),
                wheel,
                "render preserves free wheel scrolling"
            );
            app.drive_named_key_for_test(winit::keyboard::NamedKey::Escape);
            assert_eq!(
                app.settings_panel_displayed_value_for_test("font"),
                Some(base.display().to_string()),
                "cancel preserves original path"
            );
        });
    }));
    let _ = std::fs::remove_dir_all(base);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
