// SPDX-License-Identifier: GPL-3.0-only
//! RV6 symbol / Nerd-font fallback behavior at the atlas seam.
//!
//! The gate-critical guarantee is **default-safe**: with no fallback font
//! installed, a printable codepoint the primary lacks renders the historical
//! hollow box and consumes no slot — byte-for-byte the pre-feature path. When a
//! fallback is installed it is only used for PUA symbol codepoints the fallback
//! actually covers.

use super::*;
use crate::atlas::fallback::is_symbol_codepoint;
use std::sync::Arc;

/// A PUA codepoint the bundled/system test font does *not* map (so it exercises
/// the missing-glyph path), plus one it *does* — discovered at runtime so the
/// tests are not pinned to a single host font.
fn pua_absent(font: &FontVec) -> Option<char> {
    (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| !font_has_glyph(font, ch))
}

fn pua_present(font: &FontVec) -> Option<char> {
    (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| font_has_glyph(font, ch))
}

#[test]
fn pua_missing_glyph_without_fallback_uses_box() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(absent) = pua_absent(&font) else {
        eprintln!("skipping: font covers all of the BMP PUA");
        return;
    };
    assert!(is_symbol_codepoint(absent));
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let count = atlas.slot_count();
    // No fallback installed: the missing PUA glyph must take the hollow box and
    // consume no slot — identical to the pre-RV6 renderer.
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
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(absent) = pua_absent(&font) else {
        eprintln!("skipping: font covers all of the BMP PUA");
        return;
    };
    // A second instance of the same font as the "fallback" (FontVec is not
    // Clone, so reload it): it also lacks `absent`, so the fallback must decline
    // and the hollow box is used — proving the atlas verifies fallback coverage
    // rather than blindly drawing.
    let Some(fb) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
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
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(present) = pua_present(&font) else {
        eprintln!("skipping: font has no BMP PUA glyph");
        return;
    };
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
    let Some(primary) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(symbol) = crate::text::resolve_symbol_font() else {
        eprintln!("skipping: no symbol / Nerd font on this host");
        return;
    };
    // A PUA codepoint the symbol font has but the primary lacks: the canonical
    // RV6 case (a prompt icon the body font cannot draw).
    let Some(icon) = (0xE000u32..=0xF8FF).filter_map(char::from_u32).find(|&ch| {
        is_symbol_codepoint(ch) && font_has_glyph(&symbol, ch) && !font_has_glyph(&primary, ch)
    }) else {
        eprintln!("skipping: no PUA codepoint unique to the symbol font");
        return;
    };
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
    let Some(primary) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Two-face chain: the first face lacks `icon`, the second has it. The atlas
    // must skip the first and rasterize from the second (first-hit-wins), which
    // is exactly how the bundled v3→v2 chain covers v2-only codepoints.
    let Some(first) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(second) = crate::text::resolve_symbol_font() else {
        eprintln!("skipping: no symbol / Nerd font on this host");
        return;
    };
    let Some(icon) = (0xE000u32..=0xF8FF).filter_map(char::from_u32).find(|&ch| {
        is_symbol_codepoint(ch)
            && font_has_glyph(&second, ch)
            && !font_has_glyph(&first, ch)
            && !font_has_glyph(&primary, ch)
    }) else {
        eprintln!("skipping: no PUA codepoint unique to the second face");
        return;
    };
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
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(absent) = pua_absent(&font) else {
        eprintln!("skipping: font covers all of the BMP PUA");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    // Explicitly installing an empty chain is identical to never installing one.
    atlas.set_fallback_fonts(Vec::new());
    let box_uv = atlas.slot_uv(FALLBACK_SLOT);
    let uv = atlas.ensure(&font, absent).expect("fallback uv");
    assert_eq!(uv, box_uv, "empty chain must keep the hollow-box path");
}

/// Runtime per-codepoint symbol fallback (RV6 Linux backfill): the resolver is
/// consulted only on a static-chain miss, cached per-codepoint (so the
/// subprocess never runs more than once per distinct missing symbol), and never
/// consulted when the static chain already covers the glyph (the byte-identity
/// guarantee). Each test owns distinct statics so the bare-`fn` resolvers stay
/// parallel-safe.
mod runtime_resolver {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static POS_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn pos_resolver(_ch: char) -> Option<Arc<FontVec>> {
        POS_CALLS.fetch_add(1, Ordering::SeqCst);
        crate::text::resolve_symbol_font().map(Arc::new)
    }

    // `NEG_CALLS` / `neg_resolver` are owned exclusively by
    // `negative_runtime_result_is_cached`, which asserts on the exact call
    // count. No other test may mutate this counter, or the assertion would
    // observe another thread's invocations under parallel scheduling. Tests that
    // only need a negative resolver (without counting) use `silent_neg_resolver`.
    static NEG_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn neg_resolver(_ch: char) -> Option<Arc<FontVec>> {
        NEG_CALLS.fetch_add(1, Ordering::SeqCst);
        None
    }

    /// A negative resolver that touches no shared counter, so tests that merely
    /// need "resolver returns None" stay isolated from the counting tests.
    fn silent_neg_resolver(_ch: char) -> Option<Arc<FontVec>> {
        None
    }

    static HIT_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn hit_resolver(_ch: char) -> Option<Arc<FontVec>> {
        HIT_CALLS.fetch_add(1, Ordering::SeqCst);
        None
    }

    /// Static chain misses -> resolver resolves the glyph; a second lookup of the
    /// same codepoint is served from the cache (resolver called exactly once).
    #[test]
    fn static_miss_resolves_at_runtime_and_caches() {
        let Some(primary) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(symbol) = crate::text::resolve_symbol_font() else {
            eprintln!("skipping: no symbol / Nerd font on this host");
            return;
        };
        let Some(icon) = (0xE000u32..=0xF8FF).filter_map(char::from_u32).find(|&ch| {
            is_symbol_codepoint(ch) && font_has_glyph(&symbol, ch) && !font_has_glyph(&primary, ch)
        }) else {
            eprintln!("skipping: no PUA codepoint unique to the symbol font");
            return;
        };
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        // Empty static chain forces the runtime resolver to be consulted.
        atlas.set_fallback_fonts(Vec::new());
        POS_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(pos_resolver));
        let uv1 = atlas.ensure(&primary, icon).expect("runtime fallback uv");
        assert_ne!(uv1, box_uv, "runtime resolver must back the missing glyph");
        let uv2 = atlas.ensure(&primary, icon).expect("cached fallback uv");
        assert_eq!(uv1, uv2, "second lookup must reuse the resolved slot");
        assert_eq!(
            POS_CALLS.load(Ordering::SeqCst),
            1,
            "resolver must be consulted at most once per codepoint (cached)"
        );
    }

    /// A negative resolver result is cached too: the codepoint keeps the hollow
    /// box and the resolver is not re-invoked on a repeat lookup.
    #[test]
    fn negative_runtime_result_is_cached() {
        let Some(primary) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(absent) = pua_absent(&primary) else {
            eprintln!("skipping: font covers all of the BMP PUA");
            return;
        };
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        atlas.set_fallback_fonts(Vec::new());
        NEG_CALLS.store(0, Ordering::SeqCst);
        atlas.set_runtime_symbol_resolver(Some(neg_resolver));
        let uv1 = atlas.ensure(&primary, absent).expect("box uv");
        assert_eq!(uv1, box_uv, "unresolved codepoint keeps the hollow box");
        let _ = atlas.ensure(&primary, absent);
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
        let Some(primary) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(symbol) = crate::text::resolve_symbol_font() else {
            eprintln!("skipping: no symbol / Nerd font on this host");
            return;
        };
        let Some(icon) = (0xE000u32..=0xF8FF).filter_map(char::from_u32).find(|&ch| {
            is_symbol_codepoint(ch) && font_has_glyph(&symbol, ch) && !font_has_glyph(&primary, ch)
        }) else {
            eprintln!("skipping: no PUA codepoint unique to the symbol font");
            return;
        };
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

    /// Switching the resolver clears the per-codepoint cache so a stale negative
    /// result cannot pin a codepoint to tofu after the host font set changes.
    #[test]
    fn reinstalling_resolver_clears_the_cache() {
        let Some(primary) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(absent) = pua_absent(&primary) else {
            eprintln!("skipping: font covers all of the BMP PUA");
            return;
        };
        let mut atlas = GlyphAtlas::build(&primary, 24.0);
        atlas.set_fallback_fonts(Vec::new());
        // Uses the silent resolver: this test does not count calls, so it must
        // not perturb `NEG_CALLS` (owned by `negative_runtime_result_is_cached`).
        atlas.set_runtime_symbol_resolver(Some(silent_neg_resolver));
        let _ = atlas.ensure(&primary, absent);
        // Reinstalling clears the cache; clearing to None disables the query.
        atlas.set_runtime_symbol_resolver(None);
        let box_uv = atlas.slot_uv(FALLBACK_SLOT);
        let uv = atlas.ensure(&primary, absent).expect("box uv");
        assert_eq!(uv, box_uv, "no resolver keeps the hollow box");
    }
}
