// SPDX-License-Identifier: GPL-3.0-only
//! Owner-run shaping against the licensed northern Indic fixtures in
//! `tests/fixtures/fonts/s5b/northern-indic/` (OFL subsets of the Noto
//! faces, with HarfBuzz references; see the fixture README).
//!
//! Pixel oracle: the production path (owner classification, face selection,
//! harfrust shaping, cluster rasterization, and the cell build) must draw
//! exactly the frame that the same cluster rasterizer draws from the
//! HarfBuzz reference glyph run, while the per-cell path draws a different
//! frame for every reph, conjunct, and pre-base sample.

use super::*;
use crate::atlas::{CLUSTER_MIN_SCALE, ClusterFit, cluster_fit};
use crate::core::Terminal;
use crate::grid::{
    BackgroundTreatmentParams, BidiDisplayMap, ChromePin, INSTANCES_PER_QUAD, Vertex,
    build_cell_vertices_with_bidi_into,
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into,
};
use crate::selection::{CellPoint, SelectionRange, selected_text};

const PX: f32 = 28.0;
const GROUP: &str = "northern-indic";
const REFERENCE: &str = include_str!("../../tests/fixtures/fonts/s5b/northern-indic/reference.tsv");
const KNOWN_DIFF: &str = "known-diff:harfrust-0.8.4-vs-harfbuzz-14.5";
const FACES: &[(&str, &[u8])] = &[
    (
        "Devanagari-subset.ttf",
        include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Devanagari-subset.ttf"),
    ),
    (
        "Bengali-subset.ttf",
        include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Bengali-subset.ttf"),
    ),
    (
        "Gurmukhi-subset.ttf",
        include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Gurmukhi-subset.ttf"),
    ),
    (
        "Gujarati-subset.ttf",
        include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Gujarati-subset.ttf"),
    ),
    (
        "Odia-subset.ttf",
        include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Odia-subset.ttf"),
    ),
];

fn face(name: &str) -> FontHandle {
    let bytes = FACES
        .iter()
        .find(|(file, _)| *file == name)
        .unwrap_or_else(|| panic!("{name} is not a {GROUP} fixture face"))
        .1;
    FontHandle::try_from_vec(bytes.to_vec()).expect("fixture face parses")
}

fn latin_face() -> FontHandle {
    FontHandle::try_from_vec(include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec())
        .expect("bidi fixture font parses")
}

/// One `reference.tsv` row.
#[derive(Debug)]
struct Row {
    font: String,
    text: String,
    glyphs: Vec<u16>,
    clusters: Vec<u32>,
    x_offset: Vec<i32>,
    y_offset: Vec<i32>,
    x_advance: Vec<i32>,
    note: String,
}

impl Row {
    /// Reference pen positions, as `shape_owner` reports them.
    fn placed(&self) -> Vec<ClusterGlyph> {
        let mut pen = 0;
        let mut out = Vec::new();
        for i in 0..self.glyphs.len() {
            out.push(ClusterGlyph {
                id: self.glyphs[i],
                x: pen + self.x_offset[i],
                y: self.y_offset[i],
            });
            pen += self.x_advance[i];
        }
        out
    }

    fn known_diff(&self) -> bool {
        self.note.starts_with("known-diff:")
    }
}

fn ints<T: std::str::FromStr>(field: &str) -> Vec<T>
where
    T::Err: std::fmt::Debug,
{
    field
        .split(',')
        .map(|value| value.parse().expect("integer field"))
        .collect()
}

/// Parse the group's references with the agreed loader rules: eight
/// tab-separated columns, equal array lengths, and a font from the group.
fn rows() -> Vec<Row> {
    let mut out = Vec::new();
    for line in REFERENCE.lines() {
        if line.starts_with('#') || line.starts_with("font\t") || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 8, "column count: {line}");
        assert!(
            FACES.iter().any(|(file, _)| *file == fields[0]),
            "font outside the group: {line}"
        );
        let text = fields[1]
            .split_whitespace()
            .map(|token| {
                let hex = token.strip_prefix("U+").expect("U+XXXX token");
                char::from_u32(u32::from_str_radix(hex, 16).expect("hex")).expect("scalar")
            })
            .collect();
        let row = Row {
            font: fields[0].to_string(),
            text,
            glyphs: ints(fields[2]),
            clusters: ints(fields[3]),
            x_offset: ints(fields[4]),
            y_offset: ints(fields[5]),
            x_advance: ints(fields[6]),
            note: fields[7].to_string(),
        };
        let len = row.glyphs.len();
        assert!(
            len > 0
                && row.clusters.len() == len
                && row.x_offset.len() == len
                && row.y_offset.len() == len
                && row.x_advance.len() == len,
            "array lengths: {line}"
        );
        out.push(row);
    }
    out
}

/// Raw harfrust output for a row: glyph, cluster, offsets, and advance.
fn shape_raw(face: &FontHandle, text: &str) -> Vec<(u32, u32, i32, i32, i32)> {
    let font = harfrust::FontRef::from_index(face.as_slice(), face.face_index()).unwrap();
    let data = ShaperData::new(&font);
    let shaper = data.shaper(&font).build();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(Direction::LeftToRight);
    let output = shaper.shape(buffer, ShapeOptions::new());
    output
        .glyph_infos()
        .iter()
        .zip(output.glyph_positions())
        .map(|(info, pos)| {
            (
                info.glyph_id,
                info.cluster,
                pos.x_offset,
                pos.y_offset,
                pos.x_advance,
            )
        })
        .collect()
}

#[test]
fn harfrust_matches_every_northern_reference_except_the_recorded_difference() {
    let rows = rows();
    assert_eq!(rows.len(), 49);
    let mut mismatches = Vec::new();
    for row in &rows {
        let expected: Vec<_> = (0..row.glyphs.len())
            .map(|i| {
                (
                    u32::from(row.glyphs[i]),
                    row.clusters[i],
                    row.x_offset[i],
                    row.y_offset[i],
                    row.x_advance[i],
                )
            })
            .collect();
        let face = face(&row.font);
        if shape_raw(&face, &row.text) != expected {
            mismatches.push(row.note.clone());
        }
        let data = shaper_data(&face).unwrap();
        let placed = shape_owner(&face, &data, &row.text).expect("clean shaping result");
        if !row.known_diff() {
            assert_eq!(placed, row.placed(), "{row:?}");
        }
    }
    // The recorded engine-version difference is asserted, never skipped: a
    // shaper change that fixes or worsens it fails here.
    assert_eq!(mismatches, vec![format!("{KNOWN_DIFF};below-base-ra")]);
}

#[test]
fn classifier_enables_exactly_the_northern_indic_group() {
    let attrs = crate::core::Attrs::default();
    for ch in [
        '\u{0915}',
        '\u{0995}',
        '\u{0A15}',
        '\u{0A95}',
        '\u{0B15}',
        '\u{A8F2}',
        '\u{11B00}',
    ] {
        assert!(owner_is_eligible(&Cell::new(ch, attrs)), "{ch:?}");
    }
    // Latin, Arabic, emoji, box drawing, and later stage groups stay out.
    for ch in [
        'a',
        '\u{0628}',
        '\u{1F600}',
        '\u{2500}',
        '\u{0B95}',
        '\u{0D9A}',
        '\u{1780}',
        '\u{0E01}',
        '\u{11103}',
    ] {
        assert!(!owner_is_eligible(&Cell::new(ch, attrs)), "{ch:?}");
    }
    // A retained scalar from outside the group keeps the per-cell path.
    let mut mixed = Cell::new('\u{0915}', attrs);
    assert!(mixed.push_combining('\u{0301}'));
    assert!(!owner_is_eligible(&mixed));
    let mut joined = Cell::new('\u{0915}', attrs);
    assert!(joined.push_combining('\u{094D}'));
    assert!(joined.push_combining('\u{200D}'));
    assert!(joined.push_combining('\u{1CD0}'));
    assert!(owner_is_eligible(&joined));
}

#[test]
fn fit_rule_centers_shrinks_to_the_floor_then_clips() {
    // Fits: unscaled and centered.
    let fit = cluster_fit([2.0, -3.0, 12.0, 20.0], 20.0, 30.0, 10.0);
    assert_eq!(
        fit,
        ClusterFit {
            scale: 1.0,
            origin_x: 3.0
        }
    );
    // Wider than the span: one uniform scale fits it exactly.
    let fit = cluster_fit([0.0, 0.0, 30.0, 10.0], 20.0, 30.0, 10.0);
    assert!((fit.scale - 20.0 / 30.0).abs() < 1e-6, "{fit:?}");
    assert!(fit.origin_x.abs() < 1e-4, "{fit:?}");
    // Far wider: the floor holds and the run starts at the left edge.
    let fit = cluster_fit([4.0, 0.0, 104.0, 10.0], 20.0, 30.0, 10.0);
    assert_eq!(fit.scale, CLUSTER_MIN_SCALE);
    assert!((fit.origin_x + 4.0 * CLUSTER_MIN_SCALE).abs() < 1e-6);
    // Taller than the slot: the vertical limit scales it too.
    let fit = cluster_fit([0.0, -2.0, 10.0, 40.0], 20.0, 30.0, 10.0);
    assert!((fit.scale - 0.75).abs() < 1e-6, "{fit:?}");
    let fit = cluster_fit([0.0, -20.0, 10.0, 5.0], 20.0, 30.0, 10.0);
    assert!((fit.scale - CLUSTER_MIN_SCALE).abs() < 1e-6, "{fit:?}");
}

// ---------------------------------------------------------------------------
// Rendering

struct Fonts(FontHandle);

impl LigatureFonts for Fonts {
    fn ligature_font(&self, _style: FontStyle) -> &FontHandle {
        &self.0
    }
}

fn terminal(text: &str, cols: usize) -> Terminal {
    let mut terminal = Terminal::new(cols, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance(text.as_bytes());
    terminal
}

/// Make every scalar of the snapshot resident, as the live ensure pass does.
fn ensure_cells(atlas: &mut GlyphAtlas, font: &FontHandle, snapshot: &Snapshot) {
    for cell in &snapshot.cells {
        if cell.wide_continuation {
            continue;
        }
        atlas.ensure(font, cell.ch);
        for &mark in cell.combining() {
            atlas.ensure(font, mark);
        }
    }
}

#[derive(Debug, PartialEq)]
struct Frame {
    width: usize,
    cell_w: usize,
    px: Vec<[f32; 3]>,
}

impl Frame {
    fn columns(&self, columns: std::ops::Range<usize>) -> Vec<[f32; 3]> {
        let height = self.px.len() / self.width;
        let mut out = Vec::new();
        for y in 0..height {
            let start = y * self.width + columns.start * self.cell_w;
            out.extend_from_slice(&self.px[start..start + columns.len() * self.cell_w]);
        }
        out
    }
}

fn composite(snapshot: &Snapshot, atlas: &GlyphAtlas, verts: &[Vertex]) -> Frame {
    let cell_w = atlas.cell.width as usize;
    let width = snapshot.dimensions.columns * cell_w;
    let height = snapshot.dimensions.rows * atlas.cell.height as usize;
    let mut px = vec![[0.0_f32; 3]; width * height];
    for quad in verts.as_chunks::<INSTANCES_PER_QUAD>().0 {
        let v = &quad[0];
        let [x0, y0] = v.pos;
        let [x1, y1] = v.end_pos;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        for py in (y0.floor().max(0.0) as usize)..(y1.ceil() as usize).min(height) {
            let cy = py as f32 + 0.5;
            if cy < y0 || cy >= y1 {
                continue;
            }
            for px_x in (x0.floor().max(0.0) as usize)..(x1.ceil() as usize).min(width) {
                let cx = px_x as f32 + 0.5;
                if cx < x0 || cx >= x1 {
                    continue;
                }
                let alpha = if v.is_glyph > 0.5 {
                    let u = v.uv[0] + (cx - x0) / (x1 - x0) * (v.end_uv[0] - v.uv[0]);
                    let t = v.uv[1] + (cy - y0) / (y1 - y0) * (v.end_uv[1] - v.uv[1]);
                    let ax = ((u * atlas.width as f32) as usize).min(atlas.width as usize - 1);
                    let ay = ((t * atlas.height as f32) as usize).min(atlas.height as usize - 1);
                    v.color[3] * f32::from(atlas.data[ay * atlas.width as usize + ax]) / 255.0
                } else {
                    v.color[3]
                };
                let dst = &mut px[py * width + px_x];
                for (out, ink) in dst.iter_mut().zip(v.color) {
                    *out = ink * alpha + *out * (1.0 - alpha);
                }
            }
        }
    }
    Frame { width, cell_w, px }
}

fn frame(snapshot: &Snapshot, atlas: &GlyphAtlas, runs: &[LigatureRun]) -> Frame {
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
    composite(snapshot, atlas, &verts)
}

/// The single owner `text` forms on a row, as `(span, grapheme)`, or `None`
/// when the terminal splits it into several owners.
fn single_owner(snapshot: &Snapshot) -> Option<usize> {
    let cells = &snapshot.cells;
    let owners = cells
        .iter()
        .filter(|cell| !cell.wide_continuation && cell.ch != ' ')
        .count();
    (owners == 1).then(|| 1 + usize::from(cells[1].wide_continuation))
}

/// The reference glyph run drawn through the cluster rasterizer as one
/// overlay over the owner's cells.
fn oracle_run(
    atlas: &mut GlyphAtlas,
    face: &FontHandle,
    own_face: bool,
    glyphs: &[ClusterGlyph],
    column: usize,
    span: usize,
) -> LigatureRun {
    let key = atlas
        .ensure_cluster(
            face,
            fingerprint(face),
            FontStyle::Regular,
            own_face,
            span as u8,
            glyphs,
        )
        .expect("oracle cluster slot");
    LigatureRun {
        row: 0,
        start: column,
        end: column + span,
        glyphs: Arc::from([LigatureGlyph {
            key,
            source_cells: span as u8,
        }]),
    }
}

#[test]
fn production_frames_equal_the_reference_run_and_differ_from_the_per_cell_path() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut compared = 0;
    let mut changed = Vec::new();
    for row in rows().iter().filter(|row| !row.known_diff()) {
        let font = face(&row.font);
        let snapshot = terminal(&row.text, 4).snapshot();
        let Some(span) = single_owner(&snapshot) else {
            continue;
        };
        assert_eq!(snapshot.cells[0].grapheme(), row.text);
        let mut atlas = GlyphAtlas::build(&font, PX);
        ensure_cells(&mut atlas, &font, &snapshot);
        let per_cell = frame(&snapshot, &atlas, &[]);
        let mut shaper = ComplexShaper::new();
        let runs = shaper.build_runs(true, &snapshot, &Fonts(font.clone()), &mut atlas, &[]);
        assert_eq!(runs.len(), 1, "{row:?}");
        assert_eq!((runs[0].start, runs[0].end), (0, span));
        let production = frame(&snapshot, &atlas, &runs);
        let oracle = oracle_run(&mut atlas, &font, true, &row.placed(), 0, span);
        assert_eq!(production, frame(&snapshot, &atlas, &[oracle]), "{row:?}");
        // No ink leaves the owner's span.
        let blank = frame(&terminal("", 4).snapshot(), &atlas, &[]);
        assert_eq!(
            production.columns(span..4),
            blank.columns(span..4),
            "{row:?}"
        );
        if production != per_cell {
            changed.push(row.note.clone());
        }
        compared += 1;
    }
    assert!(compared >= 40, "{compared} single-owner samples");
    for note in ["reph", "conjunct", "pre-base-matra", "conjunct-pre-base"] {
        assert!(
            changed.iter().any(|changed| changed == note),
            "{note} draws differently from the per-cell path: {changed:?}"
        );
    }
    let structural = changed
        .iter()
        .filter(|note| {
            note.contains("reph") || note.contains("conjunct") || note.contains("pre-base")
        })
        .count();
    assert!(structural >= 12, "{changed:?}");
}

#[test]
fn a_fallback_face_shapes_owners_the_primary_lacks() {
    let _guard = crate::test_lock::render_globals_lock();
    let primary = latin_face();
    let devanagari = Arc::new(face("Devanagari-subset.ttf"));
    let text = "\u{0930}\u{094D}\u{0915}";
    let snapshot = terminal(text, 4).snapshot();
    let mut atlas = GlyphAtlas::build(&primary, PX);
    atlas.set_fallback_fonts(vec![Arc::clone(&devanagari)]);
    ensure_cells(&mut atlas, &primary, &snapshot);
    let runs = ComplexShaper::new().build_runs(true, &snapshot, &Fonts(primary), &mut atlas, &[]);
    assert_eq!(runs.len(), 1);
    let reference = rows()
        .into_iter()
        .find(|row| row.text == text)
        .expect("reph row");
    let span = runs[0].end;
    let oracle = oracle_run(&mut atlas, &devanagari, false, &reference.placed(), 0, span);
    assert_eq!(runs[0].glyphs[0].key, oracle.glyphs[0].key);
    assert_eq!(
        frame(&snapshot, &atlas, &runs),
        frame(&snapshot, &atlas, &[oracle])
    );
}

#[test]
fn owners_without_a_covering_face_and_ligatures_off_keep_the_per_cell_path() {
    let _guard = crate::test_lock::render_globals_lock();
    let primary = latin_face();
    let snapshot = terminal("\u{0930}\u{094D}\u{0915}", 4).snapshot();
    let mut atlas = GlyphAtlas::build(&primary, PX);
    ensure_cells(&mut atlas, &primary, &snapshot);
    let before = frame(&snapshot, &atlas, &[]);
    let mut shaper = ComplexShaper::new();
    let fonts = Fonts(primary);
    assert!(
        shaper
            .build_runs(true, &snapshot, &fonts, &mut atlas, &[])
            .is_empty()
    );
    assert_eq!(atlas.shaped_slot_count(), 0);
    assert_eq!(frame(&snapshot, &atlas, &[]), before);

    let font = face("Devanagari-subset.ttf");
    let mut atlas = GlyphAtlas::build(&font, PX);
    let off = shaper.build_runs(false, &snapshot, &Fonts(font), &mut atlas, &[]);
    assert!(off.is_empty());
    assert_eq!(atlas.shaped_slot_count(), 0);
}

#[test]
fn shaped_owners_keep_logical_selection_and_reuse_cached_shaping() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = face("Devanagari-subset.ttf");
    let text = "\u{0915}\u{094D}\u{0937}\u{093F} \u{0930}\u{094D}\u{0915}";
    let terminal = terminal(text, 10);
    let snapshot = terminal.snapshot();
    let mut atlas = GlyphAtlas::build(&font, PX);
    let mut shaper = ComplexShaper::new();
    let fonts = Fonts(font);
    let runs = shaper.build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
    assert_eq!(runs.len(), 2);
    assert_eq!(terminal.snapshot(), snapshot, "presentation only");
    let range = SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 9 },
    };
    assert_eq!(selected_text(&snapshot, range).trim_end(), text);
    let calls = shaper.shape_calls();
    assert_eq!(calls, 2);
    assert_eq!(shaper.face_lookups(), 2);
    let again = shaper.build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
    assert_eq!(again, runs);
    assert_eq!(
        shaper.shape_calls(),
        calls,
        "settled owners are not reshaped"
    );
    assert_eq!(
        shaper.face_lookups(),
        2,
        "settled owners skip face selection"
    );
    shaper.clear();
    let mut rebuilt = GlyphAtlas::build(&fonts.0, PX);
    assert_eq!(
        shaper.build_runs(true, &snapshot, &fonts, &mut rebuilt, &[]),
        runs
    );
    assert_eq!(shaper.shape_calls(), calls + 2);
}

#[test]
fn a_shaped_owner_draws_as_one_unit_on_a_reordered_row() {
    let _guard = crate::test_lock::render_globals_lock();
    let primary = latin_face();
    let devanagari = Arc::new(face("Devanagari-subset.ttf"));
    // Hebrew alef and bet, then a reph owner. Paragraphs are left to right,
    // so the Hebrew pair reverses while the owner keeps its two cells.
    let owner = "\u{0930}\u{094D}\u{0915}";
    let text = format!("\u{05D0}\u{05D1}{owner}");
    let terminal = terminal(&text, 6);
    let snapshot = terminal.snapshot();
    let wrapped: Vec<bool> = terminal
        .visible_search_rows(0)
        .iter()
        .map(|row| row.wrapped)
        .collect();
    let map = BidiDisplayMap::plan(&snapshot, &wrapped);
    assert_eq!((map.visual_column(0, 0), map.visual_column(0, 1)), (1, 0));
    let mut atlas = GlyphAtlas::build(&primary, PX);
    atlas.set_fallback_fonts(vec![Arc::clone(&devanagari)]);
    ensure_cells(&mut atlas, &primary, &snapshot);
    let runs = ComplexShaper::new().build_runs(true, &snapshot, &Fonts(primary), &mut atlas, &[]);
    assert_eq!(runs.len(), 1);
    let (start, end) = (runs[0].start, runs[0].end);
    let visual: Vec<usize> = (start..end).map(|c| map.visual_column(0, c)).collect();
    let left = *visual.iter().min().unwrap();
    assert_eq!(visual.iter().max().unwrap() - left + 1, end - start);
    let mut verts = Vec::new();
    build_cell_vertices_with_bidi_into(&mut verts, &snapshot, &atlas, &[], &runs, &map);
    let drawn = composite(&snapshot, &atlas, &verts);
    // The owner alone, drawn in logical order, is the expected block.
    let alone = self::terminal(owner, 6).snapshot();
    let alone_run = LigatureRun {
        row: 0,
        start: 0,
        end: end - start,
        glyphs: runs[0].glyphs.clone(),
    };
    let alone_frame = frame(&alone, &atlas, &[alone_run]);
    assert_eq!(
        drawn.columns(left..left + (end - start)),
        alone_frame.columns(0..end - start)
    );
}

#[test]
fn cluster_keys_never_rasterize_through_the_single_glyph_path() {
    let font = face("Devanagari-subset.ttf");
    let mut atlas = GlyphAtlas::build(&font, PX);
    let key = ShapedGlyphKey {
        face_fingerprint: 7,
        style: FontStyle::Regular,
        glyph_id: 0,
        span_cells: 2,
        anchor_cell: 0,
        mark_offset: [0, 0],
        cluster: true,
    };
    assert_eq!(atlas.ensure_shaped(&font, key), None);
    assert!(!atlas.contains_shaped(key));
    assert_eq!(atlas.shaped_slot_count(), 0);
}

#[test]
fn merged_runs_stay_in_row_major_order() {
    let run = |row, start| LigatureRun {
        row,
        start,
        end: start + 1,
        glyphs: Arc::from([]),
    };
    let mut runs = vec![run(0, 4), run(1, 0)];
    merge_runs(&mut runs, vec![run(0, 1), run(1, 3), run(0, 6)]);
    let order: Vec<_> = runs.iter().map(|r| (r.row, r.start)).collect();
    assert_eq!(order, vec![(0, 1), (0, 4), (0, 6), (1, 0), (1, 3)]);
}

#[test]
fn style_faces_keep_synthetic_bold_and_fallback_faces_do_not() {
    let _guard = crate::test_lock::render_globals_lock();
    let owner = "\u{0915}\u{094D}\u{0937}";
    let bold = format!("\x1b[1m{owner}");
    let draw = |primary: FontHandle, fallback: Option<Arc<FontHandle>>, text: &str| {
        let snapshot = terminal(text, 4).snapshot();
        let mut atlas = GlyphAtlas::build(&primary, PX);
        atlas.set_synthetic_styles(true, true, true);
        if let Some(face) = fallback {
            atlas.set_fallback_fonts(vec![face]);
        }
        let runs =
            ComplexShaper::new().build_runs(true, &snapshot, &Fonts(primary), &mut atlas, &[]);
        assert_eq!(runs.len(), 1, "{text:?}");
        let bounds = atlas.shaped_glyph_quad(runs[0].glyphs[0].key).expect("ink");
        (bounds.width, bounds.offset_x)
    };
    let devanagari = || face("Devanagari-subset.ttf");
    let regular = draw(devanagari(), None, owner);
    let emboldened = draw(devanagari(), None, &bold);
    assert!(emboldened.0 > regular.0, "{regular:?} {emboldened:?}");
    let shared = Arc::new(devanagari());
    let fallback_regular = draw(latin_face(), Some(Arc::clone(&shared)), owner);
    let fallback_bold = draw(latin_face(), Some(shared), &bold);
    assert_eq!(fallback_regular, fallback_bold);
}

#[test]
fn joiners_need_no_glyph_and_missing_scalars_never_shape() {
    let latin = latin_face();
    assert_eq!(latin.glyph_id('\u{200d}').0, 0, "fixture lacks ZWJ");
    assert!(crate::atlas::face_maps_all(
        &latin,
        &['\u{200d}', '\u{200c}']
    ));
    assert!(!crate::atlas::face_maps_all(&latin, &['\u{0915}']));
    // A face missing a scalar shapes it to .notdef, which never draws.
    let data = shaper_data(&latin).unwrap();
    assert_eq!(shape_owner(&latin, &data, "\u{0915}\u{094D}\u{0937}"), None);
}
