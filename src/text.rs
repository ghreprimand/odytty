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
//! The default text face is bundled Victor Mono (JetBrains Mono is also bundled
//! and selectable). The settings layer can still
//! provide an explicit font path or system font family; bad overrides fall back
//! to the bundled face so startup never depends on host font installation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};

use crate::core::Color;
use crate::settings::FONT_ENV;

/// The glyph atlas and its cell metrics live in [`crate::atlas`]; re-exported
/// here so `crate::text::{CellSize, GlyphAtlas}` call sites keep resolving.
pub use crate::atlas::{CellSize, FontStyle, GlyphAtlas, SubpixelMode};

/// Default bundled body font family. Victor Mono is the out-of-the-box default
/// (its `.otf`/CFF outlines rasterize cleanly through `ab_glyph`); JetBrains
/// Mono is also bundled and remains selectable via `font_family`.
pub const BUNDLED_FONT_FAMILY: &str = "Victor Mono";
/// Version of the bundled **default** family (Victor Mono).
pub const BUNDLED_FONT_VERSION: &str = "1.560";
/// Bundled but non-default family: JetBrains Mono is still shipped so existing
/// configs keep working and it stays selectable in the picker.
pub const JETBRAINS_FONT_FAMILY: &str = "JetBrains Mono";
/// Version of the bundled JetBrains Mono faces.
pub const JETBRAINS_FONT_VERSION: &str = "2.304";
pub const BUNDLED_SYMBOL_FONT_FAMILY: &str = "Symbols Nerd Font Mono";
pub const BUNDLED_SYMBOL_FONT_FILENAME: &str = "SymbolsNerdFontMono-Regular.ttf";
pub const BUNDLED_SYMBOL_FONT_RELATIVE_PATH: &str =
    "assets/fonts/nerd-fonts-symbols/SymbolsNerdFontMono-Regular.ttf";

#[cfg(feature = "bundled-symbols-font")]
const BUNDLED_SYMBOL_FONT_BYTES: &[u8] =
    include_bytes!("../assets/fonts/nerd-fonts-symbols/SymbolsNerdFontMono-Regular.ttf");

struct BundledFace {
    family: &'static str,
    weight: &'static str,
    italic: bool,
    filename: &'static str,
    bytes: &'static [u8],
}

const BUNDLED_FACES: &[BundledFace] = &[
    // Victor Mono (default family). SGR italic maps to the **Oblique** face
    // (roman slant) for readability, not the cursive Italic face — so the italic
    // row for each weight points at the `…Oblique.otf` file. The Regular weight's
    // oblique file is named `VictorMono-Oblique.otf` (no `Regular` infix).
    BundledFace {
        family: "Victor Mono",
        weight: "Thin",
        italic: false,
        filename: "VictorMono-Thin.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-Thin.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Thin",
        italic: true,
        filename: "VictorMono-ThinOblique.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-ThinOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "ExtraLight",
        italic: false,
        filename: "VictorMono-ExtraLight.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-ExtraLight.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "ExtraLight",
        italic: true,
        filename: "VictorMono-ExtraLightOblique.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-ExtraLightOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Light",
        italic: false,
        filename: "VictorMono-Light.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-Light.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Light",
        italic: true,
        filename: "VictorMono-LightOblique.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-LightOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Regular",
        italic: false,
        filename: "VictorMono-Regular.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-Regular.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Regular",
        italic: true,
        filename: "VictorMono-Oblique.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-Oblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Medium",
        italic: false,
        filename: "VictorMono-Medium.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-Medium.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Medium",
        italic: true,
        filename: "VictorMono-MediumOblique.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-MediumOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "SemiBold",
        italic: false,
        filename: "VictorMono-SemiBold.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-SemiBold.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "SemiBold",
        italic: true,
        filename: "VictorMono-SemiBoldOblique.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-SemiBoldOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Bold",
        italic: false,
        filename: "VictorMono-Bold.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-Bold.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Bold",
        italic: true,
        filename: "VictorMono-BoldOblique.otf",
        bytes: include_bytes!("../assets/fonts/victor-mono/VictorMono-BoldOblique.otf"),
    },
    // JetBrains Mono (bundled, selectable). JetBrains has no separate oblique
    // variant, so italic rows use its italic faces.
    BundledFace {
        family: "JetBrains Mono",
        weight: "Thin",
        italic: false,
        filename: "JetBrainsMono-Thin.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Thin.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Thin",
        italic: true,
        filename: "JetBrainsMono-ThinItalic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-ThinItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraLight",
        italic: false,
        filename: "JetBrainsMono-ExtraLight.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLight.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraLight",
        italic: true,
        filename: "JetBrainsMono-ExtraLightItalic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLightItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Light",
        italic: false,
        filename: "JetBrainsMono-Light.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Light.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Light",
        italic: true,
        filename: "JetBrainsMono-LightItalic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-LightItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Regular",
        italic: false,
        filename: "JetBrainsMono-Regular.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Regular",
        italic: true,
        filename: "JetBrainsMono-Italic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Italic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Medium",
        italic: false,
        filename: "JetBrainsMono-Medium.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Medium.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Medium",
        italic: true,
        filename: "JetBrainsMono-MediumItalic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-MediumItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "SemiBold",
        italic: false,
        filename: "JetBrainsMono-SemiBold.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBold.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "SemiBold",
        italic: true,
        filename: "JetBrainsMono-SemiBoldItalic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBoldItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Bold",
        italic: false,
        filename: "JetBrainsMono-Bold.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Bold",
        italic: true,
        filename: "JetBrainsMono-BoldItalic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-BoldItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraBold",
        italic: false,
        filename: "JetBrainsMono-ExtraBold.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraBold.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraBold",
        italic: true,
        filename: "JetBrainsMono-ExtraBoldItalic.ttf",
        bytes: include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraBoldItalic.ttf"),
    },
];

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

    if let Ok(font) = load_bundled_font() {
        return Ok(font);
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

pub fn is_bundled_font_family(query: &str) -> bool {
    let normalized = normalize_family(query);
    normalized == normalize_family(BUNDLED_FONT_FAMILY)
        || normalized == normalize_family(JETBRAINS_FONT_FAMILY)
        || normalized == "monospace"
}

/// Resolve a family name (possibly `"monospace"` or empty) to a concrete bundled
/// family. Unknown names fall back to the default family
/// ([`BUNDLED_FONT_FAMILY`], Victor Mono) so the bundled path always picks a
/// real face. Callers only reach the bundled path after
/// [`is_bundled_font_family`] is true, so the only realistic inputs here are the
/// two bundled family names, `"monospace"`, or an empty string.
pub fn bundled_family_for(query: &str) -> &'static str {
    let normalized = normalize_family(query);
    if normalized == normalize_family(JETBRAINS_FONT_FAMILY) {
        JETBRAINS_FONT_FAMILY
    } else {
        BUNDLED_FONT_FAMILY
    }
}

pub fn load_bundled_font() -> Result<FontVec, TextError> {
    load_bundled_face_for(BUNDLED_FONT_FAMILY, "Regular", false).ok_or(TextError::NoFont)
}

pub fn load_bundled_style(style: FontStyle) -> Result<FontVec, TextError> {
    load_bundled_style_for(BUNDLED_FONT_FAMILY, style)
}

pub fn load_bundled_weight(weight: &str, italic: bool) -> Option<FontVec> {
    load_bundled_weight_for(BUNDLED_FONT_FAMILY, weight, italic)
}

/// Load a specific style face from a named bundled family. Mirrors
/// [`load_bundled_style`] but for the explicitly chosen family (Victor Mono or
/// JetBrains Mono) rather than the default.
pub fn load_bundled_style_for(family: &str, style: FontStyle) -> Result<FontVec, TextError> {
    let family = bundled_family_for(family);
    match style {
        FontStyle::Regular => load_bundled_face_for(family, "Regular", false),
        FontStyle::Bold => load_bundled_face_for(family, "Bold", false),
        FontStyle::Italic => load_bundled_face_for(family, "Regular", true),
        FontStyle::BoldItalic => load_bundled_face_for(family, "Bold", true),
    }
    .ok_or(TextError::NoFont)
}

/// Resolve a weight (possibly `"regular"`/empty) within a named bundled family.
/// Falls back to the `Regular` face of that family when the weight is empty or
/// names the regular/normal variant.
pub fn load_bundled_weight_for(family: &str, weight: &str, italic: bool) -> Option<FontVec> {
    let family = bundled_family_for(family);
    let target = normalize_family(weight);
    if target.is_empty() || target == "regular" || target == "normal" {
        return load_bundled_face_for(family, "Regular", italic);
    }
    BUNDLED_FACES
        .iter()
        .find(|face| {
            face.family == family
                && normalize_family(face.weight) == target
                && face.italic == italic
        })
        .and_then(parse_bundled_face)
}

fn load_bundled_face_for(family: &str, weight: &str, italic: bool) -> Option<FontVec> {
    BUNDLED_FACES
        .iter()
        .find(|face| face.family == family && face.weight == weight && face.italic == italic)
        .and_then(parse_bundled_face)
}

fn parse_bundled_face(face: &BundledFace) -> Option<FontVec> {
    FontVec::try_from_vec(face.bytes.to_vec())
        .map_err(|source| TextError::Parse {
            path: format!("bundled {}", face.filename),
            source,
        })
        .ok()
}

/// The recorded filename of the bundled face that `(family, weight, italic)`
/// resolves to, or `None` when no such face is bundled. Used by tests to assert
/// the Oblique-vs-cursive-italic and family-routing decisions deterministically
/// (filename strings), independent of font-table decoding.
#[cfg(test)]
fn bundled_face_filename(family: &str, weight: &str, italic: bool) -> Option<&'static str> {
    let family = bundled_family_for(family);
    BUNDLED_FACES
        .iter()
        .find(|face| face.family == family && face.weight == weight && face.italic == italic)
        .map(|face| face.filename)
}

/// Load the bundled symbols-only Nerd Font face when the asset feature is
/// enabled. Default builds enable it so the RV6 PUA-icon fallback works without
/// host Nerd Font installation; `--no-default-features` leaves this as `None`.
pub fn resolve_bundled_symbol_font() -> Option<FontVec> {
    #[cfg(feature = "bundled-symbols-font")]
    {
        return FontVec::try_from_vec(BUNDLED_SYMBOL_FONT_BYTES.to_vec())
            .map_err(|source| TextError::Parse {
                path: format!("bundled {}", BUNDLED_SYMBOL_FONT_FILENAME),
                source,
            })
            .ok();
    }

    #[cfg(not(feature = "bundled-symbols-font"))]
    {
        None
    }
}

/// A resolved font family: the validated monospace `regular` face plus any
/// style variants discovered alongside it.
///
/// **Groundwork (F1):** only `regular` is loaded and rendered today. The
/// `bold`/`italic`/`bold_italic` paths are *discovered* by font metadata so a
/// future packet can load them into the `(style, char)`-keyed atlas without
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

// ---------------------------------------------------------------------------
// Real font metadata (ttf-parser): family name, weight, italic, monospace.
//
// Family identity is read from the font's `name` table, never guessed from the
// filename stem — `CascadiaCodeItalic.ttf` has no separator yet its real family
// is "Cascadia Code", and the regular face must be chosen by OS/2 weight (400),
// not by the shortest stem. ttf-parser is read-only here (the same parser
// ab_glyph already uses); rasterization still goes through ab_glyph.
// ---------------------------------------------------------------------------

/// Metadata read from a font file's tables, used to enumerate families and pick
/// the right face by real attributes rather than filename heuristics.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FaceMeta {
    /// Real family name (Typographic Family, name ID 16; else Family, ID 1).
    family: String,
    /// OS/2 usWeightClass as a number (Regular == 400). `Weight::Normal` when
    /// the OS/2 table is absent.
    weight: u16,
    /// OS/2 usWidthClass as a number (Normal == 5). Lets the regular-face pick
    /// prefer the normal-width face over width variants (e.g. Inconsolata ships
    /// Expanded/Condensed faces under the same typographic family).
    width: u16,
    /// Italic / oblique flag (head.macStyle / OS/2 fsSelection).
    italic: bool,
    /// post.isFixedPitch (the font's own monospace claim); a `false` here is not
    /// authoritative — some monospace fonts leave it unset, so the caller falls
    /// back to the advance-probe [`is_monospace`].
    monospaced_flag: bool,
}

/// OpenType `name` table IDs used for family identity.
const NAME_ID_FAMILY: u16 = 1;
const NAME_ID_TYPOGRAPHIC_FAMILY: u16 = 16;

/// Read [`FaceMeta`] for the first face in a font file, or `None` when the file
/// cannot be read/parsed or carries no usable family name.
fn read_face_meta(path: &Path) -> Option<FaceMeta> {
    let data = std::fs::read(path).ok()?;
    let face = ttf_parser::Face::parse(&data, 0).ok()?;
    // Exclude emoji / icon / symbol faces from text-family enumeration and
    // family-name resolution: a color-emoji font (e.g. "Noto Color Emoji")
    // can report fixed-pitch and slip past the monospace probe, listing a
    // proportional/color face as a text mono family in the picker. A real text
    // mono font always covers basic Latin; an emoji/icon font never does. This
    // does NOT affect the separate RV6 symbol/PUA-icon fallback path.
    if !has_basic_latin_coverage(&face) {
        return None;
    }
    let family = real_family_name(&face)?;
    Some(FaceMeta {
        family,
        weight: face.weight().to_number(),
        width: face.width().to_number(),
        italic: face.is_italic(),
        monospaced_flag: face.is_monospaced(),
    })
}

/// Extract the real family name from a parsed face: prefer the Typographic
/// Family (name ID 16), fall back to the legacy Family (name ID 1). Returns the
/// first non-empty Unicode-decodable record for each ID. `None` when neither is
/// present/decodable.
fn real_family_name(face: &ttf_parser::Face) -> Option<String> {
    let mut typographic: Option<String> = None;
    let mut family: Option<String> = None;
    for name in face.names() {
        let slot = match name.name_id {
            NAME_ID_TYPOGRAPHIC_FAMILY => &mut typographic,
            NAME_ID_FAMILY => &mut family,
            _ => continue,
        };
        if slot.is_some() {
            continue;
        }
        if let Some(text) = name.to_string() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                *slot = Some(trimmed.to_owned());
            }
        }
    }
    typographic.or(family)
}

/// Representative basic-Latin code points a real text font must render. An
/// emoji / icon / symbol font maps none of these, so requiring coverage of all
/// three cleanly excludes such faces from the text-family picker while never
/// false-excluding a genuine monospace text font.
const LATIN_COVERAGE_PROBE: [char; 3] = ['A', 'z', '0'];

/// Whether a face covers basic Latin (see [`LATIN_COVERAGE_PROBE`]). Used to
/// keep color-emoji / icon faces — which can falsely report fixed-pitch — out
/// of the text-family list and family-name resolution.
fn has_basic_latin_coverage(face: &ttf_parser::Face) -> bool {
    LATIN_COVERAGE_PROBE
        .iter()
        .all(|&c| face.glyph_index(c).is_some())
}

/// Whether a font file is monospace: trust the `post.isFixedPitch` flag when
/// set, otherwise fall back to the advance-width probe ([`is_monospace`]) so a
/// monospace font that leaves the flag unset is still accepted.
fn path_is_monospace(path: &Path, meta: &FaceMeta) -> bool {
    if meta.monospaced_flag {
        return true;
    }
    load_font_at(path)
        .map(|font| is_monospace(&font))
        .unwrap_or(false)
}

/// Distinct real family names that have at least one monospace face, sorted
/// case-insensitively and deduplicated. This is what the font picker lists; it
/// reads real metadata, so italic/variant files of one family collapse into a
/// single entry and proportional-only families never appear.
pub fn font_families() -> Vec<String> {
    let mut families = font_families_in_dirs(&font_search_dirs());
    if !families
        .iter()
        .any(|family| normalize_family(family) == normalize_family(BUNDLED_FONT_FAMILY))
    {
        families.push(BUNDLED_FONT_FAMILY.to_owned());
        families.sort_by_key(|name| name.to_lowercase());
    }
    families
}

/// [`font_families`] over an explicit directory set, for hermetic tests.
pub fn font_families_in_dirs(dirs: &[PathBuf]) -> Vec<String> {
    let metas = collect_font_files(dirs)
        .into_iter()
        .filter_map(|path| {
            let meta = read_face_meta(&path)?;
            let monospace = path_is_monospace(&path, &meta);
            Some((meta, monospace))
        })
        .collect::<Vec<_>>();
    distinct_monospace_families(&metas)
}

/// Pure family-collapse: distinct real family names among `metas` that have a
/// monospace face, deduped case-insensitively (first spelling wins) and sorted.
/// Factored out so the dedup/exclusion rules are testable without files.
fn distinct_monospace_families(metas: &[(FaceMeta, bool)]) -> Vec<String> {
    let mut families: Vec<String> = Vec::new();
    for (meta, monospace) in metas {
        if !monospace {
            continue;
        }
        let family = meta.family.trim();
        if family.is_empty() {
            continue;
        }
        let key = normalize_family(family);
        if key.is_empty() {
            continue;
        }
        if !families.iter().any(|f| normalize_family(f) == key) {
            families.push(family.to_owned());
        }
    }
    families.sort_by_key(|name| name.to_lowercase());
    families
}

/// Pick the index of the best "regular" face among `metas`: prefer an upright
/// face over an italic one, then the weight closest to Regular (400). This is
/// the fix for the washed-out-thin-text bug — `Thin` (100) loses to `Regular`
/// (400) by weight distance, where the old shortest-stem rule wrongly chose it.
fn pick_regular_index(metas: &[FaceMeta]) -> Option<usize> {
    metas
        .iter()
        .enumerate()
        .max_by_key(|(_, meta)| regular_rank(meta))
        .map(|(index, _)| index)
}

/// Ranking key for [`pick_regular_index`]: upright beats italic; then the
/// normal-width face beats width variants (Condensed/Expanded), so a family that
/// ships width variants under one typographic name still yields its true
/// regular; finally the weight nearest 400 wins. Higher tuple is better.
fn regular_rank(meta: &FaceMeta) -> (i32, i32, i32) {
    let upright = i32::from(!meta.italic);
    let width_closeness = -((meta.width as i32 - 5).abs());
    let weight_closeness = -((meta.weight as i32 - 400).abs());
    (upright, width_closeness, weight_closeness)
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
/// the embedded probe list. The family is matched against the real `name`-table
/// family (not the filename stem) and the regular face is chosen by OS/2 weight
/// (closest to 400, upright). Style variants are discovered by metadata but not
/// opened. Pure with respect to `dirs`, so tests can supply a fixture directory.
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

    // Family-name lookup across the search dirs, by REAL metadata family name
    // (the `name` table), never the filename stem.
    let target = normalize_family(trimmed);
    if target.is_empty() {
        return Err(FontResolveError::NotFound);
    }

    // Gather (path, meta) for every face whose real family name matches. Prefer
    // an EXACT normalized match — the picker writes exact real names, and exact
    // matching keeps "JetBrains Mono" from also catching "JetBrains Mono NL".
    // Fall back to a substring match only when nothing matches exactly, so a
    // partial user-typed `ODYTTY_FONT_FAMILY` still resolves.
    let mut exact: Vec<(PathBuf, FaceMeta)> = Vec::new();
    let mut partial: Vec<(PathBuf, FaceMeta)> = Vec::new();
    for f in &collect_font_files(dirs) {
        let Some(meta) = read_face_meta(f) else {
            continue;
        };
        let family_key = normalize_family(&meta.family);
        if family_key == target {
            exact.push((f.clone(), meta));
        } else if family_key.contains(&target) {
            partial.push((f.clone(), meta));
        }
    }
    let matched = if exact.is_empty() { partial } else { exact };
    if matched.is_empty() {
        return Err(FontResolveError::NotFound);
    }

    // Keep the monospace faces; a family that matched by name but offers no
    // monospace face reports NotMonospace (the old name-hit-but-proportional
    // behaviour), so the caller can surface a precise reason.
    let monospace: Vec<(PathBuf, FaceMeta)> = matched
        .into_iter()
        .filter(|(path, meta)| path_is_monospace(path, meta))
        .collect();
    if monospace.is_empty() {
        return Err(FontResolveError::NotMonospace);
    }

    // Select the regular face by metadata (closest to weight 400, upright),
    // never by stem length — the fix for the thin-face selection bug.
    let metas: Vec<FaceMeta> = monospace.iter().map(|(_, meta)| meta.clone()).collect();
    let regular_index = pick_regular_index(&metas).unwrap_or(0);
    let regular = monospace[regular_index].0.clone();

    // Discover style variants by metadata among the monospace faces (groundwork:
    // discovered for a future packet, not opened here).
    let bold = pick_variant(&monospace, true, false);
    let italic = pick_variant(&monospace, false, true);
    let bold_italic = pick_variant(&monospace, true, true);

    Ok(FontFamilyMatch {
        regular,
        bold,
        italic,
        bold_italic,
    })
}

/// Pick a style-variant face by metadata: `want_bold` selects faces at OS/2
/// weight ≥ 600, `want_italic` selects italic faces; among matches the one
/// closest to the canonical weight (700 bold / 400 upright) wins. Returns `None`
/// when the family has no such variant.
fn pick_variant(
    faces: &[(PathBuf, FaceMeta)],
    want_bold: bool,
    want_italic: bool,
) -> Option<PathBuf> {
    let target_weight = if want_bold { 700 } else { 400 };
    faces
        .iter()
        .filter(|(_, meta)| (meta.weight >= 600) == want_bold && meta.italic == want_italic)
        .min_by_key(|(_, meta)| (meta.weight as i32 - target_weight).abs())
        .map(|(path, _)| path.clone())
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
/// when neither the host nor the gated bundled asset can provide one.
///
/// Resolution order:
/// 1. An explicit [`SYMBOL_FONT_ENV`] path (loaded directly; a bad path yields
///    fallback search rather than aborting).
/// 2. The first file under [`font_search_dirs`] whose normalized stem contains
///    a [`SYMBOL_FONT_HINTS`] fragment, preferring the dedicated symbols-only
///    face.
/// 3. The bundled symbols-only face, when the default `bundled-symbols-font`
///    feature is enabled.
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
    resolve_symbol_font_in(&font_search_dirs()).or_else(resolve_bundled_symbol_font)
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
    /// global mutation can't race a sibling, and restores the configured default.
    #[test]
    fn enforce_contrast_rgba_seam_gates_on_the_global_floor() {
        let fg = [0.10, 0.10, 0.10, 0.5];
        let bg = [0.06, 0.06, 0.06, 1.0];
        assert_eq!(min_contrast(), crate::settings::DEFAULT_MIN_CONTRAST);

        // Raising the floor lifts the low-contrast fg and preserves alpha.
        set_min_contrast(4.5);
        let adj = enforce_contrast_rgba(fg, bg);
        assert_eq!(adj[3], fg[3], "alpha preserved");
        let c = crate::color::wcag_contrast([adj[0], adj[1], adj[2]], [bg[0], bg[1], bg[2]]);
        assert!(c >= 4.5 - 1e-3, "floor not met: {c}");

        // The explicit passthrough override remains exact.
        set_min_contrast(1.0);
        assert_eq!(enforce_contrast_rgba(fg, bg), fg);

        // Restore the configured default for sibling tests.
        set_min_contrast(crate::settings::DEFAULT_MIN_CONTRAST);
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
    fn bundled_default_and_jetbrains_faces_are_parseable_and_monospace() {
        // Both bundled families are recognized, plus the generic "monospace".
        assert!(is_bundled_font_family(BUNDLED_FONT_FAMILY)); // Victor Mono
        assert!(is_bundled_font_family(JETBRAINS_FONT_FAMILY));
        assert!(is_bundled_font_family("monospace"));
        assert!(!is_bundled_font_family("Comic Sans"));

        // Family routing: explicit JetBrains stays JetBrains; everything else
        // (monospace/empty/unknown) falls back to the default (Victor Mono).
        assert_eq!(
            bundled_family_for(JETBRAINS_FONT_FAMILY),
            JETBRAINS_FONT_FAMILY
        );
        assert_eq!(bundled_family_for(BUNDLED_FONT_FAMILY), BUNDLED_FONT_FAMILY);
        assert_eq!(bundled_family_for("monospace"), BUNDLED_FONT_FAMILY);
        assert_eq!(bundled_family_for(""), BUNDLED_FONT_FAMILY);

        // Filename routing proves the Oblique-vs-cursive decision and the family
        // split without depending on font-table decoding in tests.
        assert_eq!(
            bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", false),
            Some("VictorMono-Regular.otf")
        );
        // SGR italic for the default family resolves to the **Oblique** (roman
        // slant) face, NOT the cursive Italic face.
        assert_eq!(
            bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", true),
            Some("VictorMono-Oblique.otf")
        );
        assert_eq!(
            bundled_face_filename(BUNDLED_FONT_FAMILY, "Bold", true),
            Some("VictorMono-BoldOblique.otf")
        );
        assert_eq!(
            bundled_face_filename(JETBRAINS_FONT_FAMILY, "Regular", true),
            Some("JetBrainsMono-Italic.ttf")
        );
        assert_eq!(
            bundled_face_filename(JETBRAINS_FONT_FAMILY, "ExtraBold", false),
            Some("JetBrainsMono-ExtraBold.ttf")
        );
        // No cursive Italic face is bundled for the default family.
        assert_eq!(
            bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", false)
                .unwrap()
                .contains("Italic"),
            false
        );

        // Default family (Victor Mono): regular, a non-default weight, and the
        // SGR-italic face all parse and are monospace.
        let regular = load_bundled_font().expect("bundled default regular parses");
        assert!(
            is_monospace(&regular),
            "bundled default regular is monospace"
        );
        let semibold =
            load_bundled_weight("SemiBold", false).expect("bundled default semibold parses");
        assert!(
            is_monospace(&semibold),
            "bundled default semibold is monospace"
        );
        let italic = load_bundled_style(FontStyle::Italic).expect("bundled default italic parses");
        assert!(is_monospace(&italic), "bundled default italic is monospace");

        // JetBrains Mono remains bundled and selectable by family name.
        let jb_regular = load_bundled_style_for(JETBRAINS_FONT_FAMILY, FontStyle::Regular)
            .expect("JetBrains regular parses");
        assert!(is_monospace(&jb_regular), "JetBrains regular is monospace");
        let jb_semibold = load_bundled_weight_for(JETBRAINS_FONT_FAMILY, "SemiBold", false)
            .expect("JetBrains semibold parses");
        assert!(
            is_monospace(&jb_semibold),
            "JetBrains semibold is monospace"
        );
        let jb_italic = load_bundled_style_for(JETBRAINS_FONT_FAMILY, FontStyle::Italic)
            .expect("JetBrains italic parses");
        assert!(is_monospace(&jb_italic), "JetBrains italic is monospace");
    }

    #[cfg(feature = "bundled-symbols-font")]
    #[test]
    fn bundled_symbol_font_is_parseable_and_covers_representative_pua_icons() {
        let font = resolve_bundled_symbol_font().expect("bundled symbols font parses");
        for ch in ['\u{E0B0}', '\u{E700}', '\u{F031}', '\u{F0001}'] {
            assert_ne!(
                font.glyph_id(ch).0,
                0,
                "bundled symbols font must cover U+{:04X}",
                ch as u32
            );
        }
    }

    /// A real monospace family installed on this host (read from metadata), with
    /// the live search dirs, or `None` when the host has no monospace face.
    fn a_real_monospace_family() -> Option<(String, Vec<PathBuf>)> {
        let dirs = font_search_dirs();
        for f in collect_font_files(&dirs) {
            if let Some(meta) = read_face_meta(&f)
                && path_is_monospace(&f, &meta)
                && !meta.family.trim().is_empty()
            {
                return Some((meta.family, dirs));
            }
        }
        None
    }

    /// Trap (a)+(b) over REAL fonts: a family installed on this host resolves to
    /// a real, loadable monospace regular face — never "did not resolve", never
    /// a thin/italic face. Family identity comes from the `name` table, so the
    /// resolved regular is the one whose metadata is upright (the regular slot).
    #[test]
    fn resolve_real_family_picks_a_monospace_regular_face() {
        let Some((family, dirs)) = a_real_monospace_family() else {
            eprintln!("skipping: no system monospace family available");
            return;
        };
        let m = try_resolve_font_family(&family, &dirs).expect("real family resolves");
        let font = load_font_at(&m.regular).expect("regular face loads");
        assert!(is_monospace(&font), "resolved regular face is monospace");
        // The enumeration API lists this real family (no stem guessing).
        assert!(
            font_families_in_dirs(&dirs)
                .iter()
                .any(|f| normalize_family(f) == normalize_family(&family)),
            "font_families lists the resolved real family"
        );
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
    fn weight_face_finds_bold_within_a_family() {
        // FONT-WEIGHT-FIX: the weight resolver selects the requested weight's
        // FILE by stem within the family (the old `"{family} {weight}"` concat
        // path could not). This path is unchanged by the metadata rework: the
        // real family name the picker writes (e.g. "Cascadia Code") still
        // normalizes into the file stem, so weight selection stays robust.
        let Some(_) = system_mono_bytes() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let (dir, dirs) = weight_fixture(
            "weight_bold",
            &["CascadiaMono-Regular.ttf", "CascadiaMono-Bold.ttf"],
        );

        assert_eq!(
            resolve_font_weight_face("CascadiaMono", "Bold", &dirs),
            Some(dir.join("CascadiaMono-Bold.ttf")),
            "weight resolver selects the Bold face the old concat path could not"
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
        let path = dir.join("Proportional.ttf");
        std::fs::write(&path, &bytes).expect("write fixture font");
        let dirs = vec![dir.clone()];

        // Query by the proportional face's REAL family name (from metadata): the
        // family matches but offers no monospace face → NotMonospace.
        let Some(family) = read_face_meta(&path).map(|meta| meta.family) else {
            eprintln!("skipping: proportional face carries no family name");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert_eq!(
            try_resolve_font_family(&family, &dirs),
            Err(FontResolveError::NotMonospace),
            "a real family that matched but is proportional reports NotMonospace"
        );
        // The same reason is reported for a direct path to a proportional file.
        assert_eq!(
            try_resolve_font_family(path.to_str().unwrap(), &[]),
            Err(FontResolveError::NotMonospace),
            "a direct path to a proportional font reports NotMonospace"
        );
        // The `Option` view collapses both reasons to `None`.
        assert!(resolve_font_family(&family, &dirs).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn try_resolve_ok_agrees_with_resolve_font_family_on_success() {
        let Some((family, dirs)) = a_real_monospace_family() else {
            eprintln!("skipping: no system monospace family available");
            return;
        };
        let ok = try_resolve_font_family(&family, &dirs).expect("real family resolves");
        // The `Option` view must agree exactly on the success path.
        assert_eq!(resolve_font_family(&family, &dirs), Some(ok));
    }

    /// Synthetic [`FaceMeta`] for the pure metadata-logic traps (no files).
    /// Normal width (5); use [`fm_width`] to exercise the width tie-break.
    fn fm(family: &str, weight: u16, italic: bool) -> FaceMeta {
        fm_width(family, weight, 5, italic)
    }

    /// [`fm`] with an explicit OS/2 width class (Normal == 5).
    fn fm_width(family: &str, weight: u16, width: u16, italic: bool) -> FaceMeta {
        FaceMeta {
            family: family.to_owned(),
            weight,
            width,
            italic,
            monospaced_flag: true,
        }
    }

    // Trap (c): distinct family enumeration collapses italic + roman + every
    // weight of one family into ONE entry, excludes proportional-only families,
    // and sorts case-insensitively.
    #[test]
    fn distinct_families_dedup_styles_and_exclude_proportional_only() {
        let metas = vec![
            (fm("JetBrains Mono", 400, false), true),
            (fm("JetBrains Mono", 700, false), true), // bold of same family
            (fm("JetBrains Mono", 400, true), true),  // italic of same family
            (fm("Cascadia Code", 400, false), true),
            (fm("Helvetica", 400, false), false), // proportional-only → excluded
        ];
        assert_eq!(
            distinct_monospace_families(&metas),
            vec!["Cascadia Code".to_owned(), "JetBrains Mono".to_owned()]
        );
    }

    // Emoji/icon exclusion: a real mono text font covers basic Latin; a
    // color-emoji font does not. read_face_meta drops faces failing this probe
    // so they never list as text families (the "Noto Color Emoji" picker wart).
    #[test]
    fn latin_coverage_accepts_text_font_rejects_emoji() {
        // Positive: a real monospace text font on this host covers basic Latin.
        if let Some((_, dirs)) = a_real_monospace_family() {
            let covered = collect_font_files(&dirs).iter().any(|f| {
                let Ok(data) = std::fs::read(f) else {
                    return false;
                };
                ttf_parser::Face::parse(&data, 0)
                    .map(|face| has_basic_latin_coverage(&face))
                    .unwrap_or(false)
            });
            assert!(covered, "a text mono font must report Latin coverage");
        }
        // Negative: a color-emoji font (if installed) fails coverage AND is
        // therefore absent from read_face_meta / font_families. Skip if absent.
        let emoji = Path::new("/usr/share/fonts/noto/NotoColorEmoji.ttf");
        if emoji.is_file() {
            let data = std::fs::read(emoji).expect("read emoji font");
            if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
                assert!(
                    !has_basic_latin_coverage(&face),
                    "color-emoji font must fail the Latin-coverage probe"
                );
            }
            assert!(
                read_face_meta(emoji).is_none(),
                "emoji font must be excluded from family enumeration"
            );
        }
    }

    // Trap (b): the regular face is chosen by metadata (400, upright), NOT by
    // shortest stem / first-seen — Thin must never win. Order puts Thin first so
    // a first-wins bug would surface.
    #[test]
    fn pick_regular_prefers_400_upright_over_thin_and_italic() {
        let metas = vec![
            fm("X", 100, false), // Thin
            fm("X", 400, false), // Regular  ← expected
            fm("X", 400, true),  // Italic
            fm("X", 700, false), // Bold
        ];
        assert_eq!(pick_regular_index(&metas), Some(1));
    }

    #[test]
    fn pick_regular_breaks_weight_ties_toward_upright() {
        let metas = vec![fm("X", 400, true), fm("X", 400, false)];
        assert_eq!(
            pick_regular_index(&metas),
            Some(1),
            "upright wins at equal weight distance"
        );
    }

    // Width tie-break: a family that ships width variants under one typographic
    // name (e.g. Inconsolata's Expanded/UltraExpanded at weight 400 upright) must
    // resolve to the NORMAL-width face, not a width variant.
    #[test]
    fn pick_regular_prefers_normal_width_over_width_variants() {
        let metas = vec![
            fm_width("Inconsolata", 400, 9, false), // UltraExpanded
            fm_width("Inconsolata", 400, 3, false), // Condensed
            fm_width("Inconsolata", 400, 5, false), // Normal  ← expected
        ];
        assert_eq!(pick_regular_index(&metas), Some(2));
    }

    // Variant discovery by metadata: bold = heavy upright, italic = light
    // italic, bold-italic = heavy italic; a missing variant yields None.
    #[test]
    fn pick_variant_selects_faces_by_metadata() {
        let faces = vec![
            (PathBuf::from("/f/reg"), fm("X", 400, false)),
            (PathBuf::from("/f/bold"), fm("X", 700, false)),
            (PathBuf::from("/f/italic"), fm("X", 400, true)),
            (PathBuf::from("/f/bolditalic"), fm("X", 700, true)),
        ];
        assert_eq!(
            pick_variant(&faces, true, false),
            Some(PathBuf::from("/f/bold"))
        );
        assert_eq!(
            pick_variant(&faces, false, true),
            Some(PathBuf::from("/f/italic"))
        );
        assert_eq!(
            pick_variant(&faces, true, true),
            Some(PathBuf::from("/f/bolditalic"))
        );
        let only_upright = vec![
            (PathBuf::from("/f/reg"), fm("X", 400, false)),
            (PathBuf::from("/f/bold"), fm("X", 700, false)),
        ];
        assert_eq!(
            pick_variant(&only_upright, false, true),
            None,
            "no italic face ⇒ None"
        );
    }

    // font_families over the real host dirs: sorted, no empties, no
    // case-insensitive duplicates (trap a/c on real metadata).
    #[test]
    fn font_families_lists_real_names_without_variant_duplicates() {
        let families = font_families_in_dirs(&font_search_dirs());
        if families.is_empty() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        let mut sorted = families.clone();
        sorted.sort_by_key(|name| name.to_lowercase());
        assert_eq!(families, sorted, "families are sorted case-insensitively");
        assert!(
            families.iter().all(|f| !f.trim().is_empty()),
            "no empty family names"
        );
        let mut keys: Vec<String> = families.iter().map(|f| f.to_lowercase()).collect();
        let before = keys.len();
        keys.dedup();
        assert_eq!(
            keys.len(),
            before,
            "no case-insensitive duplicate families (sorted ⇒ consecutive)"
        );
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
