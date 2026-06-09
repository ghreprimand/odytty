//! CPU-side text rendering support: font loading, a monospace glyph atlas, and
//! terminal color resolution.
//!
//! This module is deliberately GPU-agnostic so it can be unit-tested without a
//! window or `wgpu` device. The native renderer (`crate::native`) uploads the
//! atlas bitmap to a texture and uses [`GlyphAtlas::uv_rect`] plus the color
//! helpers here to build per-cell quads.
//!
//! ## Font sourcing
//!
//! For this first prototype the font is loaded from the host at runtime: the
//! `ODYTTY_FONT` environment variable wins if set, otherwise a small list of
//! common Linux monospace paths is probed. Bundling a font into the repo for
//! fully deterministic rendering is a deliberate later decision (it means
//! committing a binary + its license to a public repo), so it is intentionally
//! not done here.
//!
//! ## Atlas layout
//!
//! Printable ASCII (`0x20..=0x7E`) is rasterized into a single 8-bit coverage
//! bitmap arranged as a fixed grid of equal cells. Every terminal cell maps 1:1
//! onto one atlas cell of identical pixel size, so the renderer only needs the
//! atlas-cell rectangle for a character — no per-glyph offset math downstream.

use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont, point};

use crate::core::Color;

/// Environment variable naming an explicit font file to load.
pub const FONT_ENV: &str = "ODYTTY_FONT";

/// First and last printable ASCII code points covered by the atlas.
const FIRST_CHAR: u32 = 0x20;
const LAST_CHAR: u32 = 0x7E;
/// Number of atlas cells per row in the bitmap grid.
const ATLAS_COLS: u32 = 16;

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

/// Load a monospace font: honor `ODYTTY_FONT`, else probe known paths.
pub fn load_font() -> Result<FontVec, TextError> {
    if let Some(path) = std::env::var_os(FONT_ENV) {
        let path = PathBuf::from(path);
        return load_font_at(&path);
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

/// Integer pixel metrics for one monospace cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    /// Cell advance width in pixels.
    pub width: u32,
    /// Cell height (ascent + descent) in pixels.
    pub height: u32,
    /// Baseline offset from the cell top, in pixels.
    pub baseline: u32,
}

/// A monospace glyph atlas: one coverage bitmap holding printable ASCII.
#[derive(Debug, Clone)]
pub struct GlyphAtlas {
    /// Atlas bitmap width in pixels.
    pub width: u32,
    /// Atlas bitmap height in pixels.
    pub height: u32,
    /// Single-channel (R8) coverage data, row-major, length `width * height`.
    pub data: Vec<u8>,
    /// Per-cell pixel metrics shared by every glyph.
    pub cell: CellSize,
    /// Atlas cells per row.
    cols: u32,
}

impl GlyphAtlas {
    /// Rasterize printable ASCII at `px` pixels into a new atlas.
    ///
    /// `px` is the physical pixel size to rasterize at (caller multiplies the
    /// logical font size by the window scale factor for crisp HiDPI text).
    pub fn build(font: &FontVec, px: f32) -> Self {
        let px = px.max(1.0);
        let scale = PxScale::from(px);
        let scaled = font.as_scaled(scale);

        // Monospace: every glyph shares the advance of a representative glyph.
        let advance = scaled.h_advance(font.glyph_id('M'));
        let ascent = scaled.ascent();
        let descent = scaled.descent(); // negative (below baseline)

        let cell_w = advance.ceil().max(1.0) as u32;
        let cell_h = (ascent - descent).ceil().max(1.0) as u32;
        let baseline = ascent.round().max(0.0) as u32;
        let cell = CellSize {
            width: cell_w,
            height: cell_h,
            baseline,
        };

        let count = LAST_CHAR - FIRST_CHAR + 1;
        let cols = ATLAS_COLS;
        let rows = count.div_ceil(cols);
        let width = cols * cell_w;
        let height = rows * cell_h;
        let mut data = vec![0u8; (width * height) as usize];

        for code in FIRST_CHAR..=LAST_CHAR {
            let ch = char::from_u32(code).unwrap_or(' ');
            let index = code - FIRST_CHAR;
            let ox = (index % cols) * cell_w;
            let oy = (index / cols) * cell_h;

            let glyph: Glyph = font
                .glyph_id(ch)
                .with_scale_and_position(scale, point(0.0, ascent));
            let Some(outline) = font.outline_glyph(glyph) else {
                continue; // e.g. space: no contours
            };
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px_x = bounds.min.x + gx as f32;
                let px_y = bounds.min.y + gy as f32;
                if px_x < 0.0 || px_y < 0.0 {
                    return;
                }
                let ax = ox + px_x as u32;
                let ay = oy + px_y as u32;
                if ax >= ox + cell_w || ay >= oy + cell_h {
                    return; // clip to the glyph's own cell
                }
                let idx = (ay * width + ax) as usize;
                let value = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
                // Keep the strongest coverage if cells ever overlap.
                if value > data[idx] {
                    data[idx] = value;
                }
            });
        }

        Self {
            width,
            height,
            data,
            cell,
            cols,
        }
    }

    /// Normalized UV rectangle `[u0, v0, u1, v1]` for a character's atlas cell,
    /// or `None` if the character is outside the covered ASCII range.
    pub fn uv_rect(&self, ch: char) -> Option<[f32; 4]> {
        let code = ch as u32;
        if !(FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return None;
        }
        let index = code - FIRST_CHAR;
        let cx = (index % self.cols) * self.cell.width;
        let cy = (index / self.cols) * self.cell.height;
        let u0 = cx as f32 / self.width as f32;
        let v0 = cy as f32 / self.height as f32;
        let u1 = (cx + self.cell.width) as f32 / self.width as f32;
        let v1 = (cy + self.cell.height) as f32 / self.height as f32;
        Some([u0, v0, u1, v1])
    }
}

/// Default foreground (light gray) and background (near-black) in sRGB bytes.
pub const DEFAULT_FG_SRGB: (u8, u8, u8) = (0xCC, 0xCC, 0xCC);
pub const DEFAULT_BG_SRGB: (u8, u8, u8) = (0x0B, 0x0C, 0x10);

/// Convert one sRGB channel byte to a linear float in `[0, 1]`.
///
/// The surface uses an sRGB texture format, which applies the linear→sRGB
/// transfer on write, so shader inputs must be linear.
fn srgb_to_linear(byte: u8) -> f32 {
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
        Color::Default => linear_rgba(DEFAULT_FG_SRGB),
        Color::Indexed(i) => linear_rgba(indexed_srgb(i)),
        Color::Rgb(r, g, b) => linear_rgba((r, g, b)),
    }
}

/// Resolve a terminal background color to linear RGBA.
pub fn background_linear(color: Color) -> [f32; 4] {
    match color {
        Color::Default => linear_rgba(DEFAULT_BG_SRGB),
        Color::Indexed(i) => linear_rgba(indexed_srgb(i)),
        Color::Rgb(r, g, b) => linear_rgba((r, g, b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_font() -> Option<FontVec> {
        load_font().ok()
    }

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

    #[test]
    fn atlas_has_positive_metrics_and_glyph_coverage() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 28.0);
        assert!(atlas.cell.width > 0 && atlas.cell.height > 0);
        assert!(atlas.cell.baseline <= atlas.cell.height);
        assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
        // A glyph with ink (e.g. 'M') must produce non-zero coverage.
        assert!(atlas.data.iter().any(|&v| v > 0));
        // UV rects exist for printable ASCII and not for control chars.
        assert!(atlas.uv_rect('A').is_some());
        assert!(atlas.uv_rect('\n').is_none());
    }
}
