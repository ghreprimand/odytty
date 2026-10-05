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
//! 3. When enabled Latin features (`calt`/`liga`/optional `ss01`/`ss02`) or
//!    Arabic joining change glyph ids over a contiguous column span, the overlay
//!    covers exactly those source columns. Every shaped glyph in the span is
//!    clipped to the span's pixel box (`anchor_cell` / `span_cells` on
//!    [`ShapedGlyphKey`]). If glyph count ≠ cell count inside the span, glyphs
//!    still share that clip — they are not free to advance into neighboring
//!    logical cells.
//! 4. Clusters that do not differ under the enabled features produce no overlay;
//!    the ordinary per-cell scalar path draws them.
//!
//! # Run breaks (compatible-run rule)
//!
//! A shaping run ends at any cell that is ineligible, that selects a different
//! bold/italic face, or that changes shaping kind (Latin/operator vs Arabic
//! joining). Ineligible cells include: wide continuations, hidden cells,
//! color-glyph coverage, cells carrying combining marks other than Arabic
//! harakat on an Arabic joining base (those marks stay on the mono combining
//! path; see the `arabic` submodule), and bases outside ASCII-graphic,
//! [`SHAPING_OPERATOR_ALLOWLIST`], and Arabic joining letters. Selection/search
//! attribute changes do **not** break runs (compositing only).
//!
//! Live overlay eligibility covers ASCII-graphic bases, a curated allowlist of
//! common non-ASCII programming operators/arrows
//! ([`SHAPING_OPERATOR_ALLOWLIST`]), and Arabic-script joining bases (dual /
//! right / left joining letters plus tatweel). Default plain-ASCII rendering
//! stays byte-identical; allowlisted scalars and Arabic letters only join
//! compatible runs when present. Arabic runs are shaped with `Script::Arabic`
//! in **logical LTR cell order** - joining forms only. On a row the
//! `bidi_reorder` display map reorders, runs split at every level change and
//! shape per level run (see the `bidi` submodule).
//! Latin/operator runs enable OpenType `calt` and `liga` together; optional
//! stylistic sets `ss01` and `ss02` are off by default and gated by
//! [`LatinShapingFeatures`]. Open-ended `ssXX` beyond those two tags is out of
//! scope. The alternate-zero control (`zero`) rides the same struct but is
//! applied equally to both shaping passes, so it never creates an overlay; it
//! only keeps a `0` inside a substituted span consistent with the scalar `0`.

use std::collections::{HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use crate::text::FontHandle;
use swash::shape::{Direction, ShapeContext};
use swash::text::Script;
use swash::{FontRef, GlyphId};

mod arabic;
mod bidi;
#[cfg(test)]
mod harakat_tests;
#[cfg(test)]
mod zero_tests;

use arabic::{
    cluster_pen_offset, is_arabic_joining_base, mapped_mark_segments, marks_join_arabic_run,
};

use crate::atlas::{FontStyle, ShapedGlyphKey};
use crate::core::{Cell, Snapshot};
use crate::grid::{BidiDisplayMap, ColorGlyphRun, ColorRunCoverage, font_style_for_attrs};

/// Maximum number of exact row plans retained by the live renderer.
pub const LIGATURE_ROW_CACHE_CAPACITY: usize = 512;

/// Curated non-ASCII scalars eligible to join shaping runs with ASCII graphics.
///
/// Inclusion criterion: single-width Unicode operators and arrows that
/// programming fonts commonly participate in OpenType `calt`/`liga` lookups
/// (comparison, logic, and arrow forms). This is a fixed allowlist — not an
/// open stylistic-set surface. Optional `ss01`/`ss02` ride
/// [`LatinShapingFeatures`] (off by default). Placeholders, emoji, and
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

/// Shaping kind for compatible-run membership. Latin/operator runs use
/// `calt`+`liga` (and optional `ss01`/`ss02`); Arabic runs use `Script::Arabic`
/// joining. Kinds never merge into one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShapingKind {
    Latin,
    Arabic,
}

#[inline]
fn shaping_kind(ch: char) -> Option<ShapingKind> {
    if is_arabic_joining_base(ch) {
        Some(ShapingKind::Arabic)
    } else if ch.is_ascii_graphic() || is_allowlisted_operator(ch) {
        Some(ShapingKind::Latin)
    } else {
        None
    }
}

/// Optional Latin stylistic-set tags applied when the master ligatures switch
/// is on. Both default off; open-ended `ssXX` beyond these two is out of scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatinShapingFeatures {
    pub ss01: bool,
    pub ss02: bool,
    /// The alternate-zero control. Unlike the stylistic sets it is set to the
    /// same value in both the plain and the contextual shaping pass, so it
    /// never creates an overlay of its own: a lone `0` keeps drawing through
    /// the scalar atlas path (whose face already maps `0` to the alternate),
    /// and a `0` inside a substituted span is shaped with the same alternate.
    pub zero: bool,
}

impl LatinShapingFeatures {
    fn on_tags(self) -> [(&'static str, u16); 5] {
        [
            ("calt", 1),
            ("liga", 1),
            ("ss01", u16::from(self.ss01)),
            ("ss02", u16::from(self.ss02)),
            ("zero", u16::from(self.zero)),
        ]
    }

    fn off_tags(self) -> [(&'static str, u16); 5] {
        [
            ("calt", 0),
            ("liga", 0),
            ("ss01", 0),
            ("ss02", 0),
            ("zero", u16::from(self.zero)),
        ]
    }
}

/// Font access needed by the shaper without coupling it to the native GPU type.
pub trait LigatureFonts {
    fn ligature_font(&self, style: FontStyle) -> &FontHandle;
}

/// One contextual glyph whose atlas slot is anchored to a source-column span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LigatureGlyph {
    pub key: ShapedGlyphKey,
    /// Source cells of the glyph's shaping cluster, starting at its anchor
    /// cell (2 for a lam-alef ligature). Presentation placement only: display
    /// order puts the glyph's pen on the cluster's leftmost visual cell. Not
    /// part of the atlas identity.
    pub source_cells: u8,
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
    /// Resolved bidi level of every cell on a reordered row; empty for a row
    /// drawn in logical order. Level runs decide shaping segments, so a row
    /// shaped under one level vector never serves another.
    levels: Vec<u8>,
}

impl RowKey {
    fn matches(
        &self,
        cells: &[Cell],
        row: usize,
        coverage: &ColorRunCoverage,
        levels: &[u8],
    ) -> bool {
        if self.cells != cells || self.levels != levels {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShapedGlyph {
    id: GlyphId,
    source_start: usize,
    /// Exclusive end column of the glyph's shaping cluster.
    source_end: usize,
    /// Font-unit pen offset from the cluster's first glyph for a glyph after
    /// the first in a marked cell's cluster; zero otherwise.
    mark_offset: [i16; 2],
    /// Whether the glyph belongs to the cluster of a cell carrying marks.
    marked: bool,
}

/// Whether two shaping results draw the same glyphs from the same source
/// cells. Cluster extents are placement data and do not decide whether an
/// overlay exists.
fn same_glyphs(off: &[ShapedGlyph], on: &[ShapedGlyph]) -> bool {
    off.len() == on.len()
        && off
            .iter()
            .zip(on)
            .all(|(a, b)| a.id == b.id && a.source_start == b.source_start)
}

/// Deterministic FIFO row-plan cache plus the reusable swash shaping context.
pub struct LigatureShaper {
    context: ShapeContext,
    entries: HashMap<u64, RowBucket>,
    fifo: VecDeque<(u64, Arc<RowKey>)>,
    entry_count: usize,
    face_fingerprints: [Option<u64>; 4],
    shape_calls: u64,
    latin_features: LatinShapingFeatures,
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
            latin_features: LatinShapingFeatures::default(),
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

    /// Build presentation runs for a snapshot using default Latin features
    /// (`calt`+`liga` on; `ss01`/`ss02` off).
    pub fn build_runs<F: LigatureFonts>(
        &mut self,
        enabled: bool,
        snapshot: &Snapshot,
        fonts: &F,
        color_runs: &[ColorGlyphRun],
    ) -> Vec<LigatureRun> {
        self.build_runs_with_features(
            enabled,
            snapshot,
            fonts,
            color_runs,
            LatinShapingFeatures::default(),
        )
    }

    /// Build presentation runs with explicit optional stylistic-set tags.
    ///
    /// Changing `latin_features` relative to the prior call clears the row-plan
    /// cache so overlays cannot leak across feature toggles.
    pub fn build_runs_with_features<F: LigatureFonts>(
        &mut self,
        enabled: bool,
        snapshot: &Snapshot,
        fonts: &F,
        color_runs: &[ColorGlyphRun],
        latin_features: LatinShapingFeatures,
    ) -> Vec<LigatureRun> {
        self.build_runs_with_features_and_bidi(
            enabled,
            snapshot,
            fonts,
            color_runs,
            latin_features,
            None,
        )
    }

    /// [`Self::build_runs_with_features`] under an optional bidi display map.
    ///
    /// Rows the map leaves in logical order shape exactly as without a map.
    /// A reordered row splits each compatible run at every level change and
    /// shapes each level run in its own direction (see the `bidi` module).
    /// Plans for reordered rows are cached under their level vector.
    pub fn build_runs_with_features_and_bidi<F: LigatureFonts>(
        &mut self,
        enabled: bool,
        snapshot: &Snapshot,
        fonts: &F,
        color_runs: &[ColorGlyphRun],
        latin_features: LatinShapingFeatures,
        bidi: Option<&BidiDisplayMap>,
    ) -> Vec<LigatureRun> {
        if !enabled {
            return Vec::new();
        }
        if latin_features != self.latin_features {
            self.clear();
            self.latin_features = latin_features;
        }
        let cols = snapshot.dimensions.columns;
        // One O(cells / 64 + runs) coverage mask serves the fingerprint,
        // cache-key comparison, and eligibility passes for every row, instead
        // of each of those scanning the whole run list per cell.
        let coverage = ColorRunCoverage::new(color_runs, cols, snapshot.dimensions.rows);
        let mut output = Vec::new();
        for (row, cells) in snapshot.cells.chunks(cols).enumerate() {
            let levels = bidi::row_levels(bidi, row, cells.len());
            let fingerprint = row_fingerprint(cells, row, &coverage, &levels);
            let cached = self.entries.get(&fingerprint).and_then(|bucket| {
                bucket
                    .iter()
                    .find(|(key, _)| key.matches(cells, row, &coverage, &levels))
                    .map(|(_, plan)| Arc::clone(plan))
            });
            let plan = if let Some(plan) = cached {
                plan
            } else {
                self.shape_calls += 1;
                let plan = Arc::new(self.shape_row(cells, fonts, row, &coverage, &levels));
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
                    levels,
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
        levels: &[u8],
    ) -> RowPlan {
        if !levels.is_empty() {
            return self.shape_row_levels(cells, fonts, row, coverage, levels);
        }
        let mut runs = Vec::new();
        for (start, end, style) in shaping_run_bounds(cells, row, coverage, fonts) {
            if end - start < 2 {
                continue;
            }
            let run_text = RunText::from_cells(&cells[start..end]);
            runs.extend(self.shape_compatible_run(
                &run_text,
                start,
                style,
                fonts.ligature_font(style),
                Direction::LeftToRight,
            ));
        }
        RowPlan { runs }
    }

    fn shape_compatible_run(
        &mut self,
        run_text: &RunText,
        column_start: usize,
        style: FontStyle,
        font: &FontHandle,
        direction: Direction,
    ) -> Vec<RelativeRun> {
        let Some(font_ref) = FontRef::from_index(font.as_slice(), 0) else {
            return Vec::new();
        };
        let arabic = run_text.text.chars().any(is_arabic_joining_base);
        let (off, on) = if arabic {
            // Joining forms vs cmap defaults (typically isolated). Live runs
            // pass LTR: cells stay in logical order. With bidi reordering on,
            // the renderer passes RTL for a right-to-left level run; swash still reports
            // clusters in logical order, and placement maps them visually.
            (
                shape_run(
                    &mut self.context,
                    font_ref,
                    run_text,
                    Script::Latin,
                    direction,
                    &[],
                ),
                shape_run(
                    &mut self.context,
                    font_ref,
                    run_text,
                    Script::Arabic,
                    direction,
                    &[],
                ),
            )
        } else {
            let features = self.latin_features;
            (
                shape_run(
                    &mut self.context,
                    font_ref,
                    run_text,
                    Script::Latin,
                    Direction::LeftToRight,
                    &features.off_tags(),
                ),
                shape_run(
                    &mut self.context,
                    font_ref,
                    run_text,
                    Script::Latin,
                    Direction::LeftToRight,
                    &features.on_tags(),
                ),
            )
        };
        // A marked Arabic cell always draws through the overlay, even where
        // joining leaves its glyph ids unchanged, so its marks take their
        // shaped positions consistently across a run.
        let marked_columns = on
            .iter()
            .filter(|glyph| glyph.marked)
            .map(|glyph| glyph.source_start);
        if same_glyphs(&off, &on) && marked_columns.clone().next().is_none() {
            return Vec::new();
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
        // Arabic joining ligatures such as lam-alef and Latin `liga`
        // substitutions may emit fewer glyphs than cells. Keep the logical
        // model unchanged by clipping the shaped presentation to the complete
        // source run.
        if off.len() != on.len() {
            return whole_run_overlay(
                &on,
                column_start,
                run_text.cell_bytes.len(),
                style,
                face_fingerprint,
            )
            .into_iter()
            .collect();
        }
        let mut changed = off
            .iter()
            .zip(&on)
            .filter_map(|(plain, contextual)| {
                (plain.id != contextual.id).then_some(plain.source_start)
            })
            .chain(marked_columns)
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
                                mark_offset: glyph.mark_offset,
                                cluster: false,
                            },
                            source_cells: cluster_cells(glyph, span.end),
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
    /// Whether each cell carries combining marks (Arabic harakat; no other
    /// marked cell joins a run).
    marked: Vec<bool>,
}

impl RunText {
    fn from_cells(cells: &[Cell]) -> Self {
        let mut text = String::new();
        let mut cell_bytes = Vec::with_capacity(cells.len());
        let mut marked = Vec::with_capacity(cells.len());
        for cell in cells {
            let start = text.len();
            // Full stored grapheme (base + combining). Only Arabic cells carry
            // marks into a run; every other eligible cell has none, so this
            // equals `ch` for the ASCII gate.
            text.push_str(&cell.grapheme());
            cell_bytes.push(start..text.len());
            marked.push(!cell.combining().is_empty());
        }
        Self {
            text,
            cell_bytes,
            marked,
        }
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
        let Some(kind) = eligible_cell_kind(&cells[start], row, start, coverage) else {
            start += 1;
            continue;
        };
        let style = font_style_for_attrs(&cells[start].attrs);
        let mut end = start + 1;
        while end < cells.len() {
            let Some(next_kind) = eligible_cell_kind(&cells[end], row, end, coverage) else {
                break;
            };
            // Selection and search treatments change foreground, background,
            // and inverse attributes cell by cell. Those are compositing
            // inputs, not shaping inputs: splitting here would replace one
            // contextual glyph with independently shaped fragments as a
            // highlight boundary crosses it. Only the font face selected by
            // bold/italic - and the shaping kind (Latin vs Arabic) - affect
            // contextual shaping.
            if next_kind != kind || font_style_for_attrs(&cells[end].attrs) != style {
                break;
            }
            end += 1;
        }
        bounds.push((start, end, style));
        start = end;
    }
    bounds
}

/// [`compatible_run_bounds`] further split around marked cells whose marks
/// the run's shaping face does not map; those cells stay on the monochrome
/// combining path.
fn shaping_run_bounds<F: LigatureFonts>(
    cells: &[Cell],
    row: usize,
    coverage: &ColorRunCoverage,
    fonts: &F,
) -> Vec<(usize, usize, FontStyle)> {
    let mut bounds = Vec::new();
    for (start, end, style) in compatible_run_bounds(cells, row, coverage) {
        let font = fonts.ligature_font(style);
        bounds.extend(
            mapped_mark_segments(cells, start, end, font)
                .into_iter()
                .map(|segment| (segment.start, segment.end, style)),
        );
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

fn row_fingerprint(cells: &[Cell], row: usize, coverage: &ColorRunCoverage, levels: &[u8]) -> u64 {
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
    // Logical-order rows carry no levels, so their fingerprint is unchanged.
    for &level in levels {
        fingerprint ^= u64::from(level) | (1 << 40);
        fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
    }
    fingerprint
}

fn eligible_cell_kind(
    cell: &Cell,
    row: usize,
    column: usize,
    coverage: &ColorRunCoverage,
) -> Option<ShapingKind> {
    // Wide continuations, hidden cells, color-glyph coverage, and combining
    // marks other than Arabic harakat on an Arabic joining base always break
    // runs so those cells stay on their dedicated paths (mono combining, wide
    // lead, color emoji). Platform-neutral - no cfg(windows) divergence.
    if cell.wide_continuation || !marks_join_arabic_run(cell) || cell.attrs.hidden() {
        return None;
    }
    if coverage.covers(row, column) {
        return None;
    }
    shaping_kind(cell.ch)
}

fn shape_run(
    context: &mut ShapeContext,
    font: FontRef<'_>,
    run_text: &RunText,
    script: Script,
    direction: Direction,
    features: &[(&str, u16)],
) -> Vec<ShapedGlyph> {
    let mut shaper = context
        .builder(font)
        .script(script)
        .direction(direction)
        .features(features.iter().copied())
        .build();
    shaper.add_str(&run_text.text);
    let mut glyphs = Vec::new();
    shaper.shape_with(|cluster| {
        let bytes = cluster.source.to_range();
        let Some(source_start) = run_text.column_at_byte(bytes.start) else {
            return;
        };
        let source_end = bytes
            .end
            .checked_sub(1)
            .and_then(|last| run_text.column_at_byte(last))
            .map_or(source_start + 1, |last| last.max(source_start) + 1);
        let marked = run_text
            .marked
            .get(source_start..source_end)
            .is_some_and(|cells| cells.contains(&true));
        let first = cluster.glyphs.first().map_or((0.0, 0.0), |g| (g.x, g.y));
        let mut advances = 0.0_f32;
        for (index, glyph) in cluster.glyphs.iter().enumerate() {
            let mark_offset = if marked && index > 0 {
                cluster_pen_offset(advances, glyph.x, glyph.y, first)
            } else {
                [0, 0]
            };
            // An attached mark sits at its base's pen plus the base advance
            // and its anchor offset; swash keeps the mark's own font
            // advance, which a monospace face may set to a full cell, so it
            // must not move the pen of later marks in the cluster.
            if !glyph.info.is_mark() {
                advances += glyph.advance;
            }
            glyphs.push(ShapedGlyph {
                id: glyph.id,
                source_start,
                source_end,
                mark_offset,
                marked,
            });
        }
    });
    glyphs
}

/// Source cells of `glyph`'s cluster, clipped at the exclusive column `end`
/// of its overlay and never below one.
fn cluster_cells(glyph: &ShapedGlyph, end: usize) -> u8 {
    let cells = glyph.source_end.min(end).saturating_sub(glyph.source_start);
    u8::try_from(cells.max(1)).unwrap_or(u8::MAX)
}

/// One overlay covering every cell in a run when shaping changes glyph count
/// (for example Arabic lam-alef or a Latin `liga` substitution). Glyphs clip to
/// the full span's pixel box.
fn whole_run_overlay(
    on: &[ShapedGlyph],
    column_start: usize,
    cell_count: usize,
    style: FontStyle,
    face_fingerprint: u64,
) -> Option<RelativeRun> {
    let span_cells = u8::try_from(cell_count).ok()?;
    if cell_count == 0 || on.is_empty() {
        return None;
    }
    let glyphs = on
        .iter()
        .filter_map(|glyph| {
            let anchor_cell = u8::try_from(glyph.source_start.min(cell_count - 1)).ok()?;
            Some(LigatureGlyph {
                key: ShapedGlyphKey {
                    face_fingerprint,
                    style,
                    glyph_id: glyph.id,
                    span_cells,
                    anchor_cell,
                    mark_offset: glyph.mark_offset,
                    cluster: false,
                },
                source_cells: cluster_cells(glyph, cell_count),
            })
        })
        .collect::<Vec<_>>();
    (!glyphs.is_empty()).then_some(RelativeRun {
        start: column_start,
        end: column_start + cell_count,
        glyphs: glyphs.into(),
    })
}

fn font_fingerprint(font: &FontHandle) -> u64 {
    let mut hasher = DefaultHasher::new();
    font.as_slice().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests;
