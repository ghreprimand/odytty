// SPDX-License-Identifier: GPL-3.0-only
//! Alternate-zero control and the contextual shaper: `zero` rides both shaping
//! passes equally, so it never creates an overlay of its own, and a `0` the
//! shaper does emit is the same alternate the scalar atlas path draws.

use super::*;
use crate::core::Terminal;
use crate::text::{JETBRAINS_FONT_FAMILY, load_bundled_style_for};

struct Fonts(FontHandle);

impl LigatureFonts for Fonts {
    fn ligature_font(&self, _style: FontStyle) -> &FontHandle {
        &self.0
    }
}

fn jetbrains() -> FontHandle {
    load_bundled_style_for(JETBRAINS_FONT_FAMILY, FontStyle::Regular).expect("bundled face")
}

fn snapshot(text: &str) -> Snapshot {
    let mut terminal = Terminal::new(16, 1);
    terminal.advance(text.as_bytes());
    terminal.snapshot()
}

const ZERO_ON: LatinShapingFeatures = LatinShapingFeatures {
    ss01: false,
    ss02: false,
    zero: true,
};

/// Rows mixing `0` with JetBrains Mono operator ligatures and plain text.
const ROWS: [&str; 8] = [
    "0==0", "a0<=0", "10:00", "0..0", "0->0", "x0x", "0xFF", "0 != 00",
];

#[test]
fn zero_never_creates_an_overlay_and_leaves_ligatures_unchanged() {
    let plain = Fonts(jetbrains());
    let zeroed = Fonts(jetbrains().with_zero_feature(true));
    for text in ROWS {
        let snap = snapshot(text);
        let before = LigatureShaper::new().build_runs(true, &snap, &plain, &[]);
        let after =
            LigatureShaper::new().build_runs_with_features(true, &snap, &zeroed, &[], ZERO_ON);
        assert_eq!(before, after, "{text:?}: identical overlays");
        for (column, ch) in text.chars().enumerate() {
            if ch == '0' {
                assert!(
                    !after.iter().any(|run| run.covers(0, column)),
                    "{text:?}: the `0` at {column} stays on the scalar path"
                );
            }
        }
    }
}

#[test]
fn shaped_zero_matches_the_scalar_alternate() {
    let font = jetbrains();
    let zeroed = font.clone().with_zero_feature(true);
    let font_ref = FontRef::from_index(font.as_slice(), 0).expect("face");
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"a0==b");
    let snap = terminal.snapshot();
    let run = RunText::from_cells(&snap.cells[..5]);
    let mut context = ShapeContext::new();
    for (features, zero) in [(LatinShapingFeatures::default(), false), (ZERO_ON, true)] {
        for tags in [features.off_tags(), features.on_tags()] {
            let glyphs = shape_run(
                &mut context,
                font_ref,
                &run,
                Script::Latin,
                Direction::LeftToRight,
                &tags,
            );
            let at_zero = glyphs
                .iter()
                .find(|glyph| glyph.source_start == 1)
                .expect("the `0` cluster");
            let expected = if zero { &zeroed } else { &font }.glyph_id('0');
            assert_eq!(at_zero.id, expected.0, "zero={zero} tags={tags:?}");
        }
    }
}

#[test]
fn toggling_zero_reshapes_instead_of_reusing_the_row_cache() {
    let fonts = Fonts(jetbrains());
    let snap = snapshot("0==0");
    let mut shaper = LigatureShaper::new();
    let _ = shaper.build_runs(true, &snap, &fonts, &[]);
    let warm = shaper.shape_calls();
    let _ = shaper.build_runs_with_features(true, &snap, &fonts, &[], ZERO_ON);
    assert!(shaper.shape_calls() > warm, "a zero toggle reshapes");
    let warm = shaper.shape_calls();
    let _ = shaper.build_runs_with_features(true, &snap, &fonts, &[], ZERO_ON);
    assert_eq!(shaper.shape_calls(), warm, "same features hit the cache");
}

#[test]
fn ligatures_off_still_shapes_nothing_with_zero_on() {
    let fonts = Fonts(jetbrains().with_zero_feature(true));
    let mut shaper = LigatureShaper::new();
    let runs = shaper.build_runs_with_features(false, &snapshot("0==0"), &fonts, &[], ZERO_ON);
    assert!(runs.is_empty());
    assert_eq!(shaper.shape_calls(), 0);
}
