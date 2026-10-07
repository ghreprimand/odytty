// SPDX-License-Identifier: GPL-3.0-only
//! Shaping-run, overlay, and atlas regressions for [`crate::ligature`].

use super::*;
use crate::atlas::GlyphAtlas;
use crate::core::{CursorStyle, Terminal};
use crate::grid::{
    BackgroundTreatmentParams, ChromePin, CursorRenderParams, append_cursor_vertices_with_origin,
    build_cell_vertices_with_focus_dim_and_origin_into,
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into,
    build_cell_vertices_with_ligatures_and_selection_into,
};
use crate::selection::{CellPoint, SelectionRange, SelectionStyle, apply_highlight, selected_text};
use crate::text;

struct Fonts(FontHandle);

impl LigatureFonts for Fonts {
    fn ligature_font(&self, _style: FontStyle) -> &FontHandle {
        &self.0
    }
}

fn snapshot(text: &str) -> Snapshot {
    let mut terminal = Terminal::new(16, 2);
    terminal.advance(text.as_bytes());
    terminal.snapshot()
}

fn glyph_geometry(vertices: &[crate::grid::Vertex]) -> Vec<([f32; 2], [f32; 2])> {
    vertices
        .iter()
        .filter(|vertex| vertex.is_glyph == 1.0)
        .map(|vertex| (vertex.pos, vertex.uv))
        .collect()
}

#[test]
fn disabled_path_is_empty_and_does_not_shape() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(false, &snapshot("a->b"), &fonts, &[]);
    assert!(runs.is_empty());
    assert_eq!(shaper.shape_calls(), 0);
    assert_eq!(shaper.cached_rows(), 0);
}

#[test]
fn contextual_run_preserves_source_columns() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snapshot("a->b"), &fonts, &[]);
    let run = runs.iter().find(|run| run.start == 1).expect("arrow run");
    assert_eq!((run.start, run.end), (1, 3));
    assert!(run.glyphs.iter().all(|glyph| glyph.key.span_cells == 2));
    assert!(run.glyphs.iter().all(|glyph| glyph.key.anchor_cell < 2));
}

#[test]
fn unchanged_rows_hit_cache_and_one_edit_misses_once() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let first = snapshot("a->b");
    let _ = shaper.build_runs(true, &first, &fonts, &[]);
    let warm = shaper.shape_calls();
    let _ = shaper.build_runs(true, &first, &fonts, &[]);
    assert_eq!(shaper.shape_calls(), warm);
    let edited = snapshot("a=>b");
    let _ = shaper.build_runs(true, &edited, &fonts, &[]);
    assert_eq!(shaper.shape_calls(), warm + 1);
}

#[test]
fn style_change_is_an_exact_cache_miss() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let _ = shaper.build_runs(true, &snapshot("->"), &fonts, &[]);
    let warm = shaper.shape_calls();
    let bold = shaper.build_runs(true, &snapshot("\x1b[1m->"), &fonts, &[]);
    assert_eq!(shaper.shape_calls(), warm + 1);
    assert!(
        bold.iter()
            .flat_map(|run| run.glyphs.iter())
            .all(|glyph| glyph.key.style == FontStyle::Bold)
    );
}

#[test]
fn synchronized_output_hold_keeps_the_released_row_plan() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let held = snapshot("a->b");
    let pending = snapshot("a=>b");
    let released_runs = shaper.build_runs(true, &held, &fonts, &[]);
    let warm = shaper.shape_calls();

    // A synchronized-output hold keeps presenting its prior snapshot. No
    // intermediate model state reaches the renderer or the row cache.
    let held_runs = shaper.build_runs(true, &held, &fonts, &[]);
    assert_eq!(held_runs, released_runs);
    assert_eq!(shaper.shape_calls(), warm);

    let pending_runs = shaper.build_runs(true, &pending, &fonts, &[]);
    assert_ne!(pending_runs, released_runs);
    assert_eq!(shaper.shape_calls(), warm + 1);
}

#[test]
fn cache_is_bounded_fifo() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    for index in 0..(LIGATURE_ROW_CACHE_CAPACITY + 16) {
        let row = format!("v{index:04}->x");
        let _ = shaper.build_runs(true, &snapshot(&row), &fonts, &[]);
    }
    assert_eq!(shaper.cached_rows(), LIGATURE_ROW_CACHE_CAPACITY);
}

#[test]
fn wide_cells_and_combining_marks_split_eligible_runs() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let snap = snapshot("a->界=>b");
    let wide = snap.cells.iter().position(|cell| cell.ch == '界').unwrap();
    assert!(snap.cells[wide + 1].wide_continuation);
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    assert!(runs.iter().all(|run| !(run.start..run.end).contains(&wide)));
    assert!(
        runs.iter()
            .all(|run| !(run.start..run.end).contains(&(wide + 1)))
    );
}

#[test]
fn run_text_maps_multibyte_grapheme_bytes_to_columns() {
    // Infrastructure proof: UTF-8 grapheme spans must not be confused with
    // column indices. A wide CJK base is three UTF-8 bytes; the mapper still
    // reports the cell index, which is what calt span detection consumes.
    let cells = [
        Cell::new('a', Default::default()),
        Cell::new('界', Default::default()),
        Cell::new('b', Default::default()),
    ];
    let run = RunText::from_cells(&cells);
    assert_eq!(run.text, "a界b");
    assert_eq!(run.cell_bytes.len(), 3);
    assert_eq!(run.cell_bytes[0], 0..1);
    assert_eq!(run.cell_bytes[1].len(), '界'.len_utf8());
    assert_eq!(run.column_at_byte(0), Some(0));
    assert_eq!(run.column_at_byte(run.cell_bytes[1].start), Some(1));
    assert_eq!(run.column_at_byte(run.cell_bytes[1].start + 1), Some(1));
    assert_eq!(run.column_at_byte(run.cell_bytes[2].start), Some(2));
    assert_eq!(run.column_at_byte(run.text.len()), None);
}

#[test]
fn combining_mark_cells_are_not_merged_into_compatible_runs() {
    let mut terminal = Terminal::new(8, 1);
    // 'a', combining acute, then '->' which would ligate if merged across.
    terminal.advance("a\u{0301}->".as_bytes());
    let snap = terminal.snapshot();
    assert_eq!(snap.cells[0].ch, 'a');
    assert_eq!(snap.cells[0].combining(), ['\u{0301}']);
    let coverage = ColorRunCoverage::new(&[], snap.dimensions.columns, snap.dimensions.rows);
    let bounds = compatible_run_bounds(&snap.cells[..snap.dimensions.columns], 0, &coverage);
    // Combining cell is skipped; the arrow forms its own run starting at
    // the first ASCII graphic after the marked base.
    assert!(
        bounds
            .iter()
            .all(|&(start, end, _)| !(start..end).contains(&0)),
        "combining-marked base must not join a shaping run: {bounds:?}"
    );
    assert!(
        bounds.iter().any(|&(start, end, _)| start == 1 && end == 3),
        "arrow after combining cell must still form a run: {bounds:?}"
    );
}

#[test]
fn mixed_font_styles_do_not_merge_compatible_runs() {
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"->\x1b[1m=>");
    let snap = terminal.snapshot();
    let coverage = ColorRunCoverage::new(&[], snap.dimensions.columns, snap.dimensions.rows);
    let bounds = compatible_run_bounds(&snap.cells[..snap.dimensions.columns], 0, &coverage);
    assert!(
        bounds
            .iter()
            .any(|&(start, end, style)| { start == 0 && end == 2 && style == FontStyle::Regular }),
        "regular arrow must be its own run: {bounds:?}"
    );
    assert!(
        bounds
            .iter()
            .any(|&(start, end, style)| start == 2 && end == 4 && style == FontStyle::Bold),
        "bold arrow must be its own run: {bounds:?}"
    );
}

#[test]
fn color_glyph_coverage_breaks_compatible_runs_like_zwj_emoji() {
    // ZWJ / color emoji occupy the color-glyph path. A coverage bit on a
    // cell must split shaping the same way a wide or combining cell does.
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let snap = snapshot("a->b");
    let key =
        crate::emoji::ColorGlyphKey::new(0, crate::emoji::ColorGlyphId::Glyph(1), 16.0, 1.0, 1);
    // Cover the '>' so the '->' ligature cannot form across the emoji cell.
    let color_runs = [ColorGlyphRun::new(0, 2, key)];
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snap, &fonts, &color_runs);
    assert!(
        runs.iter().all(|run| !run.covers(0, 2)),
        "color-covered cell must not join a shaping overlay: {runs:?}"
    );
    assert!(
        runs.iter()
            .all(|run| !(run.start..run.end).contains(&1) || run.end <= 2),
        "arrow must not ligate into a color-covered cell: {runs:?}"
    );
}

#[test]
fn plain_ascii_without_ligatures_is_byte_identical_to_scalar_path() {
    // Differential: enabled shaping on a row with no calt substitutions
    // must emit the same cell vertices as the scalar builder.
    let _guard = crate::test_lock::render_globals_lock();
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let atlas_font = text::load_bundled_font().expect("bundled font");
    let snap = snapshot("hello");
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    assert!(
        runs.is_empty(),
        "plain ASCII without calt hits must produce no overlays: {runs:?}"
    );
    let atlas = GlyphAtlas::build(&atlas_font, 24.0);

    let mut legacy = Vec::new();
    build_cell_vertices_with_focus_dim_and_origin_into(
        &mut legacy,
        &snap,
        &atlas,
        &[],
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        1.0,
        None,
        ChromePin::NONE,
    );
    let mut shaped = Vec::new();
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut shaped,
        &snap,
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
    assert_eq!(shaped, legacy);
}

#[test]
fn allowlisted_operators_join_compatible_runs_with_ascii() {
    let snap = snapshot("a→b≠c");
    let coverage = ColorRunCoverage::new(&[], snap.dimensions.columns, snap.dimensions.rows);
    let bounds = compatible_run_bounds(&snap.cells[..snap.dimensions.columns], 0, &coverage);
    // a → b ≠ c are five eligible cells in one run (all allowlisted/ASCII).
    assert!(
        bounds.iter().any(|&(start, end, _)| start == 0 && end >= 5),
        "allowlisted operators must merge with neighboring ASCII: {bounds:?}"
    );
    assert!(is_allowlisted_operator('\u{2192}'));
    assert!(is_allowlisted_operator('\u{2260}'));
    assert!(!is_allowlisted_operator('界'));
    assert!(!is_allowlisted_operator(crate::core::PLACEHOLDER_CHAR));
}

#[test]
fn allowlisted_run_preserves_ascii_ligature_span() {
    // Mixing an allowlisted operator after an ASCII ligature must not break
    // the `->` substitution or shift its source columns.
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snapshot("a->≠"), &fonts, &[]);
    let run = runs.iter().find(|run| run.start == 1).expect("arrow run");
    assert_eq!((run.start, run.end), (1, 3));
    assert!(run.glyphs.iter().all(|glyph| glyph.key.span_cells == 2));
}

#[test]
fn latin_feature_toggle_clears_row_cache() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let mut shaper = LigatureShaper::new();
    let snap = snapshot("a->b");
    let _ = shaper.build_runs(true, &snap, &fonts, &[]);
    let after_default = shaper.cached_rows();
    assert!(after_default > 0);
    let calls_after_default = shaper.shape_calls();
    let _ = shaper.build_runs_with_features(
        true,
        &snap,
        &fonts,
        &[],
        LatinShapingFeatures {
            ss01: true,
            ..LatinShapingFeatures::default()
        },
    );
    // Feature change must not reuse plans shaped under ss01=off.
    assert!(
        shaper.shape_calls() > calls_after_default,
        "ss01 toggle must reshape rather than reuse the prior cache"
    );
    let warm = shaper.shape_calls();
    let _ = shaper.build_runs_with_features(
        true,
        &snap,
        &fonts,
        &[],
        LatinShapingFeatures {
            ss01: true,
            ..LatinShapingFeatures::default()
        },
    );
    assert_eq!(shaper.shape_calls(), warm, "same features must hit cache");
}

#[test]
fn default_latin_path_enables_liga_alongside_calt() {
    // Structural: the on-tags for default Latin features include both calt
    // and liga at 1; ss01/ss02 stay 0. Glyph-level liga hits are font-
    // dependent and are not required for this invariant.
    let features = LatinShapingFeatures::default();
    assert_eq!(
        features.on_tags(),
        [
            ("calt", 1),
            ("liga", 1),
            ("ss01", 0),
            ("ss02", 0),
            ("zero", 0)
        ]
    );
    assert_eq!(
        features.off_tags(),
        [
            ("calt", 0),
            ("liga", 0),
            ("ss01", 0),
            ("ss02", 0),
            ("zero", 0)
        ]
    );
    let with_sets = LatinShapingFeatures {
        ss01: true,
        ss02: true,
        zero: false,
    };
    assert_eq!(
        with_sets.on_tags(),
        [
            ("calt", 1),
            ("liga", 1),
            ("ss01", 1),
            ("ss02", 1),
            ("zero", 0)
        ]
    );
}

#[test]
fn latin_length_changing_liga_stays_clipped_to_its_source_cells() {
    let fonts = Fonts(
        FontHandle::try_from_vec(
            include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec(),
        )
        .expect("length-changing fixture"),
    );
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snapshot("->"), &fonts, &[]);
    let run = runs.first().expect("fixture defines an arrow liga");
    assert_eq!((run.start, run.end), (0, 2));
    assert_eq!(run.glyphs.len(), 1, "length-changing substitution");
    assert!(run.glyphs.iter().all(|glyph| glyph.key.span_cells == 2));
}

fn arabic_fixture_font() -> FontHandle {
    FontHandle::try_from_vec(include_bytes!("../../tests/fixtures/fonts/arabic-marks.ttf").to_vec())
        .expect("project-authored Arabic fixture")
}

#[test]
fn arabic_joining_bases_form_compatible_runs_separate_from_latin() {
    let snap = snapshot("abكتابxy");
    let coverage = ColorRunCoverage::new(&[], snap.dimensions.columns, snap.dimensions.rows);
    let bounds = compatible_run_bounds(&snap.cells[..snap.dimensions.columns], 0, &coverage);
    assert!(
        bounds.iter().any(|&(start, end, _)| start == 0 && end == 2),
        "Latin prefix must be its own run: {bounds:?}"
    );
    assert!(
        bounds.iter().any(|&(start, end, _)| start == 2 && end == 6),
        "Arabic word must be one joining run: {bounds:?}"
    );
    assert!(
        bounds.iter().any(|&(start, end, _)| start == 6 && end == 8),
        "Latin suffix must be its own run: {bounds:?}"
    );
    assert!(is_arabic_joining_base('ك'));
    assert!(is_arabic_joining_base('ا'));
    assert!(is_arabic_joining_base('\u{0640}')); // tatweel
    assert!(!is_arabic_joining_base('a'));
    assert!(!is_arabic_joining_base('٠')); // Arabic-Indic digit: non-joining
}

#[test]
fn arabic_joining_forms_overlay_preserves_logical_columns() {
    let font = arabic_fixture_font();
    let label = "project-authored Arabic fixture";
    let fonts = Fonts(font);
    let mut shaper = LigatureShaper::new();
    // Project-authored sequence with joining forms and a lam-alef cluster.
    let word = "\u{0628}\u{0628}\u{0644}\u{0627}";
    let snap = snapshot(word);
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    let run = runs
        .iter()
        .find(|run| run.start == 0 && run.end > run.start)
        .unwrap_or_else(|| {
            panic!("expected joining overlay for {word:?} with {label}; runs={runs:?}")
        });
    assert!(
        !run.glyphs.is_empty(),
        "{label} must emit shaped glyphs for {word}"
    );
    // Overlay covers the joined prefix in logical LTR cell order. Trailing
    // letters whose cmap id already matches the joined form need no span.
    assert_eq!(run.start, 0);
    assert!(
        run.end >= 2,
        "joining must cover at least a multi-cell span: {run:?}"
    );
    // Selection/copy still report the logical characters across the word.
    let range = SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint {
            row: 0,
            column: word.chars().count() - 1,
        },
    };
    assert_eq!(selected_text(&snap, range), word);
    assert_eq!(
        snap.cells[..word.chars().count()]
            .iter()
            .map(|c| c.ch)
            .collect::<String>(),
        word
    );
}

#[test]
fn arabic_lam_alef_length_changing_ligature_still_overlays() {
    let font = arabic_fixture_font();
    let label = "project-authored Arabic fixture";
    let fonts = Fonts(font);
    let mut shaper = LigatureShaper::new();
    // Lam-alef typically collapses two cells to one glyph under Arabic shaping.
    let snap = snapshot("لا");
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    let run = runs
        .iter()
        .find(|run| run.start == 0 && run.end == 2)
        .unwrap_or_else(|| panic!("expected lam-alef overlay with {label}; runs={runs:?}"));
    assert!(
        !run.glyphs.is_empty(),
        "{label}: lam-alef must still produce an overlay when glyph count ≠ cell count"
    );
    assert!(
        run.glyphs.iter().all(|g| g.key.span_cells == 2),
        "lam-alef glyphs must clip to the two-cell span"
    );
}

#[test]
fn arabic_harakat_ride_their_base_into_the_joining_run() {
    let mut terminal = Terminal::new(8, 1);
    // Kaf + fatha combining mark, then more Arabic letters.
    terminal.advance("ك\u{064E}تاب".as_bytes());
    let snap = terminal.snapshot();
    assert_eq!(snap.cells[0].ch, 'ك');
    assert_eq!(snap.cells[0].combining(), ['\u{064E}']);
    let coverage = ColorRunCoverage::new(&[], snap.dimensions.columns, snap.dimensions.rows);
    let bounds = compatible_run_bounds(&snap.cells[..snap.dimensions.columns], 0, &coverage);
    assert_eq!(
        bounds.first().map(|&(start, end, _)| (start, end)),
        Some((0, 4)),
        "a harakat-bearing base joins the whole word: {bounds:?}"
    );
}

#[test]
fn disabled_renderer_is_byte_identical_and_allocates_nothing() {
    // Cell vertices carry resolved COLOR, not just geometry: the render
    // path floors every foreground through the process-global
    // minimum-contrast seam and resolves default/indexed colors through the
    // process-global palette. The two builds below happen at different
    // moments and are compared byte-for-byte, so a floor or palette change
    // landing between them diverges the buffers. Hold the shared
    // render-globals guard across both builds.
    let _guard = crate::test_lock::render_globals_lock();
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let atlas_font = text::load_bundled_font().expect("bundled font");
    let snap = snapshot("a->b");
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(false, &snap, &fonts, &[]);
    let atlas = GlyphAtlas::build(&atlas_font, 24.0);

    let mut legacy = Vec::new();
    build_cell_vertices_with_focus_dim_and_origin_into(
        &mut legacy,
        &snap,
        &atlas,
        &[],
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        // TEXT-BRIGHTNESS identity.
        1.0,
        None,
        ChromePin::NONE,
    );
    let mut ligature_aware = Vec::new();
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut ligature_aware,
        &snap,
        &atlas,
        &[],
        &runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        // TEXT-BRIGHTNESS identity.
        1.0,
        None,
        ChromePin::NONE,
    );

    assert_eq!(ligature_aware, legacy);
    assert_eq!(shaper.shape_calls(), 0);
    assert_eq!(shaper.cached_rows(), 0);
    assert_eq!(atlas.shaped_slot_count(), 0);
}

#[test]
fn shaped_atlas_reuses_slots_and_clips_ink_to_source_span() {
    let _guard = crate::test_lock::render_globals_lock();
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let atlas_font = text::load_bundled_font().expect("bundled font");
    let snap = snapshot("->");
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    let run = runs.first().expect("arrow ligature");
    assert_eq!((run.start, run.end), (0, 2));
    let mut atlas = GlyphAtlas::build(&atlas_font, 24.0);
    for glyph in run.glyphs.iter() {
        let _ = atlas.ensure_shaped(&atlas_font, glyph.key);
    }
    let slots = atlas.shaped_slot_count();
    let pixels = atlas.data.clone();
    for glyph in run.glyphs.iter() {
        let _ = atlas.ensure_shaped(&atlas_font, glyph.key);
    }
    assert_eq!(atlas.shaped_slot_count(), slots);
    assert_eq!(atlas.data, pixels);

    let mut vertices = Vec::new();
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut vertices,
        &snap,
        &atlas,
        &[],
        &runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        // TEXT-BRIGHTNESS identity.
        1.0,
        None,
        ChromePin::NONE,
    );
    let glyph_vertices = vertices.iter().filter(|vertex| vertex.is_glyph == 1.0);
    let mut saw_ink = false;
    for vertex in glyph_vertices {
        saw_ink = true;
        assert!(vertex.pos[0] >= 0.0);
        assert!(vertex.pos[0] <= 2.0 * atlas.cell.width as f32);
        assert!(vertex.pos[1] >= 0.0);
        assert!(vertex.pos[1] <= atlas.cell.height as f32);
        assert!(vertex.end_pos[0] >= 0.0 && vertex.end_pos[0] <= 2.0 * atlas.cell.width as f32);
        assert!(vertex.end_pos[1] >= 0.0 && vertex.end_pos[1] <= atlas.cell.height as f32);
    }
    assert!(saw_ink);
}

#[test]
fn unavailable_contextual_slots_fall_back_to_scalar_geometry() {
    // Cell vertices carry resolved COLOR, not just geometry: the render
    // path floors every foreground through the process-global
    // minimum-contrast seam and resolves default/indexed colors through the
    // process-global palette. The two builds below happen at different
    // moments and are compared byte-for-byte, so a floor or palette change
    // landing between them diverges the buffers. Hold the shared
    // render-globals guard across both builds.
    let _guard = crate::test_lock::render_globals_lock();
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let atlas_font = text::load_bundled_font().expect("bundled font");
    let snap = snapshot("->");
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    assert!(!runs.is_empty());
    let atlas = GlyphAtlas::build(&atlas_font, 24.0);
    assert_eq!(atlas.shaped_slot_count(), 0);

    let mut legacy = Vec::new();
    build_cell_vertices_with_focus_dim_and_origin_into(
        &mut legacy,
        &snap,
        &atlas,
        &[],
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        // TEXT-BRIGHTNESS identity.
        1.0,
        None,
        ChromePin::NONE,
    );
    let mut fallback = Vec::new();
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut fallback,
        &snap,
        &atlas,
        &[],
        &runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        // TEXT-BRIGHTNESS identity.
        1.0,
        None,
        ChromePin::NONE,
    );
    assert_eq!(fallback, legacy);
}

#[test]
fn logical_selection_and_cursor_shapes_remain_cell_owned() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let atlas_font = text::load_bundled_font().expect("bundled font");
    let mut terminal = Terminal::new(4, 1);
    terminal.advance(b"->\r\x1b[1C");
    let snap = terminal.snapshot();
    let range = SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 1 },
    };
    assert_eq!(selected_text(&snap, range), "->");
    assert_eq!(snap.cells[0].ch, '-');
    assert_eq!(snap.cells[1].ch, '>');

    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    let mut atlas = GlyphAtlas::build(&atlas_font, 24.0);
    for run in &runs {
        for glyph in run.glyphs.iter() {
            let _ = atlas.ensure_shaped(&atlas_font, glyph.key);
        }
    }
    let mut cells = Vec::new();
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut cells,
        &snap,
        &atlas,
        &[],
        &runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        // TEXT-BRIGHTNESS identity.
        1.0,
        None,
        ChromePin::NONE,
    );
    for style in [CursorStyle::Block, CursorStyle::Bar, CursorStyle::Underline] {
        let mut with_cursor = cells.clone();
        append_cursor_vertices_with_origin(
            &mut with_cursor,
            &snap,
            &atlas,
            style,
            [0.0, 0.0],
            CursorRenderParams::default(),
        );
        assert!(with_cursor.len() > cells.len(), "cursor style {style:?}");
    }
}

#[test]
fn every_selection_boundary_preserves_two_and_three_cell_ligature_geometry() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let atlas_font = text::load_bundled_font().expect("bundled font");
    let themed = SelectionStyle {
        fill: [0x24, 0x33, 0x52],
        fg: [0xEA, 0xEE, 0xF4],
    };

    for (text, span) in [("!=", 2usize), ("!==", 3usize)] {
        let base = snapshot(text);
        let mut shaper = LigatureShaper::new();
        let base_runs = shaper.build_runs(true, &base, &fonts, &[]);
        let base_run = base_runs
            .iter()
            .find(|run| run.start == 0 && run.end == span)
            .unwrap_or_else(|| panic!("bundled font must expose the {span}-cell {text} ligature"));
        let expected_glyphs = base_run.glyphs.clone();

        let mut atlas = GlyphAtlas::build(&atlas_font, 24.0);
        for glyph in expected_glyphs.iter() {
            let _ = atlas.ensure_shaped(&atlas_font, glyph.key);
        }
        let cell_w = atlas.cell.width as f32;
        let cell_h = atlas.cell.height as f32;

        // All contiguous partial/full cell selections cover every possible
        // start and end boundary through the contextual source span.
        for start in 0..span {
            for end in start..span {
                for selection_style in [None, Some(themed)] {
                    let mut selected = base.clone();
                    apply_highlight(
                        &mut selected,
                        SelectionRange {
                            start: CellPoint {
                                row: 0,
                                column: start,
                            },
                            end: CellPoint {
                                row: 0,
                                column: end,
                            },
                        },
                        selection_style,
                    );
                    let selected_runs = shaper.build_runs(true, &selected, &fonts, &[]);
                    let selected_run = selected_runs
                        .iter()
                        .find(|run| run.start == 0 && run.end == span)
                        .unwrap_or_else(|| {
                            panic!(
                                "selection {start}..={end} must preserve the {span}-cell {text} run"
                            )
                        });
                    assert_eq!(
                        selected_run.glyphs, expected_glyphs,
                        "selection {start}..={end} must not change contextual glyph ids"
                    );

                    // Origin zero is the single-pane Full path; a translated
                    // origin exercises the same builder used per pane in the
                    // multi-pane Full path. Both opaque and translucent cell
                    // backgrounds cover the selection compositing inputs.
                    for (origin, opacity) in
                        [([0.0, 0.0], 1.0), ([4.0 * cell_w, 2.0 * cell_h], 0.43)]
                    {
                        let mut base_vertices = Vec::new();
                        build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
                            &mut base_vertices,
                            &base,
                            &atlas,
                            &[],
                            &base_runs,
                            0.0,
                            origin,
                            BackgroundTreatmentParams::default(),
                            opacity,
                            // TEXT-BRIGHTNESS identity.
                            1.0,
                            None,
                            ChromePin::NONE,
                        );
                        // SELECTION-OPACITY: route the selected build through
                        // the selection-aware entry, passing `opacity` as the
                        // selection strength too. Unselected cells composite
                        // at `opacity`; a selected cell's surface alpha lerps
                        // UP to `A_sel = opacity + opacity*(1 - opacity)` so
                        // the selection is never weaker than its surround
                        // (the marker otherwise forces the fully-opaque
                        // default on the legacy entry point).
                        let mut selected_vertices = Vec::new();
                        build_cell_vertices_with_ligatures_and_selection_into(
                            &mut selected_vertices,
                            &selected,
                            &atlas,
                            &[],
                            &selected_runs,
                            0.0,
                            origin,
                            BackgroundTreatmentParams::default(),
                            opacity,
                            // COLORED-BG-FLOOR inert: equal alphas.
                            opacity,
                            // TEXT-BRIGHTNESS identity.
                            1.0,
                            None,
                            ChromePin::NONE,
                            opacity,
                            None,
                        );
                        assert_eq!(
                            glyph_geometry(&selected_vertices),
                            glyph_geometry(&base_vertices),
                            "selection {start}..={end} changed {text} outline/source geometry"
                        );
                        let a_sel = opacity + opacity * (1.0 - opacity);
                        assert!(
                            selected_vertices
                                .iter()
                                .filter(|vertex| vertex.is_glyph == 0.0)
                                .all(|vertex| (vertex.color[3] - opacity).abs() < 1e-6
                                    || (vertex.color[3] - a_sel).abs() < 1e-6),
                            "selection backgrounds are either the content opacity (unselected) \
                             or the lerped A_sel (selected), never weaker than the surround"
                        );
                        assert!(
                            selected_vertices
                                .iter()
                                .filter(|vertex| vertex.is_glyph == 1.0)
                                .all(|vertex| (vertex.color[3] - 1.0).abs() < 1e-6),
                            "selection must not attenuate contextual glyph coverage"
                        );

                        // CursorOnly rebuilds append to the cached Full cell
                        // segment. Prove every boundary leaves that segment
                        // byte-identical while the cursor layer changes.
                        let cached_cells = selected_vertices.clone();
                        append_cursor_vertices_with_origin(
                            &mut selected_vertices,
                            &selected,
                            &atlas,
                            CursorStyle::Block,
                            origin,
                            CursorRenderParams::default(),
                        );
                        assert_eq!(
                            &selected_vertices[..cached_cells.len()],
                            cached_cells.as_slice()
                        );
                        assert!(selected_vertices.len() > cached_cells.len());
                    }
                }
            }
        }
    }
}

#[test]
fn pane_origin_translates_ligature_without_changing_shape_plan() {
    let fonts = Fonts(text::load_bundled_font().expect("bundled font"));
    let atlas_font = text::load_bundled_font().expect("bundled font");
    let snap = snapshot("->");
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    let warm_calls = shaper.shape_calls();
    let repeated = shaper.build_runs(true, &snap, &fonts, &[]);
    assert_eq!(runs, repeated);
    assert_eq!(shaper.shape_calls(), warm_calls);

    let mut atlas = GlyphAtlas::build(&atlas_font, 24.0);
    for run in &runs {
        for glyph in run.glyphs.iter() {
            let _ = atlas.ensure_shaped(&atlas_font, glyph.key);
        }
    }
    let dx = 7.0 * atlas.cell.width as f32;
    let mut left = Vec::new();
    let mut right = Vec::new();
    for (out, origin) in [(&mut left, [0.0, 0.0]), (&mut right, [dx, 0.0])] {
        build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
            out,
            &snap,
            &atlas,
            &[],
            &runs,
            0.0,
            origin,
            BackgroundTreatmentParams::default(),
            1.0,
            // TEXT-BRIGHTNESS identity.
            1.0,
            None,
            ChromePin::NONE,
        );
    }
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(&right) {
        assert!((right.pos[0] - left.pos[0] - dx).abs() < 1e-5);
        assert_eq!(right.pos[1], left.pos[1]);
        assert_eq!(right.uv, left.uv);
    }
}

#[path = "cache_face_tests.rs"]
mod cache_face_tests;
