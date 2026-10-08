// SPDX-License-Identifier: GPL-3.0-only
use super::*;

fn panel() -> SettingsPanel {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.open_section("Rendering");
    let index = panel
        .entries
        .iter()
        .position(|e| e.key == "bidi_reorder")
        .expect("bidi row");
    panel.set_selection(index);
    panel
}

#[test]
fn override_names_change_signature_and_precede_status_and_description() {
    let mut panel = panel();
    let before = panel.render_signature();
    panel.set_environment_overrides(vec![crate::settings::BIDI_REORDER_ENV]);
    assert_ne!(before, panel.render_signature());
    panel.message = Some("Saved 1 setting change(s) to odytty.conf.".into());
    let text = panel
        .visible_lines(80, 18)
        .into_iter()
        .map(|l| l.text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Environment override: ODYTTY_BIDI_REORDER"));
    assert!(text.contains("Save still writes"));
    assert!(!text.contains("private value"));
    panel.refresh(&Settings::default());
    assert!(
        panel
            .environment_override_note(&panel.entries[panel.selected])
            .is_some()
    );
    panel.save_succeeded(1);
    assert!(
        panel
            .environment_override_note(&panel.entries[panel.selected])
            .is_some()
    );
}

#[test]
fn unrelated_and_action_rows_have_no_override_notice() {
    let mut panel = panel();
    panel.set_environment_overrides(vec![crate::settings::FONT_SIZE_ENV, ""]);
    assert!(
        panel
            .environment_override_note(&panel.entries[panel.selected])
            .is_none()
    );
    assert!(
        panel
            .environment_override_note(&super::theme_builder_action_entry())
            .is_none()
    );
    assert!(
        panel
            .environment_override_note(&super::profile_manager_action_entry())
            .is_none()
    );
}

#[test]
fn startup_only_rows_name_the_override_without_promising_live_editing() {
    let mut panel = panel();
    let entry = panel
        .all_entries
        .iter()
        .find(|e| !e.reloadable && !e.env.is_empty())
        .expect("startup setting")
        .clone();
    panel.set_environment_overrides(vec![entry.env]);
    let note = panel.environment_override_note(&entry).expect("override");
    assert!(note.contains(entry.env));
    assert!(!note.contains("Save still writes"));
}

#[test]
fn a_short_footer_keeps_save_errors_after_the_override_name() {
    let mut panel = panel();
    panel.set_environment_overrides(vec![crate::settings::BIDI_REORDER_ENV]);
    panel.save_failed("disk full".into());
    let text = panel
        .visible_lines(80, 6)
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n");
    let name = text
        .find("Environment override: ODYTTY_BIDI_REORDER")
        .expect("override name");
    let error = text
        .find("Save failed: disk full")
        .expect("save error remains visible");
    assert!(name < error);
}
