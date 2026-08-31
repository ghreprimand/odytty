// SPDX-License-Identifier: GPL-3.0-only
//! Omarchy-compatible `colors.toml` complete-palette projection.
//!
//! Independent compatibility only. Two accepted schemas (detected automatically):
//!
//! **Current named palette**: `background`, `foreground`,
//! `red`..`cyan`, `bright_red`..`bright_cyan`, `bright_foreground`, `muted`, plus
//! documented semantic stops without mixing built-in defaults:
//! - ANSI 0 background; 1 red; 2 green; 3 yellow; 4 blue; 5 magenta; 6 cyan;
//!   7 foreground; 8 muted; 9-14 bright_red..bright_cyan; 15 bright_foreground
//! - cursor <- `cursor` or else `bright_foreground`
//! - selection <- `selection` (required)
//! - search <- `yellow`
//! - border <- `muted` or else `darker_background`
//! - inactive <- `dark_foreground` or else `muted`
//! - clear <- `background`
//!
//! **Legacy color-index palette**: `color0`..`color15` plus the same semantic
//! stops with search <- `color3`.

use super::common::{assemble, parse_flat_color_map, require_color, require_palette};
use super::{ExternalPaletteError, NormalizedExternalPalette};
use crate::theme::Srgb;
use std::collections::BTreeMap;

pub fn parse_colors_toml(text: &str) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    let map = parse_flat_color_map(text)?;
    if map.contains_key("color0") {
        parse_legacy_color_index(&map)
    } else if map.contains_key("red") && map.contains_key("green") {
        parse_current_named(&map)
    } else {
        Err(ExternalPaletteError::Incomplete(
            "colors.toml must supply either current named palette keys (red, green, ...) or legacy color0..color15".to_owned(),
        ))
    }
}

fn parse_legacy_color_index(
    map: &BTreeMap<String, Srgb>,
) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    let background = require_color(map, "background")?;
    let foreground = require_color(map, "foreground")?;
    let palette = require_palette(map)?;
    let cursor = first_present(map, &["cursor", "brightforeground"]).ok_or_else(|| {
        ExternalPaletteError::Incomplete(
            "missing cursor (need cursor or bright_foreground)".to_owned(),
        )
    })?;
    let selection = require_color(map, "selection")?;
    let search = palette[3];
    let border = first_present(map, &["muted", "darkerbackground"]).ok_or_else(|| {
        ExternalPaletteError::Incomplete(
            "missing border stop (need muted or darker_background)".to_owned(),
        )
    })?;
    let inactive = first_present(map, &["darkforeground", "muted"]).ok_or_else(|| {
        ExternalPaletteError::Incomplete(
            "missing inactive stop (need dark_foreground or muted)".to_owned(),
        )
    })?;
    Ok(assemble(
        foreground, background, background, palette, cursor, selection, search, border, inactive,
    ))
}

fn parse_current_named(
    map: &BTreeMap<String, Srgb>,
) -> Result<NormalizedExternalPalette, ExternalPaletteError> {
    let background = require_color(map, "background")?;
    let foreground = require_color(map, "foreground")?;
    let selection = require_color(map, "selection")?;
    let red = require_color(map, "red")?;
    let green = require_color(map, "green")?;
    let yellow = require_color(map, "yellow")?;
    let blue = require_color(map, "blue")?;
    let magenta = require_color(map, "magenta")?;
    let cyan = require_color(map, "cyan")?;
    let muted = require_color(map, "muted")?;
    let bright_red = require_color(map, "brightred")?;
    let bright_green = require_color(map, "brightgreen")?;
    let bright_yellow = require_color(map, "brightyellow")?;
    let bright_blue = require_color(map, "brightblue")?;
    let bright_magenta = require_color(map, "brightmagenta")?;
    let bright_cyan = require_color(map, "brightcyan")?;
    let bright_foreground = require_color(map, "brightforeground")?;
    let palette = [
        background,
        red,
        green,
        yellow,
        blue,
        magenta,
        cyan,
        foreground,
        muted,
        bright_red,
        bright_green,
        bright_yellow,
        bright_blue,
        bright_magenta,
        bright_cyan,
        bright_foreground,
    ];
    let cursor = first_present(map, &["cursor", "brightforeground"]).ok_or_else(|| {
        ExternalPaletteError::Incomplete(
            "missing cursor (need cursor or bright_foreground)".to_owned(),
        )
    })?;
    let search = yellow;
    let border = first_present(map, &["muted", "darkerbackground"]).ok_or_else(|| {
        ExternalPaletteError::Incomplete(
            "missing border stop (need muted or darker_background)".to_owned(),
        )
    })?;
    let inactive = first_present(map, &["darkforeground", "muted"]).ok_or_else(|| {
        ExternalPaletteError::Incomplete(
            "missing inactive stop (need dark_foreground or muted)".to_owned(),
        )
    })?;
    Ok(assemble(
        foreground, background, background, palette, cursor, selection, search, border, inactive,
    ))
}

fn first_present(map: &BTreeMap<String, Srgb>, keys: &[&str]) -> Option<Srgb> {
    keys.iter().find_map(|key| map.get(*key).copied())
}
