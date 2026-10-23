// SPDX-License-Identifier: GPL-3.0-only
//! The block cursor over a font-shaped owner, and the per-cell placement of
//! a spacing mark when no face maps its owner.

use super::*;
use crate::core::CursorStyle;
use crate::grid::{CursorRenderParams, append_cursor_vertices_with_runs};

/// A two-cell Devanagari conjunct owner (ka, virama, ssa).
const CONJUNCT: &str = "\u{0915}\u{094D}\u{0937}";

fn with_cursor(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    runs: &[LigatureRun],
    cursor_runs: &[LigatureRun],
) -> Frame {
    let mut verts = Vec::new();
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut verts,
        snapshot,
        atlas,
        &[],
        runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        1.0,
        None,
        ChromePin::NONE,
    );
    append_cursor_vertices_with_runs(
        &mut verts,
        snapshot,
        atlas,
        CursorStyle::Block,
        [0.0, 0.0],
        CursorRenderParams::default(),
        None,
        cursor_runs,
    );
    composite(snapshot, atlas, &verts)
}

/// Pixels of `columns` that differ from `fill`: the ink drawn over it.
fn ink_mask(frame: &Frame, columns: std::ops::Range<usize>, fill: [f32; 3]) -> Vec<bool> {
    frame
        .columns(columns)
        .iter()
        .map(|pixel| *pixel != fill)
        .collect()
}

/// Fails before the shaped cursor redraw: the block over a shaped conjunct
/// drew the lead cell's own consonant and stacked its retained scalars at
/// the lead origin. It now redraws the owner's shaped glyphs clipped to the
/// block, so the ink inside the block has the conjunct's exact shape, and
/// the owner's second cell keeps its content-pass pixels.
#[test]
fn block_cursor_draws_shaped_owner_glyphs() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = face("Devanagari-subset.ttf");
    let mut terminal = Terminal::new(4, 1);
    terminal.advance(format!("{CONJUNCT}\x1b[1;1H").as_bytes());
    let snapshot = terminal.snapshot();
    assert!(snapshot.cursor_visible);
    assert_eq!(single_owner(&snapshot), Some(2));
    let mut atlas = GlyphAtlas::build(&font, PX);
    ensure_cells(&mut atlas, &font, &snapshot);
    let runs =
        ComplexShaper::new().build_runs(true, &snapshot, &Fonts(font.clone()), &mut atlas, &[]);
    assert_eq!(runs.len(), 1);

    let plain = frame(&snapshot, &atlas, &runs);
    let shaped = with_cursor(&snapshot, &atlas, &runs, &runs);
    let per_cell = with_cursor(&snapshot, &atlas, &runs, &[]);
    assert_eq!(
        shaped.columns(1..4),
        plain.columns(1..4),
        "ink outside the block keeps its content-pass pixels"
    );
    let background = plain.px[plain.px.len() - 1];
    let cursor_fill = {
        let blank = with_cursor(&terminal_with_cursor(), &atlas, &[], &[]);
        blank.px[0]
    };
    assert_ne!(cursor_fill, background, "the block is visible");
    let content_ink = ink_mask(&plain, 0..1, background);
    let cursor_ink = ink_mask(&shaped, 0..1, cursor_fill);
    assert!(
        content_ink.contains(&true),
        "the conjunct inks the lead cell"
    );
    assert_eq!(
        cursor_ink, content_ink,
        "the block redraws the conjunct's shape"
    );
    assert_ne!(
        ink_mask(&per_cell, 0..1, cursor_fill),
        content_ink,
        "the per-cell redraw draws a different shape"
    );
}

/// A visible cursor on the first cell of an otherwise blank four-column row.
fn terminal_with_cursor() -> Snapshot {
    let mut terminal = Terminal::new(4, 1);
    terminal.advance(b"\x1b[1;1H");
    terminal.snapshot()
}

/// Fails before the anchor rule: with no face mapping the owner (kha is not
/// in the fixture and no fallback is configured), no shaped run forms and the
/// per-cell path drew the retained visarga over the consonant's cell. It now
/// sits in the owner's second cell.
#[test]
fn per_cell_spacing_mark_draws_in_second_cell_when_no_face_maps_owner() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/combining-marks.ttf").to_vec(),
    )
    .expect("project-authored font fixture");
    let snapshot = terminal("\u{0916}\u{0903}", 4).snapshot();
    assert_eq!(single_owner(&snapshot), Some(2));
    let mut atlas = GlyphAtlas::build(&font, PX);
    atlas.set_fallback_fonts(Vec::new());
    ensure_cells(&mut atlas, &font, &snapshot);
    let runs =
        ComplexShaper::new().build_runs(true, &snapshot, &Fonts(font.clone()), &mut atlas, &[]);
    assert!(runs.is_empty(), "no face maps kha");
    let drawn = frame(&snapshot, &atlas, &runs);
    let blank = frame(&terminal("", 4).snapshot(), &atlas, &[]);
    let background = blank.px[0];
    assert!(
        ink_mask(&drawn, 1..2, background).contains(&true),
        "the visarga inks the second cell"
    );
    let mark_only = {
        let mut verts = Vec::new();
        let visarga = atlas
            .combining_mark_quad(FontStyle::Regular, '\u{0903}')
            .expect("visarga quad");
        build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
            &mut verts,
            &snapshot,
            &atlas,
            &[],
            &runs,
            0.0,
            [0.0, 0.0],
            BackgroundTreatmentParams::default(),
            1.0,
            1.0,
            None,
            ChromePin::NONE,
        );
        verts
            .chunks(INSTANCES_PER_QUAD)
            .filter(|quad| quad[0].is_glyph == 1.0 && quad[0].uv == [visarga.uv[0], visarga.uv[1]])
            .map(|quad| quad[0].pos[0])
            .collect::<Vec<_>>()
    };
    let cell_w = atlas.cell.width as f32;
    assert_eq!(mark_only.len(), 1);
    assert!(
        mark_only[0] >= cell_w,
        "visarga at x {} sits in the second cell",
        mark_only[0]
    );
}

/// Bidi sibling: on a reordered row the block over a shaped owner redraws
/// the owner's glyphs at the owner's visual columns, so the ink inside the
/// block has the shape the content pass drew there.
#[test]
fn block_cursor_draws_shaped_owner_glyphs_on_a_reordered_row() {
    let _guard = crate::test_lock::render_globals_lock();
    let primary = crate::text::load_bundled_font().expect("bundled font");
    let mut atlas = GlyphAtlas::build(&primary, PX);
    atlas.set_fallback_fonts(vec![Arc::new(face("Devanagari-subset.ttf"))]);
    let fonts = Fonts(primary.clone());
    // The isolate reverses at level 1, so the owner at columns 5..7 draws at
    // visual columns 3..5.
    let text = format!("x\u{2067}\u{05D0}\u{05D1}\u{05D2} {CONJUNCT} \u{05D3}\u{2069}");
    let mut term = Terminal::new(20, 1);
    term.advance(format!("{text}\x1b[1;6H").as_bytes());
    let snapshot = term.snapshot();
    assert_eq!(snapshot.cells[5].ch, '\u{0915}');
    assert_eq!(snapshot.cursor.column, 5);
    let wrapped: Vec<bool> = term
        .visible_search_rows(0)
        .iter()
        .map(|row| row.wrapped)
        .collect();
    let map = BidiDisplayMap::plan(&snapshot, &wrapped);
    assert!(map.row_is_reordered(0));
    let lead = map.visual_column(0, 5);
    assert_eq!(lead, 3);
    ensure_cells(&mut atlas, &primary, &snapshot);
    let runs = ComplexShaper::new().build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
    assert_eq!(runs.len(), 1);

    let mut content = Vec::new();
    build_cell_vertices_with_bidi_into(&mut content, &snapshot, &atlas, &[], &runs, &map);
    let plain = composite(&snapshot, &atlas, &content);
    let mut verts = content.clone();
    append_cursor_vertices_with_runs(
        &mut verts,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        [0.0, 0.0],
        CursorRenderParams::default(),
        Some(&map),
        &runs,
    );
    let shaped = composite(&snapshot, &atlas, &verts);
    let background = plain.px[plain.px.len() - 1];
    let cursor_fill = {
        let mut blank = Terminal::new(20, 1);
        blank.advance(b"\x1b[1;1H");
        let blank = blank.snapshot();
        let mut verts = Vec::new();
        append_cursor_vertices_with_runs(
            &mut verts,
            &blank,
            &atlas,
            CursorStyle::Block,
            [0.0, 0.0],
            CursorRenderParams::default(),
            None,
            &[],
        );
        composite(&blank, &atlas, &verts).px[0]
    };
    let content_ink = ink_mask(&plain, lead..lead + 1, background);
    assert!(content_ink.contains(&true), "the conjunct inks its lead");
    assert_eq!(
        ink_mask(&shaped, lead..lead + 1, cursor_fill),
        content_ink,
        "the block redraws the conjunct's shape at its visual column"
    );
    assert_eq!(
        shaped.columns(lead + 1..20),
        plain.columns(lead + 1..20),
        "ink right of the block keeps its content-pass pixels"
    );
    assert_eq!(
        shaped.columns(0..lead),
        plain.columns(0..lead),
        "ink left of the block keeps its content-pass pixels"
    );
}

/// Joining twin: the block over a letter of a run joined through a fallback
/// face redraws the run's joined glyphs clipped to the block, so the ink
/// inside the block is the joined form the content pass drew there.
#[test]
fn block_cursor_redraws_a_letter_joined_from_a_fallback_face() {
    let _guard = crate::test_lock::render_globals_lock();
    let primary = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/cluster-size-primary.ttf").to_vec(),
    )
    .expect("project-authored Latin fixture");
    let nko = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/s5b/joining/NKo-subset.ttf").to_vec(),
    )
    .expect("OFL N'Ko subset");
    let mut terminal = Terminal::new(4, 1);
    terminal.advance("\u{07CA}\u{07CB}\u{07CC}\x1b[1;2H".as_bytes());
    let snapshot = terminal.snapshot();
    assert!(snapshot.cursor_visible);
    let mut atlas = GlyphAtlas::build(&primary, PX);
    atlas.set_fallback_fonts(vec![std::sync::Arc::new(nko)]);
    ensure_cells(&mut atlas, &primary, &snapshot);
    let mut runs = crate::ligature::LigatureShaper::new().build_runs_with_faces(
        crate::ligature::ShapingSwitches {
            ligatures: true,
            scripts: true,
        },
        &snapshot,
        &Fonts(primary.clone()),
        &mut atlas,
        &[],
        crate::ligature::LatinShapingFeatures::default(),
        None,
    );
    assert_eq!(runs.len(), 1, "{runs:?}");
    for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
        atlas.ensure_shaped(&primary, glyph.key);
    }
    runs.retain(|run| run.glyphs.iter().all(|g| atlas.contains_shaped(g.key)));
    assert_eq!(runs.len(), 1, "every joined glyph is resident");

    let plain = frame(&snapshot, &atlas, &runs);
    let shaped = with_cursor(&snapshot, &atlas, &runs, &runs);
    let per_cell = with_cursor(&snapshot, &atlas, &runs, &[]);
    let background = plain.px[plain.px.len() - 1];
    let cursor_fill = {
        let mut blank = Terminal::new(4, 1);
        blank.advance(b"\x1b[1;1H");
        with_cursor(&blank.snapshot(), &atlas, &[], &[]).px[0]
    };
    let content_ink = ink_mask(&plain, 1..2, background);
    assert!(
        content_ink.contains(&true),
        "the joined letter inks its cell"
    );
    assert_eq!(
        ink_mask(&shaped, 1..2, cursor_fill),
        content_ink,
        "the block redraws the joined form"
    );
    assert_ne!(
        ink_mask(&per_cell, 1..2, cursor_fill),
        content_ink,
        "the per-cell redraw draws the cell's own fitted glyph"
    );
    assert_eq!(shaped.columns(0..1), plain.columns(0..1));
    assert_eq!(shaped.columns(2..4), plain.columns(2..4));
}
