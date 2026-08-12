// SPDX-License-Identifier: GPL-3.0-only
//! Style-face resolution and symbol/Nerd-font fallback helpers.
//!
//! Extracted byte-identical from `gpu.rs` to keep that file under the
//! modularity cap. Holds the [`StyleFonts`] four-face set plus the free
//! functions that resolve the optional symbol fallback face and the SYMMAP
//! per-codepoint font overrides. No behavior change from the in-`gpu.rs`
//! version — only the module location and the visibility qualifiers (now
//! `pub(in crate::native)` / `pub(super)` to span the extra module depth).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_glyph::FontVec;

use crate::ligature::LigatureFonts;
use crate::native::options::{NativeError, NativeOptions};
use crate::text::{self, FontStyle};

#[derive(Debug, Clone)]
pub(in crate::native) struct StyleFonts {
    regular: Arc<FontVec>,
    bold: Arc<FontVec>,
    italic: Arc<FontVec>,
    bold_italic: Arc<FontVec>,
}

impl StyleFonts {
    pub(in crate::native) fn regular(font: FontVec) -> Self {
        let font = Arc::new(font);
        Self {
            regular: font.clone(),
            bold: font.clone(),
            italic: font.clone(),
            bold_italic: font,
        }
    }

    pub(super) fn load(options: &NativeOptions) -> Result<Self, NativeError> {
        Self::load_from(
            options.font_path.as_deref(),
            &options.font_family,
            &options.font_weight,
        )
    }

    /// Resolve the four style faces for the configured family.
    ///
    /// RV7 / FONT-WEIGHT-FIX: `font_weight` is an optional weight-variant
    /// suffix. When empty (the default) the regular face is loaded via
    /// [`text::load_font_with_path`] exactly as before — byte-identical off
    /// path. When set, the weight face is selected directly within the family by
    /// [`text::resolve_font_weight_face`], which scans for the file whose stem
    /// carries both the family and the weight term WITHOUT the regular-face
    /// filter (the old `"{family} {weight}"`-concat path was self-defeating: the
    /// regular-face filter excluded the very Bold file the query found, so every
    /// weight silently fell back to regular). A missing weight face warns and
    /// falls back to the plain regular face (real faces only — never synthetic
    /// emboldening/thinning). Bold/italic discovery ALWAYS uses the plain
    /// `font_family` (not the weight query), so the SGR bold attribute stays
    /// visually distinct from the chosen base weight.
    pub(super) fn load_from(
        font_path: Option<&Path>,
        font_family: &str,
        font_weight: &str,
    ) -> Result<Self, NativeError> {
        if font_path.is_none() && text::is_bundled_font_family(font_family) {
            return Self::load_bundled(font_family, font_weight);
        }

        let regular = if font_weight.trim().is_empty() {
            text::load_font_with_path(font_path)
                .map_err(|err| NativeError::Text(err.to_string()))?
        } else {
            let family = font_family.trim();
            let weight = font_weight.trim();
            match text::resolve_font_weight_face(family, weight, &text::font_search_dirs()) {
                Some(path) => match text::load_font_at(&path) {
                    Ok(font) => font,
                    Err(err) => {
                        tracing::warn!(
                            "font_weight: {err}; falling back to the regular face for {family:?} {weight:?}"
                        );
                        text::load_font_with_path(font_path)
                            .map_err(|err| NativeError::Text(err.to_string()))?
                    }
                },
                None => {
                    tracing::warn!(
                        "font_weight: no {weight:?} face found for family {family:?}; using the regular face"
                    );
                    text::load_font_with_path(font_path)
                        .map_err(|err| NativeError::Text(err.to_string()))?
                }
            }
        };
        let mut fonts = Self::regular(regular);

        // BOLD INVARIANT: bold/italic discovery always uses the PLAIN family so
        // SGR bold contrasts with the chosen base weight (never "Light Bold").
        if let Some(matched) = text::resolve_font_family(font_family, &text::font_search_dirs()) {
            if let Some(font) = matched.bold.as_deref().and_then(load_optional_style_font) {
                fonts.bold = Arc::new(font);
            }
            if let Some(font) = matched.italic.as_deref().and_then(load_optional_style_font) {
                fonts.italic = Arc::new(font);
            }
            if let Some(font) = matched
                .bold_italic
                .as_deref()
                .and_then(load_optional_style_font)
            {
                fonts.bold_italic = Arc::new(font);
            }
        }

        Ok(fonts)
    }

    fn load_bundled(font_family: &str, font_weight: &str) -> Result<Self, NativeError> {
        // Resolve the concrete bundled family (Victor Mono default, JetBrains
        // Mono selectable, "monospace"/empty → default). All weight/style faces
        // are then drawn from that one family.
        let family = text::bundled_family_for(font_family);
        let regular = if font_weight.trim().is_empty() {
            text::load_bundled_style_for(family, FontStyle::Regular)
        } else {
            match text::load_bundled_weight_for(family, font_weight.trim(), false) {
                Some(font) => Ok(font),
                None => {
                    tracing::warn!(
                        "font_weight: bundled {family} has no {:?} weight face; using the regular face",
                        font_weight.trim()
                    );
                    text::load_bundled_style_for(family, FontStyle::Regular)
                }
            }
        }
        .map_err(|err| NativeError::Text(err.to_string()))?;

        let regular = Arc::new(regular);
        let bold = Arc::new(
            text::load_bundled_style_for(family, FontStyle::Bold)
                .map_err(|err| NativeError::Text(err.to_string()))?,
        );
        let italic = Arc::new(
            text::load_bundled_style_for(family, FontStyle::Italic)
                .map_err(|err| NativeError::Text(err.to_string()))?,
        );
        let bold_italic = Arc::new(
            text::load_bundled_style_for(family, FontStyle::BoldItalic)
                .map_err(|err| NativeError::Text(err.to_string()))?,
        );

        Ok(Self {
            regular,
            bold,
            italic,
            bold_italic,
        })
    }

    pub(in crate::native) fn font_for(&self, style: FontStyle) -> &FontVec {
        match style {
            FontStyle::Regular => &self.regular,
            FontStyle::Bold => &self.bold,
            FontStyle::Italic => &self.italic,
            FontStyle::BoldItalic => &self.bold_italic,
        }
    }

    /// Which styles have **no real face** loaded and must be synthesized from the
    /// Regular outline. Returns `(bold, italic, bold_italic)`, each `true` when
    /// that style slot is still the very same `Arc` as Regular — which is exactly
    /// the state [`StyleFonts::regular`] leaves a slot in until `load_from`
    /// replaces it with a real face from disk. Drives
    /// [`GlyphAtlas::set_synthetic_styles`].
    pub(in crate::native) fn synthetic_mask(&self) -> (bool, bool, bool) {
        (
            Arc::ptr_eq(&self.regular, &self.bold),
            Arc::ptr_eq(&self.regular, &self.italic),
            Arc::ptr_eq(&self.regular, &self.bold_italic),
        )
    }

    pub(super) fn regular_font(&self) -> &FontVec {
        &self.regular
    }
}

impl LigatureFonts for StyleFonts {
    fn ligature_font(&self, style: FontStyle) -> &FontVec {
        self.font_for(style)
    }
}

fn load_optional_style_font(path: &std::path::Path) -> Option<FontVec> {
    text::load_font_at(path).ok()
}

/// Effective RV6 symbol / Nerd-font fallback switch. The legacy env var remains
/// an override for native smoke setups; otherwise the first-class setting drives
/// the renderer.
pub(super) fn effective_symbol_fallback_enabled() -> bool {
    env_flag_override(crate::settings::SYMBOL_FALLBACK_ENV)
        .unwrap_or_else(crate::settings::symbol_fallback_enabled)
}

/// Effective explicit symbol font path. The legacy env path wins when present;
/// otherwise the first-class setting may name a font file. `None` means
/// auto-discover a suitable symbol face.
pub(super) fn effective_symbol_font_path() -> Option<PathBuf> {
    std::env::var_os(crate::settings::SYMBOL_FONT_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(crate::settings::symbol_font_path)
}

/// Resolve the ordered symbol / Nerd-font fallback **chain** when enabled, else
/// an empty `Vec`. A missing chain with the switch on is not fatal -- the
/// renderer keeps the hollow-box behavior.
///
/// Chain order is **explicit > bundled (v3, then v2) > host**, owned by
/// [`text::resolve_symbol_fonts_with_source`] so the renderer and `--show-config`
/// agree on which faces are in play. The bundled faces are the reliable default,
/// so the out-of-the-box icon path does not depend on host-installed Nerd fonts;
/// the atlas walks the chain per glyph, so coverage is the union of all faces.
pub(super) fn resolve_symbol_fallback(
    enabled: bool,
    explicit_path: Option<&Path>,
) -> Vec<Arc<FontVec>> {
    let inventory = text::FontFileInventory::new(text::font_search_dirs());
    resolve_symbol_fallback_with_inventory(enabled, explicit_path, &inventory)
}

pub(super) fn resolve_symbol_fallback_with_inventory(
    enabled: bool,
    explicit_path: Option<&Path>,
    inventory: &text::FontFileInventory,
) -> Vec<Arc<FontVec>> {
    if !enabled {
        return Vec::new();
    }
    text::resolve_symbol_fonts_with_inventory(explicit_path, inventory)
        .1
        .into_iter()
        .map(Arc::new)
        .collect()
}

/// Install the runtime per-codepoint symbol fallback resolver on `atlas` (RV6
/// Linux backfill), or clear it. On Linux/Unix (non-macOS) the resolver is a
/// `fc-match :charset` query ([`text::runtime_resolve_symbol_font`]) consulted
/// only when the static chain misses a symbol codepoint; the atlas caches each
/// codepoint, so the subprocess runs at most once per distinct missing symbol.
/// It is installed only when symbol fallback is `enabled`, so disabling the
/// feature leaves both the static chain and the runtime query off. On macOS and
/// non-Unix targets there is no runtime query (the static system tail covers the
/// platform), so the resolver is always cleared and the path stays byte-identical
/// to the pre-feature renderer.
pub(super) fn install_runtime_symbol_resolver(atlas: &mut text::GlyphAtlas, enabled: bool) {
    #[cfg(all(unix, not(target_os = "macos")))]
    let resolver = if enabled {
        Some(text::runtime_resolve_symbol_font as fn(char) -> Option<Arc<FontVec>>)
    } else {
        None
    };
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let resolver = {
        let _ = enabled;
        None
    };
    atlas.set_runtime_symbol_resolver(resolver);
}

/// Resolve a SYMMAP override map's font-family names to loaded faces (SYMMAP).
///
/// Each rule's family name is resolved against the host font search dirs; on
/// success the regular face is loaded and paired with the rule's inclusive
/// codepoint range. A family that does not resolve (missing, or not a monospace
/// face) is **skipped with a warning** — the range falls through to normal font
/// resolution rather than crashing, mirroring the resilient `symbol_fallback`
/// path. An empty map resolves to an empty `Vec` (the identity / off path).
pub(super) fn resolve_symbol_map_fonts(
    map: &crate::text::SymbolMap,
) -> Vec<(u32, u32, Arc<FontVec>)> {
    if map.is_empty() {
        return Vec::new();
    }
    let dirs = text::font_search_dirs();
    let mut result = Vec::with_capacity(map.len());
    for rule in map.rules() {
        let (start, end) = rule.bounds();
        match text::resolve_font_family(rule.font(), &dirs) {
            Some(matched) => match text::load_font_at(&matched.regular) {
                Ok(font) => result.push((start, end, Arc::new(font))),
                Err(err) => tracing::warn!(
                    "symbol_map: {err}; skipping override for U+{start:04X}-U+{end:04X}"
                ),
            },
            None => tracing::warn!(
                "symbol_map: font family {:?} not found (or not monospace); skipping override for U+{start:04X}-U+{end:04X}",
                rule.font()
            ),
        }
    }
    result
}

fn env_flag_override(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled_regular_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf")
    }

    #[test]
    fn symbol_fallback_off_resolves_empty_chain() {
        // The enable gate short-circuits before any font resolution, so a
        // disabled fallback never installs a face regardless of what is bundled
        // or on the host.
        assert!(resolve_symbol_fallback(false, None).is_empty());
    }

    #[test]
    fn symbol_fallback_default_on_resolves_bundled_chain() {
        // Out-of-the-box: enabled, no explicit override -> the bundled symbols
        // chain resolves (order explicit > bundled v3,v2 > host). The default
        // build bundles two faces (v3 + v2), so the chain has at least two.
        let chain = resolve_symbol_fallback(true, None);
        assert!(
            chain.len() >= 2,
            "enabled fallback with no override must resolve the bundled v3+v2 chain, got {}",
            chain.len()
        );
    }

    #[test]
    fn explicit_symbol_font_path_leads_the_chain() {
        // A valid explicit override loads and leads the chain (the bundled faces
        // still follow it to compose coverage).
        let path = bundled_regular_path();
        let chain = resolve_symbol_fallback(true, Some(&path));
        assert!(
            chain.len() >= 3,
            "explicit override must lead, with bundled v3+v2 following, got {}",
            chain.len()
        );
    }
}
