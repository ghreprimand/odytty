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
