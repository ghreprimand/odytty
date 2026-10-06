// SPDX-License-Identifier: GPL-3.0-only
//! Shaped owners combined with the other presentation paths on one row:
//! Latin ligature runs, color emoji clusters, wide and ambiguous cells, bidi
//! reordering, reflow with wrap padding, snapshot restore, images, and the
//! independent ligature and script switches on one row. Each
//! test asserts logical text and cell ownership, and pixels where the cell
//! build composites them.

use super::*;
use crate::core::{SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps};
use crate::emoji::{ColorGlyphAtlas, EmojiFont, EmojiRasterizer};
use crate::ligature::{LatinShapingFeatures, LigatureShaper, ShapingSwitches};

/// A two-cell Devanagari conjunct owner (ka, virama, ssa) and a one-cell
/// reph owner shape (ra, virama, ka).
const CONJUNCT: &str = "\u{0915}\u{094D}\u{0937}";
const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";

fn row_text(terminal: &Terminal, row: usize) -> String {
    let columns = terminal.snapshot().dimensions.columns;
    let range = SelectionRange {
        start: CellPoint { row, column: 0 },
        end: CellPoint {
            row,
            column: columns - 1,
        },
    };
    selected_text(&terminal.snapshot(), range)
        .trim_end()
        .to_owned()
}

/// Bundled monospace primary with the Devanagari subset as its fallback,
/// the common real configuration.
fn mixed_atlas() -> (FontHandle, GlyphAtlas) {
    let primary = crate::text::load_bundled_font().expect("bundled font");
    let mut atlas = GlyphAtlas::build(&primary, PX);
    atlas.set_fallback_fonts(vec![Arc::new(face("Devanagari-subset.ttf"))]);
    (primary, atlas)
}

/// The owner drawn alone at column 0 of a blank row of `columns`, as the
/// expected block for wherever the same owner lands.
fn owner_alone(atlas: &mut GlyphAtlas, fonts: &Fonts, owner: &str, columns: usize) -> Frame {
    let snapshot = terminal(owner, columns).snapshot();
    let runs = ComplexShaper::new().build_runs(true, &snapshot, fonts, atlas, &[]);
    assert_eq!(runs.len(), 1, "{owner:?} shapes alone");
    frame(&snapshot, atlas, &runs)
}

/// Pixel equality that reports the differing pixel count rather than
/// dumping both buffers.
#[track_caller]
fn same_pixels(left: &[[f32; 3]], right: &[[f32; 3]], what: &str) {
    assert_eq!(left.len(), right.len(), "{what}: sizes differ");
    let differing = left.iter().zip(right).filter(|(a, b)| a != b).count();
    assert_eq!(
        differing,
        0,
        "{what}: {differing} of {} pixels differ",
        left.len()
    );
}

/// `snapshot` with the cells of `row` in `columns` reset to blanks, the
/// reference for neighbours of an owner whose ink stays inside its span.
fn blanked(snapshot: &Snapshot, row: usize, columns: std::ops::Range<usize>) -> Snapshot {
    let mut out = snapshot.clone();
    let width = out.dimensions.columns;
    let blank = terminal("", 1).snapshot().cells[0];
    for column in columns {
        out.cells[row * width + column] = blank;
    }
    out
}

fn complex_spans(runs: &[LigatureRun]) -> Vec<(usize, usize, usize)> {
    runs.iter()
        .map(|run| (run.row, run.start, run.end))
        .collect()
}

#[test]
fn a_latin_ligature_an_emoji_zwj_cluster_and_a_shaped_owner_share_one_row() {
    let _guard = crate::test_lock::render_globals_lock();
    let (primary, mut atlas) = mixed_atlas();
    let fonts = Fonts(primary.clone());
    // a -> b, space, the family ZWJ cluster (two cells), space, the conjunct.
    let text = format!("a->b {FAMILY} {CONJUNCT}");
    let terminal = terminal(&text, 16);
    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.cells[5].grapheme(), FAMILY);
    assert!(snapshot.cells[6].wide_continuation);
    assert_eq!(snapshot.cells[8].grapheme(), CONJUNCT);
    assert!(snapshot.cells[9].wide_continuation);
    assert_eq!(row_text(&terminal, 0), text, "copy stays logical");

    let emoji_font = EmojiFont::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fonts/color-keycap.ttf"),
    )
    .expect("color fixture");
    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let color_runs =
        EmojiRasterizer::from_font(emoji_font).build_color_glyph_runs(&snapshot, &mut color_atlas);
    assert_eq!(color_runs.len(), 1);
    assert_eq!(
        (color_runs[0].column, color_runs[0].covered_columns),
        (5, 2)
    );

    let mut runs = LigatureShaper::new().build_runs(true, &snapshot, &fonts, &color_runs);
    for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
        atlas.ensure_shaped(&primary, glyph.key);
    }
    assert_eq!(complex_spans(&runs), vec![(0, 1, 3)], "the -> ligature");
    let latin_only = runs.clone();
    ensure_cells(&mut atlas, &primary, &snapshot);
    let complex = ComplexShaper::new().build_runs(true, &snapshot, &fonts, &mut atlas, &color_runs);
    assert_eq!(complex_spans(&complex), vec![(0, 8, 10)]);
    merge_runs(&mut runs, complex);
    assert_eq!(complex_spans(&runs), vec![(0, 1, 3), (0, 8, 10)]);
    for run in &runs {
        assert!(
            !(run.start..run.end).any(|column| (5..7).contains(&column)),
            "no overlay covers the color cluster: {run:?}"
        );
    }

    let drawn = frame(&snapshot, &atlas, &runs);
    // The Latin and emoji side is exactly the ligature-only frame of the row
    // without the owner, whose shaped ink stays inside its span.
    let without_owner = blanked(&snapshot, 0, 8..10);
    same_pixels(
        &drawn.columns(0..8),
        &frame(&without_owner, &atlas, &latin_only).columns(0..8),
        "Latin and emoji columns",
    );
    // The owner is exactly the owner drawn alone.
    let alone = owner_alone(&mut atlas, &fonts, CONJUNCT, 16);
    same_pixels(&drawn.columns(8..10), &alone.columns(0..2), "owner");
    assert_eq!(terminal.snapshot(), snapshot, "presentation only");
}

#[test]
fn wide_and_ambiguous_cells_beside_shaped_owners_keep_their_columns() {
    let _guard = crate::test_lock::render_globals_lock();
    let (primary, mut atlas) = mixed_atlas();
    let fonts = Fonts(primary.clone());
    // A CJK wide owner, the conjunct, the ambiguous middle dot, the conjunct.
    let text = format!("\u{754C}{CONJUNCT}\u{00B7}{CONJUNCT}");
    for (ambiguous_wide, second) in [(false, 5), (true, 6)] {
        let mut term = Terminal::new(12, 1);
        term.set_ambiguous_wide(ambiguous_wide);
        term.advance(b"\x1b[?25l");
        term.advance(text.as_bytes());
        let snapshot = term.snapshot();
        assert!(snapshot.cells[1].wide_continuation);
        assert_eq!(snapshot.cells[2].grapheme(), CONJUNCT);
        assert_eq!(snapshot.cells[4].ch, '\u{00B7}');
        assert_eq!(snapshot.cells[5].wide_continuation, ambiguous_wide);
        assert_eq!(snapshot.cells[second].grapheme(), CONJUNCT);
        assert_eq!(row_text(&term, 0), text, "copy stays logical");
        ensure_cells(&mut atlas, &primary, &snapshot);
        let runs = ComplexShaper::new().build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
        assert_eq!(
            complex_spans(&runs),
            vec![(0, 2, 4), (0, second, second + 2)],
            "ambiguous wide {ambiguous_wide}"
        );
        let drawn = frame(&snapshot, &atlas, &runs);
        let alone = owner_alone(&mut atlas, &fonts, CONJUNCT, 12);
        same_pixels(&drawn.columns(2..4), &alone.columns(0..2), "first owner");
        same_pixels(
            &drawn.columns(second..second + 2),
            &alone.columns(0..2),
            "second owner",
        );
        // The wide and ambiguous neighbours draw as they do without owners.
        let neighbours = frame(
            &blanked(&blanked(&snapshot, 0, 2..4), 0, second..second + 2),
            &atlas,
            &[],
        );
        same_pixels(&drawn.columns(0..2), &neighbours.columns(0..2), "wide cell");
        same_pixels(
            &drawn.columns(4..second),
            &neighbours.columns(4..second),
            "ambiguous cell",
        );
    }
}

#[test]
fn a_shaped_owner_moves_as_one_unit_inside_a_right_to_left_isolate() {
    let _guard = crate::test_lock::render_globals_lock();
    let (primary, mut atlas) = mixed_atlas();
    let fonts = Fonts(primary.clone());
    // x, then RLI (retained on the x owner: an unattached format control is
    // not), alef bet gimel, space, the conjunct, space, dalet, PDI, and a
    // Latin ligature after the isolate. The isolate reverses at level 1, so
    // the owner moves from columns 5..7 to 3..5.
    let text = format!("x\u{2067}\u{05D0}\u{05D1}\u{05D2} {CONJUNCT} \u{05D3}\u{2069} a->b");
    let term = terminal(&text, 20);
    let snapshot = term.snapshot();
    assert_eq!(row_text(&term, 0), text, "copy stays logical");
    let owner = (0..20)
        .find(|&c| snapshot.cells[c].ch == '\u{0915}')
        .expect("owner cell");
    assert!(snapshot.cells[owner + 1].wide_continuation);
    let wrapped: Vec<bool> = term
        .visible_search_rows(0)
        .iter()
        .map(|row| row.wrapped)
        .collect();
    let map = BidiDisplayMap::plan(&snapshot, &wrapped);
    assert!(map.row_is_reordered(0));
    let visual = [map.visual_column(0, owner), map.visual_column(0, owner + 1)];
    assert_eq!(visual[1], visual[0] + 1, "the owner keeps its cell order");
    assert_eq!(
        (owner, visual[0]),
        (5, 3),
        "the owner moves under reordering"
    );

    ensure_cells(&mut atlas, &primary, &snapshot);
    let mut runs = LigatureShaper::new().build_runs_bidi(&snapshot, &fonts, &[], &map);
    for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
        atlas.ensure_shaped(&primary, glyph.key);
    }
    let complex = ComplexShaper::new().build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
    assert_eq!(complex_spans(&complex), vec![(0, owner, owner + 2)]);
    merge_runs(&mut runs, complex);
    assert!(
        runs.iter()
            .any(|run| run.start > owner + 1 && run.end - run.start == 2),
        "the -> ligature survives beside the isolate: {runs:?}"
    );
    let mut verts = Vec::new();
    build_cell_vertices_with_bidi_into(&mut verts, &snapshot, &atlas, &[], &runs, &map);
    let drawn = composite(&snapshot, &atlas, &verts);
    let alone = owner_alone(&mut atlas, &fonts, CONJUNCT, 20);
    same_pixels(
        &drawn.columns(visual[0]..visual[0] + 2),
        &alone.columns(0..2),
        "the owner draws whole at its visual columns",
    );
    assert_eq!(term.snapshot(), snapshot, "presentation only");
}

#[test]
fn reflow_moves_shaped_owners_across_wrap_padding_without_splitting_them() {
    let _guard = crate::test_lock::render_globals_lock();
    let (primary, mut atlas) = mixed_atlas();
    let fonts = Fonts(primary.clone());
    let text = format!("abc{CONJUNCT}d{CONJUNCT}{FAMILY}z");
    let mut term = Terminal::new(16, 6);
    term.advance(b"\x1b[?25l");
    term.advance(text.as_bytes());
    let alone = owner_alone(&mut atlas, &fonts, CONJUNCT, 16);
    for columns in [4, 5, 7, 3, 16] {
        term.resize(columns, 6);
        let snapshot = term.snapshot();
        let found = term.search(&text, crate::core::SearchOptions::case_sensitive());
        assert_eq!(found.len(), 1, "logical search at {columns} columns");
        let mut owners = Vec::new();
        for (row, cells) in snapshot.cells.chunks(columns).enumerate() {
            for (column, cell) in cells.iter().enumerate() {
                if cell.grapheme() == CONJUNCT {
                    assert!(column + 1 < columns, "an owner never straddles a row");
                    assert!(cells[column + 1].wide_continuation);
                    owners.push((row, column, column + 2));
                }
            }
        }
        assert_eq!(owners.len(), 2, "both owners survive at {columns} columns");
        if columns == 4 {
            assert!(
                snapshot.cells[3].layout_padding,
                "the first owner wraps past generated padding"
            );
        }
        let mut selected = String::new();
        let rows = snapshot.dimensions.rows;
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 0 },
            end: CellPoint {
                row: rows - 1,
                column: columns - 1,
            },
        };
        selected.push_str(selected_text(&snapshot, range).trim_end());
        assert_eq!(
            selected.replace('\n', ""),
            text,
            "copy stays logical at {columns} columns"
        );
        ensure_cells(&mut atlas, &primary, &snapshot);
        let runs = ComplexShaper::new().build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
        assert_eq!(complex_spans(&runs), owners, "{columns} columns");
        let drawn = frame(&snapshot, &atlas, &runs);
        let row_height = drawn.px.len() / drawn.width / rows;
        for &(row, start, end) in &owners {
            let block = drawn.columns(start..end);
            let width = (end - start) * drawn.cell_w;
            let band = &block[row * row_height * width..(row + 1) * row_height * width];
            same_pixels(
                band,
                &alone.columns(0..2),
                &format!("row {row} at {columns}"),
            );
        }
    }
}

#[test]
fn snapshot_restore_round_trips_shaped_emoji_and_bidi_rows() {
    let _guard = crate::test_lock::render_globals_lock();
    let (primary, mut atlas) = mixed_atlas();
    let fonts = Fonts(primary.clone());
    let text = format!(
        "\u{05D0}\u{05D1} {CONJUNCT} {FAMILY}\r\na->b \u{0930}\u{094D}\u{0915}\u{0628}\u{064E}"
    );
    let mut term = Terminal::new(12, 4);
    term.advance(b"\x1b[?25l");
    term.advance(text.as_bytes());
    let envelope = SnapshotEnvelope::from_terminal(&term, SnapshotCaptureLimits::default());
    let decoded =
        SnapshotEnvelope::decode(&envelope.encode().unwrap(), SnapshotEnvelopeCaps::default())
            .unwrap();
    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
    let before = term.snapshot();
    let after = restored.snapshot();
    assert_eq!(after.cells, before.cells);
    assert_eq!(row_text(&restored, 0), row_text(&term, 0));
    assert_eq!(row_text(&restored, 1), row_text(&term, 1));
    let wrapped = |t: &Terminal| -> Vec<bool> {
        t.visible_search_rows(0)
            .iter()
            .map(|row| row.wrapped)
            .collect()
    };
    let map_before = BidiDisplayMap::plan(&before, &wrapped(&term));
    let map_after = BidiDisplayMap::plan(&after, &wrapped(&restored));
    assert!(map_before.row_is_reordered(0));
    for column in 0..12 {
        assert_eq!(
            map_after.visual_column(0, column),
            map_before.visual_column(0, column)
        );
    }
    ensure_cells(&mut atlas, &primary, &before);
    let runs = ComplexShaper::new().build_runs(true, &before, &fonts, &mut atlas, &[]);
    assert_eq!(runs.len(), 2, "{runs:?}");
    let restored_runs = ComplexShaper::new().build_runs(true, &after, &fonts, &mut atlas, &[]);
    assert_eq!(restored_runs, runs);
    same_pixels(
        &frame(&after, &atlas, &restored_runs).px,
        &frame(&before, &atlas, &runs).px,
        "restored frame",
    );
}

#[test]
fn an_image_placed_beside_a_shaped_row_leaves_the_owner_and_its_run_intact() {
    let _guard = crate::test_lock::render_globals_lock();
    let (primary, mut atlas) = mixed_atlas();
    let fonts = Fonts(primary.clone());
    let mut term = Terminal::new(12, 4);
    term.advance(b"\x1b[?25l");
    term.advance(format!("{CONJUNCT}x").as_bytes());
    let before = term.snapshot();
    ensure_cells(&mut atlas, &primary, &before);
    let runs = ComplexShaper::new().build_runs(true, &before, &fonts, &mut atlas, &[]);
    assert_eq!(complex_spans(&runs), vec![(0, 0, 2)]);
    // A 2x2 RGBA image, one cell, placed at the cursor after the owner.
    // Sixteen 0xFF bytes in base64.
    let payload = "/////////////////////w==";
    term.advance(format!("\x1b_Ga=T,f=32,t=d,s=2,v=2,c=1,r=1;{payload}\x1b\\").as_bytes());
    let placed = term.visible_graphics(0);
    assert_eq!(placed.len(), 1);
    assert_eq!((placed[0].row, placed[0].column), (0, 3));
    let after = term.snapshot();
    assert_eq!(after.cells[0].grapheme(), CONJUNCT);
    assert!(after.cells[1].wide_continuation);
    assert_eq!(row_text(&term, 0), format!("{CONJUNCT}x"));
    let again = ComplexShaper::new().build_runs(true, &after, &fonts, &mut atlas, &[]);
    assert_eq!(again, runs);
    same_pixels(
        &frame(&after, &atlas, &again).columns(0..2),
        &frame(&before, &atlas, &runs).columns(0..2),
        "owner beside the image",
    );
}

#[test]
fn every_recorded_frame_of_a_split_stream_shapes_only_complete_owners() {
    let _guard = crate::test_lock::render_globals_lock();
    let (primary, mut atlas) = mixed_atlas();
    let fonts = Fonts(primary.clone());
    // The replay recorder keeps the snapshot after each read; each such frame
    // is shaped as the overlay draws it.
    let text = format!("{CONJUNCT}\u{05D0}{FAMILY}\u{0930}\u{094D}\u{0915}a->b");
    let bytes = text.as_bytes();
    let mut whole = Terminal::new(16, 1);
    whole.advance(b"\x1b[?25l");
    whole.advance(bytes);
    let final_snapshot = whole.snapshot();
    ensure_cells(&mut atlas, &primary, &final_snapshot);
    let mut shaper = ComplexShaper::new();
    let final_runs = shaper.build_runs(true, &final_snapshot, &fonts, &mut atlas, &[]);
    assert_eq!(complex_spans(&final_runs), vec![(0, 0, 2), (0, 5, 7)]);
    for chunk in 1..=3 {
        let mut term = Terminal::new(16, 1);
        term.advance(b"\x1b[?25l");
        for piece in bytes.chunks(chunk) {
            term.advance(piece);
            let frame_snapshot = term.snapshot();
            ensure_cells(&mut atlas, &primary, &frame_snapshot);
            let runs = shaper.build_runs(true, &frame_snapshot, &fonts, &mut atlas, &[]);
            for run in &runs {
                let owner = &frame_snapshot.cells[run.start];
                assert!(owner_is_eligible(owner));
                let span = 1 + usize::from(
                    frame_snapshot
                        .cells
                        .get(run.start + 1)
                        .is_some_and(|cell| cell.wide_continuation),
                );
                assert_eq!(run.end - run.start, span, "a run covers its whole owner");
            }
        }
        let replayed = term.snapshot();
        assert_eq!(replayed.cells, final_snapshot.cells);
        let runs = shaper.build_runs(true, &replayed, &fonts, &mut atlas, &[]);
        assert_eq!(runs, final_runs, "{chunk}-byte reads end on the same runs");
        same_pixels(
            &frame(&replayed, &atlas, &runs).px,
            &frame(&final_snapshot, &atlas, &final_runs).px,
            "replayed final frame",
        );
    }
}

#[test]
fn ligature_and_script_switches_each_keep_their_own_scope_on_one_row() {
    let _guard = crate::test_lock::render_globals_lock();
    // The project-authored bidi fixture carries the `->` ligature and Arabic
    // joining; the Devanagari subset is the fallback for the owner.
    let primary = latin_face();
    let fonts = Fonts(primary.clone());
    let text = format!("a->b \u{0628}\u{0644}\u{0627} {CONJUNCT}");
    let term = terminal(&text, 14);
    let snapshot = term.snapshot();
    assert_eq!(snapshot.cells[9].grapheme(), CONJUNCT);
    assert_eq!(row_text(&term, 0), text, "copy stays logical");
    let draw = |switches: ShapingSwitches| {
        let mut atlas = GlyphAtlas::build(&primary, PX);
        atlas.set_fallback_fonts(vec![Arc::new(face("Devanagari-subset.ttf"))]);
        ensure_cells(&mut atlas, &primary, &snapshot);
        let mut runs = LigatureShaper::new().build_runs_with_switches(
            switches,
            &snapshot,
            &fonts,
            &[],
            LatinShapingFeatures::default(),
            None,
        );
        for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
            atlas.ensure_shaped(&primary, glyph.key);
        }
        let complex = ComplexShaper::new().build_runs_with_switches(
            switches,
            &snapshot,
            &fonts,
            &mut atlas,
            &[],
        );
        merge_runs(&mut runs, complex);
        let drawn = frame(&snapshot, &atlas, &runs);
        (complex_spans(&runs), drawn)
    };
    let switches = |ligatures, scripts| ShapingSwitches { ligatures, scripts };
    let (both, both_px) = draw(switches(true, true));
    let (latin_only, latin_px) = draw(switches(true, false));
    let (scripts_only, scripts_px) = draw(switches(false, true));
    let (none, none_px) = draw(switches(false, false));
    let arabic: Vec<_> = both
        .iter()
        .copied()
        .filter(|&(_, start, end)| start >= 5 && end <= 8)
        .collect();
    assert!(!arabic.is_empty(), "Arabic joins: {both:?}");
    let mut expected = vec![(0, 0, 4)];
    expected.extend(arabic.iter().copied());
    expected.push((0, 9, 11));
    assert_eq!(both, expected);
    assert_eq!(
        latin_only,
        vec![(0, 0, 4)],
        "script_shaping off keeps the a->b run"
    );
    let mut scripts = arabic.clone();
    scripts.push((0, 9, 11));
    assert_eq!(
        scripts_only, scripts,
        "ligatures off keeps Arabic and the owner"
    );
    assert!(none.is_empty());
    // Each switch changes only the pixels of its own scope.
    same_pixels(
        &latin_px.columns(0..5),
        &both_px.columns(0..5),
        "Latin, scripts off",
    );
    same_pixels(
        &latin_px.columns(5..14),
        &none_px.columns(5..14),
        "scripts off",
    );
    same_pixels(
        &scripts_px.columns(5..14),
        &both_px.columns(5..14),
        "scripts, ligatures off",
    );
    same_pixels(
        &scripts_px.columns(0..5),
        &none_px.columns(0..5),
        "ligatures off",
    );
    assert!(
        latin_px.columns(0..5) != none_px.columns(0..5),
        "the arrow draws"
    );
    assert!(
        scripts_px.columns(5..14) != none_px.columns(5..14),
        "the scripts draw"
    );
    assert_eq!(term.snapshot(), snapshot, "presentation only");
}
