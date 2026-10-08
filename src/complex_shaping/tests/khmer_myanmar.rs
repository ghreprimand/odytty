// SPDX-License-Identifier: GPL-3.0-only
//! Khmer and Myanmar classifier and stray-mark checks. The group's
//! reference and pixel checks run with every enabled group in the parent
//! module's `GROUPS` table.

use super::*;

#[test]
fn classifier_enables_khmer_and_myanmar() {
    let attrs = crate::core::Attrs::default();
    for ch in [
        '\u{1780}', '\u{17D2}', '\u{19E0}', '\u{1000}', '\u{1039}', '\u{A9E0}', '\u{AA60}',
    ] {
        assert!(owner_is_eligible(&Cell::new(ch, attrs)), "{ch:?}");
    }
    // Scripts outside the enabled blocks stay out.
    for ch in ['\u{10A00}', '\u{1B13}'] {
        assert!(!owner_is_eligible(&Cell::new(ch, attrs)), "{ch:?}");
    }
    let mut stack = Cell::new('\u{1780}', attrs);
    assert!(stack.push_combining('\u{17D2}'));
    assert!(stack.push_combining('\u{1780}'));
    assert!(owner_is_eligible(&stack));
    let mut kinzi = Cell::new('\u{1004}', attrs);
    assert!(kinzi.push_combining('\u{103A}'));
    assert!(kinzi.push_combining('\u{1039}'));
    assert!(kinzi.push_combining('\u{1000}'));
    assert!(owner_is_eligible(&kinzi));
    // A retained scalar from outside every enabled group keeps the per-cell
    // path.
    let mut mixed = Cell::new('\u{1780}', attrs);
    assert!(mixed.push_combining('\u{0301}'));
    assert!(!owner_is_eligible(&mixed));
}

#[test]
fn a_stray_mark_shapes_on_a_dotted_circle_only_when_the_face_maps_one() {
    let _guard = crate::test_lock::render_globals_lock();
    // The Khmer subset maps U+25CC and the Myanmar subset does not. A vowel
    // sign that starts an owner, at the line start or after a Latin letter,
    // shapes as its reference row: on the circle, or alone.
    let rows = group_rows(
        GROUPS
            .iter()
            .find(|group| group.name == "khmer-myanmar")
            .expect("enabled group"),
    );
    for (file, mark, note, circle) in [
        (
            "Khmer-subset.ttf",
            '\u{17C1}',
            "stray-mark-dotted-circle",
            true,
        ),
        (
            "Myanmar-subset.ttf",
            '\u{1031}',
            "stray-mark-no-circle",
            false,
        ),
    ] {
        let font = face(file);
        let circle_id = font.glyph_id('\u{25CC}').0;
        assert_eq!(circle_id != 0, circle, "{file} U+25CC coverage");
        let reference = rows
            .iter()
            .find(|row| row.note == note)
            .expect("stray-mark row");
        assert_eq!(reference.text, mark.to_string());
        assert_eq!(reference.glyphs.contains(&circle_id), circle, "{note}");
        assert_eq!(reference.glyphs.len(), if circle { 2 } else { 1 });
        for (text, column) in [(mark.to_string(), 0), (format!("a{mark}"), 1)] {
            let snapshot = terminal(&text, 4).snapshot();
            assert_eq!(snapshot.cells[column].grapheme(), mark.to_string());
            let mut atlas = GlyphAtlas::build(&font, PX);
            ensure_cells(&mut atlas, &font, &snapshot);
            let runs = ComplexShaper::new().build_runs(
                true,
                &snapshot,
                &Fonts(font.clone()),
                &mut atlas,
                &[],
            );
            assert_eq!(runs.len(), 1, "{text:?}");
            assert_eq!((runs[0].start, runs[0].end), (column, column + 1));
            let oracle = oracle_run(&mut atlas, &font, true, &reference.placed(), column, 1);
            assert_eq!(runs[0].glyphs[0].key, oracle.glyphs[0].key, "{text:?}");
            assert_eq!(
                frame(&snapshot, &atlas, &runs),
                frame(&snapshot, &atlas, &[oracle]),
                "{text:?}"
            );
        }
    }
}
