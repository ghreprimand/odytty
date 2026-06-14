// SPDX-License-Identifier: GPL-3.0-only
//! Blank-cell purity and basic glyph-ink containment.

use odytty::core::CursorStyle;

use crate::harness::*;

#[test]
fn blank_cell_renders_pure_background() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // All-spaces grid, cursor hidden: every pixel must be the exact background.
    let snapshot = row_snapshot(4, "");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let bg = default_bg();
    for y in 0..frame.height {
        for x in 0..frame.width {
            assert!(
                !differs(frame.pixel(x, y), bg),
                "blank grid pixel ({x},{y}) should be pure background"
            );
        }
    }
}

#[test]
fn known_glyph_inks_within_its_cell() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'H' in column 0, blanks elsewhere. 'H' is a plain ASCII glyph with no side
    // bearing overflow, so its ink stays inside cell 0; neighbor cells stay blank.
    let snapshot = row_snapshot(4, "H");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "'H' should leave visible ink in its own cell"
    );
    // A column two cells away must be untouched (no stray ink / no bleed).
    assert_eq!(
        cell_ink_count(&frame, 2, 0),
        0,
        "ink must not bleed two cells away from 'H'"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "rightmost cell must stay blank"
    );
}

#[test]
fn subpixel_atlas_composites_known_glyph() {
    let Some((_font, atlas)) = setup_subpixel() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(4, "H");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "subpixel atlas should leave visible ink in the glyph cell"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "subpixel glyph coverage must not bleed into distant blank cells"
    );
}
