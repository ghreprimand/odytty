// SPDX-License-Identifier: GPL-3.0-only
//! Pointer regression tests for list overlays.
//!
//! The row is located in the text emitted by `visible_lines`, not reconstructed
//! from a duplicated prompt/scroll offset formula.  Each test therefore means
//! "click the label the operator can see", including filtered and scrolled
//! states.

use crate::desktop::DesktopApp;
use crate::native::open_with_overlay::{OpenWithOverlay, OpenWithOverlayOutcome};
use crate::native::overlay::OverlayInput;
use crate::native::palette_overlay::{PaletteOverlay, PaletteOverlayOutcome};
use crate::native::profile_manager::{ProfileManager, ProfileManagerOutcome};
use crate::native::profile_picker::{
    ProfilePicker, ProfilePickerEntry, ProfilePickerOutcome, ProfilePickerPurpose,
};
use crate::profiles::{MAX_PROFILE_ENV_ENTRIES, ProfileCatalog};

fn rendered_row<T>(lines: &[T], text: impl Fn(&T) -> &str, needle: &str) -> usize {
    lines
        .iter()
        .position(|line| text(line).contains(needle))
        .unwrap_or_else(|| panic!("missing rendered row containing {needle:?}"))
}

#[test]
fn profile_picker_clicks_the_rendered_filtered_and_scrolled_profile_row() {
    let mut picker = ProfilePicker::new();
    let entries = (0..8)
        .map(|index| ProfilePickerEntry {
            name: format!("profile-{index}"),
            label: format!("Profile {index}"),
        })
        .collect();
    picker.open(entries, ProfilePickerPurpose::NewTab);

    // A short body forces the selected row into a scrolled viewport.
    for _ in 0..6 {
        let _ = picker.handle_input(OverlayInput::Down);
    }
    let lines = picker.visible_lines(80, 3);
    let row = rendered_row(&lines, |line| line.text.as_str(), "Profile 6");
    assert!(
        picker.click_row(row, 3),
        "the rendered profile row is clickable"
    );
    assert_eq!(
        picker.handle_input(OverlayInput::Activate),
        ProfilePickerOutcome::NewTab("profile-6".to_owned())
    );

    let _ = picker.handle_input(OverlayInput::Char('7'));
    let lines = picker.visible_lines(80, 3);
    let row = rendered_row(&lines, |line| line.text.as_str(), "Profile 7");
    assert!(
        picker.click_row(row, 3),
        "the rendered filtered row is clickable"
    );
    assert_eq!(
        picker.handle_input(OverlayInput::Activate),
        ProfilePickerOutcome::NewTab("profile-7".to_owned())
    );
}

#[test]
fn palette_clicks_the_rendered_filtered_action_row() {
    let mut palette = PaletteOverlay::new();
    palette.open_for_test(std::iter::empty::<&str>(), None);
    for ch in "settings".chars() {
        let _ = palette.handle_input(OverlayInput::Char(ch));
    }

    let lines = palette.visible_lines(80, 6);
    let row = rendered_row(&lines, |line| line.text.as_str(), "Settings");
    assert!(
        palette.click_row(row, 6),
        "the rendered palette action is clickable"
    );
    assert_eq!(
        palette.handle_input(OverlayInput::Activate),
        PaletteOverlayOutcome::Action("settings".to_owned())
    );
}

#[test]
fn open_with_clicks_the_rendered_filtered_application_row() {
    let mut overlay = OpenWithOverlay::new();
    overlay.open(vec![
        DesktopApp {
            id: "viewer.desktop".to_owned(),
            name: "Image Viewer".to_owned(),
            argv: vec!["viewer".to_owned(), "/tmp/image.png".to_owned()],
        },
        DesktopApp {
            id: "krita.desktop".to_owned(),
            name: "Krita".to_owned(),
            argv: vec!["krita".to_owned(), "/tmp/image.png".to_owned()],
        },
    ]);
    for ch in "krita".chars() {
        let _ = overlay.handle_input(OverlayInput::Char(ch));
    }

    let lines = overlay.visible_lines(80, 5);
    let row = rendered_row(&lines, |line| line.text.as_str(), "Krita");
    assert!(
        overlay.click_row(row, 5),
        "the rendered app row is clickable"
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OpenWithOverlayOutcome::Open(vec!["krita".to_owned(), "/tmp/image.png".to_owned()])
    );
}

// --- Profile manager form: rendered-row pointer lockstep across states ------

/// The row on which `needle` is drawn, in a body of `height` rows.
fn manager_row(manager: &ProfileManager, height: usize, needle: &str) -> usize {
    manager
        .visible_lines(80, height)
        .iter()
        .position(|line| line.text.contains(needle))
        .unwrap_or_else(|| panic!("missing rendered manager row containing {needle:?}"))
}

fn open_add_form(name: &str) -> ProfileManager {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);
    for ch in name.chars() {
        let _ = manager.handle_input(OverlayInput::Char(ch));
    }
    manager
}

#[test]
fn profile_form_clicks_save_in_the_last_scroll_state() {
    // A short body forces the form to scroll. Driving focus to the end brings
    // [Save]/[Cancel] into the final viewport; the press must land on the row
    // the operator sees, not a pre-scroll index.
    let mut manager = open_add_form("svc");
    for _ in 0..200 {
        let _ = manager.handle_input(OverlayInput::Down);
    }
    let height = 6;
    let save_row = manager_row(&manager, height, "[Save]");
    let outcome = manager.handle_pointer_press(80, height, save_row, 0);
    assert!(
        matches!(outcome, ProfileManagerOutcome::Persist { profile, .. } if profile.name == "svc"),
        "clicking the rendered [Save] row in the last scroll state must persist"
    );
}

#[test]
fn profile_form_clicks_cancel_in_the_last_scroll_state() {
    let mut manager = open_add_form("svc");
    for _ in 0..200 {
        let _ = manager.handle_input(OverlayInput::Down);
    }
    let height = 6;
    let cancel_row = manager_row(&manager, height, "[Cancel]");
    let outcome = manager.handle_pointer_press(80, height, cancel_row, 0);
    assert!(matches!(outcome, ProfileManagerOutcome::Consumed));
    assert!(
        manager.title().contains("Named Profiles"),
        "clicking the rendered [Cancel] row in the last scroll state returns to the catalog"
    );
}

#[test]
fn profile_form_clicks_a_specific_env_row_with_entries_present() {
    // A generous body renders the whole form so the click lands on the drawn
    // env row without a scroll offset confounding the assertion.
    let height = 400;
    let mut manager = open_add_form("envdev");
    // Add two env rows through the rendered add-row.
    let add_row = manager_row(&manager, height, "[Add environment override]");
    let _ = manager.handle_pointer_press(80, height, add_row, 0);
    let add_row = manager_row(&manager, height, "[Add environment override]");
    let _ = manager.handle_pointer_press(80, height, add_row, 0);

    // Focus the second env key row and type; only that draft slot changes.
    let key_row = manager_row(&manager, height, "Environment key 2:");
    let _ = manager.handle_pointer_press(80, height, key_row, 0);
    for ch in "SECOND".chars() {
        let _ = manager.handle_input(OverlayInput::Char(ch));
    }
    let lines: Vec<String> = manager
        .visible_lines(80, height)
        .into_iter()
        .map(|line| line.text)
        .collect();
    assert!(
        lines
            .iter()
            .any(|t| t.contains("Environment key 2: SECOND")),
        "typing after clicking env row 2 edits exactly that row"
    );
    assert!(
        lines
            .iter()
            .any(|t| t.contains("Environment key 1: ") && !t.contains("SECOND")),
        "env row 1 is untouched"
    );
}

#[test]
fn profile_form_env_limit_row_shows_a_bounded_message() {
    let height = 400;
    let mut manager = open_add_form("limitdev");
    for _ in 0..=MAX_PROFILE_ENV_ENTRIES {
        let add_row = manager_row(&manager, height, "[Add environment override]");
        let _ = manager.handle_pointer_press(80, height, add_row, 0);
    }
    let lines: Vec<String> = manager
        .visible_lines(80, height)
        .into_iter()
        .map(|line| line.text)
        .collect();
    assert!(
        lines.iter().any(|t| t.contains("limited to")),
        "a press past the env limit renders a bounded limit message"
    );
}
