// SPDX-License-Identifier: GPL-3.0-only
//! Changed font faces, unmapped bases, and public snapshot boundaries.

use super::*;

struct FaceSet([FontHandle; 4]);
impl LigatureFonts for FaceSet {
    fn ligature_font(&self, style: FontStyle) -> &FontHandle {
        &self.0[font_style_index(style)]
    }
}
fn latin() -> FontHandle {
    FontHandle::try_from_vec(include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec())
        .expect("project-authored Latin fixture")
}

#[test]
fn every_shaping_style_invalidates_cached_rows_when_its_face_changes() {
    for (index, prefix) in [(0, ""), (1, "\x1b[1m"), (2, "\x1b[3m"), (3, "\x1b[1;3m")] {
        let face = latin();
        let mut fonts = FaceSet(std::array::from_fn(|_| face.clone()));
        let snap = snapshot(&format!("{prefix}->"));
        let mut reused = LigatureShaper::new();
        let first = reused.build_runs(true, &snap, &fonts, &[]);
        assert!(
            !first.is_empty(),
            "fixture defines the original style's arrow"
        );
        fonts.0[index] = arabic_fixture_font();
        let expected = LigatureShaper::new().build_runs(true, &snap, &fonts, &[]);
        assert!(
            expected.is_empty(),
            "replacement fixture has no arrow substitution"
        );
        assert_eq!(
            reused.build_runs(true, &snap, &fonts, &[]),
            expected,
            "style {index} uses the new face"
        );
    }
}

#[test]
fn cloning_an_immutable_face_keeps_the_warm_row_plan() {
    let face = latin();
    let fonts = FaceSet(std::array::from_fn(|_| face.clone()));
    let snap = snapshot("->");
    let mut shaper = LigatureShaper::new();
    let first = shaper.build_runs(true, &snap, &fonts, &[]);
    let calls = shaper.shape_calls();
    let clones = FaceSet(std::array::from_fn(|index| fonts.0[index].clone()));
    assert_eq!(shaper.build_runs(true, &snap, &clones, &[]), first);
    assert_eq!(shaper.shape_calls(), calls);
}

#[test]
fn unmapped_arabic_base_with_a_mapped_mark_stays_on_scalar_fallback() {
    let fonts = Fonts(arabic_fixture_font());
    assert_eq!(fonts.0.glyph_id('\u{0645}').0, 0, "fixture omits meem");
    assert_ne!(fonts.0.glyph_id('\u{064E}').0, 0, "fixture maps fatha");
    let logical = "\u{0628}\u{0645}\u{064E}";
    let snap = snapshot(logical);
    let runs = LigatureShaper::new().build_runs(true, &snap, &fonts, &[]);
    assert!(
        runs.iter().all(|run| !run.covers(0, 1)),
        "missing base must not suppress scalar fallback"
    );
    assert!(
        runs.iter()
            .flat_map(|run| run.glyphs.iter())
            .all(|glyph| glyph.key.glyph_id != 0)
    );
    assert_eq!(
        selected_text(
            &snap,
            SelectionRange {
                start: CellPoint { row: 0, column: 0 },
                end: CellPoint { row: 0, column: 1 }
            }
        ),
        logical
    );
}

#[test]
fn a_public_zero_column_snapshot_returns_without_shaping() {
    let fonts = Fonts(latin());
    let mut snap = snapshot("->");
    snap.dimensions.columns = 0;
    let mut shaper = LigatureShaper::new();
    assert!(shaper.build_runs(true, &snap, &fonts, &[]).is_empty());
    assert_eq!(shaper.shape_calls(), 0);
}

/// Relocate two project-authored standalone faces into a raw collection.
fn collection() -> Vec<u8> {
    let faces: [&[u8]; 2] = [
        include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf"),
        include_bytes!("../../tests/fixtures/fonts/arabic-marks.ttf"),
    ];
    let mut out = b"ttcf".to_vec();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.resize(20, 0);
    for (index, bytes) in faces.into_iter().enumerate() {
        let base = out.len();
        out[12 + index * 4..16 + index * 4].copy_from_slice(&(base as u32).to_be_bytes());
        let mut face = bytes.to_vec();
        let count = u16::from_be_bytes(face[4..6].try_into().unwrap()) as usize;
        for record in (0..count).map(|i| 12 + i * 16) {
            let offset = u32::from_be_bytes(face[record + 8..record + 12].try_into().unwrap());
            face[record + 8..record + 12].copy_from_slice(&(offset + base as u32).to_be_bytes());
        }
        out.extend_from_slice(&face);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    out
}

#[test]
fn raw_collection_shaping_uses_the_selected_face_index() {
    let bytes = collection();
    let first = Fonts(FontHandle::from_vec_and_index(bytes.clone(), 0).expect("first face"));
    let second = Fonts(FontHandle::from_vec_and_index(bytes, 1).expect("second face"));
    let snap = snapshot("->");
    assert!(
        !LigatureShaper::new()
            .build_runs(true, &snap, &first, &[])
            .is_empty()
    );
    assert!(
        LigatureShaper::new()
            .build_runs(true, &snap, &second, &[])
            .is_empty(),
        "Arabic face has no arrow liga"
    );
}

#[test]
fn raw_collection_faces_have_distinct_atlas_fingerprints() {
    let bytes = collection();
    let first = FontHandle::from_vec_and_index(bytes.clone(), 0).expect("first face");
    let second = FontHandle::from_vec_and_index(bytes, 1).expect("second face");
    assert_ne!(font_fingerprint(&first), font_fingerprint(&second));
}

#[test]
fn owner_shaping_also_invalidates_presentations_when_a_primary_face_changes() {
    let _guard = crate::test_lock::render_globals_lock();
    let devanagari = FontHandle::try_from_vec(
        include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Devanagari-subset.ttf")
            .to_vec(),
    )
    .expect("licensed Devanagari fixture");
    let primary = text::load_bundled_font().expect("bundled body");
    let mut atlas = GlyphAtlas::build(&primary, 28.0);
    atlas.set_fallback_fonts(vec![Arc::new(devanagari.clone())]);
    let mut fonts = Fonts(devanagari);
    let snap = snapshot("\u{0915}\u{094D}\u{0937}");
    let mut reused = crate::complex_shaping::ComplexShaper::new();
    let first = reused.build_runs(true, &snap, &fonts, &mut atlas, &[]);
    assert!(!first.is_empty(), "primary face shapes the fixture owner");
    fonts.0 = primary;
    let expected = crate::complex_shaping::ComplexShaper::new().build_runs(
        true,
        &snap,
        &fonts,
        &mut atlas,
        &[],
    );
    assert!(!expected.is_empty(), "fallback face still shapes the owner");
    assert_ne!(
        first, expected,
        "primary and fallback atlas identities differ"
    );
    assert_eq!(
        reused.build_runs(true, &snap, &fonts, &mut atlas, &[]),
        expected
    );
}
