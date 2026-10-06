// SPDX-License-Identifier: GPL-3.0-only
// Project-authored bounded protocol fixtures.
use odytty::desktop::map_macos_app_paths;

#[test]
fn mapped_application_rows_never_have_an_empty_label() {
    let applications = map_macos_app_paths(vec!["/".to_owned()], "/fixture/document.png");
    assert!(
        applications
            .iter()
            .all(|application| !application.name.is_empty())
    );
}
