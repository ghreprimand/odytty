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
