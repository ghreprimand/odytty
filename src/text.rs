// SPDX-License-Identifier: GPL-3.0-only
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

/// One font file discovered by CLI font inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontInventoryEntry {
    /// Filename stem used as the v1 display name.
    pub name: String,
    /// Full path to the font file.
    pub path: PathBuf,
    /// Whether OdyTTY's monospace probe accepts the face.
    pub monospace: bool,
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

/// Inventory font files from the host's standard search directories.
pub fn font_inventory() -> Vec<FontInventoryEntry> {
    font_inventory_in_dirs(&font_search_dirs())
}

/// Inventory font files under `dirs`, sorted for stable CLI output.
///
/// This is intentionally filename-stem based. OdyTTY does not parse font naming
/// tables yet, so the v1 CLI reports the same stem names the family resolver can
/// already match.
pub fn font_inventory_in_dirs(dirs: &[PathBuf]) -> Vec<FontInventoryEntry> {
    let mut entries = collect_font_files(dirs)
        .into_iter()
        .map(|path| {
            let monospace = load_font_at(&path)
                .map(|font| is_monospace(&font))
                .unwrap_or(false);
            FontInventoryEntry {
                name: file_stem(&path),
                path,
                monospace,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    entries
}

/// Resolve a `ODYTTY_FONT_FAMILY` value to a validated monospace face.
///
/// Why a `ODYTTY_FONT_FAMILY` value could not be resolved to a usable monospace
/// face. Lets the settings/overlay layer surface a precise, user-facing reason
/// instead of a silent fallback (see [`try_resolve_font_family`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontResolveError {
    /// No font file matched the requested family name, or a direct path does not
    /// exist / is not a readable font file.
    NotFound,
    /// A matching face was found but it is proportional, not monospace.
    NotMonospace,
}

impl FontResolveError {
    /// Short, user-facing reason fragment for overlay messages.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotFound => "not found",
            Self::NotMonospace => "is not monospace",
        }
    }
}

impl std::fmt::Display for FontResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// Resolve a `ODYTTY_FONT_FAMILY` value to a validated monospace face, or report
/// **why** resolution failed (see [`FontResolveError`]).
///
/// Accepts either a direct path to a `.ttf`/`.otf` file or a family **name**
/// looked up across `dirs`. The returned `regular` face is always validated as
/// monospace (see [`is_monospace`]); a proportional font is rejected
/// (`Err(NotMonospace)`) so the caller can either surface that or fall back to
/// the embedded probe list. Style variants are discovered by filename
/// convention but not opened. Pure with respect to `dirs`, so tests can supply a
/// fixture directory.
pub fn try_resolve_font_family(
    query: &str,
    dirs: &[PathBuf],
) -> Result<FontFamilyMatch, FontResolveError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(FontResolveError::NotFound);
    }

    // Direct path to a font file: validate and use as the regular face.
    let as_path = Path::new(trimmed);
    if as_path.is_file() && has_font_ext(as_path) {
        let font = load_font_at(as_path).map_err(|_| FontResolveError::NotFound)?;
        if !is_monospace(&font) {
            return Err(FontResolveError::NotMonospace);
        }
        return Ok(FontFamilyMatch {
            regular: as_path.to_path_buf(),
            bold: None,
            italic: None,
            bold_italic: None,
        });
    }

    // Family-name lookup across the search dirs.
    let target = normalize_family(trimmed);
    if target.is_empty() {
        return Err(FontResolveError::NotFound);
    }
    let files = collect_font_files(dirs);

    // Pick the best monospace regular face whose normalized stem contains the
    // requested family. "Best" prefers an explicit "regular"/"mono" marker and a
    // shorter stem (closer to an exact family match). Track whether a regular
    // face matched the name but was proportional, so a pure non-monospace name
    // hit reports `NotMonospace` rather than `NotFound`.
    let mut regular: Option<PathBuf> = None;
    let mut best_score = i32::MIN;
    let mut name_hit_non_mono = false;
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
        match load_font_at(f) {
            Ok(font) if is_monospace(&font) => {
                if score > best_score {
                    best_score = score;
                    regular = Some(f.clone());
                }
            }
            // A regular face matched the requested name but is proportional.
            Ok(_) => name_hit_non_mono = true,
            Err(_) => {}
        }
    }
    let regular = match regular {
        Some(path) => path,
        None => {
            return Err(if name_hit_non_mono {
                FontResolveError::NotMonospace
            } else {
                FontResolveError::NotFound
            });
        }
    };

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

    Ok(FontFamilyMatch {
        regular,
        bold,
        italic,
        bold_italic,
    })
}

/// `Option` view of [`try_resolve_font_family`]: the validated monospace face,
/// or `None` on any resolution failure. Used by the loader fast paths that fall
/// back to the embedded probe list and by the style-face discovery in the
/// renderer; the resolved face is identical to the `Ok` arm above.
pub fn resolve_font_family(query: &str, dirs: &[PathBuf]) -> Option<FontFamilyMatch> {
    try_resolve_font_family(query, dirs).ok()
}

/// Resolve a specific *weight* face within a named font family (RV7 /
/// FONT-WEIGHT-FIX).
///
/// Unlike [`try_resolve_font_family`], this deliberately does **not** apply the
/// `variant_flags` regular-face filter — selecting a variant face is the whole
/// point. The historical `"{family} {weight}"`-concat-then-resolve approach was
/// self-defeating: a `"Bold"` query normalizes to a target containing `"bold"`,
/// which finds `CascadiaMono-Bold.ttf`, but the regular-face filter in
/// [`try_resolve_font_family`] then *excludes* that very file because its stem
/// is a bold variant. Net: every real weight face silently fell back to
/// regular. This function instead scans for the file whose normalized stem
/// contains BOTH the family and the weight term.
///
/// Returns the matching face path, or `None` when the family or weight is empty
/// or no file in the family carries the weight term (the caller then warns and
/// falls back to the regular face — never a crash).
///
/// Scoring among matches: a pure weight request (e.g. `"Bold"`) prefers the
/// non-italic face over its `*-BoldItalic` sibling, and — as a deterministic
/// tie-break — the shortest stem, so `"Light"` resolves to `*-Light` rather than
/// `*-ExtraLight` regardless of filesystem iteration order. A request that
/// itself names italic (`"BoldItalic"`) only matches the italic face, so the
/// non-italic preference is moot there.
pub fn resolve_font_weight_face(family: &str, weight: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let family_target = normalize_family(family);
    let weight_target = normalize_family(weight);
    if family_target.is_empty() || weight_target.is_empty() {
        return None;
    }
    let files = collect_font_files(dirs);
    let mut best: Option<(i32, PathBuf)> = None;
    for f in &files {
        let stem = normalize_family(&file_stem(f));
        // Must carry both the family and the requested weight term.
        if !stem.contains(&family_target) || !stem.contains(&weight_target) {
            continue;
        }
        // Prefer a non-italic face for a pure weight request (strong weight),
        // then the closest (shortest) stem so "Light" beats "ExtraLight".
        let (_, italic) = variant_flags(&stem);
        let score = if italic { 0 } else { 1000 } - stem.len() as i32;
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, f.clone()));
        }
    }
    best.map(|(_, path)| path)
}

/// Environment variable naming an explicit symbol / Nerd-font file for the
/// RV6 PUA-icon fallback. When set to a readable `.ttf`/`.otf`, it takes
/// precedence over the family search in [`resolve_symbol_font`].
pub const SYMBOL_FONT_ENV: &str = "ODYTTY_SYMBOL_FONT";

/// Normalized filename fragments that identify a standalone symbol / Nerd font
/// suitable as the PUA-icon fallback. Compared against
/// [`normalize_family`]-style stems (lowercase, alphanumeric-only). The
/// dedicated "Symbols Nerd Font" face is preferred because it is symbols-only
/// (no Latin glyphs to shadow the body font); any patched "* Nerd Font" face is
/// accepted as a secondary match.
const SYMBOL_FONT_HINTS: &[&str] = &["symbolsnerdfont", "nerdfont"];

/// Resolve a symbol / Nerd-font face for the RV6 PUA-icon fallback, or `None`
/// when the host has none.
///
/// Resolution order:
/// 1. An explicit [`SYMBOL_FONT_ENV`] path (loaded directly; a bad path yields
///    `None` rather than aborting).
/// 2. The first file under [`font_search_dirs`] whose normalized stem contains
///    a [`SYMBOL_FONT_HINTS`] fragment, preferring the dedicated symbols-only
///    face.
///
/// The font is only *loaded*; whether it is *used* is the caller's gate (the
/// native layer reads its enable switch before installing it on the atlas).
pub fn resolve_symbol_font() -> Option<FontVec> {
    if let Some(path) = std::env::var_os(SYMBOL_FONT_ENV) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            match load_font_at(&path) {
                Ok(font) => return Some(font),
                Err(err) => {
                    eprintln!("odytty: {err}; ignoring {SYMBOL_FONT_ENV}");
                }
            }
        }
    }
    resolve_symbol_font_in(&font_search_dirs())
}

/// Family-search half of [`resolve_symbol_font`], factored out so tests can
/// pass a hermetic fixture directory. Prefers the dedicated "Symbols Nerd Font"
/// face (hint index 0) over a general patched "* Nerd Font" face.
pub fn resolve_symbol_font_in(dirs: &[PathBuf]) -> Option<FontVec> {
    let files = collect_font_files(dirs);
    let mut best: Option<(usize, PathBuf)> = None;
    for f in &files {
        let stem = normalize_family(&file_stem(f));
        if let Some(rank) = SYMBOL_FONT_HINTS.iter().position(|h| stem.contains(h)) {
            // Lower rank == stronger hint; first file at the best rank wins.
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, f.clone()));
            }
        }
    }
    let (_, path) = best?;
    load_font_at(&path).ok()
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

/// The historical xterm sRGB values for the 16 standard ANSI colors (indices
/// 0–7 normal, 8–15 bright). This is the *baseline* (plain theme) palette and
/// the source of truth pinned by tests: selecting `plain` (or no theme) renders
/// indexed colors byte-identically to the pre-theme appearance. The active ANSI
/// palette used to resolve [`Color::Indexed`] in the 0–15 range is overridable
/// at runtime via [`set_ansi_palette`]; the theme layer sets it once at startup.
/// The 256-color cube and grayscale ramp (indices 16–255) are computed and are
/// not theme-overridable.
pub const DEFAULT_ANSI_SRGB: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), // 0  black
    (0xCD, 0x00, 0x00), // 1  red
    (0x00, 0xCD, 0x00), // 2  green
    (0xCD, 0xCD, 0x00), // 3  yellow
    (0x00, 0x00, 0xEE), // 4  blue
    (0xCD, 0x00, 0xCD), // 5  magenta
    (0x00, 0xCD, 0xCD), // 6  cyan
    (0xE5, 0xE5, 0xE5), // 7  white
    (0x7F, 0x7F, 0x7F), // 8  bright black
    (0xFF, 0x00, 0x00), // 9  bright red
    (0x00, 0xFF, 0x00), // 10 bright green
    (0xFF, 0xFF, 0x00), // 11 bright yellow
    (0x5C, 0x5C, 0xFF), // 12 bright blue
    (0xFF, 0x00, 0xFF), // 13 bright magenta
    (0x00, 0xFF, 0xFF), // 14 bright cyan
    (0xFF, 0xFF, 0xFF), // 15 bright white
];

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

/// Active 16-color ANSI palette for resolving `Color::Indexed(0..=15)`,
/// overridable by the theme layer. Stored as packed sRGB so resolution stays
/// lock-free, mirroring [`DEFAULT_FG`]/[`DEFAULT_BG`]. Presentation-only: this
/// changes how indexed colors paint, never what the terminal core stores. The
/// initial values are the historical xterm table ([`DEFAULT_ANSI_SRGB`]), so an
/// un-themed renderer is byte-identical to the pre-theme appearance.
static ANSI_PALETTE: [AtomicU32; 16] = [
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[0])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[1])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[2])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[3])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[4])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[5])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[6])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[7])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[8])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[9])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[10])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[11])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[12])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[13])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[14])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[15])),
];

/// Override the default foreground/background used to resolve `Color::Default`.
///
/// Called once at native startup by the theme layer. Affects only rendering;
/// the terminal model is unaware of it. Passing the baseline constants restores
/// the plain appearance.
pub fn set_default_colors(foreground: (u8, u8, u8), background: (u8, u8, u8)) {
    DEFAULT_FG.store(pack_srgb(foreground), Ordering::Relaxed);
    DEFAULT_BG.store(pack_srgb(background), Ordering::Relaxed);
}

/// Override the 16-color ANSI palette used to resolve `Color::Indexed(0..=15)`.
///
/// Called once at native startup by the theme layer (alongside
/// [`set_default_colors`]). Affects only rendering — the terminal model is
/// unaware of it — and is layered *below* any per-app OSC-4 dynamic-color
/// override: the render path consults the core dynamic palette first and only
/// falls back to [`indexed_srgb`] (which reads this override) when no app
/// override is set, so OSC-4 always wins over the theme. Passing
/// [`DEFAULT_ANSI_SRGB`] restores the plain appearance.
pub fn set_ansi_palette(palette: &[(u8, u8, u8); 16]) {
    for (slot, &color) in ANSI_PALETTE.iter().zip(palette.iter()) {
        slot.store(pack_srgb(color), Ordering::Relaxed);
    }
}

fn default_fg_srgb() -> (u8, u8, u8) {
    unpack_srgb(DEFAULT_FG.load(Ordering::Relaxed))
}

fn default_bg_srgb() -> (u8, u8, u8) {
    unpack_srgb(DEFAULT_BG.load(Ordering::Relaxed))
}

/// The active sRGB bytes for a standard ANSI color index (0–15), reading the
/// runtime palette override.
fn ansi_srgb(index: u8) -> (u8, u8, u8) {
    unpack_srgb(ANSI_PALETTE[index as usize].load(Ordering::Relaxed))
}

/// Convert one sRGB channel byte to a linear float in `[0, 1]`.
///
/// The surface uses an sRGB texture format, which applies the linear→sRGB
/// transfer on write, so shader inputs must be linear.
///
/// This is a thin façade over [`crate::color::srgb_to_linear`], which is the
/// single source of truth for the transfer (RV3). The value is byte-identical
/// to the historical inline formula, so `native::gpu`, `grid`, and every other
/// caller see no change.
pub fn srgb_to_linear(byte: u8) -> f32 {
    crate::color::srgb_to_linear(byte)
}

/// Perceptually dim a linear-RGBA color, preserving alpha (RV3).
///
/// This is the render-facing adapter over [`crate::color::dim_perceptual`]:
/// SGR dim/faint should scale OKLab lightness rather than naively halving each
/// linear channel, which keeps dimmed text legible and hue-stable. `amount` is
/// in `[0, 1]`; `0.0` returns the input unchanged (exact identity), so the
/// default/plain path stays byte-identical until a caller opts in.
pub fn dim_linear_rgba(color: [f32; 4], amount: f32) -> [f32; 4] {
    let [r, g, b] = crate::color::dim_perceptual([color[0], color[1], color[2]], amount);
    [r, g, b, color[3]]
}

/// Active minimum fg/bg contrast floor (RV1), stored as the bit pattern of an
/// `f32` so resolution stays lock-free, mirroring the palette seams above.
///
/// `1.0` (the default) means "no floor" — [`enforce_contrast_rgba`] is then an
/// exact identity, so an un-configured renderer is byte-identical to before.
/// The native layer sets it from `Settings::min_contrast` at startup/reload.
static MIN_CONTRAST: AtomicU32 = AtomicU32::new(1.0_f32.to_bits());

/// Override the minimum fg/bg contrast floor used by [`enforce_contrast_rgba`].
///
/// Presentation-only: it changes how text is painted to keep it legible, never
/// what the terminal core stores. `ratio <= 1.0` disables enforcement (exact
/// passthrough). Mirrors [`set_ansi_palette`]/[`set_default_colors`].
pub fn set_min_contrast(ratio: f32) {
    MIN_CONTRAST.store(ratio.to_bits(), Ordering::Relaxed);
}

/// The active minimum-contrast floor (`1.0` = disabled).
pub fn min_contrast() -> f32 {
    f32::from_bits(MIN_CONTRAST.load(Ordering::Relaxed))
}

/// Enforce the active minimum-contrast floor on a resolved linear-RGBA
/// foreground against its background, preserving alpha (RV1).
///
/// This is the render-facing seam over [`crate::color::enforce_min_contrast`]:
/// the caller passes the final per-cell `fg`/`bg` (after inverse/dim) and gets
/// back an `fg` whose WCAG contrast against `bg` meets at least the configured
/// floor, with hue preserved. When the floor is at its passthrough value
/// (`1.0`, the default) this returns `fg` unchanged, so the plain path stays
/// byte-identical until the floor is raised.
pub fn enforce_contrast_rgba(fg: [f32; 4], bg: [f32; 4]) -> [f32; 4] {
    let ratio = min_contrast();
    if ratio <= 1.0 {
        return fg;
    }
    let [r, g, b] =
        crate::color::enforce_min_contrast([fg[0], fg[1], fg[2]], [bg[0], bg[1], bg[2]], ratio);
    [r, g, b, fg[3]]
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
///
/// Indices 0–15 (the standard ANSI colors) read the active theme palette via
/// the [`set_ansi_palette`] override seam; with no override applied they return
/// the historical xterm values ([`DEFAULT_ANSI_SRGB`]). The 256-color cube and
/// grayscale ramp (16–255) are computed and not theme-overridable.
pub fn indexed_srgb(index: u8) -> (u8, u8, u8) {
    match index {
        // 16 standard ANSI colors — theme-overridable.
        0..=15 => ansi_srgb(index),
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

// ---------------------------------------------------------------------------
// SYMMAP: codepoint → override-font mapping (RV6 extension, core layer).
// ---------------------------------------------------------------------------

/// One SYMMAP rule: an **inclusive** codepoint range `start..=end` mapped to an
/// override font identifier.
///
/// The font identifier is the same query string the family resolver
/// ([`try_resolve_font_family`]) accepts — either a direct `.ttf`/`.otf` path or
/// a font-family name. This core layer stores the identifier verbatim and does
/// not resolve or load it; resolution happens at the (future) glyph call site.
///
/// Bounds are **inclusive on both ends**: a rule for `0xE000..=0xF8FF` matches
/// both `0xE000` and `0xF8FF`. Codepoints are stored as `u32` (not `char`) so a
/// range may freely span values that are not, by themselves, scalar values; the
/// lookup only ever tests real codepoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMapRule {
    start: u32,
    end: u32,
    font: String,
}

impl SymbolMapRule {
    /// Construct a rule for the inclusive range `start..=end`.
    ///
    /// Returns `None` for a **degenerate** range (`start > end`) so a malformed
    /// rule can never enter a [`SymbolMap`] — there is no panic path. A
    /// single-codepoint rule (`start == end`) is valid.
    pub fn new(start: u32, end: u32, font: impl Into<String>) -> Option<Self> {
        if start > end {
            return None;
        }
        Some(Self {
            start,
            end,
            font: font.into(),
        })
    }

    /// Whether `codepoint` falls within this rule's inclusive range.
    pub fn contains(&self, codepoint: u32) -> bool {
        self.start <= codepoint && codepoint <= self.end
    }

    /// The override font identifier (family name or path) this rule maps to.
    pub fn font(&self) -> &str {
        &self.font
    }

    /// The inclusive `(start, end)` codepoint bounds.
    pub fn bounds(&self) -> (u32, u32) {
        (self.start, self.end)
    }
}

/// SYMMAP: an ordered list of codepoint→override-font rules.
///
/// **Precedence is first-match-wins.** [`lookup`](Self::lookup) scans the rules
/// in insertion order and returns the font of the **first** rule whose inclusive
/// range contains the codepoint, so an earlier rule shadows a later overlapping
/// one. Callers that want a more-specific rule to win should insert it first.
///
/// **Empty map = identity.** With no rules, every lookup returns `None`, which
/// the glyph path treats as "use the normal font family" — i.e. the default /
/// off path is byte-identical to font resolution without SYMMAP. This is the
/// always-available bypass.
///
/// This is the thin core the glyph-resolution path will call; it does not load
/// or validate fonts (that is the call site's job) and has no settings or render
/// wiring yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolMap {
    rules: Vec<SymbolMapRule>,
}

impl SymbolMap {
    /// An empty map (the identity / off path).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the map has no rules (the identity path).
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The number of rules in the map.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Append an inclusive `start..=end` → `font` rule, returning `true` when it
    /// was accepted.
    ///
    /// A **degenerate** range (`start > end`) is rejected: the rule is dropped
    /// and `false` is returned, leaving the map unchanged. There is no panic.
    pub fn push(&mut self, start: u32, end: u32, font: impl Into<String>) -> bool {
        match SymbolMapRule::new(start, end, font) {
            Some(rule) => {
                self.rules.push(rule);
                true
            }
            None => false,
        }
    }

    /// Append an already-constructed rule (preserving first-match order).
    pub fn push_rule(&mut self, rule: SymbolMapRule) {
        self.rules.push(rule);
    }

    /// Resolve a codepoint to its override font identifier, or `None` to use the
    /// normal family. First-match-wins (see the type docs).
    pub fn lookup(&self, codepoint: u32) -> Option<&str> {
        self.rules
            .iter()
            .find(|rule| rule.contains(codepoint))
            .map(SymbolMapRule::font)
    }

    /// Convenience wrapper over [`lookup`](Self::lookup) for a `char`.
    pub fn lookup_char(&self, ch: char) -> Option<&str> {
        self.lookup(ch as u32)
    }

    /// The rules in insertion (precedence) order.
    pub fn rules(&self) -> &[SymbolMapRule] {
        &self.rules
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
    fn srgb_to_linear_delegates_byte_identically() {
        // The façade must equal the historical inline formula for every byte,
        // so native/grid callers see no change (RV3 passthrough guarantee).
        for byte in 0u16..=255 {
            let byte = byte as u8;
            let c = byte as f32 / 255.0;
            let inline = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            assert_eq!(srgb_to_linear(byte), inline);
        }
    }

    #[test]
    fn dim_linear_rgba_zero_is_identity_and_preserves_alpha() {
        let c = [0.4, 0.55, 0.2, 0.8];
        assert_eq!(dim_linear_rgba(c, 0.0), c);
        // Non-zero amount darkens the color channels but keeps alpha intact.
        let dimmed = dim_linear_rgba(c, 0.5);
        assert_eq!(dimmed[3], 0.8);
        assert!(dimmed[0] < c[0] && dimmed[1] < c[1] && dimmed[2] < c[2]);
    }

    /// Exercises the process-global `MIN_CONTRAST` seam. Kept in one test so the
    /// global mutation can't race a sibling, and restores the `1.0` default.
    #[test]
    fn enforce_contrast_rgba_seam_gates_on_the_global_floor() {
        let fg = [0.10, 0.10, 0.10, 0.5];
        let bg = [0.06, 0.06, 0.06, 1.0];
        // Default floor (1.0) is an exact identity no matter the pair.
        assert_eq!(min_contrast(), 1.0);
        assert_eq!(enforce_contrast_rgba(fg, bg), fg);

        // Raising the floor lifts the low-contrast fg and preserves alpha.
        set_min_contrast(4.5);
        let adj = enforce_contrast_rgba(fg, bg);
        assert_eq!(adj[3], fg[3], "alpha preserved");
        let c = crate::color::wcag_contrast([adj[0], adj[1], adj[2]], [bg[0], bg[1], bg[2]]);
        assert!(c >= 4.5 - 1e-3, "floor not met: {c}");

        // Restore the default so other tests see passthrough.
        set_min_contrast(1.0);
        assert_eq!(enforce_contrast_rgba(fg, bg), fg);
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

    /// Serializes the few tests that mutate the process-global ANSI palette
    /// override, so they cannot observe each other's writes when run in
    /// parallel. Held across set → assert → restore.
    static PALETTE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn default_ansi_palette_pins_historical_xterm_table() {
        // Byte-identity regression guard: the baseline ANSI palette must equal
        // the historical xterm 0–15 values exactly, so selecting `plain` (or no
        // theme) is pixel-identical to the pre-theme renderer. These literals
        // are the source of truth — duplicated here on purpose so a careless
        // edit to DEFAULT_ANSI_SRGB is caught.
        let historical: [(u8, u8, u8); 16] = [
            (0x00, 0x00, 0x00),
            (0xCD, 0x00, 0x00),
            (0x00, 0xCD, 0x00),
            (0xCD, 0xCD, 0x00),
            (0x00, 0x00, 0xEE),
            (0xCD, 0x00, 0xCD),
            (0x00, 0xCD, 0xCD),
            (0xE5, 0xE5, 0xE5),
            (0x7F, 0x7F, 0x7F),
            (0xFF, 0x00, 0x00),
            (0x00, 0xFF, 0x00),
            (0xFF, 0xFF, 0x00),
            (0x5C, 0x5C, 0xFF),
            (0xFF, 0x00, 0xFF),
            (0x00, 0xFF, 0xFF),
            (0xFF, 0xFF, 0xFF),
        ];
        assert_eq!(DEFAULT_ANSI_SRGB, historical);
    }

    #[test]
    fn indexed_srgb_resolves_ansi_range_from_palette_override() {
        let _guard = PALETTE_LOCK.lock().unwrap();
        // Default (no override): indices 0–15 equal the historical table.
        set_ansi_palette(&DEFAULT_ANSI_SRGB);
        for i in 0..16u8 {
            assert_eq!(indexed_srgb(i), DEFAULT_ANSI_SRGB[i as usize]);
        }

        // Apply a distinct palette and confirm indexed_srgb reflects it for the
        // 0–15 range while the computed cube/grayscale stay untouched.
        let mut themed = DEFAULT_ANSI_SRGB;
        for (i, slot) in themed.iter_mut().enumerate() {
            *slot = (i as u8, 0x40, 0x80);
        }
        set_ansi_palette(&themed);
        for i in 0..16u8 {
            assert_eq!(indexed_srgb(i), (i, 0x40, 0x80));
        }
        // Cube origin and a grayscale step are computed, not overridable.
        assert_eq!(indexed_srgb(16), (0, 0, 0));
        assert_eq!(indexed_srgb(231), (255, 255, 255));

        // Restore the baseline so other tests see the historical palette.
        set_ansi_palette(&DEFAULT_ANSI_SRGB);
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
    fn font_inventory_reports_stems_sorted_and_monospace_state() {
        let dir = unique_tmp_dir("inventory");
        std::fs::write(dir.join("BrokenFont.ttf"), b"not a font").expect("write broken font");

        let Some(bytes) = system_mono_bytes() else {
            let entries = font_inventory_in_dirs(std::slice::from_ref(&dir));
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "BrokenFont");
            assert!(!entries[0].monospace);
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        std::fs::write(dir.join("ZetaMono.ttf"), &bytes).expect("write zeta font");
        std::fs::write(dir.join("AlphaMono.otf"), &bytes).expect("write alpha font");

        let entries = font_inventory_in_dirs(std::slice::from_ref(&dir));
        let names = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["AlphaMono", "BrokenFont", "ZetaMono"]);
        assert!(entries[0].monospace);
        assert!(!entries[1].monospace);
        assert!(entries[2].monospace);

        let _ = std::fs::remove_dir_all(&dir);
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

    /// Lay down a multi-weight family fixture and return `(dir, dirs)`. Faces are
    /// the same monospace bytes; the filename stems drive weight matching.
    fn weight_fixture(tag: &str, faces: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        let bytes = system_mono_bytes().expect("caller guards on system font");
        let dir = unique_tmp_dir(tag);
        for name in faces {
            std::fs::write(dir.join(name), &bytes).expect("write fixture font");
        }
        let dirs = vec![dir.clone()];
        (dir, dirs)
    }

    #[test]
    fn weight_face_finds_bold_that_the_generic_resolver_excludes() {
        // FONT-WEIGHT-FIX root cause (T-FP-1 / T-variant-filter-scope): the Bold
        // face must be selected by the weight resolver, while the generic family
        // resolver still returns the REGULAR face (never the Bold variant).
        let Some(_) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let (dir, dirs) = weight_fixture(
            "weight_bold",
            &["CascadiaMono-Regular.ttf", "CascadiaMono-Bold.ttf"],
        );

        // The weight resolver finds the Bold FILE directly.
        assert_eq!(
            resolve_font_weight_face("CascadiaMono", "Bold", &dirs),
            Some(dir.join("CascadiaMono-Bold.ttf")),
            "weight resolver selects the Bold face the old concat path could not"
        );
        // The generic resolver's contract is UNCHANGED: it still returns the
        // regular face and never the Bold variant (T-variant-filter-scope).
        let generic = resolve_font_family("CascadiaMono", &dirs).expect("family resolves");
        assert_eq!(
            generic.regular,
            dir.join("CascadiaMono-Regular.ttf"),
            "generic resolver still excludes variant faces from the regular slot"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn weight_face_empty_inputs_return_none() {
        // T-FP-7 / T-regular-identity: an empty weight (or family) yields None so
        // the loader takes its unchanged regular-face path. No file scan result
        // can ever stand in for "no weight requested".
        assert!(resolve_font_weight_face("CascadiaMono", "", &[]).is_none());
        assert!(resolve_font_weight_face("CascadiaMono", "   ", &[]).is_none());
        assert!(resolve_font_weight_face("", "Bold", &[]).is_none());
        assert!(resolve_font_weight_face("  ", "Bold", &[]).is_none());
    }

    #[test]
    fn weight_face_missing_weight_returns_none_for_fallback() {
        // T-weight-not-found: a weight with no matching face returns None so the
        // caller warns and falls back to the regular face — never a crash.
        let Some(_) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let (dir, dirs) = weight_fixture(
            "weight_missing",
            &["CascadiaMono-Regular.ttf", "CascadiaMono-Bold.ttf"],
        );
        assert!(
            resolve_font_weight_face("CascadiaMono", "Black", &dirs).is_none(),
            "no Black face exists ⇒ None ⇒ caller falls back to regular"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn weight_face_light_resolves_and_beats_extralight() {
        // T-light-still-works + the ExtraLight disambiguation: "Light" must
        // resolve to the Light face, NOT ExtraLight (whose stem also contains
        // "light"). The shortest-stem tie-break makes this deterministic
        // regardless of filesystem iteration order.
        let Some(_) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let (dir, dirs) = weight_fixture(
            "weight_light",
            &[
                "CascadiaMono-Regular.ttf",
                "CascadiaMono-Light.ttf",
                "CascadiaMono-ExtraLight.ttf",
            ],
        );
        assert_eq!(
            resolve_font_weight_face("CascadiaMono", "Light", &dirs),
            Some(dir.join("CascadiaMono-Light.ttf")),
            "Light resolves to the Light face, not ExtraLight"
        );
        // ExtraLight remains addressable by its own name.
        assert_eq!(
            resolve_font_weight_face("CascadiaMono", "ExtraLight", &dirs),
            Some(dir.join("CascadiaMono-ExtraLight.ttf"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn weight_face_prefers_non_italic_for_a_pure_weight() {
        // A pure "Bold" request prefers the upright Bold face over BoldItalic,
        // while "BoldItalic" still reaches the italic face (its term only the
        // bold-italic stem carries).
        let Some(_) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let (dir, dirs) = weight_fixture(
            "weight_italic",
            &[
                "CascadiaMono-Regular.ttf",
                "CascadiaMono-Bold.ttf",
                "CascadiaMono-BoldItalic.ttf",
            ],
        );
        assert_eq!(
            resolve_font_weight_face("CascadiaMono", "Bold", &dirs),
            Some(dir.join("CascadiaMono-Bold.ttf")),
            "pure Bold prefers the upright Bold face"
        );
        assert_eq!(
            resolve_font_weight_face("CascadiaMono", "BoldItalic", &dirs),
            Some(dir.join("CascadiaMono-BoldItalic.ttf")),
            "BoldItalic reaches the bold-italic face"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn weight_face_matching_is_case_and_separator_insensitive() {
        // T-case-norm: weight matching normalizes case and separators, so
        // "semi bold" / "SemiBold" both match "CascadiaMono-SemiBold.ttf".
        let Some(_) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let (dir, dirs) = weight_fixture(
            "weight_case",
            &["CascadiaMono-Regular.ttf", "CascadiaMono-SemiBold.ttf"],
        );
        let expected = Some(dir.join("CascadiaMono-SemiBold.ttf"));
        assert_eq!(
            resolve_font_weight_face("CascadiaMono", "SemiBold", &dirs),
            expected
        );
        assert_eq!(
            resolve_font_weight_face("cascadia mono", "semi bold", &dirs),
            expected,
            "case + separator insensitive on both family and weight"
        );
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

    /// Bytes of the first available *proportional* (non-monospace) system font,
    /// or `None` when the host has only monospace faces (tests then skip). Scans
    /// the real search dirs and returns the first face that loads but fails the
    /// monospace probe; short-circuits on the first hit.
    fn system_proportional_bytes() -> Option<Vec<u8>> {
        for dir in font_search_dirs() {
            for f in collect_font_files(&[dir]) {
                if let Ok(font) = load_font_at(&f)
                    && !is_monospace(&font)
                {
                    return std::fs::read(&f).ok();
                }
            }
        }
        None
    }

    #[test]
    fn try_resolve_reports_not_found_for_missing_family() {
        assert_eq!(
            try_resolve_font_family("", &[]),
            Err(FontResolveError::NotFound)
        );
        assert_eq!(
            try_resolve_font_family("   ", &[]),
            Err(FontResolveError::NotFound)
        );
        // A real-looking name with no matching file is "not found".
        assert_eq!(
            try_resolve_font_family("DefinitelyNotAFontXYZ", &[]),
            Err(FontResolveError::NotFound)
        );
    }

    #[test]
    fn try_resolve_reports_not_monospace_for_proportional_family() {
        let Some(bytes) = system_proportional_bytes() else {
            eprintln!("skipping: no proportional system font available");
            return;
        };
        let dir = unique_tmp_dir("proportional");
        // A regular-face name that resolves by family lookup but is proportional.
        let path = dir.join("TestProp-Regular.ttf");
        std::fs::write(&path, &bytes).expect("write fixture font");
        let dirs = vec![dir.clone()];

        assert_eq!(
            try_resolve_font_family("Test Prop", &dirs),
            Err(FontResolveError::NotMonospace),
            "a name hit that is proportional reports NotMonospace"
        );
        // The same reason is reported for a direct path to a proportional file.
        assert_eq!(
            try_resolve_font_family(path.to_str().unwrap(), &[]),
            Err(FontResolveError::NotMonospace),
            "a direct path to a proportional font reports NotMonospace"
        );
        // The `Option` view collapses both reasons to `None`.
        assert!(resolve_font_family("Test Prop", &dirs).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn try_resolve_ok_agrees_with_resolve_font_family_on_success() {
        let Some(bytes) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let dir = unique_tmp_dir("ok");
        std::fs::write(dir.join("TestMono-Regular.ttf"), &bytes).expect("write fixture font");
        let dirs = vec![dir.clone()];

        let ok = try_resolve_font_family("Test Mono", &dirs).expect("family resolves");
        assert_eq!(ok.regular, dir.join("TestMono-Regular.ttf"));
        // The `Option` view must agree exactly on the success path.
        assert_eq!(resolve_font_family("Test Mono", &dirs), Some(ok));

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

    #[test]
    fn resolve_symbol_font_prefers_the_dedicated_symbols_face() {
        let Some(bytes) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let dir = unique_tmp_dir("symbolfont");
        // A plain body font, a patched family font, and the dedicated symbols
        // face — same bytes; the *names* drive selection. The dedicated
        // "Symbols Nerd Font" must win over the general "* Nerd Font" face, and
        // a non-Nerd font must be ignored entirely.
        std::fs::write(dir.join("DejaVuSansMono.ttf"), &bytes).expect("write body font");
        std::fs::write(dir.join("FiraCodeNerdFont-Regular.ttf"), &bytes).expect("write nerd font");
        std::fs::write(dir.join("SymbolsNerdFont-Regular.ttf"), &bytes).expect("write symbols");
        let dirs = vec![dir.clone()];

        // It resolves to *a* Nerd font (loadable), and the preference ranking
        // selects the symbols-only face when present.
        assert!(
            resolve_symbol_font_in(&dirs).is_some(),
            "a symbol font should resolve from the fixture dir"
        );

        // With only the body font present, nothing resolves.
        let plain = unique_tmp_dir("symbolfont-plain");
        std::fs::write(plain.join("DejaVuSansMono.ttf"), &bytes).expect("write body font");
        assert!(
            resolve_symbol_font_in(&[plain.clone()]).is_none(),
            "a non-Nerd font dir must not resolve a symbol font"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&plain);
    }

    // --- SYMMAP core ------------------------------------------------------

    #[test]
    fn symbolmap_empty_is_identity() {
        // The off / default path: an empty map returns None for every probe,
        // including the codepoint extremes — i.e. font resolution is untouched.
        let map = SymbolMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
        for cp in [0u32, 0x41, 0xE000, 0xF8FF, 0x10_FFFF] {
            assert_eq!(map.lookup(cp), None, "empty map must not map U+{cp:04X}");
        }
        assert_eq!(map.lookup_char('A'), None);
    }

    #[test]
    fn symbolmap_inclusive_bounds_match_both_ends() {
        // Bounds are inclusive: both endpoints map; the codepoints just outside
        // the range do not.
        let mut map = SymbolMap::new();
        assert!(map.push(0xE000, 0xF8FF, "Symbols Nerd Font"));
        assert_eq!(map.lookup(0xE000), Some("Symbols Nerd Font")); // start (inclusive)
        assert_eq!(map.lookup(0xF8FF), Some("Symbols Nerd Font")); // end (inclusive)
        assert_eq!(map.lookup(0xE000 - 1), None); // just below
        assert_eq!(map.lookup(0xF8FF + 1), None); // just above
    }

    #[test]
    fn symbolmap_single_codepoint_range_is_valid() {
        let mut map = SymbolMap::new();
        assert!(map.push(0x2603, 0x2603, "Emoji"));
        assert_eq!(map.lookup(0x2603), Some("Emoji"));
        assert_eq!(map.lookup(0x2602), None);
        assert_eq!(map.lookup(0x2604), None);
    }

    #[test]
    fn symbolmap_first_match_wins_on_overlap() {
        // Precedence is deterministic: the FIRST inserted rule whose range
        // contains the codepoint wins, shadowing a later overlapping rule.
        let mut map = SymbolMap::new();
        assert!(map.push(0x2600, 0x27BF, "First"));
        assert!(map.push(0x2700, 0x2710, "Second")); // overlaps the first
        // A codepoint in BOTH ranges resolves to the first-inserted rule.
        assert_eq!(map.lookup(0x2705), Some("First"));
        // A codepoint only in the second range still resolves to the second.
        assert_eq!(map.lookup(0x2710), Some("First")); // 0x2710 is inside 0x2600..=0x27BF too
        // A codepoint only the second rule could cover (outside the first).
        let mut map2 = SymbolMap::new();
        assert!(map2.push(0x100, 0x200, "A"));
        assert!(map2.push(0x150, 0x250, "B"));
        assert_eq!(map2.lookup(0x180), Some("A")); // overlap → first wins
        assert_eq!(map2.lookup(0x220), Some("B")); // only second covers it
    }

    #[test]
    fn symbolmap_degenerate_range_is_rejected_without_panic() {
        // start > end must never enter the map and must not panic.
        let mut map = SymbolMap::new();
        assert!(!map.push(0xF8FF, 0xE000, "Backwards"));
        assert!(map.is_empty(), "degenerate rule must not be stored");
        assert_eq!(map.lookup(0xE800), None);
        // The rule constructor agrees.
        assert!(SymbolMapRule::new(10, 5, "x").is_none());
        assert!(SymbolMapRule::new(5, 5, "x").is_some()); // equal bounds are valid
        assert!(SymbolMapRule::new(5, 10, "x").is_some());
    }

    #[test]
    fn symbolmap_disjoint_ranges_resolve_independently() {
        let mut map = SymbolMap::new();
        assert!(map.push(0x2500, 0x257F, "BoxDrawing")); // box-drawing
        assert!(map.push(0xE000, 0xF8FF, "Nerd")); // private use area
        assert_eq!(map.lookup(0x2550), Some("BoxDrawing"));
        assert_eq!(map.lookup(0xE700), Some("Nerd"));
        assert_eq!(map.lookup(0x0041), None); // 'A' — unmapped, normal family
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn symbolmap_rule_accessors_round_trip() {
        let rule = SymbolMapRule::new(0xE000, 0xF8FF, "Symbols Nerd Font").unwrap();
        assert_eq!(rule.bounds(), (0xE000, 0xF8FF));
        assert_eq!(rule.font(), "Symbols Nerd Font");
        assert!(rule.contains(0xE000));
        assert!(rule.contains(0xF8FF));
        assert!(!rule.contains(0xDFFF));
        let mut map = SymbolMap::new();
        map.push_rule(rule.clone());
        assert_eq!(map.rules(), std::slice::from_ref(&rule));
    }

    #[test]
    fn symbolmap_lookup_char_matches_lookup_codepoint() {
        let mut map = SymbolMap::new();
        assert!(map.push('☀' as u32, '⛿' as u32, "Weather"));
        assert_eq!(map.lookup_char('☀'), map.lookup('☀' as u32));
        assert_eq!(map.lookup_char('☀'), Some("Weather"));
    }
}
