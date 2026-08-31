// SPDX-License-Identifier: GPL-3.0-only
//! Complete-palette parsers for each supported source format.

mod colors_json;
mod colors_toml;
mod common;
mod odytty;

use crate::theme::{Srgb, Theme, ThemeSpec};

pub use colors_json::parse_colors_json;
pub use colors_toml::parse_colors_toml;
pub use odytty::parse_odytty_or_base16;

/// Selected local palette source format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExternalPaletteProvider {
    /// Explicit OdyTTY theme-format file (complete color keys required). Also
    /// accepts Base16 `base00`..`base0F` aliases with the documented projection.
    #[default]
    OdyttyAnsi,
    /// Omarchy-compatible flat `colors.toml` (independent compatibility).
    ColorsToml,
    /// pywal-compatible `colors.json` (independent compatibility).
    ColorsJson,
}

impl ExternalPaletteProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OdyttyAnsi => "odytty",
            Self::ColorsToml => "colors_toml",
            Self::ColorsJson => "colors_json",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let key: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        match key.as_str() {
            "odytty" | "odyttyansi" | "ansi" | "theme" | "base16" => Some(Self::OdyttyAnsi),
            "colorstoml" | "toml" | "omarchy" | "omarchycompat" => Some(Self::ColorsToml),
            "colorsjson" | "json" | "pywal" | "pywalcompat" | "wal" => Some(Self::ColorsJson),
            _ => None,
        }
    }
}

/// Fail-closed parse / validation error. Never partial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalPaletteError {
    Empty,
    Oversized,
    TooManyLines,
    TooManyEntries,
    Malformed(String),
    Incomplete(String),
    Unsupported(String),
}

impl std::fmt::Display for ExternalPaletteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "external palette file is empty"),
            Self::Oversized => write!(f, "external palette file exceeds size limit"),
            Self::TooManyLines => write!(f, "external palette file exceeds line limit"),
            Self::TooManyEntries => write!(f, "external palette file exceeds entry limit"),
            Self::Malformed(message) => write!(f, "malformed external palette: {message}"),
            Self::Incomplete(message) => write!(f, "incomplete external palette: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported external palette: {message}"),
        }
    }
}

impl std::error::Error for ExternalPaletteError {}

/// A complete, provider-neutral palette ready to project into [`Theme`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedExternalPalette {
    pub foreground: Srgb,
    pub background: Srgb,
    pub clear: Srgb,
    pub palette: [Srgb; 16],
    pub cursor: Srgb,
    pub selection: Srgb,
    pub search: Srgb,
    pub border: Srgb,
    pub inactive: Srgb,
}

impl NormalizedExternalPalette {
    /// Project into a runtime [`Theme`] through the existing ThemeSpec seam.
    pub fn to_theme(&self) -> Theme {
        let spec = ThemeSpec {
            name: "custom".to_owned(),
            appearance: crate::theme::Appearance::Dark,
            foreground: self.foreground,
            background: self.background,
            clear: self.clear,
            palette: self.palette,
            cursor: self.cursor,
            selection: self.selection,
            search: self.search,
            border: self.border,
            inactive: self.inactive,
            font_family: None,
            font_size: None,
            visual: crate::theme::VisualEffect::Off,
        };
        spec.to_theme()
    }
}

/// Parse raw file bytes for `provider`. Callers must already have applied the
/// byte-size cap; this still enforces line/entry caps.
pub fn parse_palette_bytes(
    provider: ExternalPaletteProvider,
    bytes: &[u8],
) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    if bytes.is_empty() {
        return Err(ExternalPaletteError::Empty);
    }
    if bytes.len() as u64 > super::MAX_EXTERNAL_PALETTE_BYTES {
        return Err(ExternalPaletteError::Oversized);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ExternalPaletteError::Malformed("file is not valid UTF-8".to_owned()))?;
    match provider {
        ExternalPaletteProvider::OdyttyAnsi => parse_odytty_or_base16(text),
        ExternalPaletteProvider::ColorsToml => parse_colors_toml(text),
        ExternalPaletteProvider::ColorsJson => parse_colors_json(text),
    }
}
