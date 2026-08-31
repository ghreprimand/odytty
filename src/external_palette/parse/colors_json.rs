// SPDX-License-Identifier: GPL-3.0-only
//! pywal-compatible `colors.json` complete-palette projection.
//!
//! Independent compatibility only. Required source shape (pywal
//! `colors_to_dict`):
//! ```json
//! { "special": { "background", "foreground", "cursor" },
//!   "colors": { "color0" .. "color15" } }
//! ```
//! Documented OdyTTY projection for Theme roles pywal does not name:
//! clear <- special.background; selection/border/inactive <- color8;
//! search <- color3. No built-in theme defaults are mixed in.

use super::common::assemble;
use super::{ExternalPaletteError, NormalizedExternalPalette};
use crate::external_palette::MAX_EXTERNAL_PALETTE_ENTRIES;
use crate::profiles::{Json, parse_json};
use crate::theme::{Srgb, parse_hex};

pub fn parse_colors_json(text: &str) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    let root = parse_json(text)
        .map_err(|error| ExternalPaletteError::Malformed(format!("json: {error}")))?;
    let Json::Obj(entries) = root else {
        return Err(ExternalPaletteError::Malformed(
            "root value must be a JSON object".to_owned(),
        ));
    };
    if entries.len() > MAX_EXTERNAL_PALETTE_ENTRIES {
        return Err(ExternalPaletteError::TooManyEntries);
    }

    let special = object_field(&entries, "special")?;
    let colors = object_field(&entries, "colors")?;

    let background = json_color(special, "background")?;
    let foreground = json_color(special, "foreground")?;
    let cursor = json_color(special, "cursor")?;

    let mut palette = [(0u8, 0u8, 0u8); 16];
    for (index, slot) in palette.iter_mut().enumerate() {
        let key = format!("color{index}");
        *slot = json_color(colors, &key)?;
    }

    Ok(assemble(
        foreground, background, background, palette, cursor, palette[8], palette[3], palette[8],
        palette[8],
    ))
}

fn object_field<'a>(
    entries: &'a [(String, Json)],
    key: &str,
) -> Result<&'a [(String, Json)], ExternalPaletteError> {
    let value = entries
        .iter()
        .find(|(entry_key, _)| entry_key == key)
        .map(|(_, value)| value)
        .ok_or_else(|| ExternalPaletteError::Incomplete(format!("missing {key:?} object")))?;
    match value {
        Json::Obj(inner) => {
            if inner.len() > MAX_EXTERNAL_PALETTE_ENTRIES {
                return Err(ExternalPaletteError::TooManyEntries);
            }
            Ok(inner.as_slice())
        }
        _ => Err(ExternalPaletteError::Malformed(format!(
            "{key:?} must be a JSON object"
        ))),
    }
}

fn json_color(entries: &[(String, Json)], key: &str) -> Result<Srgb, ExternalPaletteError> {
    let value = entries
        .iter()
        .find(|(entry_key, _)| entry_key == key)
        .map(|(_, value)| value)
        .ok_or_else(|| ExternalPaletteError::Incomplete(format!("missing color {key:?}")))?;
    let Some(text) = value.as_str() else {
        return Err(ExternalPaletteError::Malformed(format!(
            "{key:?} must be a hex color string"
        )));
    };
    parse_hex(text).ok_or_else(|| {
        ExternalPaletteError::Malformed(format!("invalid hex color for {key:?}: {text:?}"))
    })
}
