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

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};

use crate::core::Color;
use crate::settings::FONT_ENV;

/// The glyph atlas and its cell metrics live in [`crate::atlas`]; re-exported
/// here so `crate::text::{CellSize, GlyphAtlas}` call sites keep resolving.
pub use crate::atlas::{CellSize, FontStyle, GlyphAtlas, SubpixelMode};

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
///
/// **Resilient by design (F1):** a bad explicit `font_path` (missing,
/// unreadable, or unparseable) must never abort startup. The explicit path is
/// tried first; on failure a one-line stderr notice is emitted and loading
/// falls back to probing the host candidate list. Only when nothing at all
/// loads does this return [`TextError::NoFont`]. The settings layer resolves
/// `ODYTTY_FONT_FAMILY` to a validated path *before* this point, so by the time
/// a path reaches here it has usually already been monospace-checked; this
/// fallback is the final safety net for `ODYTTY_FONT` direct paths.
pub fn load_font_with_path(font_path: Option<&Path>) -> Result<FontVec, TextError> {
    if let Some(path) = font_path {
        match load_font_at(path) {
            Ok(font) => return Ok(font),
            Err(err) => {
                eprintln!("odytty: {err}; falling back to a probed system font");
            }
        }
    }

    for candidate in font_candidates() {
        if candidate.exists()
            && let Ok(font) = load_font_at(&candidate)
        {
            return Ok(font);
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

/// A resolved font family: the validated monospace `regular` face plus any
/// style variants discovered alongside it.
///
/// **Groundwork (F1):** only `regular` is loaded and rendered today. The
/// `bold`/`italic`/`bold_italic` paths are *discovered* by filename convention
/// so a future packet can load them into the `(style, char)`-keyed atlas without
/// re-running discovery; they are intentionally not opened here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFamilyMatch {
    /// Path to the validated monospace regular face.
    pub regular: PathBuf,
    /// Bold face, if a sibling file was found.
    pub bold: Option<PathBuf>,
    /// Italic/oblique face, if found.
    pub italic: Option<PathBuf>,
    /// Bold-italic face, if found.
    pub bold_italic: Option<PathBuf>,
}

/// Standard Linux font search roots, plus per-user font dirs when `HOME` is set.
/// Only existing directories are returned. Used by the settings layer to resolve
/// `ODYTTY_FONT_FAMILY`; tests pass explicit dirs instead for hermeticity.
pub fn font_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// Resolve a `ODYTTY_FONT_FAMILY` value to a validated monospace face.
///
/// Accepts either a direct path to a `.ttf`/`.otf` file or a family **name**
/// looked up across `dirs`. The returned `regular` face is always validated as
/// monospace (see [`is_monospace`]); a proportional font is rejected (returns
/// `None`) so the caller can fall back to the embedded probe list. Style
/// variants are discovered by filename convention but not opened. Pure with
/// respect to `dirs`, so tests can supply a fixture directory.
pub fn resolve_font_family(query: &str, dirs: &[PathBuf]) -> Option<FontFamilyMatch> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Direct path to a font file: validate and use as the regular face.
    let as_path = Path::new(trimmed);
    if as_path.is_file() && has_font_ext(as_path) {
        let font = load_font_at(as_path).ok()?;
        if !is_monospace(&font) {
            return None;
        }
        return Some(FontFamilyMatch {
            regular: as_path.to_path_buf(),
            bold: None,
            italic: None,
            bold_italic: None,
        });
    }

    // Family-name lookup across the search dirs.
    let target = normalize_family(trimmed);
    if target.is_empty() {
        return None;
    }
    let files = collect_font_files(dirs);

    // Pick the best monospace regular face whose normalized stem contains the
    // requested family. "Best" prefers an explicit "regular"/"mono" marker and a
    // shorter stem (closer to an exact family match).
    let mut regular: Option<PathBuf> = None;
    let mut best_score = i32::MIN;
    for f in &files {
        let stem = normalize_family(&file_stem(f));
        if !stem.contains(&target) {
            continue;
        }
        if variant_flags(&stem) != (false, false) {
            continue; // not a regular face
        }
        let mut score = 0i32;
        if stem.contains("regular") {
            score += 2;
        }
        if stem.contains("mono") {
            score += 1;
        }
        score -= stem.len() as i32;
        if score <= best_score {
            continue;
        }
        if let Ok(font) = load_font_at(f)
            && is_monospace(&font)
        {
            best_score = score;
            regular = Some(f.clone());
        }
    }
    let regular = regular?;

    // Discover style variants sharing the family target (first match wins).
    let mut bold = None;
    let mut italic = None;
    let mut bold_italic = None;
    for f in &files {
        let stem = normalize_family(&file_stem(f));
        if !stem.contains(&target) {
            continue;
        }
        match variant_flags(&stem) {
            (true, true) => {
                bold_italic.get_or_insert_with(|| f.clone());
            }
            (true, false) => {
                bold.get_or_insert_with(|| f.clone());
            }
            (false, true) => {
                italic.get_or_insert_with(|| f.clone());
            }
            (false, false) => {}
        }
    }

    Some(FontFamilyMatch {
        regular,
        bold,
        italic,
        bold_italic,
    })
}

/// Whether a font's representative glyphs share one advance width (monospace).
///
/// Compares the horizontal advance of several probe glyphs at a fixed scale; a
/// proportional font (where, e.g., `i` is narrower than `M`) is rejected. Glyphs
/// the font lacks are skipped; at least one probe must resolve.
pub fn is_monospace(font: &FontVec) -> bool {
    let scaled = font.as_scaled(PxScale::from(64.0));
    let probe = ['i', 'l', '.', 'M', 'W', 'm', 'x', '@'];
    let mut advance: Option<f32> = None;
    for ch in probe {
        let id = font.glyph_id(ch);
        if id.0 == 0 {
            continue; // font lacks this probe glyph
        }
        let a = scaled.h_advance(id);
        if a <= 0.0 {
            return false;
        }
        match advance {
            None => advance = Some(a),
            // Allow a sub-pixel tolerance for hinting/rounding noise.
            Some(prev) if (prev - a).abs() > 0.5 => return false,
            Some(_) => {}
        }
    }
    advance.is_some()
}

/// Lowercased alphanumeric-only form of a family/stem name, so "DejaVu Sans
/// Mono" and "DejaVuSansMono" compare equal.
fn normalize_family(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// `(bold, italic)` flags inferred from a normalized stem.
fn variant_flags(normalized_stem: &str) -> (bool, bool) {
    let bold = normalized_stem.contains("bold");
    let italic = normalized_stem.contains("italic") || normalized_stem.contains("oblique");
    (bold, italic)
}

/// Whether a path has a `.ttf`/`.otf`/`.ttc` extension (case-insensitive).
fn has_font_ext(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("ttf") | Some("otf") | Some("ttc")
    )
}

/// File stem (name without extension) as a lossy string.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Bounded recursive collection of font files under `dirs`. Depth and total file
/// count are capped so a pathological tree cannot stall startup.
fn collect_font_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 6;
    const MAX_FILES: usize = 20_000;
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH || out.len() >= MAX_FILES {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push((path, depth + 1));
            } else if ft.is_file() && has_font_ext(&path) {
                out.push(path);
                if out.len() >= MAX_FILES {
                    break;
                }
            }
        }
    }
    out
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

    #[test]
    fn normalize_family_is_case_and_separator_insensitive() {
        assert_eq!(normalize_family("DejaVu Sans Mono"), "dejavusansmono");
        assert_eq!(normalize_family("dejavu-sans_mono"), "dejavusansmono");
        assert_eq!(normalize_family("  JetBrains  Mono  "), "jetbrainsmono");
        assert_eq!(normalize_family("!!!"), "");
    }

    #[test]
    fn variant_flags_classify_styles() {
        assert_eq!(variant_flags("dejavusansmono"), (false, false));
        assert_eq!(variant_flags("dejavusansmonobold"), (true, false));
        assert_eq!(variant_flags("dejavusansmonoitalic"), (false, true));
        assert_eq!(variant_flags("dejavusansmonooblique"), (false, true));
        assert_eq!(variant_flags("dejavusansmonobolditalic"), (true, true));
    }

    #[test]
    fn has_font_ext_matches_known_extensions() {
        assert!(has_font_ext(Path::new("/x/Foo.ttf")));
        assert!(has_font_ext(Path::new("/x/Foo.OTF")));
        assert!(has_font_ext(Path::new("/x/Foo.ttc")));
        assert!(!has_font_ext(Path::new("/x/Foo.png")));
        assert!(!has_font_ext(Path::new("/x/Foo")));
    }

    #[test]
    fn empty_or_nonsense_family_resolves_to_none() {
        assert!(resolve_font_family("", &[]).is_none());
        assert!(resolve_font_family("   ", &[]).is_none());
        // A directory with no fonts cannot satisfy a real-looking family name.
        assert!(resolve_font_family("DefinitelyNotAFont", &[]).is_none());
    }

    /// Bytes of the first available system monospace font, or `None` when the
    /// host has no candidate (tests then skip).
    fn system_mono_bytes() -> Option<Vec<u8>> {
        font_candidates()
            .into_iter()
            .find(|c| c.exists())
            .and_then(|c| std::fs::read(&c).ok())
    }

    /// A unique temp dir for fixture fonts; best-effort cleanup by the caller.
    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "odytty_f1_{tag}_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn loaded_system_font_is_monospace() {
        let Some(bytes) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let font = FontVec::try_from_vec(bytes).expect("parse system font");
        assert!(is_monospace(&font), "probed default should be monospace");
    }

    #[test]
    fn resolve_family_finds_regular_and_style_variants() {
        let Some(bytes) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let dir = unique_tmp_dir("variants");
        // Lay down a family with all four faces (same bytes; names drive matching).
        for name in [
            "TestMono-Regular.ttf",
            "TestMono-Bold.ttf",
            "TestMono-Italic.ttf",
            "TestMono-BoldItalic.ttf",
        ] {
            std::fs::write(dir.join(name), &bytes).expect("write fixture font");
        }
        let dirs = vec![dir.clone()];

        let m = resolve_font_family("Test Mono", &dirs).expect("family resolves");
        assert_eq!(m.regular, dir.join("TestMono-Regular.ttf"));
        assert_eq!(m.bold, Some(dir.join("TestMono-Bold.ttf")));
        assert_eq!(m.italic, Some(dir.join("TestMono-Italic.ttf")));
        assert_eq!(m.bold_italic, Some(dir.join("TestMono-BoldItalic.ttf")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_family_accepts_a_direct_path() {
        let Some(bytes) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let dir = unique_tmp_dir("direct");
        let path = dir.join("SomeMono.otf");
        std::fs::write(&path, &bytes).expect("write fixture font");

        let m = resolve_font_family(path.to_str().unwrap(), &[]).expect("path resolves");
        assert_eq!(m.regular, path);
        assert!(m.bold.is_none() && m.italic.is_none() && m.bold_italic.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_font_with_path_falls_back_on_bad_path() {
        // A bogus explicit path must not error when the host has a probe font.
        let bogus = Path::new("/nonexistent/not-a-font.ttf");
        match load_font_with_path(Some(bogus)) {
            Ok(_) => {} // fell back to a probed font
            Err(TextError::NoFont) => {
                eprintln!("skipping: no system font to fall back to");
            }
            Err(other) => panic!("bad path should fall back, not error: {other}"),
        }
    }
}
