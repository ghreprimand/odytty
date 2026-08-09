// SPDX-License-Identifier: GPL-3.0-only
//! Parsers for compound and path-like settings values.

use super::*;

/// Parse a `ODYTTY_SYMBOL_MAP` string into a [`crate::text::SymbolMap`] (SYMMAP).
///
/// Grammar: semicolon-separated rules, each `U+XXXX[-U+YYYY]=FontFamilyName`.
/// A single codepoint (`U+XXXX=Name`) is treated as the range `XXXX..=XXXX`.
/// Hex is case-insensitive; the `U+`/`u+` prefix is required on each codepoint.
/// The font name is everything after the first `=`, trimmed, and may contain
/// spaces. Malformed entries (no `=`, empty font, bad codepoint, degenerate
/// range) are warned and skipped — a bad rule never aborts startup, and an
/// empty result is the identity (no override). First-match-wins preserves the
/// written order.
pub(super) fn parse_symbol_map(raw: &str, warn: &mut impl FnMut(&str)) -> crate::text::SymbolMap {
    let mut map = crate::text::SymbolMap::new();
    let parse_cp = |s: &str| -> Option<u32> {
        let hex = s.strip_prefix("U+").or_else(|| s.strip_prefix("u+"))?;
        u32::from_str_radix(hex, 16).ok()
    };
    for entry in raw.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((range_part, font_part)) = entry.split_once('=') else {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has no '=' separator; skipping"
            ));
            continue;
        };
        let range_part = range_part.trim();
        let font_name = font_part.trim();
        if font_name.is_empty() {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has an empty font name; skipping"
            ));
            continue;
        }
        let (start_str, end_str) = match range_part.split_once('-') {
            Some((s, e)) => (s.trim(), e.trim()),
            None => (range_part, range_part),
        };
        let (Some(start), Some(end)) = (parse_cp(start_str), parse_cp(end_str)) else {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has an invalid codepoint range (expected U+XXXX[-U+YYYY]); skipping"
            ));
            continue;
        };
        if !map.push(start, end, font_name) {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has start > end; skipping"
            ));
        }
    }
    map
}

/// Serialize a [`crate::text::SymbolMap`] back to its `ODYTTY_SYMBOL_MAP` config
/// string (the inverse of [`parse_symbol_map`]). Each rule renders as
/// `U+XXXX-U+YYYY=Font` (or `U+XXXX=Font` when start == end), joined by `; `, so
/// the persisted value round-trips through the parser byte-for-byte.
pub(super) fn format_symbol_map(map: &crate::text::SymbolMap) -> String {
    map.rules()
        .iter()
        .map(|rule| {
            let (start, end) = rule.bounds();
            if start == end {
                format!("U+{start:04X}={}", rule.font())
            } else {
                format!("U+{start:04X}-U+{end:04X}={}", rule.font())
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn parse_symbol_font_path(raw: OsString) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    match raw.into_string() {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        }
        Err(value) => Some(PathBuf::from(value)),
    }
}

/// Normalize an `ODYTTY_FONT_WEIGHT` value (RV7). Trims surrounding whitespace
/// and treats `regular`/`normal` (case-insensitive) as the identity case,
/// returning an empty string so the effective font query is the plain
/// `font_family` exactly as before. Any other token is preserved verbatim
/// (e.g. `Light`, `SemiBold`) to be appended to the family at face resolution.
pub(super) fn parse_font_weight_variant(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("regular")
        || trimmed.eq_ignore_ascii_case("normal")
    {
        String::new()
    } else {
        trimmed.to_string()
    }
}
