// SPDX-License-Identifier: GPL-3.0-only
//! Presentation-only programming-ligature shaping.
//!
//! The terminal model remains one logical character per grid cell. This module
//! shapes eligible ASCII runs with OpenType `calt`, records only contextual
//! substitutions, and anchors every shaped glyph to its source grid column.
//! Shaped advances never move terminal columns.

use std::collections::{HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ab_glyph::FontVec;
use swash::shape::{Direction, ShapeContext};
use swash::text::Script;
use swash::{FontRef, GlyphId};

use crate::atlas::{FontStyle, ShapedGlyphKey};
use crate::core::{Cell, Snapshot};
use crate::grid::{ColorGlyphRun, ColorRunCoverage, font_style_for_attrs};

/// Maximum number of exact row plans retained by the live renderer.
pub const LIGATURE_ROW_CACHE_CAPACITY: usize = 512;

/// Font access needed by the shaper without coupling it to the native GPU type.
pub trait LigatureFonts {
    fn ligature_font(&self, style: FontStyle) -> &FontVec;
}

/// One contextual glyph whose atlas slot is anchored to a source-column span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LigatureGlyph {
    pub key: ShapedGlyphKey,
}

/// A substituted source-cell span. Scalar glyphs inside the span are suppressed
/// and replaced by the shaped glyphs, while backgrounds and decorations remain
/// cell-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigatureRun {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub glyphs: Arc<[LigatureGlyph]>,
}

impl LigatureRun {
    pub fn covers(&self, row: usize, column: usize) -> bool {
        self.row == row && (self.start..self.end).contains(&column)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RelativeRun {
    start: usize,
    end: usize,
    glyphs: Arc<[LigatureGlyph]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowKey {
    cells: Vec<Cell>,
    color_glyphs: Vec<bool>,
}

impl RowKey {
    fn matches(&self, cells: &[Cell], row: usize, coverage: &ColorRunCoverage) -> bool {
        if self.cells != cells {
            return false;
        }
        if coverage.is_empty() {
            return self.color_glyphs.is_empty();
        }
        self.color_glyphs.len() == cells.len()
            && self
                .color_glyphs
                .iter()
                .enumerate()
                .all(|(column, color_glyph)| *color_glyph == coverage.covers(row, column))
    }
}

#[derive(Clone, Debug)]
struct RowPlan {
    runs: Vec<RelativeRun>,
}

type RowBucket = Vec<(Arc<RowKey>, Arc<RowPlan>)>;

#[derive(Clone, Copy, Debug)]
struct ShapedGlyph {
    id: GlyphId,
    source_start: usize,
}

/// Deterministic FIFO row-plan cache plus the reusable swash shaping context.
pub struct LigatureShaper {
    context: ShapeContext,
    entries: HashMap<u64, RowBucket>,
    fifo: VecDeque<(u64, Arc<RowKey>)>,
    entry_count: usize,
    face_fingerprints: [Option<u64>; 4],
    shape_calls: u64,
}

impl Default for LigatureShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl LigatureShaper {
    pub fn new() -> Self {
        Self {
            context: ShapeContext::new(),
            entries: HashMap::new(),
            fifo: VecDeque::new(),
            entry_count: 0,
            face_fingerprints: [None; 4],
            shape_calls: 0,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.fifo.clear();
        self.entry_count = 0;
        self.face_fingerprints = [None; 4];
    }

    pub fn cached_rows(&self) -> usize {
        self.entry_count
    }

    pub fn shape_calls(&self) -> u64 {
        self.shape_calls
    }

    /// Build presentation runs for a snapshot. When disabled, this returns
    /// immediately without constructing row keys or touching swash.
    pub fn build_runs<F: LigatureFonts>(
        &mut self,
        enabled: bool,
        snapshot: &Snapshot,
        fonts: &F,
        color_runs: &[ColorGlyphRun],
    ) -> Vec<LigatureRun> {
        if !enabled {
            return Vec::new();
        }
        let cols = snapshot.dimensions.columns;
        // One O(cells / 64 + runs) coverage mask serves the fingerprint,
        // cache-key comparison, and eligibility passes for every row, instead
        // of each of those scanning the whole run list per cell.
        let coverage = ColorRunCoverage::new(color_runs, cols, snapshot.dimensions.rows);
        let mut output = Vec::new();
        for (row, cells) in snapshot.cells.chunks(cols).enumerate() {
            let fingerprint = row_fingerprint(cells, row, &coverage);
            let cached = self.entries.get(&fingerprint).and_then(|bucket| {
                bucket
                    .iter()
                    .find(|(key, _)| key.matches(cells, row, &coverage))
                    .map(|(_, plan)| Arc::clone(plan))
            });
            let plan = if let Some(plan) = cached {
                plan
            } else {
                self.shape_calls += 1;
                let plan = Arc::new(self.shape_row(cells, fonts, row, &coverage));
                if self.entry_count == LIGATURE_ROW_CACHE_CAPACITY
                    && let Some((oldest_fingerprint, oldest_key)) = self.fifo.pop_front()
                {
                    let remove_bucket =
                        if let Some(bucket) = self.entries.get_mut(&oldest_fingerprint) {
                            bucket.retain(|(key, _)| !Arc::ptr_eq(key, &oldest_key));
                            bucket.is_empty()
                        } else {
                            false
                        };
                    if remove_bucket {
                        self.entries.remove(&oldest_fingerprint);
                    }
                    self.entry_count -= 1;
                }
                let key = Arc::new(RowKey {
                    cells: cells.to_vec(),
                    color_glyphs: if coverage.is_empty() {
                        Vec::new()
                    } else {
                        cells
                            .iter()
                            .enumerate()
                            .map(|(column, _)| coverage.covers(row, column))
                            .collect()
                    },
                });
                self.fifo.push_back((fingerprint, Arc::clone(&key)));
                self.entries
                    .entry(fingerprint)
                    .or_default()
                    .push((key, Arc::clone(&plan)));
                self.entry_count += 1;
                plan
            };
            output.extend(plan.runs.iter().map(|run| LigatureRun {
                row,
                start: run.start,
                end: run.end,
                glyphs: run.glyphs.clone(),
            }));
        }
        output
    }

    fn shape_row<F: LigatureFonts>(
        &mut self,
        cells: &[Cell],
        fonts: &F,
        row: usize,
        coverage: &ColorRunCoverage,
    ) -> RowPlan {
        let mut runs = Vec::new();
        let mut start = 0;
        while start < cells.len() {
            if !eligible_cell(&cells[start], row, start, coverage) {
                start += 1;
                continue;
            }
            let style = font_style_for_attrs(&cells[start].attrs);
            let mut end = start + 1;
            while end < cells.len()
                && eligible_cell(&cells[end], row, end, coverage)
                // Selection and search treatments change foreground,
                // background, and inverse attributes cell by cell. Those are
                // compositing inputs, not shaping inputs: splitting here would
                // replace one contextual glyph with independently shaped
                // fragments as a highlight boundary crosses it. Only the font
                // face selected by bold/italic affects contextual shaping.
                && font_style_for_attrs(&cells[end].attrs) == style
            {
                end += 1;
            }
            if end - start >= 2 {
                let text: String = cells[start..end].iter().map(|cell| cell.ch).collect();
                runs.extend(self.shape_ascii_run(&text, start, style, fonts.ligature_font(style)));
            }
            start = end;
        }
        RowPlan { runs }
    }

    fn shape_ascii_run(
        &mut self,
        text: &str,
        column_start: usize,
        style: FontStyle,
        font: &FontVec,
    ) -> Vec<RelativeRun> {
        let Some(font_ref) = FontRef::from_index(font.as_slice(), 0) else {
            return Vec::new();
        };
        let off = shape(&mut self.context, font_ref, text, 0);
        let on = shape(&mut self.context, font_ref, text, 1);
        if off.len() != on.len() {
            return Vec::new();
        }
        let mut changed = off
            .iter()
            .zip(&on)
            .filter_map(|(plain, contextual)| {
                (plain.id != contextual.id).then_some(plain.source_start)
            })
            .collect::<Vec<_>>();
        changed.sort_unstable();
        changed.dedup();
        let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
        for column in changed {
            match spans.last_mut() {
                Some(span) if span.end == column => span.end += 1,
                _ => spans.push(column..column + 1),
            }
        }
        let fingerprint_slot = &mut self.face_fingerprints[font_style_index(style)];
        let face_fingerprint = match *fingerprint_slot {
            Some(fingerprint) => fingerprint,
            None => {
                let fingerprint = font_fingerprint(font);
                *fingerprint_slot = Some(fingerprint);
                fingerprint
            }
        };
        spans
            .into_iter()
            .filter_map(|span| {
                let span_cells = u8::try_from(span.len()).ok()?;
                let glyphs = on
                    .iter()
                    .filter(|glyph| span.contains(&glyph.source_start))
                    .filter_map(|glyph| {
                        let anchor_cell = u8::try_from(glyph.source_start - span.start).ok()?;
                        Some(LigatureGlyph {
                            key: ShapedGlyphKey {
                                face_fingerprint,
                                style,
                                glyph_id: glyph.id,
                                span_cells,
                                anchor_cell,
                            },
                        })
                    })
                    .collect::<Vec<_>>();
                (!glyphs.is_empty()).then_some(RelativeRun {
                    start: column_start + span.start,
                    end: column_start + span.end,
                    glyphs: glyphs.into(),
                })
            })
            .collect()
    }
}

fn font_style_index(style: FontStyle) -> usize {
    match style {
        FontStyle::Regular => 0,
        FontStyle::Bold => 1,
        FontStyle::Italic => 2,
        FontStyle::BoldItalic => 3,
    }
}

fn row_fingerprint(cells: &[Cell], row: usize, coverage: &ColorRunCoverage) -> u64 {
    // Candidate lookup only. `RowKey::matches` exactly verifies every cell and
    // color-glyph bit before accepting a cached plan, so collisions can cost a
    // bucket scan but can never reuse incorrect presentation data.
    let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64 ^ cells.len() as u64;
    if coverage.is_empty() {
        for cell in cells {
            let value = cell.ch as u64 | ((cell.wide_continuation as u64) << 32);
            fingerprint ^= value;
            fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
        }
    } else {
        for (column, cell) in cells.iter().enumerate() {
            let color_glyph = coverage.covers(row, column);
            let value = cell.ch as u64
                | ((cell.wide_continuation as u64) << 32)
                | ((color_glyph as u64) << 33);
            fingerprint ^= value;
            fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fingerprint
}

fn eligible_cell(cell: &Cell, row: usize, column: usize, coverage: &ColorRunCoverage) -> bool {
    cell.ch.is_ascii_graphic()
        && !cell.wide_continuation
        && cell.combining().is_empty()
        && !cell.attrs.hidden()
        && !coverage.covers(row, column)
}

fn shape(context: &mut ShapeContext, font: FontRef<'_>, text: &str, calt: u16) -> Vec<ShapedGlyph> {
    let mut shaper = context
        .builder(font)
        .script(Script::Latin)
        .direction(Direction::LeftToRight)
        .features([("calt", calt)])
        .build();
    shaper.add_str(text);
    let mut glyphs = Vec::new();
    shaper.shape_with(|cluster| {
        for glyph in cluster.glyphs {
            glyphs.push(ShapedGlyph {
                id: glyph.id,
                source_start: cluster.source.to_range().start,
            });
        }
    });
    glyphs
}

fn font_fingerprint(font: &FontVec) -> u64 {
    let mut hasher = DefaultHasher::new();
    font.as_slice().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::GlyphAtlas;
    use crate::core::{CursorStyle, Terminal};
    use crate::grid::{
        BackgroundTreatmentParams, ChromePin, CursorRenderParams,
        append_cursor_vertices_with_origin, build_cell_vertices_with_focus_dim_and_origin_into,
        build_cell_vertices_with_focus_dim_origin_and_ligatures_into,
        build_cell_vertices_with_ligatures_and_selection_into,
    };
    use crate::selection::{
        CellPoint, SelectionRange, SelectionStyle, apply_highlight, selected_text,
    };
    use crate::text;

    struct Fonts(FontVec);

    impl LigatureFonts for Fonts {
        fn ligature_font(&self, _style: FontStyle) -> &FontVec {
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
    fn disabled_renderer_is_byte_identical_and_allocates_nothing() {
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
        }
        assert!(saw_ink);
    }

    #[test]
    fn unavailable_contextual_slots_fall_back_to_scalar_geometry() {
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
                .unwrap_or_else(|| {
                    panic!("bundled font must expose the {span}-cell {text} ligature")
                });
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
                                None,
                                ChromePin::NONE,
                                opacity,
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
}
