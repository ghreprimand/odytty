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
                        eprintln!(
                            "odytty: font_weight: {err}; falling back to the regular face for {family:?} {weight:?}"
                        );
                        text::load_font_with_path(font_path)
                            .map_err(|err| NativeError::Text(err.to_string()))?
                    }
                },
                None => {
                    eprintln!(
                        "odytty: font_weight: no {weight:?} face found for family {family:?}; using the regular face"
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
                    eprintln!(
                        "odytty: font_weight: bundled {family} has no {:?} weight face; using the regular face",
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

/// Resolve the symbol / Nerd-font fallback face when enabled and a font is
/// available, else `None`. A missing font with the switch on is not fatal — the
/// renderer keeps the hollow-box behavior.
pub(super) fn resolve_symbol_fallback(
    enabled: bool,
    explicit_path: Option<&Path>,
) -> Option<Arc<FontVec>> {
    resolve_symbol_fallback_with_dirs(
        enabled,
        explicit_path,
        &text::font_search_dirs(),
        text::resolve_bundled_symbol_font,
    )
}

fn resolve_symbol_fallback_with_dirs(
    enabled: bool,
    explicit_path: Option<&Path>,
    search_dirs: &[PathBuf],
    bundled: impl FnOnce() -> Option<FontVec>,
) -> Option<Arc<FontVec>> {
    if !enabled {
        return None;
    }
    if let Some(path) = explicit_path {
        match text::load_font_at(path) {
            Ok(font) => return Some(Arc::new(font)),
            Err(err) => {
                eprintln!("odytty: {err}; falling back to symbol font search");
            }
        }
    }
    text::resolve_symbol_font_in(search_dirs)
        .or_else(bundled)
        .map(Arc::new)
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
                Err(err) => eprintln!(
                    "odytty: symbol_map: {err}; skipping override for U+{start:04X}-U+{end:04X}"
                ),
            },
            None => eprintln!(
                "odytty: symbol_map: font family {:?} not found (or not monospace); skipping override for U+{start:04X}-U+{end:04X}",
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn bundled_regular_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf")
    }

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "odytty-gpu-fonts-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn symbol_fallback_off_resolves_none_without_touching_loaders() {
        let resolved = resolve_symbol_fallback_with_dirs(false, None, &[], || {
            panic!("bundled loader must not run when fallback is off")
        });
        assert!(resolved.is_none());
    }

    #[test]
    fn symbol_fallback_default_on_can_resolve_bundled_face() {
        let resolved =
            resolve_symbol_fallback_with_dirs(true, None, &[], text::resolve_bundled_symbol_font);
        assert!(
            resolved.is_some(),
            "enabled fallback should use the bundled face after host search misses"
        );
    }

    #[test]
    fn explicit_symbol_font_path_beats_bundled_face() {
        let path = bundled_regular_path();
        let resolved = resolve_symbol_fallback_with_dirs(true, Some(&path), &[], || {
            panic!("bundled loader must not run after a valid explicit path")
        });
        assert!(resolved.is_some());
    }

    #[test]
    fn host_symbol_font_beats_bundled_face() {
        let dir = unique_tmp_dir("host-symbol");
        let src = bundled_regular_path();
        let dst = dir.join("SymbolsNerdFont-Regular.ttf");
        std::fs::copy(src, &dst).expect("copy fixture font");

        let resolved = resolve_symbol_fallback_with_dirs(true, None, &[dir.clone()], || {
            panic!("bundled loader must not run after host symbol search resolves")
        });
        assert!(resolved.is_some());

        let _ = std::fs::remove_dir_all(dir);
    }
}
