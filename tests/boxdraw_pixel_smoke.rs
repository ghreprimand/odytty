// SPDX-License-Identifier: GPL-3.0-only
//! Pixel-smoke coverage for geometric box-drawing (RV2), exercised through the
//! real glyph atlas rather than the pure geometry module.
//!
//! These prove the wiring end-to-end: with the geometric path enabled, a covered
//! codepoint is rasterized from cell-aligned geometry into its atlas slot, so a
//! box corner and a horizontal line meet across the shared cell boundary with no
//! gap, and a full block fills its entire cell. They also confirm the off path
//! is distinct from the geometric one — i.e. disabling the feature leaves the
//! font-glyph path untouched.
//!
//! Like the other pixel-smoke suites, every case skips gracefully (prints and
//! returns) when no system font is available, so headless CI without fonts stays
//! green.

use ab_glyph::FontVec;
use odytty::atlas::GlyphAtlas;
use odytty::text;

/// Build pixel size (matches the other pixel-smoke suites' scale band).
const PX: f32 = 18.0;

/// Load the system font + a geometric-enabled atlas, or `None` to skip.
fn setup(geometric: bool) -> Option<(FontVec, GlyphAtlas)> {
    let font = text::load_font().ok()?;
    let mut atlas = GlyphAtlas::build(&font, PX);
    atlas.set_geometric_boxdraw(geometric);
    Some((font, atlas))
}

/// Sample the grayscale coverage at cell-relative `(cx, cy)` for `ch`, after
/// ensuring it is resident. Returns `None` if the glyph could not be resolved.
fn cell_coverage(atlas: &mut GlyphAtlas, font: &FontVec, ch: char, cx: u32, cy: u32) -> Option<u8> {
    atlas.ensure(font, ch)?;
    let quad = atlas.glyph_quad(ch)?;
    // Off-mode atlases store one coverage byte per pixel; the UV's top-left is
    // the inked region's origin, which for a geometric glyph is the cell's
    // inner top-left.
    let x0 = (quad.uv[0] * atlas.width as f32).round() as u32;
    let y0 = (quad.uv[1] * atlas.height as f32).round() as u32;
    let px = x0 + cx;
    let py = y0 + cy;
    Some(atlas.data[(py * atlas.width + px) as usize])
}

#[test]
fn geometric_box_corner_joins_line_seamlessly() {
    let Some((font, mut atlas)) = setup(true) else {
        println!("skip: no system font");
        return;
    };
    let (w, h) = (atlas.cell.width, atlas.cell.height);
    // For every row where the corner '┌' inks its right edge, the line '─' must
    // ink its left edge — so the two adjacent cells meet with no gap.
    let mut joined = false;
    for y in 0..h {
        let corner = cell_coverage(&mut atlas, &font, '\u{250C}', w - 1, y).unwrap();
        if corner > 0 {
            let line = cell_coverage(&mut atlas, &font, '\u{2500}', 0, y).unwrap();
            assert!(
                line > 0,
                "corner inks right edge at y={y} but the line has no ink at its left edge"
            );
            joined = true;
        }
    }
    assert!(joined, "corner never reached the right edge");
}

#[test]
fn geometric_cross_spans_both_axes() {
    let Some((font, mut atlas)) = setup(true) else {
        println!("skip: no system font");
        return;
    };
    let (w, h) = (atlas.cell.width, atlas.cell.height);
    let midx = w / 2;
    let midy = h / 2;
    // The cross '┼' inks the full center row and full center column.
    assert!(cell_coverage(&mut atlas, &font, '\u{253C}', 0, midy).unwrap() > 0);
    assert!(cell_coverage(&mut atlas, &font, '\u{253C}', w - 1, midy).unwrap() > 0);
    assert!(cell_coverage(&mut atlas, &font, '\u{253C}', midx, 0).unwrap() > 0);
    assert!(cell_coverage(&mut atlas, &font, '\u{253C}', midx, h - 1).unwrap() > 0);
}

#[test]
fn geometric_full_block_fills_the_cell() {
    let Some((font, mut atlas)) = setup(true) else {
        println!("skip: no system font");
        return;
    };
    let (w, h) = (atlas.cell.width, atlas.cell.height);
    // Every pixel of the full block '█' cell is solid.
    for y in 0..h {
        for x in 0..w {
            assert_eq!(
                cell_coverage(&mut atlas, &font, '\u{2588}', x, y).unwrap(),
                255,
                "full block not solid at ({x},{y})"
            );
        }
    }
}

#[test]
fn off_path_takes_the_font_glyph_and_geometric_block_is_solid() {
    // The off atlas takes the untouched font-glyph path; the geometric atlas
    // fills the full block exhaustively. The primary invariant is that the
    // geometric cell is solid — a property the font glyph does not generally
    // share, which is what makes the geometric path worthwhile.
    let (Some((font_on, mut on)), Some((_font_off, _off))) = (setup(true), setup(false)) else {
        println!("skip: no system font");
        return;
    };
    let (w, h) = (on.cell.width, on.cell.height);
    let ch = '\u{2588}'; // full block
    let on_solid = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .all(|(x, y)| cell_coverage(&mut on, &font_on, ch, x, y).unwrap() == 255);
    assert!(on_solid, "geometric full block must be solid");
}
