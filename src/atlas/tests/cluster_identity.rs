// SPDX-License-Identifier: GPL-3.0-only
//! A cached owner run is trusted only when it is the run being asked for.
//! The cache key carries a 64-bit digest of the run, and the run is untrusted
//! terminal text, so a digest shared by two different runs must never draw the
//! first run's pixels for the second.

use super::*;
use crate::atlas::cluster::ClusterGlyph;

const PX: f32 = 20.0;

fn font() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec(),
    )
    .expect("project-authored font fixture")
}

/// Two distinct inked glyphs of the fixture face, as one-glyph runs.
fn two_runs(font: &FontHandle) -> ([ClusterGlyph; 1], [ClusterGlyph; 1]) {
    let mut inked = ('A'..='z').filter_map(|ch| {
        let id = font.glyph_id(ch);
        let outline = font.outline(id)?;
        (!outline.curves.is_empty()).then_some(ClusterGlyph {
            id: id.0,
            x: 0,
            y: 0,
        })
    });
    let first = inked.next().expect("an inked glyph");
    let second = inked
        .find(|g| g.id != first.id)
        .expect("a second inked glyph");
    ([first], [second])
}

#[test]
fn a_digest_shared_by_two_runs_never_serves_the_first_runs_slot() {
    let font = font();
    let (first, second) = two_runs(&font);
    let mut atlas = GlyphAtlas::build(&font, PX);
    let fingerprint = 41;

    let key = atlas
        .ensure_cluster_digest(7, &font, fingerprint, FontStyle::Regular, true, 1, &first)
        .expect("the first run draws");
    let resident = atlas.shaped_slot_count();
    // The same run under the same digest is a hit.
    assert_eq!(
        atlas.ensure_cluster_digest(7, &font, fingerprint, FontStyle::Regular, true, 1, &first),
        Some(key)
    );
    // A different run with the same digest is refused, not answered with the
    // first run's slot, and nothing is stored for it.
    assert_eq!(
        atlas.ensure_cluster_digest(7, &font, fingerprint, FontStyle::Regular, true, 1, &second),
        None
    );
    assert_eq!(atlas.shaped_slot_count(), resident);
    // The same glyphs under another face fingerprint or style source are a
    // different run too.
    assert_eq!(
        atlas.ensure_cluster_digest(
            7,
            &font,
            fingerprint + 1,
            FontStyle::Regular,
            true,
            1,
            &first
        ),
        None
    );
    assert_eq!(
        atlas.ensure_cluster_digest(7, &font, fingerprint, FontStyle::Regular, false, 1, &first),
        None
    );
}

#[test]
fn distinct_runs_keep_distinct_slots_and_repeat_as_hits() {
    let font = font();
    let (first, second) = two_runs(&font);
    let mut atlas = GlyphAtlas::build(&font, PX);
    let mut draw = |glyphs: &[ClusterGlyph]| {
        atlas
            .ensure_cluster(&font, 41, FontStyle::Regular, true, 1, glyphs)
            .expect("run draws")
    };
    let (a, b) = (draw(&first), draw(&second));
    assert_ne!(a, b);
    assert_eq!(draw(&first), a);
    assert_eq!(draw(&second), b);
}

#[test]
fn tall_fallback_line_metrics_do_not_shrink_cluster_ink() {
    let primary = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/cluster-size-primary.ttf").to_vec(),
    )
    .expect("project-authored primary face");
    let fallback = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/cluster-size-tall.ttf").to_vec(),
    )
    .expect("project-authored tall-metric face");
    let mut atlas = GlyphAtlas::build(&primary, PX);
    let reference = atlas.glyph_quad('M').expect("primary ink");
    let glyphs = [ClusterGlyph {
        id: fallback.glyph_id('\u{b95}').0,
        x: 0,
        y: 0,
    }];
    let key = atlas
        .ensure_cluster(&fallback, 91, FontStyle::Regular, false, 1, &glyphs)
        .expect("fallback cluster ink");
    let ink = atlas.shaped_glyph_quad(key).expect("cluster quad");
    assert!(
        ink.height as f32 >= reference.height as f32 * 0.9,
        "equal outlines must retain at least 90% of primary height: {ink:?} vs {reference:?}"
    );
    assert_eq!(ink.height, reference.height);
    assert_eq!(ink.width, reference.width);
    assert_eq!(atlas.cell, GlyphAtlas::build(&primary, PX).cell);
}
