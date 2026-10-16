// SPDX-License-Identifier: GPL-3.0-only
//! A shaped owner whose ink is wider than its cells borrows the following
//! blank cell: drawn larger across one more cell, with widths, ownership,
//! selection, copy, and the blank's own content unchanged.

use super::*;
use crate::complex_shaping::{BorrowRules, RowTexts, embed_blocked_rows};
use crate::core::CursorStyle;
use crate::graphics::{GraphicsProtocol, PlacementId, SourceRect, StoredImageId, VisiblePlacement};
use crate::grid::{CursorRenderParams, append_cursor_vertices_with_runs};

/// A Tamil split-vowel owner (ka, au) whose shaped ink is about four cells
/// wide in its two cells.
const TAMIL: &str = "\u{0B95}\u{0BCC}";
/// A Javanese owner (ka, taling, tarung) wider than its cells.
const JAVANESE: &str = "\u{A98F}\u{A9BA}\u{A9B4}";

/// Runs for `snapshot` with `font` as the style face, borrowing under
/// `rules` (`None` never borrows).
fn runs_with(
    snapshot: &Snapshot,
    font: &FontHandle,
    atlas: &mut GlyphAtlas,
    rules: Option<&BorrowRules>,
) -> Vec<LigatureRun> {
    ComplexShaper::new().build_runs_borrowing(
        true,
        snapshot,
        &Fonts(font.clone()),
        atlas,
        &[],
        rules,
    )
}

fn setup(font_name: &str, text: &str, columns: usize) -> (FontHandle, GlyphAtlas, Snapshot) {
    let font = face(font_name);
    let snapshot = terminal(text, columns).snapshot();
    let mut atlas = GlyphAtlas::build(&font, PX);
    ensure_cells(&mut atlas, &font, &snapshot);
    (font, atlas, snapshot)
}

/// The run's cluster key span, in cells.
fn key_span(run: &LigatureRun) -> usize {
    usize::from(run.glyphs[0].key.span_cells)
}

/// Inked pixel columns of `frame` against the blank `background`.
fn inked_columns(frame: &Frame, background: [f32; 3]) -> std::collections::BTreeSet<usize> {
    frame
        .px
        .iter()
        .enumerate()
        .filter(|(_, pixel)| **pixel != background)
        .map(|(index, _)| index % frame.width)
        .collect()
}

/// Fails before borrowing: the owner was fitted into its own two cells at
/// about 0.6 scale. With a blank after it the owner now draws across three
/// cells, wider than before, exactly as the reference cluster rasterized
/// into a three-cell slot, while its run keeps its two source cells.
#[test]
fn tamil_wide_owner_draws_larger_into_following_blank() {
    let _guard = crate::test_lock::render_globals_lock();
    let (font, mut atlas, snapshot) = setup("Tamil-subset.ttf", &format!("{TAMIL} "), 6);
    let span = single_owner(&snapshot).expect("one owner");
    assert_eq!(span, 2);
    let plain = runs_with(&snapshot, &font, &mut atlas, None);
    let borrowed = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(6)));
    assert_eq!(key_span(&plain[0]), span);
    assert_eq!(
        (borrowed[0].start, borrowed[0].end),
        (0, span),
        "source cells"
    );
    assert_eq!(key_span(&borrowed[0]), span + 1, "one borrowed cell");
    assert_eq!(borrowed[0].paint_end(), span + 1);

    let blank = frame(&terminal("", 6).snapshot(), &atlas, &[]);
    let background = blank.px[0];
    let before = frame(&snapshot, &atlas, &plain);
    let after = frame(&snapshot, &atlas, &borrowed);
    let cell_w = after.cell_w;
    let before_ink = inked_columns(&before, background);
    let after_ink = inked_columns(&after, background);
    assert!(before_ink.iter().all(|&x| x < span * cell_w));
    assert!(
        after_ink.iter().any(|&x| x >= span * cell_w),
        "ink reaches the blank"
    );
    assert!(
        after_ink.iter().all(|&x| x < (span + 1) * cell_w),
        "never past the borrowed cell"
    );
    assert!(
        after_ink.len() > before_ink.len(),
        "drawn larger: {} vs {} inked columns",
        after_ink.len(),
        before_ink.len()
    );
    let data = crate::complex_shaping::shaper_data(&font).expect("shaper data");
    let shaped = crate::complex_shaping::shape_owner(&data, TAMIL).expect("shaped");
    let oracle = oracle_run(&mut atlas, &font, true, &shaped, 0, span + 1);
    let oracle = LigatureRun {
        end: span,
        ..oracle
    };
    assert_eq!(after, frame(&snapshot, &atlas, &[oracle]));
}

/// Fails before borrowing. A Javanese owner borrows exactly one cell, and
/// its ink stays inside that cell.
#[test]
fn javanese_owner_borrows_one_cell() {
    let _guard = crate::test_lock::render_globals_lock();
    let (font, mut atlas, snapshot) = setup("Javanese-subset.ttf", &format!("{JAVANESE}  "), 8);
    let span = single_owner(&snapshot).expect("one owner");
    let runs = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(8)));
    assert_eq!(runs.len(), 1);
    assert_eq!(key_span(&runs[0]), span + 1);
    let blank = frame(&terminal("", 8).snapshot(), &atlas, &[]);
    let drawn = frame(&snapshot, &atlas, &runs);
    assert_eq!(
        drawn.columns(span + 1..8),
        blank.columns(span + 1..8),
        "nothing past the one borrowed cell"
    );
}

/// An owner whose ink fits its own cells takes its own slot even when a
/// blank follows.
#[test]
fn an_owner_that_fits_does_not_borrow() {
    let _guard = crate::test_lock::render_globals_lock();
    let (font, mut atlas, snapshot) =
        setup("Devanagari-subset.ttf", "\u{0915}\u{094D}\u{0937} ", 6);
    let span = single_owner(&snapshot).expect("one owner");
    let plain = runs_with(&snapshot, &font, &mut atlas, None);
    let borrowed = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(6)));
    assert_eq!(key_span(&borrowed[0]), span);
    assert_eq!(borrowed, plain);
}

#[test]
fn no_borrow_into_nonblank() {
    let _guard = crate::test_lock::render_globals_lock();
    for text in [
        format!("{TAMIL}x"),
        format!("{TAMIL}\u{4E00}"),
        format!("{TAMIL} \u{0301}"),
        format!("{TAMIL}{TAMIL}"),
    ] {
        let (font, mut atlas, snapshot) = setup("Tamil-subset.ttf", &text, 8);
        let runs = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(8)));
        assert_eq!(runs[0].paint_end(), runs[0].end, "{text:?}: {runs:?}");
    }
    // A hidden blank does not lend either.
    let (font, mut atlas, _) = setup("Tamil-subset.ttf", "", 6);
    let snapshot = terminal(&format!("{TAMIL}\x1b[8m \x1b[0m"), 6).snapshot();
    ensure_cells(&mut atlas, &font, &snapshot);
    let runs = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(6)));
    assert_eq!(runs[0].paint_end(), runs[0].end);
}

#[test]
fn no_borrow_at_last_column() {
    let _guard = crate::test_lock::render_globals_lock();
    let (font, mut atlas, snapshot) = setup("Tamil-subset.ttf", &format!("  {TAMIL}"), 4);
    assert!(snapshot.cells[3].wide_continuation);
    let runs = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(4)));
    assert_eq!((runs[0].start, runs[0].paint_end()), (2, 4));
}

/// An owner ending a soft-wrapped row never borrows the next row's first
/// cell.
#[test]
fn no_borrow_across_wrap() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = face("Tamil-subset.ttf");
    let mut term = Terminal::new(4, 2);
    term.advance(b"\x1b[?25l");
    term.advance(format!("  {TAMIL} x").as_bytes());
    let snapshot = term.snapshot();
    assert!(term.visible_search_rows(0)[0].wrapped);
    assert_eq!(snapshot.cells[4].ch, ' ');
    let mut atlas = GlyphAtlas::build(&font, PX);
    ensure_cells(&mut atlas, &font, &snapshot);
    let runs = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(4)));
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].paint_end(), runs[0].end);
}

fn placement(row: usize) -> VisiblePlacement {
    VisiblePlacement {
        id: PlacementId(1),
        image_id: StoredImageId(1),
        protocol: GraphicsProtocol::Sixel,
        row,
        column: 0,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        },
        display_columns: 1,
        display_rows: 1,
        pixel_offset_x: 0,
        pixel_offset_y: 0,
        z_index: 0,
        generation: 0,
    }
}

/// Rows an image covers never lend; a Kitty placeholder cell is not blank.
#[test]
fn no_borrow_under_image_or_placeholder() {
    let _guard = crate::test_lock::render_globals_lock();
    let (font, mut atlas, snapshot) = setup("Tamil-subset.ttf", &format!("{TAMIL} "), 6);
    let texts = RowTexts::capture(&snapshot);
    assert_eq!(texts.blocked_rows(&snapshot, &[]), vec![false]);
    let blocked = texts.blocked_rows(&snapshot, &[placement(0)]);
    assert_eq!(blocked, vec![true]);
    let rules = BorrowRules {
        map: None,
        blocked: &blocked,
        columns: 0..6,
    };
    let runs = runs_with(&snapshot, &font, &mut atlas, Some(&rules));
    assert_eq!(runs[0].paint_end(), runs[0].end);

    let mut with_placeholder = snapshot.clone();
    with_placeholder.cells[2].ch = crate::core::PLACEHOLDER_CHAR;
    let runs = runs_with(
        &with_placeholder,
        &font,
        &mut atlas,
        Some(&BorrowRules::open(6)),
    );
    assert_eq!(runs[0].paint_end(), runs[0].end);
}

/// Rows the overlay painters rewrote never lend, attribute-only paint
/// (selection, search highlights) keeps a row lending, and a row that held
/// no multi-scalar owner before painting never lends.
#[test]
fn rows_overlay_painters_rewrote_never_lend() {
    let mut term = Terminal::new(6, 3);
    term.advance(format!("{TAMIL} \r\n{TAMIL} \r\nab").as_bytes());
    let snapshot = term.snapshot();
    let texts = RowTexts::capture(&snapshot);
    let mut painted = snapshot.clone();
    painted.cells[0].attrs.set_inverse(true);
    painted.cells[6 + 3].ch = 'q';
    painted.cells[12].push_combining('\u{0BCC}');
    assert_eq!(
        texts.blocked_rows(&painted, &[]),
        vec![false, true, true],
        "highlight keeps row 0, rewritten row 1 and painted owner row 2 never lend"
    );
    assert_eq!(
        embed_blocked_rows(&[false, true], 4, 1),
        vec![true, false, true, true]
    );
    let mut resized = painted.clone();
    resized.dimensions.rows = 2;
    resized.cells.truncate(12);
    assert_eq!(texts.blocked_rows(&resized, &[]), vec![true, true]);
}

/// Bidi: an owner at an odd level never borrows, and on a reordered row the
/// blank must be drawn immediately to the owner's right.
#[test]
fn no_borrow_for_odd_level_owner() {
    let mut term = Terminal::new(8, 1);
    // A pointed Hebrew owner, a blank, and a Hebrew letter: a right-to-left
    // paragraph, so the owner sits at level 1.
    term.advance("\u{05D0}\u{05B8} \u{05D1}".as_bytes());
    let snapshot = term.snapshot();
    let map = BidiDisplayMap::plan(&snapshot, &[false]);
    assert!(map.row_is_reordered(0));
    assert_eq!(map.level(0, 0) % 2, 1);
    let rules = BorrowRules {
        map: Some(&map),
        blocked: &[],
        columns: 0..8,
    };
    let coverage = ColorRunCoverage::new(&[], 8, 1);
    assert!(!rules.allows(&snapshot.cells, 0, 0, 1, &coverage));
    assert!(BorrowRules::open(8).allows(&snapshot.cells, 0, 0, 1, &coverage));

    // An even-level Tamil owner inside a right-to-left isolate lends only
    // when its blank is drawn to its right.
    let mut term = Terminal::new(12, 1);
    term.advance(format!("\u{2067}\u{05D0} {TAMIL} \u{05D1}\u{2069}").as_bytes());
    let snapshot = term.snapshot();
    let map = BidiDisplayMap::plan(&snapshot, &[false]);
    let owner = (0..12)
        .find(|&column| snapshot.cells[column].ch == '\u{0B95}')
        .expect("owner");
    assert_eq!(map.level(0, owner) % 2, 0);
    let rules = BorrowRules {
        map: Some(&map),
        blocked: &[],
        columns: 0..12,
    };
    let adjacent = map.visual_column(0, owner + 2) == map.visual_column(0, owner) + 2;
    assert_eq!(
        rules.allows(&snapshot.cells, 0, owner, 2, &coverage),
        adjacent
    );
}

/// The cursor cell counts as blank, and a block cursor on the borrowed blank
/// redraws the owner's ink there, clipped to the block.
#[test]
fn borrow_survives_cursor_on_blank() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = face("Tamil-subset.ttf");
    let mut term = Terminal::new(6, 1);
    term.advance(format!("{TAMIL} \x1b[1;3H").as_bytes());
    let snapshot = term.snapshot();
    assert!(snapshot.cursor_visible);
    assert_eq!(snapshot.cursor.column, 2);
    let mut atlas = GlyphAtlas::build(&font, PX);
    ensure_cells(&mut atlas, &font, &snapshot);
    let runs = runs_with(&snapshot, &font, &mut atlas, Some(&BorrowRules::open(6)));
    assert_eq!(key_span(&runs[0]), 3, "the cursor cell still lends");
    let mut cursor = Vec::new();
    append_cursor_vertices_with_runs(
        &mut cursor,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        [0.0, 0.0],
        CursorRenderParams::default(),
        None,
        &runs,
    );
    let cell_w = atlas.cell.width as f32;
    let glyphs: Vec<_> = cursor
        .chunks(INSTANCES_PER_QUAD)
        .filter(|quad| quad[0].is_glyph == 1.0)
        .collect();
    assert!(!glyphs.is_empty(), "the block redraws the borrowed ink");
    assert!(
        glyphs
            .iter()
            .all(|quad| quad[0].pos[0] >= 2.0 * cell_w && quad[0].end_pos[0] <= 3.0 * cell_w)
    );
}

/// Selection and copy of the borrowed blank are unchanged: the blank keeps
/// its own highlight background, the owner's ink draws over it, and copy
/// reads the logical text.
#[test]
fn selection_highlight_extent_unchanged_by_borrow() {
    let _guard = crate::test_lock::render_globals_lock();
    let (font, mut atlas, snapshot) = setup("Tamil-subset.ttf", &format!("{TAMIL} x"), 6);
    let range = SelectionRange {
        start: CellPoint { row: 0, column: 2 },
        end: CellPoint { row: 0, column: 3 },
    };
    let mut selected = snapshot.clone();
    crate::selection::apply_highlight(&mut selected, range, None);
    assert_eq!(selected_text(&selected, range), " x");
    let plain = runs_with(&selected, &font, &mut atlas, None);
    let borrowed = runs_with(&selected, &font, &mut atlas, Some(&BorrowRules::open(6)));
    assert_eq!(key_span(&borrowed[0]), 3);
    let backgrounds = |runs: &[LigatureRun]| {
        let mut verts = Vec::new();
        build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
            &mut verts,
            &selected,
            &atlas,
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
        verts
            .chunks(INSTANCES_PER_QUAD)
            .filter(|quad| quad[0].is_glyph == 0.0)
            .map(|quad| (quad[0].pos, quad[0].end_pos, quad[0].color))
            .collect::<Vec<_>>()
    };
    assert_eq!(backgrounds(&borrowed), backgrounds(&plain));
    assert_eq!(
        selected_text(
            &snapshot,
            SelectionRange {
                start: CellPoint { row: 0, column: 0 },
                end: CellPoint { row: 0, column: 5 },
            }
        )
        .trim_end(),
        format!("{TAMIL} x")
    );
}

/// Borrowing is presentation only. The blank keeps its cell, the snapshot is
/// untouched, and writing into the blank (the typed echo) makes the next
/// build draw the owner in its own cells again, through the same warm shaper
/// caches.
#[test]
fn typing_into_borrowed_blank_repaints_owner() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = face("Tamil-subset.ttf");
    let mut term = Terminal::new(6, 1);
    term.advance(b"\x1b[?25l");
    term.advance(format!("{TAMIL} ").as_bytes());
    let mut atlas = GlyphAtlas::build(&font, PX);
    let mut shaper = ComplexShaper::new();
    let fonts = Fonts(font.clone());
    let rules = BorrowRules::open(6);
    let first = term.snapshot();
    ensure_cells(&mut atlas, &font, &first);
    let runs = shaper.build_runs_borrowing(true, &first, &fonts, &mut atlas, &[], Some(&rules));
    assert_eq!(key_span(&runs[0]), 3);
    assert_eq!(term.snapshot(), first, "presentation only");
    assert_eq!(first.cells[2].ch, ' ');

    // The echo of a typed x over the blank.
    term.advance(b"\x08x");
    let second = term.snapshot();
    assert_eq!(second.cells[2].ch, 'x');
    ensure_cells(&mut atlas, &font, &second);
    let runs = shaper.build_runs_borrowing(true, &second, &fonts, &mut atlas, &[], Some(&rules));
    assert_eq!(key_span(&runs[0]), 2, "the owner draws in its own cells");
    let drawn = frame(&second, &atlas, &runs);
    let alone = frame(&terminal("x", 1).snapshot(), &atlas, &[]);
    assert_eq!(
        drawn.columns(2..3),
        alone.columns(0..1),
        "the typed x draws clean"
    );
}

/// On a reordered row an even-level owner whose logical successor is drawn
/// immediately to its right borrows it, and its ink stays inside the owner's
/// visual cells plus that one.
#[test]
fn borrowed_owner_on_a_reordered_row_draws_into_its_visual_neighbor() {
    let _guard = crate::test_lock::render_globals_lock();
    let font = face("Tamil-subset.ttf");
    let mut term = Terminal::new(12, 1);
    term.advance(b"\x1b[?25l");
    term.advance(format!("a \u{05D0}\u{05D1} {TAMIL}  z").as_bytes());
    let snapshot = term.snapshot();
    let map = BidiDisplayMap::plan(&snapshot, &[false]);
    assert!(map.row_is_reordered(0));
    let owner = (0..12)
        .find(|&column| snapshot.cells[column].ch == '\u{0B95}')
        .expect("owner");
    let lead = map.visual_column(0, owner);
    assert_eq!(map.visual_column(0, owner + 2), lead + 2);
    let mut atlas = GlyphAtlas::build(&font, PX);
    ensure_cells(&mut atlas, &font, &snapshot);
    let rules = BorrowRules {
        map: Some(&map),
        blocked: &[],
        columns: 0..12,
    };
    let runs = runs_with(&snapshot, &font, &mut atlas, Some(&rules));
    let run = runs
        .iter()
        .find(|run| run.start == owner)
        .expect("owner run");
    assert_eq!(key_span(run), 3);
    let mut verts = Vec::new();
    build_cell_vertices_with_bidi_into(&mut verts, &snapshot, &atlas, &[], &runs, &map);
    let drawn = composite(&snapshot, &atlas, &verts);
    let mut without = snapshot.clone();
    for column in owner..owner + 2 {
        without.cells[column] = terminal("", 1).snapshot().cells[0];
    }
    let mut verts = Vec::new();
    build_cell_vertices_with_bidi_into(&mut verts, &without, &atlas, &[], &[], &map);
    let rest = composite(&without, &atlas, &verts);
    let cell_w = drawn.cell_w;
    let changed: Vec<usize> = drawn
        .px
        .iter()
        .zip(&rest.px)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(index, _)| index % drawn.width / cell_w)
        .collect();
    assert!(
        changed.contains(&(lead + 2)),
        "ink reaches the borrowed cell"
    );
    assert!(
        changed
            .iter()
            .all(|&column| (lead..lead + 3).contains(&column)),
        "ink stays in the owner's cells and the borrowed one: {changed:?}"
    );
}
