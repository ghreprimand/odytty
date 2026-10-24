// SPDX-License-Identifier: GPL-3.0-only
//! Symbol / Nerd-font fallback behavior at the atlas seam.
//!
//! The gate-critical guarantee is **default-safe**: with no fallback font
//! installed, a printable codepoint the primary lacks renders the historical
//! hollow box and consumes no slot - byte-for-byte the pre-feature path. When a
//! fallback is installed it is used for printable spacing codepoints the
//! fallback actually covers.

use super::*;
use crate::atlas::fallback::is_symbol_codepoint;
use std::sync::Arc;

const NON_SYMBOL_FALLBACK: char = '\u{2200}';

/// A PUA codepoint the bundled/system test font does *not* map (so it exercises
/// the missing-glyph path), plus one it *does* - discovered at runtime so the
/// tests are not pinned to a single host font.
fn pua_absent(font: &FontHandle) -> Option<char> {
    (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| !font_has_glyph(font, ch))
}

fn pua_present(font: &FontHandle) -> Option<char> {
    (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| font_has_glyph(font, ch))
}

pub(super) fn marker_blank_font() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/symbol-markers-blank.ttf").to_vec(),
    )
    .expect("parse blank marker fixture")
}

fn marker_inked_font() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/symbol-markers-inked.ttf").to_vec(),
    )
    .expect("parse inked marker fixture")
}

#[test]
fn ascii_missing_from_primary_rasterizes_from_static_fallback() {
    let primary = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/s5b/brahmic-a/Balinese-subset.ttf").to_vec(),
    )
    .expect("Balinese subset fixture");
    let fallback = Arc::new(test_font());
    assert_eq!(primary.glyph_id('a').0, 0, "primary fixture omits ASCII a");
    assert!(
        font_has_glyph(&fallback, 'a'),
        "fallback fixture covers ASCII a"
    );
    assert!(
        font_has_glyph(&fallback, '\u{00E9}'),
        "fallback fixture covers Latin-1 e-acute"
    );

    let mut atlas = GlyphAtlas::build(&primary, 24.0);
    let a_uv = atlas.uv_rect('a').expect("ASCII atlas cell");
    assert_eq!(cell_ink(&atlas, a_uv), 0, "primary has no ASCII a ink");
    atlas.set_fallback_fonts(vec![fallback]);

    assert!(
        cell_ink(&atlas, a_uv) > 0,
        "ASCII a must rasterize visibly from the fallback face"
    );

    let latin_uv = atlas
        .ensure(&primary, '\u{00E9}')
        .expect("Latin-1 glyph atlas cell");
    assert_ne!(latin_uv, atlas.slot_uv(FALLBACK_SLOT));
    assert!(
        cell_ink(&atlas, latin_uv) > 0,
        "Latin-1 e-acute must rasterize visibly from the fallback face"
    );
}

#[test]
fn fallback_chain_keeps_covered_primary_ascii_byte_identical() {
    let primary = test_font();
    let fallback = Arc::new(
        FontHandle::try_from_vec(
            include_bytes!("../../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec(),
        )
        .expect("Latin fallback fixture"),
    );
    for code in FIRST_CHAR..=LAST_CHAR {
        let ch = char::from_u32(code).expect("printable ASCII codepoint");
        assert!(
            font_has_glyph(&primary, ch),
            "primary fixture covers {ch:?}"
        );
    }

    let mut atlas = GlyphAtlas::build(&primary, 24.0);
    let primary_bytes = atlas.data.clone();
    atlas.set_fallback_fonts(vec![fallback]);
    for code in FIRST_CHAR..=LAST_CHAR {
        let ch = char::from_u32(code).expect("printable ASCII codepoint");
        atlas.ensure(&primary, ch).expect("prebuilt ASCII cell");
    }
    assert_eq!(atlas.data, primary_bytes);
}

/// A symbol face carrying U+E0A0 as a wide block, standing in for an installed
/// symbol or Nerd font. No primary fixture in this module maps it.
pub(super) fn symbol_block_font() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/symbol-pua-block.ttf").to_vec(),
    )
    .expect("parse block symbol fixture")
}

/// A symbol face carrying U+E0A0 as a thin bar, so it leaves less ink than
/// [`symbol_block_font`] for the same codepoint.
pub(super) fn symbol_bar_font() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/symbol-pua-bar.ttf").to_vec(),
    )
    .expect("parse bar symbol fixture")
}

/// The private-use codepoint both symbol fixtures map.
pub(super) const SYMBOL_ICON: char = '\u{E0A0}';

#[test]
fn confirmed_symbol_markers_reject_blank_glyph_coverage() {
    for ch in ['\u{2731}', '\u{25CF}'] {
        assert!(is_symbol_codepoint(ch));
        assert!(
            !glyph_coverage_decision(ch, true, false),
            "{ch:?} must not count a cmap hit with no ink as coverage"
        );
        assert!(
            glyph_coverage_decision(ch, true, true),
            "{ch:?} must count a cmap hit with ink as coverage"
        );
        assert!(
            !glyph_coverage_decision(ch, false, true),
            "{ch:?} must not count ink without a cmap hit as coverage"
        );
    }

    assert!(
        !glyph_coverage_decision(NON_SYMBOL_FALLBACK, true, false),
        "printable fallback candidates must not count blank outlines as coverage"
    );
    assert!(
        glyph_coverage_decision(NON_SYMBOL_FALLBACK, true, true),
        "printable fallback candidates with ink count as coverage"
    );
    assert!(
        glyph_coverage_decision('\u{0301}', true, false),
        "combining marks keep the cmap-only decision because they never fall back standalone"
    );
}

#[test]
fn confirmed_symbol_markers_reject_blank_font_and_accept_inked_font() {
    let blank = marker_blank_font();
    let inked = marker_inked_font();

    for ch in ['\u{2731}', '\u{25CF}'] {
        assert!(
            blank.glyph_id(ch).0 != 0,
            "blank fixture must map {ch:?} in its cmap"
        );
        assert!(
            !font_has_glyph(&blank, ch),
            "blank fixture must not count as covering {ch:?}"
        );
        assert!(
            font_has_glyph(&inked, ch),
            "inked fixture must count as covering {ch:?}"
        );
    }
}

#[test]
fn confirmed_symbol_markers_fall_through_blank_faces_to_inked_fallback() {
    for ch in ['\u{2731}', '\u{25CF}'] {
        let primary = marker_blank_font();
        let blank_static = Arc::new(marker_blank_font());
        let inked_static = Arc::new(marker_inked_font());
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        let count = atlas.slot_count();

        atlas.set_fallback_fonts(vec![blank_static, inked_static]);

        let uv = atlas.ensure(&primary, ch).expect("marker fallback uv");
        assert_ne!(
            uv, box_uv,
            "{ch:?} must resolve from the inked fallback, not tofu"
        );
        assert!(
            atlas.slot_count() > count,
            "{ch:?} must allocate a real fallback slot"
        );
        assert!(
            cell_ink(&atlas, uv) > 0,
            "{ch:?} fallback slot must contain visible ink"
        );
    }
}

#[test]
fn non_symbol_missing_glyph_uses_static_fallback_chain() {
    assert!(!is_symbol_codepoint(NON_SYMBOL_FALLBACK));
    assert!(should_attempt_fallback(NON_SYMBOL_FALLBACK));

    let primary = marker_blank_font();
    let inked_static = Arc::new(marker_inked_font());
    assert!(
        !font_has_glyph(&primary, NON_SYMBOL_FALLBACK),
        "primary fixture must not cover the non-symbol codepoint"
    );
    assert!(
        font_has_glyph(&inked_static, NON_SYMBOL_FALLBACK),
        "inked fixture must cover the non-symbol codepoint"
    );

    let mut atlas = GlyphAtlas::build(&primary, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    atlas.set_fallback_fonts(vec![inked_static]);

    let uv = atlas
        .ensure(&primary, NON_SYMBOL_FALLBACK)
        .expect("non-symbol fallback uv");
    assert_ne!(
        uv, box_uv,
        "non-symbol printable glyph must resolve from the fallback chain"
    );
    assert!(
        atlas.slot_count() > count,
        "non-symbol fallback must allocate a real slot"
    );
    assert!(
        cell_ink(&atlas, uv) > 0,
        "non-symbol fallback slot must contain visible ink"
    );
}

#[test]
fn excluded_categories_do_not_attempt_fallback() {
    for ch in ['\u{0301}', '\u{0007}', '\u{200D}', '\u{FE0F}'] {
        assert!(
            !should_attempt_fallback(ch),
            "{ch:?} must be excluded from standalone fallback"
        );
        let primary = marker_blank_font();
        let inked_static = Arc::new(marker_inked_font());
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        let count = atlas.slot_count();
        atlas.set_fallback_fonts(vec![inked_static]);

        let uv = atlas.ensure(&primary, ch);
        if wants_glyph(ch) {
            assert_eq!(uv, Some(box_uv), "{ch:?} must stay on the hollow box");
        } else {
            assert_eq!(uv, None, "{ch:?} must draw no standalone glyph");
        }
        assert_eq!(
            atlas.slot_count(),
            count,
            "{ch:?} must not allocate through fallback"
        );
    }
}

#[test]
fn pua_missing_glyph_without_fallback_uses_box() {
    let font = test_font();
    let absent = pua_absent(&font).expect("bundled face: font covers all of the BMP PUA");
    assert!(is_symbol_codepoint(absent));
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    // No fallback installed: the missing PUA glyph must take the hollow box and
    // consume no slot - identical to a renderer without symbol fallback.
    let uv = atlas.ensure(&font, absent).expect("fallback uv");
    assert_eq!(uv, box_uv, "missing PUA glyph must use the hollow box");
    assert_eq!(
        atlas.slot_count(),
        count,
        "fallback must not consume a slot"
    );
    assert!(!atlas.take_dirty(), "no pixels changed, so not dirty");
}

#[test]
fn fallback_present_but_lacking_glyph_uses_box() {
    let font = test_font();
    let absent = pua_absent(&font).expect("bundled face: font covers all of the BMP PUA");
    // A second instance of the same font as the "fallback" (reload so Arc
    // identity is distinct): it also lacks `absent`, so the fallback must decline
    // and the hollow box is used - proving the atlas verifies fallback coverage
    // rather than blindly drawing.
    let fb = test_font();
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas.set_fallback_fonts(vec![Arc::new(fb)]);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    let uv = atlas.ensure(&font, absent).expect("fallback uv");
    assert_eq!(uv, box_uv);
    assert_eq!(
        atlas.slot_count(),
        count,
        "fallback must not consume a slot"
    );
}

#[test]
fn primary_covered_pua_glyph_renders_from_primary() {
    let font = test_font();
    let present = pua_present(&font).expect("bundled face: font has no BMP PUA glyph");
    assert!(is_symbol_codepoint(present));
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    // A PUA codepoint the primary font *does* cover must still render from the
    // primary (classification never hijacks a covered glyph), allocating a real
    // slot distinct from the hollow box.
    let uv = atlas.ensure(&font, present).expect("real glyph uv");
    assert_ne!(uv, box_uv, "covered PUA glyph must not use the hollow box");
    assert!(
        atlas.slot_count() > count,
        "covered PUA glyph must allocate a real slot"
    );
}

#[test]
fn fallback_renders_pua_glyph_when_a_symbol_font_is_available() {
    // A fixture face stands in for the installed symbol font, so the test
    // asserts on every host. The primary maps no private-use codepoint.
    let primary = marker_blank_font();
    let symbol = symbol_block_font();
    let icon = SYMBOL_ICON;
    assert!(is_symbol_codepoint(icon));
    assert!(font_has_glyph(&symbol, icon) && !font_has_glyph(&primary, icon));
    // The canonical Symbol-fallback case: a prompt icon the body font cannot draw.
    let mut atlas = GlyphAtlas::build(&primary, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    atlas.set_fallback_fonts(vec![Arc::new(symbol)]);
    let uv = atlas.ensure(&primary, icon).expect("fallback glyph uv");
    assert_ne!(uv, box_uv, "fallback glyph must not use the hollow box");
    assert!(
        atlas.slot_count() > count,
        "fallback glyph must allocate a real slot"
    );
    assert!(
        cell_ink(&atlas, uv) > 0,
        "fallback glyph should rasterize visible ink"
    );
}

#[test]
fn chain_walk_resolves_from_a_later_face_when_the_first_lacks_the_glyph() {
    let primary = marker_blank_font();
    // Two-face chain: the first face lacks `icon`, the second has it. The atlas
    // must skip the first and rasterize from the second (first-hit-wins), which
    // is exactly how the bundled v3→v2 chain covers v2-only codepoints.
    let first = marker_inked_font();
    let second = symbol_block_font();
    let icon = SYMBOL_ICON;
    assert!(is_symbol_codepoint(icon));
    assert!(
        font_has_glyph(&second, icon)
            && !font_has_glyph(&first, icon)
            && !font_has_glyph(&primary, icon)
    );
    let mut atlas = GlyphAtlas::build(&primary, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    // Order matters: `first` (no glyph) leads, `second` (has glyph) follows.
    atlas.set_fallback_fonts(vec![Arc::new(first), Arc::new(second)]);
    let uv = atlas.ensure(&primary, icon).expect("chain fallback uv");
    assert_ne!(
        uv, box_uv,
        "chain must resolve the glyph from the later face, not the hollow box"
    );
    assert!(
        cell_ink(&atlas, uv) > 0,
        "chain-resolved glyph should rasterize visible ink"
    );
}

#[test]
fn empty_chain_keeps_the_hollow_box() {
    let font = test_font();
    let absent = pua_absent(&font).expect("bundled face: font covers all of the BMP PUA");
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    // Explicitly installing an empty chain is identical to never installing one.
    atlas.set_fallback_fonts(Vec::new());
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let uv = atlas.ensure(&font, absent).expect("fallback uv");
    assert_eq!(uv, box_uv, "empty chain must keep the hollow-box path");
}

/// Runtime per-codepoint symbol fallback on Linux: the resolver is
/// consulted only on a static-chain miss, cached per-codepoint (so the
/// subprocess never runs more than once per distinct missing codepoint), and never
/// consulted when the static chain already covers the glyph (the byte-identity
/// guarantee). Each test owns distinct statics so the bare-`fn` resolvers stay
/// parallel-safe.
mod runtime_resolver {
    use super::*;
    use crate::atlas::RuntimeSymbol;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static POS_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn pos_resolver(_ch: char) -> RuntimeSymbol {
        POS_CALLS.fetch_add(1, Ordering::SeqCst);
        RuntimeSymbol::Ready(Some(Arc::new(symbol_block_font())))
    }

    // `NEG_CALLS` / `neg_resolver` are owned exclusively by
    // `negative_runtime_result_is_cached`, which asserts on the exact call
    // count. No other test may mutate this counter, or the assertion would
    // observe another thread's invocations under parallel scheduling.
    static NEG_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn neg_resolver(_ch: char) -> RuntimeSymbol {
        NEG_CALLS.fetch_add(1, Ordering::SeqCst);
        RuntimeSymbol::Ready(None)
    }

    static HIT_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn hit_resolver(_ch: char) -> RuntimeSymbol {
        HIT_CALLS.fetch_add(1, Ordering::SeqCst);
        RuntimeSymbol::Ready(None)
    }

    static UNIVERSAL_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn universal_resolver(_ch: char) -> RuntimeSymbol {
        UNIVERSAL_CALLS.fetch_add(1, Ordering::SeqCst);
        RuntimeSymbol::Ready(Some(Arc::new(marker_inked_font())))
    }

    // Owned by `reinstalling_resolver_clears_the_cache`, which asserts on the
    // exact call count.
    static CLEAR_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn clear_counting_resolver(_ch: char) -> RuntimeSymbol {
        CLEAR_CALLS.fetch_add(1, Ordering::SeqCst);
        RuntimeSymbol::Ready(None)
    }

    static ASCII_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn ascii_resolver(_ch: char) -> RuntimeSymbol {
        ASCII_CALLS.fetch_add(1, Ordering::SeqCst);
        RuntimeSymbol::Ready(Some(Arc::new(test_font())))
    }

    #[test]
    fn missing_ascii_uses_runtime_fallback_only_when_ensured() {
        let primary = FontHandle::try_from_vec(
            include_bytes!("../../../tests/fixtures/fonts/s5b/brahmic-a/Balinese-subset.ttf")
                .to_vec(),
        )
        .expect("Balinese subset fixture");
        assert_eq!(primary.glyph_id('a').0, 0, "primary fixture omits ASCII a");
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        atlas.set_fallback_fonts(Vec::new());
        ASCII_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(ascii_resolver));
        assert_eq!(ASCII_CALLS.load(Ordering::SeqCst), 0, "setup stays lazy");

        let uv = atlas.ensure(&primary, 'a').expect("ASCII atlas cell");
        assert!(
            cell_ink(&atlas, uv) > 0,
            "runtime fallback must draw ASCII a"
        );
        atlas
            .ensure(&primary, 'a')
            .expect("cached ASCII atlas cell");
        assert_eq!(ASCII_CALLS.load(Ordering::SeqCst), 1, "answer is cached");
    }

    /// The final answer of one `symbol_fallback` query. The resolver tests
    /// query it directly: `ensure` stores its first answer in the atlas
    /// (`dynamic`), so a second `ensure` of the same codepoint never reaches
    /// the per-codepoint resolver cache these tests are about.
    fn settled(answer: crate::atlas::SymbolFallback) -> Option<Arc<FontHandle>> {
        match answer {
            crate::atlas::SymbolFallback::Settled(face) => face,
            crate::atlas::SymbolFallback::Pending => panic!("the test resolvers never defer"),
        }
    }

    static EXCLUDED_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn excluded_resolver(_ch: char) -> RuntimeSymbol {
        EXCLUDED_CALLS.fetch_add(1, Ordering::SeqCst);
        RuntimeSymbol::Ready(Some(Arc::new(marker_inked_font())))
    }

    /// Static chain misses -> resolver resolves the glyph; a second lookup of the
    /// same codepoint is served from the cache (resolver called exactly once).
    #[test]
    fn static_miss_resolves_at_runtime_and_caches() {
        // A fixture face stands in for the installed symbol font, so the test
        // asserts on every host. The primary maps no private-use codepoint.
        let primary = marker_blank_font();
        let symbol = symbol_block_font();
        let icon = SYMBOL_ICON;
        assert!(is_symbol_codepoint(icon));
        assert!(font_has_glyph(&symbol, icon) && !font_has_glyph(&primary, icon));
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        // Empty static chain forces the runtime resolver to be consulted.
        atlas.set_fallback_fonts(Vec::new());
        POS_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(pos_resolver));
        let uv1 = atlas.ensure(&primary, icon).expect("runtime fallback uv");
        assert_ne!(uv1, box_uv, "runtime resolver must back the missing glyph");
        assert_eq!(POS_CALLS.load(Ordering::SeqCst), 1, "one resolver query");
        // The atlas keeps the placed slot, and the resolver cache answers a
        // direct repeat query without calling the resolver again.
        let uv2 = atlas.ensure(&primary, icon).expect("cached fallback uv");
        assert_eq!(uv1, uv2, "second lookup must reuse the resolved slot");
        assert!(settled(atlas.symbol_fallback(icon)).is_some());
        assert_eq!(
            POS_CALLS.load(Ordering::SeqCst),
            1,
            "resolver must be consulted at most once per codepoint (cached)"
        );
    }

    /// Non-symbol printable codepoints also reach the runtime resolver, and the
    /// positive result is cached just like the historical PUA path.
    #[test]
    fn non_symbol_static_miss_resolves_at_runtime_and_caches() {
        let primary = marker_blank_font();
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        atlas.set_fallback_fonts(Vec::new());
        UNIVERSAL_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(universal_resolver));

        let uv1 = atlas
            .ensure(&primary, NON_SYMBOL_FALLBACK)
            .expect("runtime non-symbol fallback uv");
        assert_ne!(
            uv1, box_uv,
            "runtime resolver must back the missing non-symbol glyph"
        );
        let uv2 = atlas
            .ensure(&primary, NON_SYMBOL_FALLBACK)
            .expect("cached runtime non-symbol fallback uv");
        assert_eq!(uv1, uv2, "second lookup must reuse the resolved slot");
        assert!(settled(atlas.symbol_fallback(NON_SYMBOL_FALLBACK)).is_some());
        assert_eq!(
            UNIVERSAL_CALLS.load(Ordering::SeqCst),
            1,
            "resolver must be consulted at most once per non-symbol codepoint"
        );
    }

    /// Excluded categories must not reach the runtime resolver even when one is
    /// installed and willing to return a font.
    #[test]
    fn excluded_categories_never_consult_runtime_resolver() {
        for ch in ['\u{0301}', '\u{0007}', '\u{200D}', '\u{FE0F}'] {
            let primary = marker_blank_font();
            let mut atlas = GlyphAtlas::build(&primary, 24.0);
            let box_uv = atlas.slot_uv(FALLBACK_SLOT);
            let count = atlas.slot_count();
            atlas.set_fallback_fonts(Vec::new());
            EXCLUDED_CALLS.store(0, Ordering::SeqCst);
            atlas.set_runtime_symbol_resolver(Some(excluded_resolver));

            let uv = atlas.ensure(&primary, ch);
            if wants_glyph(ch) {
                assert_eq!(uv, Some(box_uv), "{ch:?} must stay on the hollow box");
            } else {
                assert_eq!(uv, None, "{ch:?} must draw no standalone glyph");
            }
            assert_eq!(
                EXCLUDED_CALLS.load(Ordering::SeqCst),
                0,
                "{ch:?} must not consult the runtime resolver"
            );
            assert_eq!(
                atlas.slot_count(),
                count,
                "{ch:?} must not allocate through runtime fallback"
            );
        }
    }

    /// A negative resolver result is cached too: the codepoint keeps the hollow
    /// box and the resolver is not re-invoked on a repeat lookup.
    #[test]
    fn negative_runtime_result_is_cached() {
        let primary = test_font();
        let absent = pua_absent(&primary).expect("bundled face: font covers all of the BMP PUA");
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        atlas.set_fallback_fonts(Vec::new());
        NEG_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(neg_resolver));
        let uv1 = atlas.ensure(&primary, absent).expect("box uv");
        assert_eq!(uv1, box_uv, "unresolved codepoint keeps the hollow box");
        assert_eq!(
            NEG_CALLS.load(Ordering::SeqCst),
            1,
            "the first lookup queries the resolver once"
        );
        // The repeat goes through the resolver cache directly, since `ensure`
        // answers from its own slot and never gets there.
        assert!(settled(atlas.symbol_fallback(absent)).is_none());
        assert!(settled(atlas.symbol_fallback(absent)).is_none());
        assert_eq!(
            NEG_CALLS.load(Ordering::SeqCst),
            1,
            "a negative result must be cached, not re-queried"
        );
    }

    /// When the static chain already covers the glyph the runtime resolver is
    /// never consulted -- already-resolving codepoints take the exact same path
    /// as before the runtime query existed.
    #[test]
    fn static_hit_never_consults_runtime_resolver() {
        // A fixture face stands in for the installed symbol font, so the test
        // asserts on every host. The primary maps no private-use codepoint.
        let primary = marker_blank_font();
        let symbol = symbol_block_font();
        let icon = SYMBOL_ICON;
        assert!(is_symbol_codepoint(icon));
        assert!(font_has_glyph(&symbol, icon) && !font_has_glyph(&primary, icon));
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        // Static chain covers `icon`; resolver installed but must stay unused.
        atlas.set_fallback_fonts(vec![Arc::new(symbol)]);
        HIT_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(hit_resolver));
        let uv = atlas.ensure(&primary, icon).expect("static fallback uv");
        assert_ne!(uv, box_uv, "static chain must resolve the glyph");
        assert_eq!(
            HIT_CALLS.load(Ordering::SeqCst),
            0,
            "static-covered glyph must not consult the runtime resolver"
        );
    }

    /// Switching the resolver clears the per-codepoint cache: the same resolver
    /// is queried again after a reinstall, and clearing it to `None` disables
    /// the query.
    #[test]
    fn reinstalling_resolver_clears_the_cache() {
        let primary = test_font();
        let absent = pua_absent(&primary).expect("bundled face: font covers all of the BMP PUA");
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        atlas.set_fallback_fonts(Vec::new());
        CLEAR_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(clear_counting_resolver));
        assert!(settled(atlas.symbol_fallback(absent)).is_none());
        assert!(settled(atlas.symbol_fallback(absent)).is_none());
        assert_eq!(
            CLEAR_CALLS.load(Ordering::SeqCst),
            1,
            "second query is cached"
        );
        // Reinstalling clears the cache, so the resolver is asked again.
        atlas.set_runtime_symbol_resolver(Some(clear_counting_resolver));
        assert!(settled(atlas.symbol_fallback(absent)).is_none());
        assert_eq!(
            CLEAR_CALLS.load(Ordering::SeqCst),
            2,
            "a reinstall must drop the cached answer"
        );
        // Clearing to None disables the query: the hollow box stays.
        atlas.set_runtime_symbol_resolver(None);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        let uv = atlas.ensure(&primary, absent).expect("box uv");
        assert_eq!(uv, box_uv, "no resolver keeps the hollow box");
        assert_eq!(
            CLEAR_CALLS.load(Ordering::SeqCst),
            2,
            "no query without a resolver"
        );
    }
}
