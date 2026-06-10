//! CPU-side text support: font loading and terminal color resolution.
//!
//! This module is deliberately GPU-agnostic so it can be unit-tested without a
//! window or `wgpu` device. The monospace glyph atlas lives in [`crate::atlas`];
//! its [`CellSize`]/[`GlyphAtlas`] types are re-exported here so existing
//! `crate::text::…` call sites keep resolving. The native renderer uses the
//! color helpers below plus the atlas to build per-cell quads.
//!
//! ## Font sourcing
//!
//! For this first prototype the font is loaded from the host at runtime: the
//! the settings layer can provide an explicit font path, otherwise a small list
//! of common Linux monospace paths is probed. Bundling a font into the repo for
//! fully deterministic rendering is a deliberate later decision (it means
//! committing a binary + its license to a public repo), so it is intentionally
//! not done here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use ab_glyph::FontVec;

use crate::core::Color;
use crate::settings::FONT_ENV;

/// The glyph atlas and its cell metrics live in [`crate::atlas`]; re-exported
/// here so `crate::text::{CellSize, GlyphAtlas}` call sites keep resolving.
pub use crate::atlas::{CellSize, GlyphAtlas};

/// Errors from font loading.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// No usable font file was found on the host.
    #[error("no monospace font found (set {FONT_ENV} to a .ttf/.otf path)")]
    NoFont,
    /// A font file was found but could not be parsed.
    #[error("failed to parse font {path}: {source}")]
    Parse {
        path: String,
        source: ab_glyph::InvalidFont,
    },
    /// The font file could not be read.
    #[error("failed to read font {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
}

/// Common Linux monospace font locations, probed in order.
fn font_candidates() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

/// Load a monospace font from the host's default candidate list.
pub fn load_font() -> Result<FontVec, TextError> {
    load_font_with_path(None)
}

/// Load a monospace font: honor an explicit settings path, else probe known
/// paths.
pub fn load_font_with_path(font_path: Option<&Path>) -> Result<FontVec, TextError> {
    if let Some(path) = font_path {
        return load_font_at(path);
    }

    for candidate in font_candidates() {
        if candidate.exists() {
            return load_font_at(&candidate);
        }
    }
    Err(TextError::NoFont)
}

/// Load and parse a font from an explicit path.
pub fn load_font_at(path: &Path) -> Result<FontVec, TextError> {
    let bytes = std::fs::read(path).map_err(|source| TextError::Read {
        path: path.display().to_string(),
        source,
    })?;
    FontVec::try_from_vec(bytes).map_err(|source| TextError::Parse {
        path: path.display().to_string(),
        source,
    })
}

/// Default foreground (light gray) and background (near-black) in sRGB bytes.
///
/// These are the *baseline* defaults (the plain theme). The active default used
/// when resolving `Color::Default` is overridable at runtime via
/// [`set_default_colors`]; the theme layer sets it once at startup. Core terminal
/// semantics never read these — only presentation does.
pub const DEFAULT_FG_SRGB: (u8, u8, u8) = (0xCC, 0xCC, 0xCC);
pub const DEFAULT_BG_SRGB: (u8, u8, u8) = (0x0B, 0x0C, 0x10);

/// Pack an sRGB triple into a `u32` for atomic storage (`0x00RRGGBB`).
const fn pack_srgb(c: (u8, u8, u8)) -> u32 {
    ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | (c.2 as u32)
}

/// Unpack a `0x00RRGGBB` value back into an sRGB triple.
fn unpack_srgb(v: u32) -> (u8, u8, u8) {
    (
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    )
}

/// Active default foreground/background for `Color::Default`, overridable by the
/// theme layer. Stored as packed sRGB so resolution stays lock-free. This is a
/// presentation-only override: it changes how `Color::Default` paints, never
/// what the terminal core stores.
static DEFAULT_FG: AtomicU32 = AtomicU32::new(pack_srgb(DEFAULT_FG_SRGB));
static DEFAULT_BG: AtomicU32 = AtomicU32::new(pack_srgb(DEFAULT_BG_SRGB));

/// Override the default foreground/background used to resolve `Color::Default`.
///
/// Called once at native startup by the theme layer. Affects only rendering;
/// the terminal model is unaware of it. Passing the baseline constants restores
/// the plain appearance.
pub fn set_default_colors(foreground: (u8, u8, u8), background: (u8, u8, u8)) {
    DEFAULT_FG.store(pack_srgb(foreground), Ordering::Relaxed);
    DEFAULT_BG.store(pack_srgb(background), Ordering::Relaxed);
}

fn default_fg_srgb() -> (u8, u8, u8) {
    unpack_srgb(DEFAULT_FG.load(Ordering::Relaxed))
}

fn default_bg_srgb() -> (u8, u8, u8) {
    unpack_srgb(DEFAULT_BG.load(Ordering::Relaxed))
}

/// Convert one sRGB channel byte to a linear float in `[0, 1]`.
///
/// The surface uses an sRGB texture format, which applies the linear→sRGB
/// transfer on write, so shader inputs must be linear.
pub fn srgb_to_linear(byte: u8) -> f32 {
    let c = byte as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear-RGBA (opaque) for an sRGB triple.
fn linear_rgba(srgb: (u8, u8, u8)) -> [f32; 4] {
    [
        srgb_to_linear(srgb.0),
        srgb_to_linear(srgb.1),
        srgb_to_linear(srgb.2),
        1.0,
    ]
}

/// The sRGB bytes for an xterm 256-color palette index.
pub fn indexed_srgb(index: u8) -> (u8, u8, u8) {
    match index {
        // 16 standard ANSI colors.
        0 => (0x00, 0x00, 0x00),
        1 => (0xCD, 0x00, 0x00),
        2 => (0x00, 0xCD, 0x00),
        3 => (0xCD, 0xCD, 0x00),
        4 => (0x00, 0x00, 0xEE),
        5 => (0xCD, 0x00, 0xCD),
        6 => (0x00, 0xCD, 0xCD),
        7 => (0xE5, 0xE5, 0xE5),
        8 => (0x7F, 0x7F, 0x7F),
        9 => (0xFF, 0x00, 0x00),
        10 => (0x00, 0xFF, 0x00),
        11 => (0xFF, 0xFF, 0x00),
        12 => (0x5C, 0x5C, 0xFF),
        13 => (0xFF, 0x00, 0xFF),
        14 => (0x00, 0xFF, 0xFF),
        15 => (0xFF, 0xFF, 0xFF),
        // 6x6x6 color cube.
        16..=231 => {
            let i = index - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let level = |v: u8| -> u8 { if v == 0 { 0 } else { 55 + v * 40 } };
            (level(r), level(g), level(b))
        }
        // 24-step grayscale ramp.
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    }
}

/// Resolve a terminal foreground color to linear RGBA.
pub fn foreground_linear(color: Color) -> [f32; 4] {
    match color {
        Color::Default => linear_rgba(default_fg_srgb()),
        Color::Indexed(i) => linear_rgba(indexed_srgb(i)),
        Color::Rgb(r, g, b) => linear_rgba((r, g, b)),
    }
}

/// Resolve a terminal background color to linear RGBA.
pub fn background_linear(color: Color) -> [f32; 4] {
    match color {
        Color::Default => linear_rgba(default_bg_srgb()),
        Color::Indexed(i) => linear_rgba(indexed_srgb(i)),
        Color::Rgb(r, g, b) => linear_rgba((r, g, b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_endpoints_map_to_linear_endpoints() {
        assert_eq!(srgb_to_linear(0), 0.0);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn color_cube_corners_are_correct() {
        // index 16 is the cube origin (black), 231 is white.
        assert_eq!(indexed_srgb(16), (0, 0, 0));
        assert_eq!(indexed_srgb(231), (255, 255, 255));
    }

    #[test]
    fn grayscale_ramp_is_monotonic() {
        let mut last = 0u8;
        for i in 232..=255u8 {
            let (v, _, _) = indexed_srgb(i);
            assert!(v >= last);
            last = v;
        }
    }

    #[test]
    fn rgb_color_passes_through() {
        let c = foreground_linear(Color::Rgb(255, 0, 0));
        assert!((c[0] - 1.0).abs() < 1e-6);
        assert_eq!(c[1], 0.0);
        assert_eq!(c[3], 1.0);
    }
}
