// SPDX-License-Identifier: GPL-3.0-only
//! Presentation-only shaping runs for the cell grid.
//!
//! # Model
//!
//! The terminal model remains one logical character per grid cell. Compatible
//! cells are grouped into shaping runs, each cell contributing its stored
//! grapheme cluster ([`Cell::grapheme`] — base plus any combining marks). Runs
//! are shaped with `swash`; OpenType `calt` substitutions become presentation
//! overlays ([`LigatureRun`]) while backgrounds, decorations, selection, search,
//! copy, and cursor placement stay cell-owned. Shaped advances never move
//! terminal columns.
//!
//! # Cluster → cell anchoring
//!
//! 1. Each eligible cell occupies one contiguous UTF-8 byte span in the run
//!    string (its grapheme). A parallel `cell_bytes` table maps those spans back
//!    to run-relative column indices.
//! 2. A swash cluster whose `source` byte range starts inside cell *i* is
//!    anchored to column *i*. Glyph ids from that cluster inherit that column
//!    as `source_start`.
//! 3. When `calt` changes glyph ids over a contiguous column span, the overlay
//!    covers exactly those source columns. Every shaped glyph in the span is
//!    clipped to the span's pixel box (`anchor_cell` / `span_cells` on
//!    [`ShapedGlyphKey`]). If glyph count ≠ cell count inside the span, glyphs
//!    still share that clip — they are not free to advance into neighboring
//!    logical cells.
//! 4. Clusters that do not differ under `calt` produce no overlay; the ordinary
//!    per-cell scalar path draws them.
//!
//! # Run breaks (compatible-run rule)
//!
//! A shaping run ends at any cell that is ineligible or that selects a different
//! bold/italic face. Ineligible cells include: wide continuations, hidden
//! cells, color-glyph coverage, cells carrying combining marks (marks stay on
//! the mono combining path), and bases outside ASCII-graphic plus
//! [`SHAPING_OPERATOR_ALLOWLIST`]. Selection/search attribute changes do
//! **not** break runs (compositing only).
//!
//! Live overlay eligibility remains ASCII-graphic bases **plus** a curated
//! allowlist of common non-ASCII programming operators/arrows
//! ([`SHAPING_OPERATOR_ALLOWLIST`]). Default plain-ASCII rendering stays
//! byte-identical; allowlisted scalars only join compatible runs when present.
//! Open-ended stylistic sets (`ssXX`) are out of scope. The grapheme and
//! byte-to-column plumbing is the shared substrate for that allowlist.

use std::collections::{HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::ops::Range;
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

/// Curated non-ASCII scalars eligible to join shaping runs with ASCII graphics.
///
/// Inclusion criterion: single-width Unicode operators and arrows that
/// programming fonts commonly participate in OpenType `calt`/`liga` lookups
/// (comparison, logic, and arrow forms). This is a fixed allowlist — not an
/// open stylistic-set surface (`ss01`… are deferred). Placeholders, emoji, and
/// wide East-Asian ideographs stay out. Platform-neutral.
pub const SHAPING_OPERATOR_ALLOWLIST: &[char] = &[
    // Arrows
    '\u{2190}', // ←
    '\u{2192}', // →
    '\u{2194}', // ↔
    '\u{21D0}', // ⇐
    '\u{21D2}', // ⇒
    '\u{21D4}', // ⇔
    '\u{21A6}', // ↦
    // Comparisons / approx
    '\u{2260}', // ≠
    '\u{2264}', // ≤
    '\u{2265}', // ≥
    '\u{226A}', // ≪
    '\u{226B}', // ≫
    '\u{2248}', // ≈
    '\u{2261}', // ≡
    // Logic / set-ish
    '\u{2227}', // ∧
    '\u{2228}', // ∨
    '\u{00AC}', // ¬
    '\u{2205}', // ∅
    // Misc operators
    '\u{00D7}', // ×
    '\u{00F7}', // ÷
    '\u{2212}', // −
    '\u{2026}', // …
    '\u{00B7}', // ·
    '\u{2218}', // ∘
];

#[inline]
fn is_allowlisted_operator(ch: char) -> bool {
    // Small fixed table: linear scan beats a HashSet for ~24 entries and keeps
    // the hot path allocation-free.
    SHAPING_OPERATOR_ALLOWLIST.contains(&ch)
}

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
        for (start, end, style) in compatible_run_bounds(cells, row, coverage) {
            if end - start < 2 {
                continue;
            }
            let run_text = RunText::from_cells(&cells[start..end]);
            runs.extend(self.shape_compatible_run(
                &run_text,
                start,
                style,
                fonts.ligature_font(style),
            ));
        }
        RowPlan { runs }
    }

    fn shape_compatible_run(
        &mut self,
        run_text: &RunText,
        column_start: usize,
        style: FontStyle,
        font: &FontVec,
    ) -> Vec<RelativeRun> {
        let Some(font_ref) = FontRef::from_index(font.as_slice(), 0) else {
            return Vec::new();
        };
        let off = shape(&mut self.context, font_ref, run_text, 0);
        let on = shape(&mut self.context, font_ref, run_text, 1);
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
        let mut spans: Vec<Range<usize>> = Vec::new();
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

/// Grapheme-concatenated shaping string plus the byte→column table that maps
/// swash `SourceRange` starts back to run-relative cell indices.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RunText {
    text: String,
    /// Byte range of each cell's grapheme inside [`Self::text`], in run order.
    cell_bytes: Vec<Range<usize>>,
}

impl RunText {
    fn from_cells(cells: &[Cell]) -> Self {
        let mut text = String::new();
        let mut cell_bytes = Vec::with_capacity(cells.len());
        for cell in cells {
            let start = text.len();
            // Full stored grapheme (base + combining). Eligible live cells have
            // an empty combining list, so this equals `ch` for the ASCII gate;
            // the table still records the correct UTF-8 span for any future
            // curated non-ASCII eligibility.
            text.push_str(&cell.grapheme());
            cell_bytes.push(start..text.len());
        }
        Self { text, cell_bytes }
    }

    /// Column index of the cell whose grapheme owns `byte`, or `None` if `byte`
    /// falls outside every cell span (including `byte == text.len()`).
    fn column_at_byte(&self, byte: usize) -> Option<usize> {
        self.cell_bytes
            .iter()
            .position(|range| range.start <= byte && byte < range.end)
    }
}

/// Inclusive-exclusive `[start, end)` bounds of every compatible shaping run on
/// a row, with the shared [`FontStyle`] of each run.
fn compatible_run_bounds(
    cells: &[Cell],
    row: usize,
    coverage: &ColorRunCoverage,
) -> Vec<(usize, usize, FontStyle)> {
    let mut bounds = Vec::new();
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
            // Selection and search treatments change foreground, background,
            // and inverse attributes cell by cell. Those are compositing
            // inputs, not shaping inputs: splitting here would replace one
            // contextual glyph with independently shaped fragments as a
            // highlight boundary crosses it. Only the font face selected by
            // bold/italic affects contextual shaping.
            && font_style_for_attrs(&cells[end].attrs) == style
        {
            end += 1;
        }
        bounds.push((start, end, style));
        start = end;
    }
    bounds
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
    // Live overlay gate: ASCII-graphic bases, or a curated non-ASCII operator
    // from [`SHAPING_OPERATOR_ALLOWLIST`]. Combining marks, wide continuations,
    // hidden cells, and color-glyph coverage always break runs so those cells
    // stay on their dedicated paths (mono combining, wide lead, color emoji).
    // Platform-neutral — no cfg(windows) divergence.
    (cell.ch.is_ascii_graphic() || is_allowlisted_operator(cell.ch))
        && !cell.wide_continuation
        && cell.combining().is_empty()
        && !cell.attrs.hidden()
        && !coverage.covers(row, column)
}

fn shape(
    context: &mut ShapeContext,
    font: FontRef<'_>,
    run_text: &RunText,
    calt: u16,
) -> Vec<ShapedGlyph> {
    let mut shaper = context
        .builder(font)
        .script(Script::Latin)
        .direction(Direction::LeftToRight)
        .features([("calt", calt)])
        .build();
    shaper.add_str(&run_text.text);
    let mut glyphs = Vec::new();
    shaper.shape_with(|cluster| {
        let Some(source_start) = run_text.column_at_byte(cluster.source.to_range().start) else {
            return;
        };
        for glyph in cluster.glyphs {
            glyphs.push(ShapedGlyph {
                id: glyph.id,
                source_start,
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
            bounds.iter().any(|&(start, end, style)| {
                start == 0 && end == 2 && style == FontStyle::Regular
            }),
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
}
