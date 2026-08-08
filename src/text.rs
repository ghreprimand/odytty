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

/// Bundled **legacy v2** symbols face (Nerd Fonts 2.3.3). Shipped alongside the
/// v3 face above so the glyph pack covers *both* codepoint eras out of the box:
/// Nerd Fonts v3 relocated thousands of PUA icons (Material Design, Weather,
/// Font Awesome, …) and emptied their v2 slots, but real-world shell configs
/// still emit the v2 codepoints (e.g. the archway `U+F557` and python `U+F81F`).
/// The v2 face fills exactly those gaps in the symbol-fallback chain.
pub const BUNDLED_SYMBOL_FONT_V2_FILENAME: &str = "SymbolsNerdFontMono-v2-Regular.ttf";
pub const BUNDLED_SYMBOL_FONT_V2_RELATIVE_PATH: &str =
    "assets/fonts/nerd-fonts-symbols-v2/SymbolsNerdFontMono-v2-Regular.ttf";

#[cfg(feature = "bundled-symbols-font")]
const BUNDLED_SYMBOL_FONT_BYTES: &[u8] =
    include_bytes!("../assets/fonts/nerd-fonts-symbols/SymbolsNerdFontMono-Regular.ttf");

#[cfg(feature = "bundled-symbols-font")]
const BUNDLED_SYMBOL_FONT_V2_BYTES: &[u8] =
    include_bytes!("../assets/fonts/nerd-fonts-symbols-v2/SymbolsNerdFontMono-v2-Regular.ttf");

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

/// The embedded bytes of the bundled face `(family, weight, italic)` resolves
/// to, or `None`. Test-only: lets a regression assert that two style/weight
/// selections load **genuinely distinct** faces (different embedded files), not
/// the same face collapsed via a fallback, without decoding font tables.
#[cfg(test)]
fn bundled_face_bytes(family: &str, weight: &str, italic: bool) -> Option<&'static [u8]> {
    let family = bundled_family_for(family);
    BUNDLED_FACES
        .iter()
        .find(|face| face.family == family && face.weight == weight && face.italic == italic)
        .map(|face| face.bytes)
}

/// Load the bundled symbols-only Nerd Font face (v3) when the asset feature is
/// enabled. Default builds enable it so the RV6 PUA-icon fallback works without
/// host Nerd Font installation; `--no-default-features` leaves this as `None`.
pub fn resolve_bundled_symbol_font() -> Option<FontVec> {
    #[cfg(feature = "bundled-symbols-font")]
    {
        FontVec::try_from_vec(BUNDLED_SYMBOL_FONT_BYTES.to_vec())
            .map_err(|source| TextError::Parse {
                path: format!("bundled {}", BUNDLED_SYMBOL_FONT_FILENAME),
                source,
            })
            .ok()
    }

    #[cfg(not(feature = "bundled-symbols-font"))]
    {
        None
    }
}

/// Load the bundled **legacy v2** symbols face (Nerd Fonts 2.3.3) when the asset
/// feature is enabled. Paired with [`resolve_bundled_symbol_font`] (v3) in the
/// fallback chain so the v2 codepoints v3 relocated still resolve out of the box.
pub fn resolve_bundled_symbol_font_v2() -> Option<FontVec> {
    #[cfg(feature = "bundled-symbols-font")]
    {
        FontVec::try_from_vec(BUNDLED_SYMBOL_FONT_V2_BYTES.to_vec())
            .map_err(|source| TextError::Parse {
                path: format!("bundled {}", BUNDLED_SYMBOL_FONT_V2_FILENAME),
                source,
            })
            .ok()
    }

    #[cfg(not(feature = "bundled-symbols-font"))]
    {
        None
    }
}

/// The bundled symbol faces in chain order: **v3 first, then v2**. v3 carries
/// the current Nerd Fonts layout (and the bulk of modern config icons); v2 fills
/// only the slots v3 emptied. Empty when the `bundled-symbols-font` feature is
/// off. Order matters: a codepoint present in both resolves to its v3 rendition.
pub fn resolve_bundled_symbol_fonts() -> Vec<FontVec> {
    let mut fonts = Vec::new();
    if let Some(v3) = resolve_bundled_symbol_font() {
        fonts.push(v3);
    }
    if let Some(v2) = resolve_bundled_symbol_font_v2() {
        fonts.push(v2);
    }
    fonts
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

/// Standard platform font search roots, plus per-user font dirs when available.
/// Only existing directories are returned. Used by the settings layer to
/// resolve `ODYTTY_FONT_FAMILY`; tests pass explicit dirs instead for
/// hermeticity.
pub fn font_search_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let mut dirs = vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
    ];
    #[cfg(windows)]
    let mut dirs = {
        let mut dirs = Vec::new();
        if let Some(windir) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(windir).join("Fonts"));
        }
        if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local_appdata)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
        dirs
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        dirs.push(home.join("Library/Fonts"));
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join(".fonts"));
        }
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

/// The two font families OdyTTY bundles and can always load from compiled-in
/// bytes, regardless of host installation: the default (Victor Mono) first,
/// then JetBrains Mono. Both are always selectable in the picker's **Bundled
/// Fonts** group.
pub const BUNDLED_FONT_FAMILIES: [&str; 2] = [BUNDLED_FONT_FAMILY, JETBRAINS_FONT_FAMILY];

/// Font families split into picker subgroups: **bundled** (always present,
/// loaded from compiled-in bytes) and **system** (host monospace families read
/// from [`font_search_dirs`]). A host copy of a bundled family is dropped from
/// `system` so it is not listed twice — picking the bundled entry always
/// resolves the version-pinned shipped face.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontFamilyGroups {
    /// Bundled families, in ship order (default first). Always non-empty.
    pub bundled: Vec<String>,
    /// Host monospace families, sorted, excluding any bundled family name.
    pub system: Vec<String>,
}

/// Build the picker's grouped family inventory from the live host search dirs.
/// See [`font_families_grouped_in_dirs`] for the hermetic, dir-scoped core.
pub fn font_families_grouped() -> FontFamilyGroups {
    font_families_grouped_in_dirs(&font_search_dirs())
}

/// [`font_families_grouped`] over an explicit directory set, for hermetic tests.
///
/// The bundled group is fixed ([`BUNDLED_FONT_FAMILIES`]); the system group is
/// the distinct host monospace families under `dirs` with any bundled family
/// removed (case-insensitive), so a host-installed copy of Victor / JetBrains
/// Mono never double-lists — the bundled entry already covers it.
pub fn font_families_grouped_in_dirs(dirs: &[PathBuf]) -> FontFamilyGroups {
    let bundled: Vec<String> = BUNDLED_FONT_FAMILIES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let bundled_keys: Vec<String> = bundled.iter().map(|f| normalize_family(f)).collect();
    let system = font_families_in_dirs(dirs)
        .into_iter()
        .filter(|family| {
            let key = normalize_family(family);
            !bundled_keys.contains(&key)
        })
        .collect();
    FontFamilyGroups { bundled, system }
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

/// macOS system faces appended to the tail of the symbol-fallback chain. The
/// bundled and host Nerd faces patch the Private Use Area but cover only a
/// sparse subset of the *standard* Unicode symbol/dingbat/pictograph blocks, so
/// glyphs TUIs emit outside the PUA — the teardrop-asterisk spinner `U+273B`,
/// the `U+2733`/`U+2736`/`U+2737` star asterisks, the `U+2713`/`U+2717` check
/// and ballot marks, the `U+23BF` result-branch — fall through to the hollow-box
/// tofu slot. Menlo (the system monospace) covers the dingbats/marks; Apple
/// Symbols covers Miscellaneous Technical glyphs like `U+23BF`. STIX Two Math
/// backstops the rest — it is the only commonly-present face with *monochrome*
/// (SGR-colorable, unlike the color-emoji face) glyphs for the record bullet
/// `U+23FA` and the large squares `U+2B1B`/`U+2B1C` that drive a TUI's status
/// markers and block grids. They sit *after* the Nerd faces so PUA icons still
/// resolve from the pinned faces first, and their Latin glyphs never shadow the
/// body font because glyph fallback is only consulted after the primary face
/// misses a printable spacing codepoint. Broadest coverage first; each is
/// skipped silently if absent.
#[cfg(target_os = "macos")]
const SYSTEM_SYMBOL_FALLBACK_FONTS: &[&str] = &[
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
];

/// Linux/Unix (non-macOS) symbol-fallback tail: normalized filename-stem hints
/// for the broad-coverage system symbol faces that commonly backfill standard
/// Unicode dingbats/symbols/pictographs the bundled Nerd faces lack. Unlike the
/// macOS arm (fixed absolute paths) Linux font locations vary by distro, so
/// these are matched against [`normalize_family`]-style stems of files under
/// [`font_search_dirs`] and appended to the chain when present (skipped silently
/// if absent, same effect as the macOS list). This is the deterministic *floor*:
/// it covers hosts that ship Noto Symbols / Symbola / DejaVu, but cannot promise
/// coverage of arbitrary printable codepoints on hosts that ship none of them --
/// the runtime [`runtime_resolve_symbol_font`] query is the actual backfill
/// there. Broadest coverage first; the appended faces never shadow the body font
/// because glyph fallback is only consulted after the primary face misses a
/// printable spacing codepoint.
#[cfg(all(unix, not(target_os = "macos")))]
const LINUX_SYMBOL_FALLBACK_HINTS: &[&str] = &[
    "notosanssymbols2",
    "notosanssymbols",
    "symbola",
    "dejavusans",
    "unifont",
];

/// Resolve the Linux/Unix system symbol-fallback tail (see
/// [`LINUX_SYMBOL_FALLBACK_HINTS`]): for each hint, in priority order, the first
/// file under `dirs` whose normalized stem contains it, loaded and de-duplicated
/// by path. Returns `(source, font)` pairs index-aligned with how the chain is
/// built. Absent faces are skipped silently.
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_symbol_fallback_faces(dirs: &[PathBuf]) -> Vec<(SymbolFontSource, FontVec)> {
    let files = collect_font_files(dirs);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for hint in LINUX_SYMBOL_FALLBACK_HINTS {
        if let Some(path) = files
            .iter()
            .find(|f| normalize_family(&file_stem(f)).contains(hint))
            && seen.insert(path.clone())
            && let Ok(font) = load_font_at(path)
        {
            out.push((SymbolFontSource::Host(path.clone()), font));
        }
    }
    out
}

/// Windows symbol-fallback tail: normalized **filename-stem** hints for the
/// always-present system faces that cover the standard Unicode
/// dingbats/symbols/Miscellaneous-Technical glyphs the bundled icon-only Nerd
/// faces lack (e.g. the result-branch `U+23BF` and the check `U+2714` that
/// Claude Code and other TUIs emit). Windows has no cheap per-codepoint runtime
/// resolver analogous to Linux's `fc-match`, so — like the macOS arm — this
/// static tail is the deterministic *floor*.
///
/// These are matched (like the Linux arm) against [`normalize_family`]-style
/// stems of files under [`font_search_dirs`]' Windows roots (`WINDIR\Fonts` +
/// per-user LOCALAPPDATA fonts), so the hints are the on-disk *filenames*, not
/// the OpenType family names. `seguisym` (`seguisym.ttf`, Segoe UI Symbol,
/// shipped since Windows 7) is broadest-first: it covers Arrows, Miscellaneous
/// Technical, Geometric Shapes, Miscellaneous Symbols and Dingbats — including
/// both reported codepoints. `segmdl2` (`segmdl2.ttf`, the MDL2 assets icon
/// face) and `cambria` (`cambria.ttc`, Cambria / Cambria Math) backstop any
/// Segoe UI Symbol gaps with monochrome outlines. Each is skipped silently if
/// absent, and none shadows the body font because glyph fallback is only
/// consulted after the primary face misses a printable spacing codepoint.
#[cfg(windows)]
const WINDOWS_SYMBOL_FALLBACK_HINTS: &[&str] = &["seguisym", "segmdl2", "cambria"];

/// Resolve the Windows system symbol-fallback tail (see
/// [`WINDOWS_SYMBOL_FALLBACK_HINTS`]): for each hint, in priority order, the
/// first file under `dirs` whose normalized stem contains it, loaded and
/// de-duplicated by path. Returns `(source, font)` pairs index-aligned with how
/// the chain is built. Absent faces are skipped silently. Mirrors
/// [`linux_symbol_fallback_faces`].
#[cfg(windows)]
fn windows_symbol_fallback_faces(dirs: &[PathBuf]) -> Vec<(SymbolFontSource, FontVec)> {
    let files = collect_font_files(dirs);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for hint in WINDOWS_SYMBOL_FALLBACK_HINTS {
        if let Some(path) = files
            .iter()
            .find(|f| normalize_family(&file_stem(f)).contains(hint))
            && seen.insert(path.clone())
            && let Ok(font) = load_font_at(path)
        {
            out.push((SymbolFontSource::Host(path.clone()), font));
        }
    }
    out
}

/// Resolve a symbol / Nerd-font face for the RV6 PUA-icon fallback, or `None`
/// when neither the bundled asset nor the host can provide one.
///
/// Resolution order (precedence: **explicit > bundled > host**):
/// 1. An explicit [`SYMBOL_FONT_ENV`] path (loaded directly; a bad path yields
///    fallback resolution rather than aborting).
/// 2. The bundled symbols-only face, when the default `bundled-symbols-font`
///    feature is enabled. This is the reliable, version-pinned default so the
///    out-of-the-box icon path never depends on which fonts the host happens to
///    have installed.
/// 3. The first file under [`font_search_dirs`] whose normalized stem contains
///    a [`SYMBOL_FONT_HINTS`] fragment -- only reached when the bundled asset is
///    absent (e.g. `--no-default-features`).
///
/// The font is only *loaded*; whether it is *used* is the caller's gate (the
/// native layer reads its enable switch before installing it on the atlas).
pub fn resolve_symbol_font() -> Option<FontVec> {
    let explicit = std::env::var_os(SYMBOL_FONT_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    resolve_symbol_font_with_source(explicit.as_deref(), &font_search_dirs()).1
}

/// Where the RV6 symbol / Nerd-font fallback face resolved from, for
/// diagnostics (`--show-config`). Carries the concrete path for the explicit
/// and host cases so operators can see exactly which file is in play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolFontSource {
    /// No fallback face is available (the bundled asset is absent and neither
    /// an explicit nor a host face resolved).
    None,
    /// An explicit user-named file (`ODYTTY_SYMBOL_FONT` env or the
    /// `symbol_font` setting).
    Explicit(PathBuf),
    /// The bundled symbols-only face shipped with odytty (version-pinned).
    Bundled,
    /// A host-discovered "* Nerd Font" face (only when no bundled asset).
    Host(PathBuf),
}

impl SymbolFontSource {
    /// Stable, script-friendly description for `--show-config`:
    /// `none`, `explicit:<path>`, `bundled`, or `host:<path>`.
    pub fn describe(&self) -> String {
        match self {
            SymbolFontSource::None => "none".to_owned(),
            SymbolFontSource::Explicit(path) => format!("explicit:{}", path.display()),
            SymbolFontSource::Bundled => "bundled".to_owned(),
            SymbolFontSource::Host(path) => format!("host:{}", path.display()),
        }
    }
}

/// Resolve the symbol / Nerd-font fallback face and report **where** it came
/// from, under the precedence **explicit > bundled > host**.
///
/// This is the single source of truth for symbol-fallback resolution: the
/// native renderer uses the loaded `FontVec`, and `--show-config` uses the
/// [`SymbolFontSource`] for diagnostics, so the reported source can never drift
/// from what the renderer actually installs.
///
/// `explicit_path` is the user's explicit override (`ODYTTY_SYMBOL_FONT` or the
/// `symbol_font` setting); a path that fails to load is reported via `eprintln!`
/// and resolution falls through to the bundled/host search rather than aborting.
pub fn resolve_symbol_font_with_source(
    explicit_path: Option<&Path>,
    dirs: &[PathBuf],
) -> (SymbolFontSource, Option<FontVec>) {
    if let Some(path) = explicit_path {
        match load_font_at(path) {
            Ok(font) => return (SymbolFontSource::Explicit(path.to_path_buf()), Some(font)),
            Err(err) => {
                eprintln!("odytty: {err}; falling back to the bundled symbol font");
            }
        }
    }
    // Bundled before host: the shipped face is known-good and version-pinned, so
    // the out-of-the-box icon path is identical on every machine regardless of
    // which Nerd fonts the host has installed.
    if let Some(font) = resolve_bundled_symbol_font() {
        return (SymbolFontSource::Bundled, Some(font));
    }
    // Last resort (bundled asset absent, e.g. `--no-default-features`): a
    // host-discovered symbol/Nerd face.
    if let Some(path) = resolve_symbol_font_path_in(dirs)
        && let Ok(font) = load_font_at(&path)
    {
        return (SymbolFontSource::Host(path), Some(font));
    }
    (SymbolFontSource::None, None)
}

/// Resolve the **ordered symbol-fallback chain** and report where each face came
/// from. This is the coverage-composing counterpart to
/// [`resolve_symbol_font_with_source`] (which returns only the single best
/// face): the atlas walks the chain per glyph and rasterizes from the first face
/// that actually has the codepoint, so coverage is the *union* of every face.
///
/// Chain order (precedence **explicit > bundled > host**):
/// 1. An explicit [`SYMBOL_FONT_ENV`] / `symbol_font` override (a bad path is
///    reported and skipped rather than aborting).
/// 2. The bundled faces — v3 then v2 (see [`resolve_bundled_symbol_fonts`]) —
///    so the out-of-the-box glyph pack covers both Nerd Font codepoint eras
///    regardless of host font installation.
/// 3. A host-discovered "* Nerd Font" face, which can extend coverage with any
///    extra glyphs the bundled faces lack.
///
/// The returned `sources` and `fonts` are index-aligned. An empty `fonts`
/// (every source failed and no bundled asset) keeps the historical hollow-box
/// behavior.
pub fn resolve_symbol_fonts_with_source(
    explicit_path: Option<&Path>,
    dirs: &[PathBuf],
) -> (Vec<SymbolFontSource>, Vec<FontVec>) {
    let mut sources = Vec::new();
    let mut fonts = Vec::new();

    if let Some(path) = explicit_path {
        match load_font_at(path) {
            Ok(font) => {
                sources.push(SymbolFontSource::Explicit(path.to_path_buf()));
                fonts.push(font);
            }
            Err(err) => {
                eprintln!("odytty: {err}; falling back to the bundled symbol fonts");
            }
        }
    }

    // Bundled faces (v3 then v2): the known-good, version-pinned core of the
    // chain, identical on every machine.
    for font in resolve_bundled_symbol_fonts() {
        sources.push(SymbolFontSource::Bundled);
        fonts.push(font);
    }

    // Host-discovered symbol/Nerd face: extends coverage for any glyph the
    // bundled faces lack, and is the sole source under `--no-default-features`.
    if let Some(path) = resolve_symbol_font_path_in(dirs)
        && let Ok(font) = load_font_at(&path)
    {
        sources.push(SymbolFontSource::Host(path));
        fonts.push(font);
    }

    // macOS: the Nerd faces above cover the PUA icon ranges but lack most
    // standard Unicode dingbats/symbols/pictographs that TUIs emit. Append the
    // always-present system faces that DO cover them (see
    // [`SYSTEM_SYMBOL_FALLBACK_FONTS`]) so they render instead of tofu.
    #[cfg(target_os = "macos")]
    for path in SYSTEM_SYMBOL_FALLBACK_FONTS {
        let path = Path::new(path);
        if let Ok(font) = load_font_at(path) {
            sources.push(SymbolFontSource::Host(path.to_path_buf()));
            fonts.push(font);
        }
    }

    // Linux/Unix: the static system symbol tail (Noto Symbols / Symbola / DejaVu
    // / Unifont, when installed). This is the deterministic floor; codepoints no
    // installed face covers are backfilled at render time by the cached
    // [`runtime_resolve_symbol_font`] query the atlas calls on a static miss.
    #[cfg(all(unix, not(target_os = "macos")))]
    for (source, font) in linux_symbol_fallback_faces(dirs) {
        sources.push(source);
        fonts.push(font);
    }

    // Windows: the bundled Nerd faces are icon-only (PUA) and lack standard
    // Unicode dingbats/symbols/Miscellaneous-Technical glyphs TUIs emit. Append
    // the always-present Segoe UI Symbol tail (see
    // [`WINDOWS_SYMBOL_FALLBACK_HINTS`]) so glyphs like the check `U+2714` and
    // the result-branch `U+23BF` render instead of tofu. Static floor only —
    // Windows has no cheap `fc-match` runtime-resolver analog.
    #[cfg(windows)]
    for (source, font) in windows_symbol_fallback_faces(dirs) {
        sources.push(source);
        fonts.push(font);
    }

    (sources, fonts)
}

/// Whether `font` provides a usable **monochrome outline** for `ch`: it has the
/// codepoint in its cmap (`glyph_id != 0`) and an inked vector outline. This is
/// the symbol-fallback face filter: color/bitmap-only faces and blank
/// placeholder outlines both render nothing useful in the coverage atlas, so
/// they must not block a later fallback face.
pub fn font_provides_outline_glyph(font: &FontVec, ch: char) -> bool {
    let id = font.glyph_id(ch);
    id.0 != 0
        && font.outline(id).is_some_and(|outline| {
            !outline.curves.is_empty()
                && outline.bounds.min.x != outline.bounds.max.x
                && outline.bounds.min.y != outline.bounds.max.y
        })
}

/// Runtime per-codepoint glyph fallback via fontconfig (RV6 Linux backfill).
///
/// Invoked by the glyph atlas **only** when a printable spacing codepoint misses
/// the static fallback chain (and the result is cached per-codepoint by the
/// atlas, so this shells out at most once per distinct missing codepoint --
/// never on the hot path repeatedly). It runs
/// `fc-match -f %{file} :charset=<hex>` to ask fontconfig for a host face that
/// covers the codepoint, then loads it and rejects color/bitmap-only faces via
/// [`font_provides_outline_glyph`] so only a monochrome outline face is
/// installed. Read-only, local-only subprocess (no network, no user data),
/// mirroring the emoji discovery path's `fc-match` use. Returns `None` when
/// fontconfig is absent (e.g. headless CI), when no face covers the codepoint,
/// or when the only match is color/bitmap-only -- all of which preserve the
/// historical hollow-box behavior.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn runtime_resolve_symbol_font(ch: char) -> Option<std::sync::Arc<FontVec>> {
    let charset = format!(":charset={:x}", ch as u32);
    let output = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", &charset])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() || !path.is_file() {
        return None;
    }
    let font = load_font_at(&path).ok()?;
    if !font_provides_outline_glyph(&font, ch) {
        return None;
    }
    Some(std::sync::Arc::new(font))
}

/// Resolve only the **source** of the symbol-fallback face. Convenience wrapper
/// over [`resolve_symbol_font_with_source`] for `--show-config`, which needs the
/// label but not the rasterizable `FontVec`.
pub fn resolve_symbol_font_source(
    explicit_path: Option<&Path>,
    dirs: &[PathBuf],
) -> SymbolFontSource {
    resolve_symbol_font_with_source(explicit_path, dirs).0
}

/// Family-search half of [`resolve_symbol_font`], factored out so tests can
/// pass a hermetic fixture directory. Prefers the dedicated "Symbols Nerd Font"
/// face (hint index 0) over a general patched "* Nerd Font" face.
pub fn resolve_symbol_font_in(dirs: &[PathBuf]) -> Option<FontVec> {
    resolve_symbol_font_path_in(dirs).and_then(|path| load_font_at(&path).ok())
}

/// The path of the best host-discovered symbol / Nerd font under `dirs`, or
/// `None`. The path-returning core of [`resolve_symbol_font_in`]: prefers the
/// dedicated "Symbols Nerd Font" face (hint index 0) over a general patched
/// "* Nerd Font" face. Exposed so [`resolve_symbol_font_with_source`] can label
/// the resolved host file without re-scanning.
pub fn resolve_symbol_font_path_in(dirs: &[PathBuf]) -> Option<PathBuf> {
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
    best.map(|(_, path)| path)
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

/// Test-only snapshot of every process-global color seam in this module.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorGlobals {
    pub(crate) default_fg: (u8, u8, u8),
    pub(crate) default_bg: (u8, u8, u8),
    pub(crate) ansi_palette: [(u8, u8, u8); 16],
}

/// Capture the process-global color seams so the shared render-globals guard
/// can hold a baseline and write it back verbatim. Restoration goes through the
/// public [`set_default_colors`] and [`set_ansi_palette`] setters, so there is
/// no second write path to keep in step with this reader.
#[cfg(test)]
pub(crate) fn color_globals_for_test() -> ColorGlobals {
    let mut ansi_palette = [(0u8, 0u8, 0u8); 16];
    for (index, slot) in ansi_palette.iter_mut().enumerate() {
        *slot = ansi_srgb(index as u8);
    }
    ColorGlobals {
        default_fg: default_fg_srgb(),
        default_bg: default_bg_srgb(),
        ansi_palette,
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

/// TEXT-BRIGHTNESS: lift a linear-RGBA glyph foreground toward white with a
/// soft knee, preserving alpha.
///
/// For in-gamut channels, `c' = 1 - (1 - c)^b` for `b >= 1.0`: identity at
/// `b == 1.0` (early-returned, exact — the plain path stays byte-identical),
/// monotonic in both the channel and the knob, and `c' < 1` whenever `c < 1`,
/// so near-white ink compresses smoothly instead of clipping flat and channel
/// ordering is preserved — colors lighten without fully desaturating. Black is
/// a fixed point: the curve lifts mid-tones and dim colors, not `#000` ink,
/// which would only lose contrast on light backgrounds.
///
/// Out-of-gamut channels are preserved exactly. The minimum-contrast floor can
/// produce values above `1.0`; those carry useful energy in the float scene
/// target used by bloom/CRT. Clamping them only when brightness is enabled
/// would make the raised setting darker than the identity path. Applied by the
/// vertex build AFTER [`enforce_contrast_rgba`], so a floor-corrected color is
/// the lift's input and the ramp cannot undo the floor's direction of correction.
pub fn lift_brightness_rgba(color: [f32; 4], brightness: f32) -> [f32; 4] {
    if brightness <= 1.0 {
        return color;
    }
    let lift = |c: f32| {
        if !(0.0..=1.0).contains(&c) {
            return c;
        }
        1.0 - (1.0 - c).powf(brightness)
    };
    [lift(color[0]), lift(color[1]), lift(color[2]), color[3]]
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
mod tests;
