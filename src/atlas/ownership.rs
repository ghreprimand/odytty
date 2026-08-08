// SPDX-License-Identifier: GPL-3.0-only
//! Runtime font-source, synthesis, geometry, and capacity policy owned by an atlas.

use super::*;

impl GlyphAtlas {
    /// Declare which styles have **no real face** and must be synthesized from
    /// the Regular outline. `bold`/`italic`/`bold_italic` are `true` when the
    /// loaded font family lacks that face, so glyphs rasterized for it should be
    /// emboldened and/or sheared rather than rendered as a plain Regular copy.
    ///
    /// The native layer computes these from `Arc` identity of its loaded faces
    /// (a style slot still pointing at the Regular `Arc` means no real face) and
    /// calls this **immediately after** [`Self::build`], before any styled glyph
    /// is inserted. The mask only governs glyphs rasterized after it is set;
    /// because a font change rebuilds the atlas from scratch, swapping in a real
    /// face clears the corresponding bit and the synthetic slots vanish with the
    /// old atlas — invalidation is by construction, exactly like every other
    /// dynamic slot. Calling this is idempotent and never rewrites existing
    /// pixels, so the live path sets it once on a freshly built (empty-dynamic)
    /// atlas.
    pub fn set_synthetic_styles(&mut self, bold: bool, italic: bool, bold_italic: bool) {
        self.synthetic = (bold as u8) | ((italic as u8) << 1) | ((bold_italic as u8) << 2);
    }

    /// Enable or disable geometric box-drawing / block / Powerline rendering
    /// (RV2). When enabled, codepoints [`crate::boxdraw::covers`] recognizes are
    /// rasterized from computed cell-aligned geometry instead of the font glyph,
    /// so TUI borders, progress bars and powerline prompts are pixel-perfect and
    /// seamless at any cell size; everything else still uses the font.
    ///
    /// `false` (the default) is a true no-op: every glyph takes the font path
    /// and the atlas is byte-identical to the pre-feature renderer. Like
    /// [`Self::set_synthetic_styles`], this only affects glyphs rasterized after
    /// it is set, so the native layer calls it on a freshly built atlas and
    /// rebuilds when the setting toggles (resident slots are never rewritten).
    pub fn set_geometric_boxdraw(&mut self, on: bool) {
        self.geometric = on;
    }

    /// Install (or clear) the symbol / Nerd-font fallback **chain** (RV6).
    ///
    /// When non-empty, a printable spacing codepoint the **primary** font lacks
    /// is rasterized from the first chain face that has it; controls, format
    /// characters, whitespace, combining marks, and variation selectors keep the
    /// historical hollow-box/no-glyph path. The chain composes coverage across
    /// faces (bundled v3, then v2, then a host face), so a glyph absent from
    /// earlier faces still resolves from a later one. An empty `Vec` (the
    /// default) restores the pre-feature missing-glyph path exactly, so a build
    /// with no fallback is byte-identical to the minimal renderer.
    ///
    /// Like [`Self::set_synthetic_styles`] / [`Self::set_geometric_boxdraw`],
    /// this only governs glyphs rasterized after it is set; the native layer
    /// installs it on a freshly built atlas and reinstalls it after a rebuild,
    /// so the dynamic region never mixes resolved and unresolved fallbacks.
    pub fn set_fallback_fonts(&mut self, fonts: Vec<Arc<FontVec>>) {
        self.fallback_chain = fonts;
    }

    /// Install the resolved SYMMAP override faces (`(start, end, face)` ranges,
    /// first-match-wins). An empty `Vec` restores the no-override path. The
    /// native layer calls this after build and on every atlas rebuild; like the
    /// fallback/geometric switches it only governs glyphs rasterized after it is
    /// set, and the atlas is rebuilt (clearing the dynamic region) when the map
    /// changes, so cached slots never mix faces.
    pub fn set_symbol_map_fonts(&mut self, fonts: Vec<(u32, u32, Arc<FontVec>)>) {
        self.symbol_map_fonts = fonts;
    }

    /// Bind dynamic growth to the active device's maximum 2D texture height.
    /// Existing base glyphs stay resident; new glyphs use the fallback once
    /// another complete atlas row would cross `max_dimension`.
    pub fn set_texture_dimension_limit(&mut self, max_dimension: u32) {
        let rows = max_dimension / slot_h(self.cell);
        let reachable_rows = self.capacity_rows
            + rows.saturating_sub(self.capacity_rows) / ATLAS_GROW_ROWS * ATLAS_GROW_ROWS;
        self.max_slots = reachable_rows
            .saturating_mul(self.cols)
            .min(MAX_ATLAS_SLOTS)
            .max(self.next_slot);
    }

    /// Install (or clear) the runtime per-codepoint glyph fallback resolver
    /// (RV6 Linux backfill). When `Some`, a printable spacing codepoint that
    /// misses the static [`Self::fallback_chain`] triggers a single, cached call
    /// to the resolver (the native layer wires this to a `fc-match :charset`
    /// query -- see [`crate::text::runtime_resolve_symbol_font`]). `None` (the
    /// default, and the only state on macOS/non-Unix) disables the runtime
    /// query, so the glyph path stays byte-identical to the pre-feature
    /// renderer; the static chain still resolves exactly as before either way.
    /// Switching the resolver clears the per-codepoint cache so a stale negative
    /// result never pins a codepoint to tofu after the host font set changes.
    /// Like the fallback/geometric switches, the native layer reinstalls it
    /// after every atlas rebuild.
    pub fn set_runtime_symbol_resolver(&mut self, resolver: Option<RuntimeSymbolResolver>) {
        self.runtime_symbol_resolver = resolver;
        self.runtime_symbol_cache.clear();
    }

    /// The SYMMAP override face for `ch`, or `None` when no rule matches (the
    /// identity / off path). With no rules the `Vec` is empty and the scan is
    /// skipped entirely, so the default costs nothing. First-match-wins matches
    /// `text::SymbolMap` precedence.
    pub(super) fn symbol_map_font_for(&self, ch: char) -> Option<Arc<FontVec>> {
        if self.symbol_map_fonts.is_empty() {
            return None;
        }
        let cp = ch as u32;
        self.symbol_map_fonts
            .iter()
            .find(|(start, end, _)| *start <= cp && cp <= *end)
            .map(|(_, _, font)| Arc::clone(font))
    }

    /// The fallback font to rasterize `ch` from when the primary lacks it, or
    /// `None` to keep the hollow-box behavior. Walks the fallback chain and
    /// returns the **first** face that has a glyph for `ch` -- but only when
    /// `ch` is a printable spacing codepoint. A codepoint no chain face provides
    /// (or an empty chain) yields `None`, preserving the hollow-box path.
    pub(super) fn symbol_fallback(&mut self, ch: char) -> Option<Arc<FontVec>> {
        if !should_attempt_fallback(ch) {
            return None;
        }
        // Static chain first: bundled Nerd faces, host face, and (macOS) the
        // system tail. A codepoint covered here takes the exact same path as
        // before the runtime resolver existed, so already-resolving glyphs are
        // byte-identical. When the resolver is `None` (the default and the only
        // state off-Linux) this is the whole function, identical to the
        // pre-feature behavior including the empty-chain case.
        if let Some(fb) = self.fallback_chain.iter().find(|fb| font_has_glyph(fb, ch)) {
            return Some(Arc::clone(fb));
        }
        // Static chain missed. Consult the runtime resolver (Linux fc-match)
        // exactly once per codepoint, caching the result -- including a negative
        // result -- so the subprocess never runs on the hot path more than once
        // per distinct missing codepoint.
        let resolver = self.runtime_symbol_resolver?;
        if let Some(cached) = self.runtime_symbol_cache.get(&ch) {
            return cached.clone();
        }
        let resolved = resolver(ch);
        self.runtime_symbol_cache.insert(ch, resolved.clone());
        resolved
    }

    /// The [`SynthTransform`] to apply when rasterizing `style`. Returns the
    /// identity transform for [`FontStyle::Regular`] and for any style whose
    /// synthetic bit is clear (a real face is present); otherwise the matching
    /// emboldening (bold) and/or shear (italic), sized from the build pixel size.
    pub(super) fn synth_for(&self, style: FontStyle) -> SynthTransform {
        let bit = match style {
            FontStyle::Regular => return SynthTransform::none(),
            FontStyle::Bold => 0,
            FontStyle::Italic => 1,
            FontStyle::BoldItalic => 2,
        };
        if self.synthetic & (1 << bit) == 0 {
            return SynthTransform::none();
        }
        let bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
        let italic = matches!(style, FontStyle::Italic | FontStyle::BoldItalic);
        SynthTransform {
            // 1px at typical sizes, 2px on large HiDPI cells; never 0 when bold.
            embolden_px: if bold {
                (self.px / 24.0).round().max(1.0) as u32
            } else {
                0
            },
            shear: if italic { ITALIC_SHEAR } else { 0.0 },
        }
    }
}
