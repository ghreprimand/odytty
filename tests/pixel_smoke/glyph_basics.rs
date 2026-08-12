// SPDX-License-Identifier: GPL-3.0-only
//! Blank-cell purity and basic glyph-ink containment.

use crate::harness::*;
use odytty::core::{CursorStyle, Terminal};
use odytty::grid;

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
fn placeholder_char_emits_no_glyph_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // U+10EEEE stays in the Snapshot for copy/paste, but must not paint tofu.
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(format!("{}", odytty::core::PLACEHOLDER_CHAR).as_bytes());
    // Diacritics on a placeholder encode Kitty ids — also not ink.
    terminal.advance(format!("\r\n{}\u{0305}\u{0305}", odytty::core::PLACEHOLDER_CHAR).as_bytes());
    let snapshot = terminal.snapshot();
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    assert_eq!(
        cell_ink_count(&frame, 0, 0),
        0,
        "placeholder base must emit no coverage glyph ink"
    );
    assert_eq!(
        cell_ink_count(&frame, 0, 1),
        0,
        "placeholder diacritics must emit no coverage glyph ink"
    );
}

#[test]
fn placeholder_underline_decoration_still_draws() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(format!("\x1b[4m{}", odytty::core::PLACEHOLDER_CHAR).as_bytes());
    let snapshot = terminal.snapshot();
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let rect = grid::underline_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    let row = (((rect[1] + rect[3]) / 2.0) as usize).min(frame.height - 1);
    let bg = default_bg();
    let inked: usize = (0..frame.cell_w)
        .filter(|&x| differs(frame.pixel(x, row), bg))
        .count();
    assert!(
        inked >= frame.cell_w - 1,
        "placeholder cells remain decoration-owned: underline row {row} must ink (got {inked})"
    );
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
