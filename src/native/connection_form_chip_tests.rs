// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored Browse chip geometry and render-signature fixtures.
use super::*;

fn form_with_key(path: &str) -> ConnectionForm {
    let mut form = ConnectionForm::new();
    form.open_add(Vec::new());
    // IdentityFile is an Advanced field.
    form.advanced = true;
    form.identity_file = path.to_owned();
    form
}

/// The chip span is `(start, end)` columns of the row text; the text under it
/// is what the click opens the browser for.
fn chip_text(form: &ConnectionForm, width: usize) -> Option<String> {
    let (start, end) = form.identity_browse_chip_span(width)?;
    let text = form.render_field(FormField::IdentityFile, width).text;
    Some(text.chars().skip(start).take(end - start).collect())
}

#[test]
fn chip_span_covers_the_drawn_chip() {
    let form = form_with_key("/keys/id_ed25519");
    assert_eq!(chip_text(&form, 120).as_deref(), Some("[Browse]"));
    let empty = form_with_key("");
    assert_eq!(
        chip_text(&empty, 120).as_deref(),
        Some("[Browse ~/.ssh keys]")
    );
}

/// A key path that itself contains the chip's opening text is value text, not
/// the chip, when the real chip is cut off by a narrow body.
#[test]
fn a_literal_browse_marker_in_the_path_is_not_the_chip() {
    let form = form_with_key("/keys/[Browse-me]/id");
    // Label and path plus one gap cell: the real chip is entirely cut off.
    let width = form.identity_row_parts(false).0.chars().count() - 1;
    // The path is drawn, the gap and chip are clipped away: no chip to click.
    assert_eq!(chip_text(&form, width), None, "width {width}");
    // With room for the real chip the span is the real chip.
    assert_eq!(chip_text(&form, 120).as_deref(), Some("[Browse]"));
    // A click on the path text only focuses the field.
    let mut clicked = form_with_key("/keys/[Browse-me]/id");
    clicked.visible_lines(width, 25);
    let row = (0..25)
        .find(|&row| clicked.field_at_row(row) == Some(FormField::IdentityFile))
        .expect("identity row is rendered");
    let outcome = clicked.handle_pointer_press(row, 20, width);
    assert_eq!(outcome, ConnectionFormOutcome::Consumed);
}

/// A chip cut partway stays clickable over the cells that are drawn while its
/// `[Browse` opening is fully visible; cut further, it is not a target.
#[test]
fn a_partly_clipped_chip_stays_clickable() {
    let form = form_with_key("/k");
    let prefix = form.identity_row_parts(false).0.chars().count();
    assert_eq!(chip_text(&form, prefix + 7).as_deref(), Some("[Browse"));
    assert_eq!(chip_text(&form, prefix + 6), None);
    assert_eq!(chip_text(&form, prefix), None);
}

#[test]
fn the_browser_signature_covers_the_candidate_names() {
    let candidates = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let signature = |names: &[&str]| {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.browse = Some(KeyBrowse::open(candidates(names)));
        form.render_signature()
    };
    assert_ne!(
        signature(&["/k/id_a", "/k/id_b"]),
        signature(&["/k/id_c", "/k/id_d"]),
        "same count, query and selection, different names"
    );
    assert_eq!(
        signature(&["/k/id_a", "/k/id_b"]),
        signature(&["/k/id_a", "/k/id_b"])
    );
}
