// SPDX-License-Identifier: GPL-3.0-only
// Project-authored font uses the adjacent font-fixture LICENSE.txt.
use odytty::atlas::CellSize;
use odytty::core::{Attrs, Cell, Color, Snapshot, Terminal};
use odytty::emoji::{ColorGlyphAtlas, EmojiFont, EmojiRasterizer};
use odytty::selection::{CellPoint, SelectionRange, SelectionStyle, apply_highlight};
use std::path::PathBuf;

fn rasterizer() -> EmojiRasterizer {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fonts/legacy-color-clusters.ttf");
    EmojiRasterizer::from_font(
        EmojiFont::load(path).expect("project-authored cluster fixture loads"),
    )
}
fn legacy_cells(kind: &str, next_attrs: Attrs) -> Vec<Cell> {
    let attrs = Attrs::default();
    match kind {
        "flag" => vec![Cell::new('🇦', attrs), Cell::new('🇧', next_attrs)],
        "modifier" => vec![
            Cell::new('🔥', attrs),
            Cell::wide_spacer(attrs),
            Cell::new('🏻', next_attrs),
            Cell::wide_spacer(next_attrs),
        ],
        "zwj" => {
            let mut terminal = Terminal::new(2, 1);
            terminal.advance("\u{1f525}\u{200d}".as_bytes());
            let head = terminal.snapshot().cells[0];
            assert_eq!(head.combining(), ['\u{200d}']);
            vec![
                head,
                Cell::wide_spacer(attrs),
                Cell::new('🔥', next_attrs),
                Cell::wide_spacer(next_attrs),
            ]
        }
        _ => unreachable!(),
    }
}
fn covered_width(kind: &str, attrs: Attrs) -> Vec<(usize, u8)> {
    covered_width_decorated(kind, attrs, |_| {})
}
fn covered_width_decorated(
    kind: &str,
    attrs: Attrs,
    decorate: impl FnOnce(&mut Snapshot),
) -> Vec<(usize, u8)> {
    let cells = legacy_cells(kind, attrs);
    let mut snapshot = Terminal::new(cells.len(), 1).snapshot();
    snapshot.cells = cells;
    decorate(&mut snapshot);
    let mut atlas = ColorGlyphAtlas::new(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    rasterizer()
        .build_color_glyph_runs(&snapshot, &mut atlas)
        .iter()
        .map(|run| (run.column, run.covered_columns))
        .collect()
}

#[test]
fn matching_legacy_owners_form_color_clusters_through_real_runs() {
    for (kind, width) in [("flag", 2), ("modifier", 4), ("zwj", 4)] {
        assert_eq!(
            covered_width(kind, Attrs::default()),
            [(0, width)],
            "kind={kind}"
        );
    }
}

#[test]
fn hidden_legacy_flag_owner_is_not_consumed_into_a_color_cluster() {
    let mut attrs = Attrs::default();
    attrs.set_hidden(true);
    assert_eq!(covered_width("flag", attrs), [(0, 1)]);
}

#[test]
fn hidden_legacy_modifier_owner_is_not_consumed_into_a_color_cluster() {
    let mut attrs = Attrs::default();
    attrs.set_hidden(true);
    assert_eq!(covered_width("modifier", attrs), [(0, 2)]);
}

fn differing_rendition_runs(kind: &str) -> Vec<(usize, u8)> {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Rgb(1, 2, 3);
    covered_width(kind, attrs)
}
#[test]
fn differing_legacy_flag_renditions_remain_separate_color_runs() {
    assert_eq!(differing_rendition_runs("flag"), [(0, 1), (1, 1)]);
}
#[test]
fn differing_legacy_modifier_renditions_remain_separate_color_runs() {
    assert_eq!(differing_rendition_runs("modifier"), [(0, 2), (2, 2)]);
}
#[test]
fn differing_legacy_zwj_renditions_do_not_suppress_the_next_owner() {
    // The head is drawn on its own two columns and the next owner keeps its own
    // run; neither is dropped or merged.
    assert_eq!(differing_rendition_runs("zwj"), [(0, 2), (2, 2)]);
}

/// Highlight only the columns of the cluster's second owner, the way a partial
/// selection does before the frame is built.
fn select_second_owner(kind: &str, themed: Option<SelectionStyle>) -> impl FnOnce(&mut Snapshot) {
    let first = if kind == "flag" { 1 } else { 2 };
    let last = if kind == "flag" { 1 } else { 3 };
    move |snapshot| {
        apply_highlight(
            snapshot,
            SelectionRange {
                start: CellPoint {
                    row: 0,
                    column: first,
                },
                end: CellPoint {
                    row: 0,
                    column: last,
                },
            },
            themed,
        );
    }
}

#[test]
fn a_partial_selection_does_not_split_a_legacy_color_cluster() {
    let themed = SelectionStyle {
        fill: [10, 20, 30],
        fg: [200, 210, 220],
    };
    for (kind, width) in [("flag", 2), ("modifier", 4), ("zwj", 4)] {
        for style in [None, Some(themed)] {
            assert_eq!(
                covered_width_decorated(kind, Attrs::default(), select_second_owner(kind, style)),
                [(0, width)],
                "kind={kind} themed={}",
                style.is_some()
            );
        }
    }
}

#[test]
fn a_selection_covering_the_whole_legacy_cluster_keeps_it_joined() {
    for (kind, width, last) in [("flag", 2, 1), ("modifier", 4, 3), ("zwj", 4, 3)] {
        let runs = covered_width_decorated(kind, Attrs::default(), |snapshot| {
            apply_highlight(
                snapshot,
                SelectionRange {
                    start: CellPoint { row: 0, column: 0 },
                    end: CellPoint {
                        row: 0,
                        column: last,
                    },
                },
                None,
            );
        });
        assert_eq!(runs, [(0, width)], "kind={kind}");
    }
}

#[test]
fn a_selection_does_not_hide_a_stored_restyle_inside_the_cluster() {
    // The stored difference is not a colour, so the highlight cannot explain it.
    let mut bold = Attrs::default();
    bold.set_bold(true);
    assert_eq!(
        covered_width_decorated("flag", bold, select_second_owner("flag", None)),
        [(0, 1), (1, 1)]
    );
    assert_eq!(
        covered_width_decorated("modifier", bold, select_second_owner("modifier", None)),
        [(0, 2), (2, 2)]
    );
}
