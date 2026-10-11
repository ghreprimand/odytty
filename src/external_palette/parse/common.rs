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
/// Values may be bare `#RRGGBB` or quoted. A key the providers read as a color
/// must hold one, and may appear once; any other key is metadata and its
/// non-color values are ignored. Callers require the complete set of color keys.
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
        let known = is_known_color_key(&key);
        let value = strip_quotes(value_raw.trim());
        let color = if value.is_empty() {
            None
        } else {
            parse_hex(value)
        };
        let Some(color) = color else {
            // A key a provider reads must hold a color: a malformed value is
            // refused instead of leaving an earlier or fallback value in its
            // place. Other keys are metadata (booleans, strings) and ignored.
            if known {
                return Err(ExternalPaletteError::Malformed(format!(
                    "line {}: {key} needs a color value",
                    line_index + 1
                )));
            }
            continue;
        };
        if known && map.contains_key(&key) {
            return Err(ExternalPaletteError::Malformed(format!(
                "line {}: duplicate color key {key}",
                line_index + 1
            )));
        }
        if map.len() >= MAX_EXTERNAL_PALETTE_ENTRIES && !map.contains_key(&key) {
            return Err(ExternalPaletteError::TooManyEntries);
        }
        map.insert(key, color);
    }
    Ok(map)
}

/// Lowercase a flat key and drop its documented separators (`_` and `-`, so
/// `bright_red` and `bright-red` name `brightred`). A key with any other
/// character is metadata: it keeps its lowercase spelling and so can never
/// collide with a key a provider reads.
fn normalize_flat_key(raw: &str) -> String {
    let key = raw.trim();
    if key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    } else {
        key.to_lowercase()
    }
}

/// The normalized keys the flat providers read as colors.
fn is_known_color_key(key: &str) -> bool {
    const NAMED: &[&str] = &[
        "foreground",
        "background",
        "clear",
        "cursor",
        "selection",
        "search",
        "border",
        "inactive",
        "muted",
        "darkerbackground",
        "darkforeground",
        "brightforeground",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "brightred",
        "brightgreen",
        "brightyellow",
        "brightblue",
        "brightmagenta",
        "brightcyan",
    ];
    if NAMED.contains(&key) {
        return true;
    }
    let indexed = |prefix: &str, radix: u32, width: Option<usize>, max: u32| {
        key.strip_prefix(prefix).is_some_and(|digits| {
            !digits.is_empty()
                && width.is_none_or(|width| digits.len() == width)
                && digits.chars().all(|c| c.is_digit(radix))
                && u32::from_str_radix(digits, radix).is_ok_and(|index| index <= max)
                && (width.is_some() || digits == "0" || !digits.starts_with('0'))
        })
    };
    indexed("color", 10, None, 15) || indexed("base", 16, Some(2), 15)
}

/// Keep hashes inside colours; only whitespace-separated suffixes are comments.
fn strip_quotes(value: &str) -> &str {
    let value = value.trim();
    if let Some(quote @ (b'"' | b'\'')) = value.as_bytes().first().copied() {
        let Some(end) = value[1..].find(char::from(quote)).map(|index| index + 1) else {
            return value;
        };
        let suffix = &value[end + 1..];
        if suffix.is_empty() || is_comment_suffix(suffix) {
            return &value[1..end];
        }
        return value;
    }
    if let Some((end, _)) = value.char_indices().find(|(_, c)| c.is_whitespace())
        && is_comment_suffix(&value[end..])
    {
        return &value[..end];
    }
    value
}

fn is_comment_suffix(suffix: &str) -> bool {
    suffix.starts_with(char::is_whitespace) && suffix.trim_start().starts_with('#')
}
