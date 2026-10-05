// SPDX-License-Identifier: GPL-3.0-only
//! Arabic harakat inside joining runs, against the synthetic
//! `tests/fixtures/fonts/arabic-marks.ttf` face (project-authored, see the
//! fixture README). Its fatha and shadda attach to every letter through a
//! top anchor at font-unit offset (700, 200) from the base pen; kasra has no
//! attachment; damma is not mapped.

use super::*;
use crate::atlas::GlyphAtlas;
use crate::core::Terminal;
use crate::grid::{
    BackgroundTreatmentParams, ChromePin, Vertex, build_cell_vertices_with_bidi_into,
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into,
};
use crate::selection::{CellPoint, SelectionRange, selected_text};

const PX: f32 = 28.0;
const BEH: char = '\u{0628}';
const LAM: char = '\u{0644}';
const ALEF: char = '\u{0627}';
const FATHA: char = '\u{064E}';
const DAMMA: char = '\u{064F}';
const KASRA: char = '\u{0650}';
const SHADDA: char = '\u{0651}';
/// Fixture glyph ids (see `generate_arabic_marks_fixture.py`).
const BEH_INIT: u16 = 3;
const SHADDA_FATHA: u16 = 17;
/// The fixture's mark-to-base offset for top marks, in font units.
const TOP_OFFSET: [i16; 2] = [700, 200];

struct Fonts(FontHandle);

impl LigatureFonts for Fonts {
    fn ligature_font(&self, _style: FontStyle) -> &FontHandle {
        &self.0
    }
}

fn font() -> FontHandle {
    FontHandle::try_from_vec(include_bytes!("../../tests/fixtures/fonts/arabic-marks.ttf").to_vec())
        .expect("arabic marks fixture parses")
}

fn terminal(text: &str) -> Terminal {
    let mut terminal = Terminal::new(10, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance(text.as_bytes());
    terminal
}

fn runs_for(text: &str) -> (Snapshot, Vec<LigatureRun>) {
    let snapshot = terminal(text).snapshot();
    let runs = LigatureShaper::new().build_runs(true, &snapshot, &Fonts(font()), &[]);
    (snapshot, runs)
}

fn bounds_of(snapshot: &Snapshot) -> Vec<(usize, usize)> {
    let cols = snapshot.dimensions.columns;
    let coverage = ColorRunCoverage::new(&[], cols, 1);
    shaping_run_bounds(&snapshot.cells[..cols], 0, &coverage, &Fonts(font()))
        .into_iter()
        .map(|(start, end, _)| (start, end))
        .collect()
}

fn glyph_vertices(verts: &[Vertex]) -> impl Iterator<Item = &Vertex> {
    verts.iter().filter(|vertex| vertex.is_glyph > 0.5)
}

fn draws_uv(verts: &[Vertex], uv: [f32; 4]) -> Option<&Vertex> {
    glyph_vertices(verts)
        .find(|vertex| (vertex.uv[0] - uv[0]).abs() < 1e-6 && (vertex.uv[1] - uv[1]).abs() < 1e-6)
}

#[test]
fn harakat_table_holds_only_transparent_arabic_marks() {
    use swash::text::{Codepoint as _, JoiningType};
    let mut count = 0;
    for value in 0x0600..=0x08FF_u32 {
        let Some(ch) = char::from_u32(value) else {
            continue;
        };
        if arabic::is_arabic_harakat(ch) {
            count += 1;
            assert_eq!(
                ch.properties().joining_type(),
                JoiningType::T,
                "{value:04X}"
            );
            assert!(!is_arabic_joining_base(ch), "{value:04X}");
        }
    }
    assert_eq!(count, 11 + 21 + 1 + 7 + 6 + 2 + 4 + 15 + 29);
    // Format characters and spacing signs inside the ranges' gaps stay out,
    // as do the Unicode 14 marks the shaper does not treat as transparent.
    for ch in [
        '\u{06DD}', '\u{06DE}', '\u{06E5}', '\u{06E9}', '\u{08CA}', '\u{08D2}', '\u{08E2}',
        '\u{0640}',
    ] {
        assert!(!arabic::is_arabic_harakat(ch), "{:04X}", u32::from(ch));
    }
}

#[test]
fn a_harakat_bearing_base_joins_its_arabic_run() {
    let snapshot = terminal(&format!("{BEH}{FATHA}{LAM}{BEH} x")).snapshot();
    assert_eq!(snapshot.cells[0].combining(), [FATHA]);
    assert_eq!(bounds_of(&snapshot), [(0, 3), (4, 5)]);
}

#[test]
fn other_marks_and_unmapped_harakat_keep_the_monochrome_path() {
    // A Latin combining acute on an Arabic base is not a harakat.
    let acute = terminal(&format!("{BEH}\u{0301}{LAM}{BEH}")).snapshot();
    assert_eq!(bounds_of(&acute), [(1, 3)]);
    // Damma is a harakat the fixture face does not map: no .notdef overlay.
    let damma = terminal(&format!("{BEH}{LAM}{DAMMA}{BEH}{LAM}")).snapshot();
    assert_eq!(bounds_of(&damma), [(0, 1), (2, 4)]);
    let runs = LigatureShaper::new().build_runs(true, &damma, &Fonts(font()), &[]);
    assert!(
        runs.iter().all(|run| !run.covers(0, 1)),
        "the unmapped mark's cell draws per cell: {runs:?}"
    );
}

#[test]
fn joined_marked_base_carries_its_positioned_mark_into_the_overlay() {
    let word = format!("{BEH}{FATHA}{LAM}{BEH}");
    let (snapshot, runs) = runs_for(&word);
    let run = runs
        .iter()
        .find(|run| run.covers(0, 0))
        .unwrap_or_else(|| panic!("the marked base is joined: {runs:?}"));
    let glyphs: Vec<_> = run
        .glyphs
        .iter()
        .filter(|glyph| usize::from(glyph.key.anchor_cell) + run.start == 0)
        .map(|glyph| (glyph.key.glyph_id, glyph.key.mark_offset))
        .collect();
    let fatha = font().glyph_id(FATHA).0;
    assert_eq!(glyphs, [(BEH_INIT, [0, 0]), (fatha, TOP_OFFSET)]);
    // Every glyph other than the mark keeps a zero offset.
    assert_eq!(
        run.glyphs
            .iter()
            .filter(|glyph| glyph.key.mark_offset != [0, 0])
            .count(),
        1
    );
    // Copy and selection still return the logical text.
    let range = SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 2 },
    };
    assert_eq!(selected_text(&snapshot, range), word);
}

#[test]
fn marked_and_unmarked_words_join_identically_outside_the_mark() {
    let (_, marked) = runs_for(&format!("{BEH}{FATHA}{LAM}{BEH}"));
    let (_, plain) = runs_for(&format!("{BEH}{LAM}{BEH}"));
    let ids = |runs: &[LigatureRun]| {
        runs.iter()
            .flat_map(|run| {
                run.glyphs
                    .iter()
                    .filter(|glyph| glyph.key.mark_offset == [0, 0])
                    .map(move |glyph| {
                        (
                            run.start + usize::from(glyph.key.anchor_cell),
                            glyph.key.glyph_id,
                        )
                    })
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&marked), ids(&plain));
}

#[test]
fn composed_and_unattached_marks_keep_their_shaped_placement() {
    // Shadda then fatha compose into one glyph that attaches like fatha.
    let (_, runs) = runs_for(&format!("{BEH}{SHADDA}{FATHA}{LAM}{BEH}"));
    let composed: Vec<_> = runs
        .iter()
        .flat_map(|run| run.glyphs.iter())
        .filter(|glyph| glyph.key.glyph_id == SHADDA_FATHA)
        .map(|glyph| glyph.key.mark_offset)
        .collect();
    assert_eq!(composed, [TOP_OFFSET]);
    // Kasra has no attachment: its pen follows the base advance (600 units).
    let (_, runs) = runs_for(&format!("{BEH}{KASRA}{LAM}{BEH}"));
    let kasra = font().glyph_id(KASRA).0;
    let offsets: Vec<_> = runs
        .iter()
        .flat_map(|run| run.glyphs.iter())
        .filter(|glyph| glyph.key.glyph_id == kasra)
        .map(|glyph| glyph.key.mark_offset)
        .collect();
    assert_eq!(offsets, [[600, 0]]);
}

#[test]
fn renderer_draws_the_positioned_mark_instead_of_the_monochrome_one() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = font();
    let (snapshot, runs) = runs_for(&format!("{BEH}{FATHA}{LAM}{BEH}"));
    let mut atlas = GlyphAtlas::build(&font, PX);
    for ch in [BEH, LAM, FATHA] {
        atlas.ensure(&font, ch);
    }
    for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
        atlas.ensure_shaped(&font, glyph.key);
    }
    let mark_key = runs
        .iter()
        .flat_map(|run| run.glyphs.iter())
        .find(|glyph| glyph.key.mark_offset == TOP_OFFSET)
        .expect("positioned mark")
        .key;
    let shaped = atlas.shaped_glyph_quad(mark_key).expect("mark ink");
    let mono = atlas
        .combining_mark_quad(FontStyle::Regular, FATHA)
        .expect("mono mark");
    // 200 font units higher and 100 units right of the unattached pen.
    let units = font.px_per_unit(PX);
    assert!(
        ((mono.offset_y - shaped.offset_y) as f32 - 200.0 * units).abs() <= 1.0,
        "raised by the anchor: shaped {shaped:?} mono {mono:?}"
    );
    assert!(
        ((shaped.offset_x - mono.offset_x) as f32 - 100.0 * units).abs() <= 1.0,
        "shifted by the anchor: shaped {shaped:?} mono {mono:?}"
    );
    let mut verts = Vec::new();
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
    let drawn = draws_uv(&verts, shaped.uv).expect("the positioned mark draws");
    let cell_w = atlas.cell.width as f32;
    assert!(
        drawn.pos[0] >= 0.0 && drawn.end_pos[0] <= cell_w,
        "{drawn:?}"
    );
    assert!(
        draws_uv(&verts, mono.uv).is_none(),
        "the monochrome mark is not drawn a second time"
    );
}

#[test]
fn display_order_keeps_the_mark_on_its_base_visual_cell() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = font();
    let terminal = terminal(&format!("x {BEH}{FATHA}{LAM}{ALEF} c"));
    let snapshot = terminal.snapshot();
    let wrapped: Vec<bool> = terminal
        .visible_search_rows(0)
        .iter()
        .map(|row| row.wrapped)
        .collect();
    let map = crate::grid::BidiDisplayMap::plan(&snapshot, &wrapped);
    assert_eq!(
        (2..5).map(|c| map.visual_column(0, c)).collect::<Vec<_>>(),
        [4, 3, 2]
    );
    let runs = LigatureShaper::new().build_runs_bidi(&snapshot, &Fonts(font.clone()), &[], &map);
    let mut atlas = GlyphAtlas::build(&font, PX);
    for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
        atlas.ensure_shaped(&font, glyph.key);
    }
    let mark_key = runs
        .iter()
        .flat_map(|run| run.glyphs.iter())
        .find(|glyph| glyph.key.mark_offset == TOP_OFFSET)
        .unwrap_or_else(|| panic!("positioned mark in the right-to-left run: {runs:?}"))
        .key;
    let shaped = atlas.shaped_glyph_quad(mark_key).expect("mark ink");
    let mut verts = Vec::new();
    build_cell_vertices_with_bidi_into(&mut verts, &snapshot, &atlas, &[], &runs, &map);
    let drawn = draws_uv(&verts, shaped.uv).expect("the mark draws");
    let cell_w = atlas.cell.width as f32;
    assert!(
        drawn.pos[0] >= 4.0 * cell_w && drawn.end_pos[0] <= 5.0 * cell_w,
        "beh draws at visual column 4 and its mark rides it: {drawn:?}"
    );
}

#[test]
fn a_marked_letter_whose_form_does_not_change_still_positions_its_mark() {
    // Alef never joins forward, so alef and beh both keep their isolated
    // (cmap) glyphs here; only the mark makes the overlay.
    let (_, runs) = runs_for(&format!("{ALEF}{FATHA}{BEH}"));
    let fatha = font().glyph_id(FATHA).0;
    let marked: Vec<_> = runs
        .iter()
        .flat_map(|run| {
            run.glyphs.iter().map(move |glyph| {
                (
                    run.start + usize::from(glyph.key.anchor_cell),
                    glyph.key.glyph_id,
                    glyph.key.mark_offset,
                )
            })
        })
        .filter(|&(column, _, _)| column == 0)
        .collect();
    assert_eq!(
        marked,
        [(0, font().glyph_id(ALEF).0, [0, 0]), (0, fatha, TOP_OFFSET)],
        "{runs:?}"
    );
    assert!(
        runs.iter().all(|run| !run.covers(0, 1)),
        "the unmarked, unchanged beh keeps the per-cell path: {runs:?}"
    );
}

#[test]
fn an_attached_mark_with_a_cell_advance_does_not_push_later_marks() {
    // Shadda attaches to beh and carries a full-cell font advance; kasra
    // after it is unattached and must still be drawn from the base advance.
    let (_, runs) = runs_for(&format!("{BEH}{SHADDA}{KASRA}{LAM}{BEH}"));
    let shadda = font().glyph_id(SHADDA).0;
    let kasra = font().glyph_id(KASRA).0;
    let offsets: Vec<_> = runs
        .iter()
        .flat_map(|run| run.glyphs.iter())
        .filter(|glyph| glyph.key.mark_offset != [0, 0])
        .map(|glyph| (glyph.key.glyph_id, glyph.key.mark_offset))
        .collect();
    assert_eq!(offsets, [(shadda, TOP_OFFSET), (kasra, [600, 0])]);
}
