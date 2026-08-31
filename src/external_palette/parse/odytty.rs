// SPDX-License-Identifier: GPL-3.0-only
//! Explicit OdyTTY theme-format and Base16-compatible complete palettes.

use super::common::{assemble, parse_flat_color_map, require_color, require_palette};
use super::{ExternalPaletteError, NormalizedExternalPalette};
use crate::theme::Srgb;
use std::collections::BTreeMap;

/// Parse an explicit OdyTTY theme-format file that supplies a *complete* color
/// payload, or a Base16 `base00`..`base0F` map with the documented projection.
///
/// Required OdyTTY keys: `foreground`, `background`, `cursor`, `selection`,
/// `search`, `border`, `inactive`, `color0`..`color15`. `clear` may be omitted
/// and then equals `background` (documented ThemeSpec rule).
///
/// Base16 projection (when `base00`..`base0F` are all present and OdyTTY keys
/// are not):
/// - background/clear <- base00; foreground <- base05; cursor <- base07
/// - selection <- base02; search <- base0A; border <- base03; inactive <- base03
/// - ANSI: color0=base00, color1=base08, color2=base0B, color3=base0A,
///   color4=base0D, color5=base0E, color6=base0C, color7=base05,
///   color8=base03, color9=base08, color10=base0B, color11=base0A,
///   color12=base0D, color13=base0E, color14=base0C, color15=base07
pub fn parse_odytty_or_base16(
    text: &str,
) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    let map = parse_flat_color_map(text)?;
    if looks_like_complete_odytty(&map) {
        return parse_odytty_complete(&map);
    }
    if looks_like_complete_base16(&map) {
        return parse_base16_complete(&map);
    }
    Err(ExternalPaletteError::Incomplete(
        "need a complete OdyTTY color payload (fg/bg/cursor/selection/search/border/inactive/color0-15) or complete Base16 base00-base0F set".to_owned(),
    ))
}

fn looks_like_complete_odytty(map: &BTreeMap<String, Srgb>) -> bool {
    [
        "foreground",
        "background",
        "cursor",
        "selection",
        "search",
        "border",
        "inactive",
    ]
    .iter()
    .all(|key| map.contains_key(*key))
        && (0..16).all(|i| map.contains_key(&format!("color{i}")))
}

fn looks_like_complete_base16(map: &BTreeMap<String, Srgb>) -> bool {
    (0..16).all(|i| map.contains_key(&format!("base{i:02x}")))
}

fn parse_odytty_complete(
    map: &BTreeMap<String, Srgb>,
) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    let background = require_color(map, "background")?;
    let clear = map.get("clear").copied().unwrap_or(background);
    Ok(assemble(
        require_color(map, "foreground")?,
        background,
        clear,
        require_palette(map)?,
        require_color(map, "cursor")?,
        require_color(map, "selection")?,
        require_color(map, "search")?,
        require_color(map, "border")?,
        require_color(map, "inactive")?,
    ))
}

fn parse_base16_complete(
    map: &BTreeMap<String, Srgb>,
) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    let base = |index: u8| require_color(map, &format!("base{index:02x}"));
    let palette = [
        base(0x00)?, // color0
        base(0x08)?, // color1
        base(0x0b)?, // color2
        base(0x0a)?, // color3
        base(0x0d)?, // color4
        base(0x0e)?, // color5
        base(0x0c)?, // color6
        base(0x05)?, // color7
        base(0x03)?, // color8
        base(0x08)?, // color9
        base(0x0b)?, // color10
        base(0x0a)?, // color11
        base(0x0d)?, // color12
        base(0x0e)?, // color13
        base(0x0c)?, // color14
        base(0x07)?, // color15
    ];
    Ok(assemble(
        base(0x05)?,
        base(0x00)?,
        base(0x00)?,
        palette,
        base(0x07)?,
        base(0x02)?,
        base(0x0a)?,
        base(0x03)?,
        base(0x03)?,
    ))
}
