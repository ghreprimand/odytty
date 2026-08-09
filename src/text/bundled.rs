// SPDX-License-Identifier: GPL-3.0-only
//! Bundled font faces and font-file loading.
//!
//! Owns the compiled-in Victor Mono / JetBrains Mono / Symbols Nerd Font
//! tables and every path that turns bytes or a filesystem path into a
//! parsed [`FontVec`]. Startup never depends on host font installation:
//! an unusable override falls back to a bundled face here.

use std::path::{Path, PathBuf};

use ab_glyph::FontVec;

use crate::settings::FONT_ENV;

use super::FontStyle;
use super::discovery::normalize_family;

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
pub(super) const BUNDLED_SYMBOL_FONT_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/nerd-fonts-symbols/SymbolsNerdFontMono-Regular.ttf");

#[cfg(feature = "bundled-symbols-font")]
const BUNDLED_SYMBOL_FONT_V2_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/nerd-fonts-symbols-v2/SymbolsNerdFontMono-v2-Regular.ttf");

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
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-Thin.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Thin",
        italic: true,
        filename: "VictorMono-ThinOblique.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-ThinOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "ExtraLight",
        italic: false,
        filename: "VictorMono-ExtraLight.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-ExtraLight.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "ExtraLight",
        italic: true,
        filename: "VictorMono-ExtraLightOblique.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-ExtraLightOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Light",
        italic: false,
        filename: "VictorMono-Light.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-Light.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Light",
        italic: true,
        filename: "VictorMono-LightOblique.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-LightOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Regular",
        italic: false,
        filename: "VictorMono-Regular.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-Regular.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Regular",
        italic: true,
        filename: "VictorMono-Oblique.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-Oblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Medium",
        italic: false,
        filename: "VictorMono-Medium.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-Medium.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Medium",
        italic: true,
        filename: "VictorMono-MediumOblique.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-MediumOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "SemiBold",
        italic: false,
        filename: "VictorMono-SemiBold.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-SemiBold.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "SemiBold",
        italic: true,
        filename: "VictorMono-SemiBoldOblique.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-SemiBoldOblique.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Bold",
        italic: false,
        filename: "VictorMono-Bold.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-Bold.otf"),
    },
    BundledFace {
        family: "Victor Mono",
        weight: "Bold",
        italic: true,
        filename: "VictorMono-BoldOblique.otf",
        bytes: include_bytes!("../../assets/fonts/victor-mono/VictorMono-BoldOblique.otf"),
    },
    // JetBrains Mono (bundled, selectable). JetBrains has no separate oblique
    // variant, so italic rows use its italic faces.
    BundledFace {
        family: "JetBrains Mono",
        weight: "Thin",
        italic: false,
        filename: "JetBrainsMono-Thin.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Thin.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Thin",
        italic: true,
        filename: "JetBrainsMono-ThinItalic.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-ThinItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraLight",
        italic: false,
        filename: "JetBrainsMono-ExtraLight.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLight.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraLight",
        italic: true,
        filename: "JetBrainsMono-ExtraLightItalic.ttf",
        bytes: include_bytes!(
            "../../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLightItalic.ttf"
        ),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Light",
        italic: false,
        filename: "JetBrainsMono-Light.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Light.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Light",
        italic: true,
        filename: "JetBrainsMono-LightItalic.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-LightItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Regular",
        italic: false,
        filename: "JetBrainsMono-Regular.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Regular",
        italic: true,
        filename: "JetBrainsMono-Italic.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Italic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Medium",
        italic: false,
        filename: "JetBrainsMono-Medium.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Medium.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Medium",
        italic: true,
        filename: "JetBrainsMono-MediumItalic.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-MediumItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "SemiBold",
        italic: false,
        filename: "JetBrainsMono-SemiBold.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBold.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "SemiBold",
        italic: true,
        filename: "JetBrainsMono-SemiBoldItalic.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBoldItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Bold",
        italic: false,
        filename: "JetBrainsMono-Bold.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "Bold",
        italic: true,
        filename: "JetBrainsMono-BoldItalic.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-BoldItalic.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraBold",
        italic: false,
        filename: "JetBrainsMono-ExtraBold.ttf",
        bytes: include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraBold.ttf"),
    },
    BundledFace {
        family: "JetBrains Mono",
        weight: "ExtraBold",
        italic: true,
        filename: "JetBrainsMono-ExtraBoldItalic.ttf",
        bytes: include_bytes!(
            "../../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraBoldItalic.ttf"
        ),
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
pub(super) fn font_candidates() -> Vec<PathBuf> {
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
    let bytes = crate::font_file::read_font_file(path).map_err(|source| TextError::Read {
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
pub(super) fn bundled_face_filename(
    family: &str,
    weight: &str,
    italic: bool,
) -> Option<&'static str> {
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
pub(super) fn bundled_face_bytes(
    family: &str,
    weight: &str,
    italic: bool,
) -> Option<&'static [u8]> {
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
