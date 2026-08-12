// SPDX-License-Identifier: GPL-3.0-only
//! Process-wide settings published to renderer-owned runtime caches.

use super::*;

/// Runtime flag mirroring [`Settings::synthetic_styles`], published process-wide
/// so the GPU renderer can read it without threading `Settings` through the
/// `NativeOptions` seam (whose construction literals live in a separate
/// module). Defaults to `true` (synthesis on); the native entry point
/// publishes the resolved setting at startup and the config-reload path
/// republishes it on change. This mirrors the existing process-global pattern
/// used for default cell colors ([`crate::text::set_default_colors`]).
static SYNTHETIC_STYLES_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Publish the synthetic-styles kill switch so the renderer's atlas-build path
/// can gate font synthesis. Called at startup and whenever the config reloads.
pub fn set_synthetic_styles_enabled(enabled: bool) {
    SYNTHETIC_STYLES_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Read the published synthetic-styles flag. `true` means synthesize missing
/// bold/italic faces from the regular outline; `false` forces the atlas mask off
/// so styled cells render as plain regular glyphs.
pub fn synthetic_styles_enabled() -> bool {
    SYNTHETIC_STYLES_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Runtime flag mirroring [`Settings::geometric_boxdraw`], published
/// process-wide so the GPU renderer can apply it to every rebuilt glyph atlas
/// without threading `Settings` through the native options seam. Defaults to
/// `false`, preserving the font-rasterized plain path unless explicitly
/// enabled.
static GEOMETRIC_BOXDRAW_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Publish the geometric box-drawing switch so the renderer's atlas-build path
/// can enable pixel geometry for box/block/Powerline glyphs. Called at startup
/// and whenever the config reloads.
pub fn set_geometric_boxdraw_enabled(enabled: bool) {
    GEOMETRIC_BOXDRAW_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Read the published geometric box-drawing flag. `false` is exact passthrough
/// to font glyph rasterization; `true` enables atlas-owned geometry for covered
/// codepoints.
pub fn geometric_boxdraw_enabled() -> bool {
    GEOMETRIC_BOXDRAW_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Process-wide contextual shaping switch. `false` is the exact scalar renderer
/// path and allocates no shaping cache entries.
static LIGATURES_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(DEFAULT_LIGATURES);

pub fn set_ligatures_enabled(enabled: bool) {
    LIGATURES_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn ligatures_enabled() -> bool {
    LIGATURES_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Optional OpenType `ss01` stylistic set. Off by default; ignored while the
/// master ligatures switch is off.
static LIGATURE_SS01_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(DEFAULT_LIGATURE_SS01);

pub fn set_ligature_ss01_enabled(enabled: bool) {
    LIGATURE_SS01_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn ligature_ss01_enabled() -> bool {
    LIGATURE_SS01_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Optional OpenType `ss02` stylistic set. Off by default; ignored while the
/// master ligatures switch is off.
static LIGATURE_SS02_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(DEFAULT_LIGATURE_SS02);

pub fn set_ligature_ss02_enabled(enabled: bool) {
    LIGATURE_SS02_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn ligature_ss02_enabled() -> bool {
    LIGATURE_SS02_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Runtime flag mirroring [`Settings::symbol_fallback`], published
/// process-wide so the GPU renderer can rebuild the glyph atlas when live
/// settings enable or disable the RV6 symbol / Nerd-font fallback. Defaults to
/// `true`, with a bundled symbols face intended to make PUA prompt icons work
/// out of the box; users can still disable it explicitly.
static SYMBOL_FALLBACK_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Optional explicit symbol / Nerd-font path mirroring [`Settings::symbol_font`].
/// `None` means the renderer falls back to its host font search.
static SYMBOL_FONT_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn set_symbol_fallback_enabled(enabled: bool) {
    SYMBOL_FALLBACK_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn symbol_fallback_enabled() -> bool {
    SYMBOL_FALLBACK_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_symbol_font_path(path: Option<PathBuf>) {
    let mut slot = SYMBOL_FONT_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = path;
}

pub fn symbol_font_path() -> Option<PathBuf> {
    SYMBOL_FONT_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Live SYMMAP override map (`Settings::symbol_map`), published process-wide so
/// the GPU renderer can resolve override font families and rebuild the glyph
/// atlas when the map changes. The default is empty (the identity / off path).
static SYMBOL_MAP: std::sync::Mutex<Option<crate::text::SymbolMap>> = std::sync::Mutex::new(None);

pub fn set_symbol_map(map: crate::text::SymbolMap) {
    let mut slot = SYMBOL_MAP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(map);
}

pub fn symbol_map() -> crate::text::SymbolMap {
    SYMBOL_MAP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_default()
}
