// SPDX-License-Identifier: GPL-3.0-only
//! Shared helpers for complete-palette parsers.

use std::collections::BTreeMap;

use crate::theme::{Srgb, parse_hex};

use super::{ExternalPaletteError, NormalizedExternalPalette};
use crate::external_palette::{MAX_EXTERNAL_PALETTE_ENTRIES, MAX_EXTERNAL_PALETTE_LINES};

pub(super) fn require_color(
    map: &BTreeMap<String, Srgb>,
    key: &str,
) -> Result<Srgb, ExternalPaletteError> {
    map.get(key)
        .copied()
        .ok_or_else(|| ExternalPaletteError::Incomplete(format!("missing required color {key:?}")))
}

pub(super) fn require_palette(
    map: &BTreeMap<String, Srgb>,
) -> Result<[Srgb; 16], ExternalPaletteError> {
    let mut palette = [(0u8, 0u8, 0u8); 16];
    for (index, slot) in palette.iter_mut().enumerate() {
        let key = format!("color{index}");
        *slot = require_color(map, &key)?;
    }
    Ok(palette)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assemble(
    foreground: Srgb,
    background: Srgb,
    clear: Srgb,
    palette: [Srgb; 16],
    cursor: Srgb,
    selection: Srgb,
    search: Srgb,
    border: Srgb,
    inactive: Srgb,
) -> NormalizedExternalPalette {
    NormalizedExternalPalette {
        foreground,
        background,
        clear,
        palette,
        cursor,
        selection,
        search,
        border,
        inactive,
    }
}

/// Parse line-oriented `key = value` color maps (theme / colors.toml style).
/// Values may be bare `#RRGGBB` or quoted. Unknown non-color keys are ignored
/// only after the complete required set is collected by the caller.
pub(super) fn parse_flat_color_map(
    text: &str,
) -> Result<BTreeMap<String, Srgb>, ExternalPaletteError> {
    let mut map = BTreeMap::new();
    let mut lines = 0usize;
    for (line_index, line) in text.lines().enumerate() {
        lines += 1;
        if lines > MAX_EXTERNAL_PALETTE_LINES {
            return Err(ExternalPaletteError::TooManyLines);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let Some((key_raw, value_raw)) = trimmed.split_once('=') else {
            return Err(ExternalPaletteError::Malformed(format!(
                "line {}: expected key = value",
                line_index + 1
            )));
        };
        let key = normalize_flat_key(key_raw);
        if key.is_empty() {
            return Err(ExternalPaletteError::Malformed(format!(
                "line {}: empty key",
                line_index + 1
            )));
        }
        let value = strip_quotes(value_raw.trim());
        if value.is_empty() {
            continue;
        }
        let Some(color) = parse_hex(value) else {
            // Non-color values (booleans, strings) are ignored for color maps.
            continue;
        };
        if map.len() >= MAX_EXTERNAL_PALETTE_ENTRIES && !map.contains_key(&key) {
            return Err(ExternalPaletteError::TooManyEntries);
        }
        map.insert(key, color);
    }
    Ok(map)
}

fn normalize_flat_key(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn strip_quotes(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return &value[1..value.len() - 1];
        }
    }
    // Strip trailing inline comments only when `#` is preceded by whitespace so
    // bare `#RRGGBB` color values are preserved.
    if let Some(index) = value.find('#')
        && index > 0
        && value[..index]
            .chars()
            .last()
            .is_some_and(|c| c.is_whitespace())
    {
        return value[..index].trim();
    }
    value
}
