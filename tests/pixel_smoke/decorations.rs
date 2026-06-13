//! Underline/strikethrough decoration rows, box-drawing seam continuity, wide
//! glyph spanning, and the bar cursor stripe.

use odytty::core::{CursorStyle, Terminal};
use odytty::grid;

use crate::harness::*;

#[test]
fn underline_attribute_inks_a_decoration_row() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(1, "\x1b[4mU");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    // The documented underline rect; assert that its row band is fully inked
    // across the cell width (a continuous foreground bar).
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
        "underline row {row} should be inked across the full cell width (got {inked}/{})",
        frame.cell_w
    );
}

#[test]
fn strikethrough_attribute_inks_a_decoration_row() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(1, "\x1b[9mS");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    let rect = grid::strikethrough_rect(
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
        "strikethrough row {row} should be inked across the full cell width (got {inked}/{})",
        frame.cell_w
    );
}

#[test]
fn box_drawing_horizontal_joins_across_the_cell_seam() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // U+2500 BOX DRAWINGS LIGHT HORIZONTAL in two adjacent cells. The classic
    // seam-continuity check: the horizontal stroke must run unbroken across the
    // boundary between the two cells with no blank column at the join.
    if !ensure_real_glyph(&mut atlas, &font, '\u{2500}') {
        eprintln!("skipping: font lacks U+2500 box-drawing glyph");
        return;
    }
    let snapshot = row_snapshot(2, "\u{2500}\u{2500}");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let bg = default_bg();

    // Find a row where the stroke inks in the left cell.
    let mut stroke_row = None;
    for y in 0..frame.height {
        if differs(frame.pixel(frame.cell_w / 2, y), bg) {
            stroke_row = Some(y);
            break;
        }
    }
    let Some(row) = stroke_row else {
        panic!("U+2500 produced no horizontal stroke ink");
    };

    // Across the full two-cell width, every column on the stroke row must be
    // inked — no gap, including the two columns straddling the seam.
    let mut gaps = Vec::new();
    for x in 0..frame.width {
        if !differs(frame.pixel(x, row), bg) {
            gaps.push(x);
        }
    }
    assert!(
        gaps.is_empty(),
        "box-drawing stroke row {row} has blank columns {gaps:?} (seam discontinuity)"
    );
}

#[test]
fn wide_char_spans_two_cells_without_double_draw() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A wide char occupies a lead cell + a continuation spacer. Whether the font
    // has the glyph or renders the fallback box, exactly one glyph quad is
    // emitted for the pair (the spacer draws nothing), and ink stays within the
    // two-cell span. '世' is East-Asian Wide.
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    term.advance("世".as_bytes());
    let snapshot = term.snapshot();
    assert!(
        snapshot.cells[1].wide_continuation,
        "precondition: column 1 is a wide continuation spacer"
    );

    // Vertex-level: exactly one glyph quad across the whole grid (no double-draw
    // from the continuation cell).
    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, CursorStyle::Block);
    let glyph_quads = verts
        .chunks_exact(grid::VERTS_PER_QUAD)
        .filter(|q| q[0].is_glyph > 0.5)
        .count();
    assert_eq!(
        glyph_quads, 1,
        "a wide char must emit exactly one glyph quad, not one per cell"
    );

    // Pixel-level: ink exists in the spanned region and nowhere past it (the two
    // trailing blank cells stay pure background).
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let span_ink = cell_ink_count(&frame, 0, 0) + cell_ink_count(&frame, 1, 0);
    assert!(
        span_ink > 0,
        "wide glyph should leave ink across its two cells"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "ink must not extend past the wide char's two-cell span"
    );
}

#[test]
fn wide_glyph_inks_across_the_seam_when_supported() {
    // W1: with a CJK/fullwidth-capable font, a width-2 glyph rasterizes into a
    // two-cell slot and inks across the cell seam — not clipped at the lead
    // cell's right edge. Skips on hosts without such a font (the validation host
    // here), where the always-running atlas unit test
    // `rasterize_clip_width_relieves_wide_glyph_clipping` proves the mechanism.
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(wide) = find_supported_wide_glyph(&mut atlas, &font) else {
        eprintln!("skipping: no wide (CJK/fullwidth) glyph in the loaded font");
        return;
    };

    // Grid: wide lead at cols 0-1, a narrow neighbour at col 2, blank at col 3.
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    term.advance(wide.to_string().as_bytes());
    term.advance(b"M");
    let snapshot = term.snapshot();
    assert!(
        snapshot.cells[1].wide_continuation,
        "precondition: column 1 is a wide continuation spacer"
    );
    assert!(!snapshot.cells[2].wide_continuation && snapshot.cells[2].ch == 'M');

    // Exactly one glyph quad for the wide pair (no continuation double-draw),
    // plus one for the narrow neighbour = 2 glyph quads total.
    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, CursorStyle::Block);
    let glyph_quads = verts
        .chunks_exact(grid::VERTS_PER_QUAD)
        .filter(|q| q[0].is_glyph > 0.5)
        .count();
    assert_eq!(
        glyph_quads, 2,
        "wide char + narrow neighbour must emit exactly two glyph quads"
    );

    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    // Ink crosses the seam: BOTH the lead cell and the continuation cell carry
    // real glyph ink (a clipped single-cell glyph would leave cell 1 near-empty).
    let lead_ink = cell_ink_count(&frame, 0, 0);
    let cont_ink = cell_ink_count(&frame, 1, 0);
    assert!(lead_ink > 0, "wide glyph lead cell should hold ink");
    assert!(
        cont_ink > 0,
        "wide glyph must ink across the seam into its continuation cell, not clip"
    );
    // Narrow neighbour unaffected: it has its own ink and the wide glyph does not
    // bleed past its two-cell span into the trailing blank.
    assert!(
        cell_ink_count(&frame, 2, 0) > 0,
        "narrow neighbour should render"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "ink must not extend past the wide span + neighbour"
    );
}

#[test]
fn bar_cursor_inks_only_a_thin_left_stripe() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Visible bar cursor on a blank cell at (0,0): a thin vertical stripe at the
    // cell's left edge, not a full-cell block. Assert the left column inks while
    // the right half of the cell stays background.
    let term = Terminal::new(2, 1);
    let snapshot = term.snapshot();
    let frame = composite(&snapshot, &atlas, CursorStyle::Bar);
    let bg = default_bg();

    let mid_y = frame.cell_h / 2;
    assert!(
        differs(frame.pixel(0, mid_y), bg),
        "bar cursor should ink the cell's left edge"
    );
    let right_x = frame.cell_w - 1;
    assert!(
        !differs(frame.pixel(right_x, mid_y), bg),
        "bar cursor must not fill the right side of the cell"
    );
}
