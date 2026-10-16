// SPDX-License-Identifier: GPL-3.0-only
//! Paragraph properties in the display map: right-to-left and auto paragraph
//! levels from SCP and the setting default, explicit mode from BDSM, layout
//! padding at the row end in the paragraph direction, and the joining rule
//! for explicit paragraphs.

use super::*;
use crate::core::{BidiBaseDirection, Terminal};
use crate::ligature::{LigatureFonts, LigatureShaper};
use crate::text::{FontHandle, FontStyle};

fn terminal(text: &str, cols: usize, rows: usize) -> Terminal {
    let mut terminal = Terminal::new(cols, rows);
    terminal.advance(text.as_bytes());
    terminal
}

/// The map the live frame plans, with `default` as the setting.
fn plan(terminal: &Terminal, default: BidiBaseDirection) -> BidiDisplayMap {
    let snapshot = terminal.snapshot();
    let (wrapped, properties): (Vec<bool>, Vec<_>) = terminal
        .visible_search_rows(0)
        .iter()
        .map(|row| (row.wrapped, row.paragraph_bidi))
        .unzip();
    let (rows, overflow) =
        terminal.paragraph_context_rows(0, max_bidi_context_rows(snapshot.dimensions.columns));
    let context = BidiParagraphContext {
        properties: rows
            .first()
            .map(|row| row.paragraph_bidi)
            .unwrap_or_default(),
        rows: rows.into_iter().map(|row| row.cells).collect(),
        overflow,
    };
    BidiDisplayMap::plan_with_paragraphs(
        &snapshot,
        &wrapped,
        &context,
        BidiParagraphs {
            properties: &properties,
            default_direction: default,
        },
    )
}

fn visual_row(map: &BidiDisplayMap, row: usize, columns: usize) -> Vec<usize> {
    (0..columns).map(|c| map.visual_column(row, c)).collect()
}

/// I-24 for one map: per row, visual columns are a permutation and the
/// logical map inverts it.
fn assert_rows_reversible(map: &BidiDisplayMap, rows: usize, columns: usize) {
    for row in 0..rows {
        let mut visual = visual_row(map, row, columns);
        for (column, visual_column) in visual.iter().enumerate() {
            assert_eq!(map.logical_column(row, *visual_column), column);
        }
        visual.sort_unstable();
        assert_eq!(visual, (0..columns).collect::<Vec<_>>(), "row {row}");
    }
}

#[test]
fn rtl_paragraph_places_logical_column_zero_at_right_edge() {
    let terminal = terminal("\x1b[2 k\u{05D0}\u{05D1} xy", 8, 2);
    let map = plan(&terminal, BidiBaseDirection::LeftToRight);
    assert_eq!(visual_row(&map, 0, 8), [7, 6, 5, 3, 4, 2, 1, 0]);
    assert_eq!(map.level(0, 0), 1);
    assert_eq!(map.level(0, 3), 2);
    // Row 1 was never reached by a line feed, so it keeps the power-on
    // value and the setting's left-to-right direction.
    assert!(!map.row_is_reordered(1));
    assert_rows_reversible(&map, 2, 8);
}

#[test]
fn scp_default_follows_the_setting_and_scp_one_overrides_it() {
    let terminal = terminal("ab\r\n\x1b[1 kcd", 6, 2);
    let map = plan(&terminal, BidiBaseDirection::RightToLeft);
    assert_eq!(visual_row(&map, 0, 6), [4, 5, 3, 2, 1, 0]);
    assert!(!map.row_is_reordered(1));
    let map = plan(&terminal, BidiBaseDirection::LeftToRight);
    assert!(map.is_identity());
}

#[test]
fn auto_paragraph_resolves_first_strong_rtl() {
    let terminal = terminal("1 \u{05D0}b\r\n1 a\u{05D0}", 6, 2);
    let map = plan(&terminal, BidiBaseDirection::Auto);
    // Row 0: the first strong character is alef, so the paragraph is right
    // to left and the digit sits at the right edge.
    assert_eq!(map.visual_column(0, 0), 5);
    assert_eq!(map.level(0, 3), 2, "b is left to right inside it");
    // Row 1: the first strong character is a, left to right.
    assert_eq!(map.visual_column(1, 0), 0);
    assert_rows_reversible(&map, 2, 6);
}

#[test]
fn a_paragraph_opening_in_scrollback_takes_its_first_rows_properties() {
    // SCP 2 stamps the first row; the paragraph wraps over three rows of a
    // two-row screen, so its first row is in scrollback.
    let terminal = terminal("\x1b[2 kabcdefghij", 4, 2);
    let map = plan(&terminal, BidiBaseDirection::LeftToRight);
    assert!(
        map.row_is_reordered(0),
        "the context row's properties apply"
    );
    assert_eq!(map.level(0, 0), 2);
}

#[test]
fn explicit_rtl_reverses_columns_and_mirrors() {
    let terminal = terminal("\x1b[8l\x1b[2 k(\u{05D0}b[", 6, 1);
    let map = plan(&terminal, BidiBaseDirection::LeftToRight);
    assert!(map.row_is_explicit(0));
    assert_eq!(visual_row(&map, 0, 6), [5, 4, 3, 2, 1, 0]);
    assert!((0..6).all(|c| map.level(0, c) == 1));
    assert_eq!(map.mirrored_char(0, 0), Some(')'));
    assert_eq!(map.mirrored_char(0, 3), Some(']'));
    assert_eq!(map.mirrored_char(0, 1), None);
}

#[test]
fn explicit_ltr_is_identity_even_with_right_to_left_text() {
    for default in [
        BidiBaseDirection::LeftToRight,
        BidiBaseDirection::RightToLeft,
        BidiBaseDirection::Auto,
    ] {
        // Explicit mode with the default direction: right to left only when
        // the setting says so; auto counts as left to right.
        let default_mode = terminal("\x1b[8l\u{05D0}\u{05D1} xy", 6, 1);
        let map = plan(&default_mode, default);
        assert_eq!(
            map.is_identity(),
            default != BidiBaseDirection::RightToLeft,
            "{default:?}"
        );
        let selected = terminal("\x1b[8l\x1b[1 k\u{05D0}\u{05D1} xy", 6, 1);
        assert!(plan(&selected, default).is_identity(), "{default:?}");
    }
}

#[test]
fn layout_padding_is_excluded_and_trails_in_paragraph_direction() {
    // "abcd" fills four of five columns; the wide character does not fit,
    // leaves a layout padding cell at column 4, and wraps.
    // Right to left: alef is at the right edge, "bcd" is one left-to-right
    // run before it, and the padding is the row end, the left edge. Left to
    // right: the padding stays at the right edge.
    for (scp, expected, level) in [
        ("\x1b[2 k", [4, 1, 2, 3, 0], 1),
        ("\x1b[1 k", [0, 1, 2, 3, 4], 0),
    ] {
        let terminal = terminal(&format!("{scp}\u{05D0}bcd\u{4E16}"), 5, 2);
        assert!(
            terminal.snapshot().cells[4].layout_padding,
            "fixture is live"
        );
        let map = plan(&terminal, BidiBaseDirection::LeftToRight);
        assert_eq!(visual_row(&map, 0, 5), expected, "{scp:?}");
        assert_eq!(map.level(0, 4), level, "padding takes the paragraph level");
        assert_rows_reversible(&map, 2, 5);
    }
}

fn font() -> FontHandle {
    FontHandle::try_from_vec(include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec())
        .expect("bidi fixture font parses")
}

struct Fonts(FontHandle);

impl LigatureFonts for Fonts {
    fn ligature_font(&self, _style: FontStyle) -> &FontHandle {
        &self.0
    }
}

#[test]
fn explicit_paragraph_draws_no_joining_overlay() {
    let _render = crate::test_lock::render_globals_lock();
    let fonts = Fonts(font());
    let word = "\u{0628}\u{0644}\u{0627}";
    let implicit = terminal(&format!("\x1b[2 k{word}"), 8, 1);
    let map = plan(&implicit, BidiBaseDirection::LeftToRight);
    let runs = LigatureShaper::new().build_runs_bidi(&implicit.snapshot(), &fonts, &[], &map);
    assert!(!runs.is_empty(), "an implicit paragraph joins the run");
    let explicit = terminal(&format!("\x1b[8l\x1b[2 k{word}"), 8, 1);
    let map = plan(&explicit, BidiBaseDirection::LeftToRight);
    assert!(map.row_is_explicit(0));
    let runs = LigatureShaper::new().build_runs_bidi(&explicit.snapshot(), &fonts, &[], &map);
    assert!(
        runs.is_empty(),
        "explicit text is drawn cell by cell: {runs:?}"
    );
}

#[test]
fn explicit_and_implicit_right_to_left_rows_hash_apart() {
    let implicit = plan(
        &terminal("\x1b[2 k\u{05D0}", 4, 1),
        BidiBaseDirection::LeftToRight,
    );
    let explicit = plan(
        &terminal("\x1b[8l\x1b[2 k\u{05D0}", 4, 1),
        BidiBaseDirection::LeftToRight,
    );
    assert_eq!(visual_row(&implicit, 0, 4), visual_row(&explicit, 0, 4));
    assert_ne!(implicit.placement_hash(), explicit.placement_hash());
}
