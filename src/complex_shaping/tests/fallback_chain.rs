// SPDX-License-Identifier: GPL-3.0-only
//! Every enabled group's single-owner rows shaped from the fallback chain
//! under a monospace style face that lacks the script, the common real
//! configuration.

use super::*;

#[test]
fn every_enabled_group_shapes_from_the_fallback_chain_under_a_monospace_face() {
    let _guard = crate::test_lock::render_globals_lock();
    for group in GROUPS {
        let mut compared = 0;
        for row in group_rows(group).iter().filter(|row| !row.known_diff()) {
            let snapshot = terminal(&row.text, 4).snapshot();
            let Some(span) = single_owner(&snapshot) else {
                continue;
            };
            let primary = latin_face();
            let fallback = Arc::new(face(&row.font));
            let mut atlas = GlyphAtlas::build(&primary, PX);
            atlas.set_fallback_fonts(vec![Arc::clone(&fallback)]);
            ensure_cells(&mut atlas, &primary, &snapshot);
            let runs =
                ComplexShaper::new().build_runs(true, &snapshot, &Fonts(primary), &mut atlas, &[]);
            assert_eq!(runs.len(), 1, "{row:?}");
            assert_eq!((runs[0].start, runs[0].end), (0, span), "{row:?}");
            let oracle = oracle_run(&mut atlas, &fallback, false, &row.placed(), 0, span);
            assert_eq!(runs[0].glyphs[0].key, oracle.glyphs[0].key, "{row:?}");
            assert_eq!(
                frame(&snapshot, &atlas, &runs),
                frame(&snapshot, &atlas, &[oracle]),
                "{row:?}"
            );
            compared += 1;
        }
        assert!(compared >= group.owner_floor, "{}: {compared}", group.name);
    }
}

#[test]
fn tall_metric_fallback_owner_keeps_primary_ink_height_and_logical_cells() {
    let _guard = crate::test_lock::render_globals_lock();
    let primary = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/cluster-size-primary.ttf").to_vec(),
    )
    .expect("project-authored primary face");
    let fallback = Arc::new(
        FontHandle::try_from_vec(
            include_bytes!("../../../tests/fixtures/fonts/cluster-size-tall.ttf").to_vec(),
        )
        .expect("project-authored tall-metric face"),
    );
    let text = "\u{b95}\u{bcd}";
    let terminal = terminal(text, 4);
    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.cells[0].grapheme(), text);
    assert_eq!(single_owner(&snapshot), Some(1));
    let mut atlas = GlyphAtlas::build(&primary, PX);
    let primary_height = atlas.glyph_quad('M').expect("primary ink").height;
    let geometry = atlas.cell;
    atlas.set_fallback_fonts(vec![fallback]);
    let fonts = Fonts(primary);
    let mut shaper = ComplexShaper::new();
    assert!(
        shaper
            .build_runs(false, &snapshot, &fonts, &mut atlas, &[])
            .is_empty()
    );
    let runs = shaper.build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].start, runs[0].end), (0, 1));
    let ink = atlas
        .shaped_glyph_quad(runs[0].glyphs[0].key)
        .expect("owner ink");
    assert_eq!(
        ink.height, primary_height,
        "equal outlines keep equal height"
    );
    assert_eq!(atlas.cell, geometry);
    assert_eq!(terminal.snapshot().cells, snapshot.cells);
    assert_eq!(
        selected_text(
            &snapshot,
            SelectionRange {
                start: CellPoint { row: 0, column: 0 },
                end: CellPoint { row: 0, column: 0 }
            }
        ),
        text
    );
}
