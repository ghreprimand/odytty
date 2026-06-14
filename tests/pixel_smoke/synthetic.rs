// SPDX-License-Identifier: GPL-3.0-only
//! Synthetic bold/italic synthesis and the synthetic-styles kill switch.

use odytty::atlas::{FontStyle, GlyphAtlas};
use odytty::core::CursorStyle;

use crate::harness::*;

/// End-to-end: with synthetic bold enabled and only a Regular face, a bold row
/// composites strictly more ink than the same row in Regular — the emboldening
/// reaches the rendered frame through the real grid → atlas → composite path.
#[test]
fn synthetic_bold_row_inks_heavier_than_regular() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    atlas.set_synthetic_styles(true, false, false);
    let text = "MMMM";
    let cols = text.len();

    ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
    ensure_styled_row(&mut atlas, &font, FontStyle::Bold, text);

    let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
    let bold = composite(
        &styled_row_snapshot(cols, b"\x1b[1m", text),
        &atlas,
        CursorStyle::Block,
    );

    let reg_ink: usize = (0..cols).map(|c| cell_ink_count(&regular, c, 0)).sum();
    let bold_ink: usize = (0..cols).map(|c| cell_ink_count(&bold, c, 0)).sum();
    assert!(reg_ink > 0, "regular row should ink");
    assert!(
        bold_ink > reg_ink,
        "synthetic bold row should ink heavier (bold={bold_ink}, regular={reg_ink})"
    );
}

/// End-to-end: a synthetic-italic row leans right above the baseline. Comparing
/// the inked-pixel centroid of the cell's top quarter against its bottom quarter
/// (summed across the row) shows the top sitting clearly right of the bottom,
/// whereas a Regular row of the same text shows a much smaller delta.
#[test]
fn synthetic_italic_row_leans_right() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    atlas.set_synthetic_styles(false, true, false);
    // A single tall glyph: no neighbor cell for sheared overhang to bleed into,
    // so the top-vs-bottom centroid cleanly reflects the shear.
    let text = "I";
    let cols = text.len();

    ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
    ensure_styled_row(&mut atlas, &font, FontStyle::Italic, text);

    let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
    let italic = composite(
        &styled_row_snapshot(cols, b"\x1b[3m", text),
        &atlas,
        CursorStyle::Block,
    );

    let ital_delta = row_top_minus_bottom_centroid(&italic, cols);
    let reg_delta = row_top_minus_bottom_centroid(&regular, cols);
    let (Some(ital_delta), Some(reg_delta)) = (ital_delta, reg_delta) else {
        eprintln!("skipping: row inked too sparsely to measure lean");
        return;
    };
    assert!(
        ital_delta > reg_delta,
        "synthetic italic should lean further right at top than regular \
         (italic={ital_delta:.2}, regular={reg_delta:.2})"
    );
    assert!(
        ital_delta > 0.0,
        "synthetic italic top should lean right of its bottom (delta={ital_delta:.2})"
    );
}

/// The synthetic-styles kill switch (atlas mask forced fully off, as the
/// `synthetic_styles = off` setting drives) makes a bold row composite
/// **identically** to the same row in Regular: with no real bold face the bold
/// slot is rasterized straight from the Regular outline with no double-strike,
/// so its per-cell ink matches Regular exactly. This is the rendered-frame
/// contract behind disabling synthesis.
#[test]
fn synthetic_mask_off_renders_bold_identical_to_regular() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    atlas.set_synthetic_styles(false, false, false);
    let text = "MMMM";
    let cols = text.len();

    ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
    ensure_styled_row(&mut atlas, &font, FontStyle::Bold, text);

    let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
    let bold = composite(
        &styled_row_snapshot(cols, b"\x1b[1m", text),
        &atlas,
        CursorStyle::Block,
    );

    let mut any_ink = false;
    for c in 0..cols {
        let reg = cell_ink_count(&regular, c, 0);
        let bld = cell_ink_count(&bold, c, 0);
        assert_eq!(
            bld, reg,
            "mask-off bold cell {c} must match regular ink (bold={bld}, regular={reg})"
        );
        any_ink |= reg > 0;
    }
    assert!(any_ink, "row should ink");
}

/// Toggling the synthetic mask gates synthesis end-to-end: built with bold
/// synthesis off the bold row matches Regular ink; rebuilt with it on, the same
/// row inks strictly heavier. This is exactly the difference a live
/// `synthetic_styles` toggle drives through the renderer's atlas rebuild seam.
#[test]
fn synthetic_mask_toggle_gates_bold_weight() {
    let Some((font, _atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let text = "MMMM";
    let cols = text.len();

    let measure = |synth_bold: bool| -> (usize, usize) {
        let mut atlas = GlyphAtlas::build(&font, PX);
        atlas.set_synthetic_styles(synth_bold, false, false);
        ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
        ensure_styled_row(&mut atlas, &font, FontStyle::Bold, text);
        let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
        let bold = composite(
            &styled_row_snapshot(cols, b"\x1b[1m", text),
            &atlas,
            CursorStyle::Block,
        );
        let reg: usize = (0..cols).map(|c| cell_ink_count(&regular, c, 0)).sum();
        let bld: usize = (0..cols).map(|c| cell_ink_count(&bold, c, 0)).sum();
        (reg, bld)
    };

    let (reg_off, bold_off) = measure(false);
    let (reg_on, bold_on) = measure(true);
    assert!(reg_off > 0 && reg_on > 0, "regular row should ink");
    assert_eq!(bold_off, reg_off, "mask off: bold must match regular ink");
    assert!(
        bold_on > reg_on,
        "mask on: synthetic bold inks heavier (on={bold_on}, regular={reg_on})"
    );
}
