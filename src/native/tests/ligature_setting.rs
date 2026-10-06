// SPDX-License-Identifier: GPL-3.0-only
//! The Settings > Rendering "Programming ligatures" row over a pane holding a
//! shaped-script owner, a Latin ligature, and an emoji cluster, driven through
//! the real overlay key path: each toggle applies live, re-keys the frame
//! cache through the presentation epoch, persists on Save, and leaves the
//! independent script shaping switch on, while the terminal's cells and copy
//! text stay unchanged.

use super::*;
use winit::keyboard::{Key as WinitKey, NamedKey};

/// Devanagari ka, virama, ssa (a two-cell owner), a Latin `->`, and the
/// family ZWJ cluster.
const LINE: &str =
    "\u{0915}\u{094D}\u{0937} a->b \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";

fn key(app: &mut App, key: WinitKey, ctrl: bool) {
    app.drive_overlay_key_for_test(key, ctrl, false);
}

fn temp_conf(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("odytty-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("odytty.conf");
    std::fs::write(&path, "# kept\n").unwrap();
    path
}

/// Open Settings and drill into the section holding `ligatures`, with the
/// selection on that row.
fn select_ligatures_row(app: &mut App) {
    app.open_settings_overlay_for_test();
    for section in 0..24 {
        for _ in 0..section {
            key(app, WinitKey::Named(NamedKey::ArrowDown), false);
        }
        key(app, WinitKey::Named(NamedKey::Enter), false);
        let signature = app.overlay_signature_for_test();
        if let Some(target) = signature
            .panel
            .entries
            .iter()
            .position(|entry| entry.key == "ligatures")
        {
            for _ in signature.panel.selected..target {
                key(app, WinitKey::Named(NamedKey::ArrowDown), false);
            }
            return;
        }
        key(app, WinitKey::Named(NamedKey::Escape), false);
        for _ in 0..section {
            key(app, WinitKey::Named(NamedKey::ArrowUp), false);
        }
    }
    panic!("no Settings section lists ligatures");
}

#[test]
fn ligatures_row_rekeys_the_frame_both_ways_over_shaped_content() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(40, 8),
        Settings::default(),
    );
    terminal.lock().expect("terminal").advance(LINE.as_bytes());
    let cells = terminal.lock().expect("terminal").snapshot().cells;
    let conf = temp_conf("ligatures-panel");
    app.set_config_path_for_test(conf.clone());
    select_ligatures_row(&mut app);
    assert_eq!(
        app.settings_panel_displayed_value_for_test("ligatures"),
        Some("on".to_owned()),
        "on by default"
    );

    let epoch = app.presentation_epoch_for_test();
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert_eq!(
        app.settings_panel_displayed_value_for_test("ligatures"),
        Some("off".to_owned())
    );
    assert!(
        app.presentation_epoch_for_test() > epoch,
        "turning ligatures off re-keys the render signature"
    );
    let switches = app.shaping_switches_for_test();
    assert!(
        !switches.ligatures && switches.scripts,
        "the ligatures row leaves script shaping on"
    );
    key(&mut app, WinitKey::Character("s".into()), true);
    let saved = std::fs::read_to_string(&conf).unwrap();
    assert!(saved.contains("ligatures = off"), "{saved}");
    assert!(saved.contains("# kept"));

    let epoch = app.presentation_epoch_for_test();
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert_eq!(
        app.settings_panel_displayed_value_for_test("ligatures"),
        Some("on".to_owned())
    );
    assert!(
        app.presentation_epoch_for_test() > epoch,
        "turning ligatures on re-keys the render signature"
    );

    let after = terminal.lock().expect("terminal").snapshot();
    assert_eq!(after.cells, cells, "presentation only: cells are unchanged");
    let range = crate::selection::SelectionRange {
        start: crate::selection::CellPoint { row: 0, column: 0 },
        end: crate::selection::CellPoint { row: 0, column: 39 },
    };
    assert_eq!(
        crate::selection::selected_text(&after, range).trim_end(),
        LINE
    );
    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}
