// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored short form viewport and pointer fixtures.
use super::*;
#[test]
fn every_control_stays_visible_at_each_height() {
    for advanced in [false, true] {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.advanced = advanced;
        for height in 1..=25 {
            for field in form.fields() {
                form.focus = field;
                let lines = form.visible_lines(72, height);
                assert!(lines.len() <= height);
                let focused = lines
                    .iter()
                    .position(|line| line.focused)
                    .expect("visible focus");
                assert_eq!(form.field_at_row(focused), Some(field));
                for (row, line) in lines.iter().enumerate() {
                    if let Some(target) = form.field_at_row(row) {
                        assert_eq!(line.text, form.render_field(target, 72).text);
                    }
                }
                assert_eq!(form.field_at_row(height), None);
            }
        }
    }
}
#[test]
fn pointer_uses_the_last_rendered_window_until_redraw() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    assert_eq!(form.field_at_row(0), None);
    form.visible_lines(72, 3);
    for _ in 0..7 {
        form.handle_input(OverlayInput::Down);
    }
    assert_eq!(form.focus, FormField::Cancel);
    form.handle_pointer_press(0, 0, 72);
    assert_eq!(form.focus, FormField::Alias);
    form.focus = FormField::Cancel;
    form.visible_lines(72, 3);
    assert_eq!(form.field_at_row(2), Some(FormField::Cancel));
    assert_eq!(
        form.handle_pointer_press(2, 0, 72),
        ConnectionFormOutcome::Close
    );
    assert!(form.visible_lines(72, 0).is_empty());
    assert_eq!(form.field_at_row(0), None);
}
#[test]
fn help_and_browser_rows_do_not_map_to_form_controls() {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    let lines = form.visible_lines(30, 40);
    let help = lines
        .iter()
        .position(|line| line.text.contains("Quick-connect name"))
        .unwrap();
    for row in help..lines.len() {
        assert_eq!(form.field_at_row(row), None);
    }
    form.focus = FormField::Advanced;
    form.activate_focus();
    form.focus = FormField::IdentityFile;
    form.visible_lines(72, 2);
    form.open_key_browse(vec!["/fixtures/keys/example".into()]);
    form.visible_lines(72, 2);
    assert_eq!(form.field_at_row(0), None);
}
