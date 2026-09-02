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
use crate::native::profile_picker::{
    ProfilePicker, ProfilePickerEntry, ProfilePickerOutcome, ProfilePickerPurpose,
};

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
