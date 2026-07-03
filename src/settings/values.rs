// SPDX-License-Identifier: GPL-3.0-only
//! Private value parsers and display helpers extracted from `settings.rs`.
//!
//! Every `parse_*` function here mirrors a single config/env knob; the
//! matching `*_display` and `format_*` helpers format values back to the
//! human-readable form used by the overlay and config writeback.
//!
//! Visibility: items called only from `settings.rs` (the parent) are
//! `pub(super)`. Items only called within this module are private. Nothing
//! here is pub to the crate — callers outside this file reach the helpers
//! through `settings`'s own scope (which imports this module via
//! `use self::values::*;`).

use std::ffi::OsStr;
use std::time::Duration;

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;

// Brings in all types, consts, and pub(super)/private items from settings.rs
// (CursorBlink, BindableAction, KeyBinding* types, ScrollDragSpeed, CvdMode,
// RenderQuality, consts::*, normalize_name, etc.).
use super::*;

// ---------------------------------------------------------------------------
// Private helpers (only used within this module)
// ---------------------------------------------------------------------------

fn parse_cursor_style(raw: &str) -> Option<CursorStyle> {
    match normalize_name(raw).as_str() {
        "block" => Some(CursorStyle::Block),
        "underline" | "under" => Some(CursorStyle::Underline),
        "bar" | "ibeam" | "beam" | "vertical" => Some(CursorStyle::Bar),
        _ => None,
    }
}

fn parse_bounded_float(
    raw: Option<&OsStr>,
    env: &str,
    label: &str,
    default: f32,
    min: f32,
    max: f32,
    warn: &mut impl FnMut(&str),
) -> f32 {
    let Some(raw) = raw else {
        return default;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return default;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{env}={trimmed:?} is not a valid {label}; using {default}"
            ));
            return default;
        }
    };

    parsed.clamp(min, max)
}

fn parse_key_binding_entry(entry: &str, warn: &mut impl FnMut(&str)) -> Option<KeyBindingOverride> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Some((chord_raw, action_raw)) = trimmed.split_once('=') else {
        warn(&format!(
            "{KEYBINDS_ENV} entry {trimmed:?} is missing '='; skipping"
        ));
        return None;
    };
    let Some(chord) = parse_key_chord(chord_raw.trim()) else {
        warn(&format!(
            "{KEYBINDS_ENV} entry {trimmed:?} has an invalid key chord; skipping"
        ));
        return None;
    };
    let Some(action) = BindableAction::parse(action_raw.trim()) else {
        warn(&format!(
            "{KEYBINDS_ENV} entry {trimmed:?} has an unknown action; skipping"
        ));
        return None;
    };
    Some(KeyBindingOverride { chord, action })
}

fn parse_key_chord(raw: &str) -> Option<KeyChord> {
    let mut modifiers = KeyBindingModifiers::default();
    let mut key = None;
    for token in raw
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        match normalize_name(token).as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.alt = true,
            "super" | "meta" | "cmd" | "command" | "win" | "windows" => {
                modifiers.super_key = true;
            }
            _ if key.is_none() => key = parse_key_binding_key(token),
            _ => return None,
        }
    }
    Some(KeyChord {
        modifiers,
        key: key?,
    })
}

fn parse_key_binding_key(raw: &str) -> Option<KeyBindingKey> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.len() == 1 {
        let ch = lower.chars().next()?;
        if ch.is_ascii_graphic() && ch != '+' && ch != '=' {
            return Some(KeyBindingKey::Character(ch));
        }
    }

    let named = match normalize_name(trimmed).as_str() {
        "comma" => return Some(KeyBindingKey::Character(',')),
        "enter" | "return" => KeyBindingNamedKey::Enter,
        "backspace" | "bksp" => KeyBindingNamedKey::Backspace,
        "esc" | "escape" => KeyBindingNamedKey::Escape,
        "tab" => KeyBindingNamedKey::Tab,
        "space" | "spacebar" => KeyBindingNamedKey::Space,
        "pageup" | "pgup" => KeyBindingNamedKey::PageUp,
        "pagedown" | "pgdn" => KeyBindingNamedKey::PageDown,
        "home" => KeyBindingNamedKey::Home,
        "end" => KeyBindingNamedKey::End,
        "delete" | "del" => KeyBindingNamedKey::Delete,
        "insert" | "ins" => KeyBindingNamedKey::Insert,
        "up" | "arrowup" => KeyBindingNamedKey::ArrowUp,
        "down" | "arrowdown" => KeyBindingNamedKey::ArrowDown,
        "left" | "arrowleft" => KeyBindingNamedKey::ArrowLeft,
        "right" | "arrowright" => KeyBindingNamedKey::ArrowRight,
        f_key if f_key.starts_with('f') => {
            let number = f_key[1..].parse::<u8>().ok()?;
            if (1..=24).contains(&number) {
                KeyBindingNamedKey::F(number)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(KeyBindingKey::Named(named))
}

pub(super) fn format_chord(chord: KeyChord) -> String {
    let mut parts = Vec::new();
    if chord.modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if chord.modifiers.shift {
        parts.push("shift".to_owned());
    }
    if chord.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if chord.modifiers.super_key {
        parts.push("super".to_owned());
    }
    parts.push(format_key(chord.key));
    parts.join("+")
}

fn format_key(key: KeyBindingKey) -> String {
    match key {
        KeyBindingKey::Character(',') => "comma".to_owned(),
        KeyBindingKey::Character(ch) => ch.to_string(),
        KeyBindingKey::Named(named) => match named {
            KeyBindingNamedKey::Enter => "enter".to_owned(),
            KeyBindingNamedKey::Backspace => "backspace".to_owned(),
            KeyBindingNamedKey::Escape => "esc".to_owned(),
            KeyBindingNamedKey::Tab => "tab".to_owned(),
            KeyBindingNamedKey::Space => "space".to_owned(),
            KeyBindingNamedKey::PageUp => "pageup".to_owned(),
            KeyBindingNamedKey::PageDown => "pagedown".to_owned(),
            KeyBindingNamedKey::Home => "home".to_owned(),
            KeyBindingNamedKey::End => "end".to_owned(),
            KeyBindingNamedKey::Delete => "delete".to_owned(),
            KeyBindingNamedKey::Insert => "insert".to_owned(),
            KeyBindingNamedKey::ArrowUp => "up".to_owned(),
            KeyBindingNamedKey::ArrowDown => "down".to_owned(),
            KeyBindingNamedKey::ArrowLeft => "left".to_owned(),
            KeyBindingNamedKey::ArrowRight => "right".to_owned(),
            KeyBindingNamedKey::F(number) => format!("f{number}"),
        },
    }
}

// ---------------------------------------------------------------------------
// Pub(super) helpers — called from settings.rs and/or sibling submodules
// ---------------------------------------------------------------------------

/// Parse `ODYTTY_CURSOR_STYLE`, falling back to the default block shape with one
/// warning on an unrecognized value. Empty/unset is the silent default.
pub(super) fn parse_cursor_style_setting(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> CursorStyle {
    let Some(raw) = raw else {
        return CursorStyle::Bar;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return CursorStyle::Bar;
    }
    match parse_cursor_style(trimmed) {
        Some(style) => style,
        None => {
            warn(&format!(
                "{CURSOR_STYLE_ENV}={trimmed:?} is not block|underline|bar; using bar"
            ));
            CursorStyle::Bar
        }
    }
}

/// Parse `ODYTTY_CURSOR_BLINK`, falling back to the default `auto` policy with
/// one warning on an unrecognized value. Empty/unset is the silent default.
pub(super) fn parse_cursor_blink_setting(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> CursorBlink {
    let Some(raw) = raw else {
        return CursorBlink::On;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return CursorBlink::On;
    }
    match CursorBlink::parse(trimmed) {
        Some(policy) => policy,
        None => {
            warn(&format!(
                "{CURSOR_BLINK_ENV}={trimmed:?} is not on|off|auto; using on"
            ));
            CursorBlink::On
        }
    }
}

pub(super) fn parse_font_size(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_FONT_SIZE_PX;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_FONT_SIZE_PX;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{FONT_SIZE_ENV}={trimmed:?} is not a valid pixel size; using {DEFAULT_FONT_SIZE_PX}"
            ));
            return DEFAULT_FONT_SIZE_PX;
        }
    };

    parsed.clamp(MIN_FONT_SIZE_PX, MAX_FONT_SIZE_PX)
}

pub(super) fn parse_text_gamma(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_TEXT_GAMMA;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_TEXT_GAMMA;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{TEXT_GAMMA_ENV}={trimmed:?} is not a valid gamma value; using {DEFAULT_TEXT_GAMMA}"
            ));
            return DEFAULT_TEXT_GAMMA;
        }
    };

    parsed.clamp(MIN_TEXT_GAMMA, MAX_TEXT_GAMMA)
}

pub(super) fn parse_stem_darken(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_STEM_DARKEN;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_STEM_DARKEN;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{STEM_DARKEN_ENV}={trimmed:?} is not a valid stem-darken strength; using {DEFAULT_STEM_DARKEN}"
            ));
            return DEFAULT_STEM_DARKEN;
        }
    };

    parsed.clamp(MIN_STEM_DARKEN, MAX_STEM_DARKEN)
}

pub(super) fn parse_line_height(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_LINE_HEIGHT;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_LINE_HEIGHT;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{LINE_HEIGHT_ENV}={trimmed:?} is not a valid line-height multiplier; using {DEFAULT_LINE_HEIGHT}"
            ));
            return DEFAULT_LINE_HEIGHT;
        }
    };

    parsed.clamp(MIN_LINE_HEIGHT, MAX_LINE_HEIGHT)
}

pub(super) fn parse_box_thickness(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_BOX_THICKNESS;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_BOX_THICKNESS;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{BOX_THICKNESS_ENV}={trimmed:?} is not a valid box-thickness multiplier; using {DEFAULT_BOX_THICKNESS}"
            ));
            return DEFAULT_BOX_THICKNESS;
        }
    };

    parsed.clamp(MIN_BOX_THICKNESS, MAX_BOX_THICKNESS)
}

pub(super) fn parse_min_contrast(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_MIN_CONTRAST;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_MIN_CONTRAST;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{MIN_CONTRAST_ENV}={trimmed:?} is not a valid contrast ratio; using {DEFAULT_MIN_CONTRAST}"
            ));
            return DEFAULT_MIN_CONTRAST;
        }
    };

    parsed.clamp(MIN_MIN_CONTRAST, MAX_MIN_CONTRAST)
}

pub(super) fn parse_focus_dim(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_FOCUS_DIM;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_FOCUS_DIM;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{FOCUS_DIM_ENV}={trimmed:?} is not a valid focus-dim amount; using {DEFAULT_FOCUS_DIM}"
            ));
            return DEFAULT_FOCUS_DIM;
        }
    };

    parsed.clamp(MIN_FOCUS_DIM, MAX_FOCUS_DIM)
}

pub(super) fn parse_inactive_pane_dim(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_INACTIVE_PANE_DIM;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_INACTIVE_PANE_DIM;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{INACTIVE_PANE_DIM_ENV}={trimmed:?} is not a valid inactive-pane-dim amount; using {DEFAULT_INACTIVE_PANE_DIM}"
            ));
            return DEFAULT_INACTIVE_PANE_DIM;
        }
    };

    parsed.clamp(MIN_INACTIVE_PANE_DIM, MAX_INACTIVE_PANE_DIM)
}

/// Parse the mouse-wheel scroll multiplier (MOUSE-WHEEL-SPEED). Mirrors the other
/// numeric parsers: an absent/blank value yields the default; a non-finite or
/// unparseable value warns and falls back; otherwise it is clamped to
/// `[MIN_SCROLL_WHEEL_LINES, MAX_SCROLL_WHEEL_LINES]`.
pub(super) fn parse_scroll_wheel_lines(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_SCROLL_WHEEL_LINES;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_SCROLL_WHEEL_LINES;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{SCROLL_WHEEL_LINES_ENV}={trimmed:?} is not a valid wheel-line count; using {DEFAULT_SCROLL_WHEEL_LINES}"
            ));
            return DEFAULT_SCROLL_WHEEL_LINES;
        }
    };

    parsed.clamp(MIN_SCROLL_WHEEL_LINES, MAX_SCROLL_WHEEL_LINES)
}

/// Parse the scrollback retention cap (SCROLLBACK-CAP). Mirrors the other numeric
/// parsers: absent/blank yields the default; a non-finite or unparseable value
/// warns and falls back; otherwise it is clamped to
/// `[MIN_SCROLLBACK_LINES, MAX_SCROLLBACK_LINES]`. `0` is a valid value meaning
/// "unbounded" — the cap is then disabled.
pub(super) fn parse_scrollback_lines(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_SCROLLBACK_LINES;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_SCROLLBACK_LINES;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{SCROLLBACK_LINES_ENV}={trimmed:?} is not a valid scrollback line count; using {DEFAULT_SCROLLBACK_LINES}"
            ));
            return DEFAULT_SCROLLBACK_LINES;
        }
    };

    parsed.clamp(MIN_SCROLLBACK_LINES, MAX_SCROLLBACK_LINES)
}

pub(super) fn parse_scroll_drag_speed(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> ScrollDragSpeed {
    let Some(raw) = raw else {
        return ScrollDragSpeed::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return ScrollDragSpeed::default();
    }
    match ScrollDragSpeed::parse(trimmed) {
        Some(speed) => speed,
        None => {
            warn(&format!(
                "{SCROLL_DRAG_SPEED_ENV}={trimmed:?} is not ramp|legacy; using ramp"
            ));
            ScrollDragSpeed::default()
        }
    }
}

pub(super) fn parse_smart_ctrl_c(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> SmartCtrlC {
    let Some(raw) = raw else {
        return SmartCtrlC::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return SmartCtrlC::default();
    }
    match SmartCtrlC::parse(trimmed) {
        Some(mode) => mode,
        None => {
            warn(&format!(
                "{SMART_CTRL_C_ENV}={trimmed:?} is not off|copy-or-interrupt; using off"
            ));
            SmartCtrlC::default()
        }
    }
}

pub(super) fn parse_cvd_mode(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> CvdMode {
    let Some(raw) = raw else {
        return CvdMode::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return CvdMode::default();
    }
    match CvdMode::parse(trimmed) {
        Some(mode) => mode,
        None => {
            warn(&format!(
                "{CVD_MODE_ENV}={trimmed:?} is not off|protan|deutan|tritan; using off"
            ));
            CvdMode::default()
        }
    }
}

pub(super) fn parse_bell(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> BellMode {
    let Some(raw) = raw else {
        return BellMode::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return BellMode::default();
    }
    match BellMode::parse(trimmed) {
        Some(mode) => mode,
        None => {
            warn(&format!(
                "{BELL_ENV}={trimmed:?} is not off|visual|urgent|all; using urgent"
            ));
            BellMode::default()
        }
    }
}

pub(super) fn parse_tab_bar_placement(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> TabBarPlacement {
    let Some(raw) = raw else {
        return TabBarPlacement::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return TabBarPlacement::default();
    }
    match TabBarPlacement::parse(trimmed) {
        Some(placement) => placement,
        None => {
            warn(&format!(
                "{TAB_BAR_PLACEMENT_ENV}={trimmed:?} is not top|left|right; using top"
            ));
            TabBarPlacement::default()
        }
    }
}

/// Shared numeric parser for the F4-P1 rail/panel knobs: an absent/blank value
/// yields `default`; a non-finite or unparseable value warns (`what` names the
/// quantity) and falls back; otherwise the value is clamped to `[min, max]`.
fn parse_clamped_f32(
    raw: Option<&OsStr>,
    env: &str,
    what: &str,
    default: f32,
    min: f32,
    max: f32,
    warn: &mut impl FnMut(&str),
) -> f32 {
    let Some(raw) = raw else {
        return default;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return default;
    }
    match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value.clamp(min, max),
        _ => {
            warn(&format!(
                "{env}={trimmed:?} is not a valid {what}; using {default}"
            ));
            default
        }
    }
}

/// Parse the vertical rail width mode (F4-P4): `auto | <cols>`.
///
/// - absent / blank → the default (`Auto`);
/// - `"auto"` (any case) → `Auto`;
/// - a finite number → `Manual`, rounded and clamped to the absolute widget
///   bounds `[MIN_TAB_RAIL_WIDTH, MAX_TAB_RAIL_WIDTH]` — this is the migration
///   path for old numeric configs, which keep their exact width;
/// - anything else → warn and fall back to `Auto`.
pub(super) fn parse_tab_rail_width(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> TabRailWidth {
    let Some(raw) = raw else {
        return TabRailWidth::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return TabRailWidth::default();
    }
    if trimmed.eq_ignore_ascii_case("auto") {
        return TabRailWidth::Auto;
    }
    match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => {
            let cols = value.round().clamp(MIN_TAB_RAIL_WIDTH, MAX_TAB_RAIL_WIDTH) as u16;
            TabRailWidth::Manual(cols)
        }
        _ => {
            warn(&format!(
                "{TAB_RAIL_WIDTH_ENV}={trimmed:?} is not auto or a cell count; using auto"
            ));
            TabRailWidth::default()
        }
    }
}

/// Parse the `auto`-mode upper clamp in cells (F4-P4).
pub(super) fn parse_tab_rail_max_width(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_clamped_f32(
        raw,
        TAB_RAIL_MAX_WIDTH_ENV,
        "rail max width",
        DEFAULT_TAB_RAIL_MAX_WIDTH,
        MIN_TAB_RAIL_MAX_WIDTH,
        MAX_TAB_RAIL_MAX_WIDTH,
        warn,
    )
}

/// Parse the inter-slot gap in rows (F4-P1).
pub(super) fn parse_tab_rail_gap(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_clamped_f32(
        raw,
        TAB_RAIL_GAP_ENV,
        "rail gap",
        DEFAULT_TAB_RAIL_GAP,
        MIN_TAB_RAIL_GAP,
        MAX_TAB_RAIL_GAP,
        warn,
    )
}

/// Parse the rail slot height in rows (F4-P1; clamped to `{1, 2}`).
pub(super) fn parse_tab_rail_slot_rows(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_clamped_f32(
        raw,
        TAB_RAIL_SLOT_ROWS_ENV,
        "rail slot-row count",
        DEFAULT_TAB_RAIL_SLOT_ROWS,
        MIN_TAB_RAIL_SLOT_ROWS,
        MAX_TAB_RAIL_SLOT_ROWS,
        warn,
    )
}

/// Parse the unified tab-panel strength (F4-P1).
pub(super) fn parse_tab_panel_strength(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_clamped_f32(
        raw,
        TAB_PANEL_STRENGTH_ENV,
        "tab-panel strength",
        DEFAULT_TAB_PANEL_STRENGTH,
        MIN_TAB_PANEL_STRENGTH,
        MAX_TAB_PANEL_STRENGTH,
        warn,
    )
}

/// Parse the rail auto-hide reveal-zone width in px (F4-P1; behavior in P3).
pub(super) fn parse_tab_rail_reveal_px(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_clamped_f32(
        raw,
        TAB_RAIL_REVEAL_PX_ENV,
        "rail reveal width",
        DEFAULT_TAB_RAIL_REVEAL_PX,
        MIN_TAB_RAIL_REVEAL_PX,
        MAX_TAB_RAIL_REVEAL_PX,
        warn,
    )
}

pub(super) fn parse_cvd_strength(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_CVD_STRENGTH;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_CVD_STRENGTH;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{CVD_STRENGTH_ENV}={trimmed:?} is not a valid strength; using {DEFAULT_CVD_STRENGTH}"
            ));
            return DEFAULT_CVD_STRENGTH;
        }
    };

    parsed.clamp(MIN_CVD_STRENGTH, MAX_CVD_STRENGTH)
}

pub(super) fn parse_render_quality(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> RenderQuality {
    let Some(raw) = raw else {
        return RenderQuality::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return RenderQuality::default();
    }
    match RenderQuality::parse(trimmed) {
        Some(quality) => quality,
        None => {
            warn(&format!(
                "{RENDER_QUALITY_ENV}={trimmed:?} is not plain|balanced|high; using high"
            ));
            RenderQuality::default()
        }
    }
}

pub(super) fn parse_background_treatment(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> BackgroundTreatment {
    let Some(raw) = raw else {
        return BackgroundTreatment::default();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return BackgroundTreatment::default();
    }
    match BackgroundTreatment::parse(trimmed) {
        Some(treatment) => treatment,
        None => {
            warn(&format!(
                "{BACKGROUND_TREATMENT_ENV}={trimmed:?} is not off|gradient|vignette|image; using off"
            ));
            BackgroundTreatment::default()
        }
    }
}

pub(super) fn parse_window_padding(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_WINDOW_PADDING_PX;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_WINDOW_PADDING_PX;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{WINDOW_PADDING_ENV}={trimmed:?} is not a valid padding value; using {DEFAULT_WINDOW_PADDING_PX}"
            ));
            return DEFAULT_WINDOW_PADDING_PX;
        }
    };

    parsed.clamp(MIN_WINDOW_PADDING_PX, MAX_WINDOW_PADDING_PX)
}

/// Parse the cell background-opacity multiplier (`ODYTTY_CELL_BG_OPACITY`).
/// `1.0` (default) is the identity / off path. Out-of-range or invalid values
/// warn and fall back to the opaque default; valid values clamp to `[0,1]`.
pub(super) fn parse_cell_bg_opacity(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_CELL_BG_OPACITY;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_CELL_BG_OPACITY;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{CELL_BG_OPACITY_ENV}={trimmed:?} is not a valid opacity; using {DEFAULT_CELL_BG_OPACITY}"
            ));
            return DEFAULT_CELL_BG_OPACITY;
        }
    };

    parsed.clamp(MIN_CELL_BG_OPACITY, MAX_CELL_BG_OPACITY)
}

/// Parse the background-image blur radius (`ODYTTY_BACKGROUND_BLUR_RADIUS`).
/// `0` (default) means no blur. Invalid values warn and fall back to `0`; valid
/// values clamp to `MAX_BACKGROUND_BLUR_RADIUS`.
pub(super) fn parse_background_blur_radius(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> u32 {
    let Some(raw) = raw else {
        return DEFAULT_BACKGROUND_BLUR_RADIUS;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_BACKGROUND_BLUR_RADIUS;
    }

    match trimmed.parse::<u32>() {
        Ok(value) => value.min(MAX_BACKGROUND_BLUR_RADIUS),
        Err(_) => {
            warn(&format!(
                "{BACKGROUND_BLUR_RADIUS_ENV}={trimmed:?} is not a valid blur radius; using {DEFAULT_BACKGROUND_BLUR_RADIUS}"
            ));
            DEFAULT_BACKGROUND_BLUR_RADIUS
        }
    }
}

/// Parse the explicit background-image scrim override
/// (`ODYTTY_BACKGROUND_IMAGE_SCRIM`). Absent / empty / `auto` ⇒ `None`
/// (auto-compute the floor-safe scrim). A valid value clamps to `[0,1]`; an
/// invalid value warns and falls back to `None`.
pub(super) fn parse_background_image_scrim(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> Option<f32> {
    let raw = raw?;
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() || normalize_name(trimmed) == "auto" {
        return None;
    }

    match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => {
            Some(value.clamp(MIN_BACKGROUND_IMAGE_SCRIM, MAX_BACKGROUND_IMAGE_SCRIM))
        }
        _ => {
            warn(&format!(
                "{BACKGROUND_IMAGE_SCRIM_ENV}={trimmed:?} is not a valid scrim amount; using the auto-computed scrim"
            ));
            None
        }
    }
}

pub(super) fn parse_bloom_threshold(
    raw: Option<&OsStr>,
    default: f32,
    warn: &mut impl FnMut(&str),
) -> f32 {
    let Some(raw) = raw else {
        return default;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() || normalize_name(trimmed) == "auto" {
        return default;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{BLOOM_THRESHOLD_ENV}={trimmed:?} is not a valid bloom threshold; using {default}"
            ));
            return default;
        }
    };

    parsed.clamp(MIN_BLOOM_THRESHOLD, MAX_BLOOM_THRESHOLD)
}

pub(super) fn parse_bloom_intensity(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_BLOOM_INTENSITY;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_BLOOM_INTENSITY;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{BLOOM_INTENSITY_ENV}={trimmed:?} is not a valid bloom intensity; using {DEFAULT_BLOOM_INTENSITY}"
            ));
            return DEFAULT_BLOOM_INTENSITY;
        }
    };

    parsed.clamp(MIN_BLOOM_INTENSITY, MAX_BLOOM_INTENSITY)
}

pub(super) fn parse_bloom_radius(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_BLOOM_RADIUS;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_BLOOM_RADIUS;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{BLOOM_RADIUS_ENV}={trimmed:?} is not a valid bloom radius; using {DEFAULT_BLOOM_RADIUS}"
            ));
            return DEFAULT_BLOOM_RADIUS;
        }
    };

    parsed.clamp(MIN_BLOOM_RADIUS, MAX_BLOOM_RADIUS)
}

pub(super) fn parse_crt_scanline_intensity(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> f32 {
    parse_bounded_float(
        raw,
        CRT_SCANLINE_INTENSITY_ENV,
        "CRT scanline intensity",
        DEFAULT_CRT_SCANLINE_INTENSITY,
        MIN_CRT_SCANLINE_INTENSITY,
        MAX_CRT_SCANLINE_INTENSITY,
        warn,
    )
}

pub(super) fn parse_crt_scanline_period(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_bounded_float(
        raw,
        CRT_SCANLINE_PERIOD_ENV,
        "CRT scanline period",
        DEFAULT_CRT_SCANLINE_PERIOD,
        MIN_CRT_SCANLINE_PERIOD,
        MAX_CRT_SCANLINE_PERIOD,
        warn,
    )
}

pub(super) fn parse_crt_vignette_strength(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_bounded_float(
        raw,
        CRT_VIGNETTE_STRENGTH_ENV,
        "CRT vignette strength",
        DEFAULT_CRT_VIGNETTE_STRENGTH,
        MIN_CRT_VIGNETTE_STRENGTH,
        MAX_CRT_VIGNETTE_STRENGTH,
        warn,
    )
}
pub(super) fn parse_crt_curvature(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    parse_bounded_float(
        raw,
        CRT_CURVATURE_ENV,
        "CRT curvature",
        DEFAULT_CRT_CURVATURE,
        MIN_CRT_CURVATURE,
        MAX_CRT_CURVATURE,
        warn,
    )
}

pub(super) fn parse_subpixel(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> SubpixelMode {
    let Some(raw) = raw else {
        return SubpixelMode::Off;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return SubpixelMode::Off;
    }

    match normalize_name(trimmed).as_str() {
        "off" | "none" | "false" | "0" => SubpixelMode::Off,
        "rgb" => SubpixelMode::Rgb,
        "bgr" => SubpixelMode::Bgr,
        _ => {
            warn(&format!(
                "{SUBPIXEL_ENV}={trimmed:?} is not off|rgb|bgr; using off"
            ));
            SubpixelMode::Off
        }
    }
}

pub(super) fn parse_autoclose(raw: Option<&OsStr>) -> Option<Duration> {
    let raw = raw?;
    let ms: u64 = raw.to_string_lossy().trim().parse().ok()?;
    (ms > 0).then_some(Duration::from_millis(ms))
}

pub(super) fn parse_bool_setting(
    raw: Option<&OsStr>,
    env: &str,
    default: bool,
    warn: &mut impl FnMut(&str),
) -> bool {
    let Some(raw) = raw else {
        return default;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return default;
    }
    match normalize_name(trimmed).as_str() {
        "1" | "true" | "yes" | "on" | "enabled" | "enable" => true,
        "0" | "false" | "no" | "off" | "disabled" | "disable" => false,
        _ => {
            warn(&format!("{env}={trimmed:?} is not on|off; using {default}"));
            default
        }
    }
}

pub(super) fn parse_key_bindings(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> Vec<KeyBindingOverride> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let value = raw.to_string_lossy();
    value
        .split([',', ';'])
        .filter_map(|entry| parse_key_binding_entry(entry, warn))
        .collect()
}

/// The default multiplexer prefix chord, `Ctrl-b` (§7), matching tmux.
pub(super) fn default_pane_prefix() -> Option<KeyChord> {
    Some(KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl: true,
            shift: false,
            alt: false,
            super_key: false,
        },
        key: KeyBindingKey::Character('b'),
    })
}

/// Parse `ODYTTY_PANE_PREFIX` into the multiplexer prefix chord (§7). Unset is
/// the `Ctrl-b` default; `off`/`none`/`disabled` turns the prefix model off
/// entirely (`None`), restoring the pre-§7 byte-identical input path; any other
/// value parses as a chord (e.g. `ctrl+a`), warning + falling back to the
/// default on an invalid chord.
pub(super) fn parse_pane_prefix(
    raw: Option<&OsStr>,
    warn: &mut impl FnMut(&str),
) -> Option<KeyChord> {
    let Some(raw) = raw else {
        return default_pane_prefix();
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return default_pane_prefix();
    }
    match normalize_name(trimmed).as_str() {
        "off" | "none" | "disabled" | "disable" => return None,
        _ => {}
    }
    match parse_key_chord(trimmed) {
        Some(chord) => Some(chord),
        None => {
            warn(&format!(
                "{PANE_PREFIX_ENV}={trimmed:?} is not a valid key chord; using the Ctrl-b default"
            ));
            default_pane_prefix()
        }
    }
}

/// Format the multiplexer prefix chord back to its config value (§7): `off`
/// when disabled, else the chord string (e.g. `ctrl+b`).
pub(super) fn pane_prefix_display(prefix: Option<KeyChord>) -> String {
    match prefix {
        Some(chord) => format_chord(chord),
        None => "off".to_owned(),
    }
}

pub(super) fn format_float(value: f32) -> String {
    let formatted = format!("{value:.2}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

pub(super) fn bool_display(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

pub(super) fn subpixel_display(value: SubpixelMode) -> &'static str {
    match value {
        SubpixelMode::Off => "off",
        SubpixelMode::Rgb => "rgb",
        SubpixelMode::Bgr => "bgr",
    }
}

pub(super) fn cursor_style_display(value: CursorStyle) -> &'static str {
    match value {
        CursorStyle::Block => "block",
        CursorStyle::Underline => "underline",
        CursorStyle::Bar => "bar",
    }
}

pub(super) fn key_bindings_display(bindings: &[KeyBindingOverride]) -> String {
    if bindings.is_empty() {
        return "default key bindings".to_owned();
    }

    bindings
        .iter()
        .map(format_key_binding)
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn format_key_binding(binding: &KeyBindingOverride) -> String {
    format!(
        "{}={}",
        format_chord(binding.chord),
        bindable_action_name(binding.action)
    )
}

pub(super) fn bindable_action_name(action: BindableAction) -> &'static str {
    match action {
        BindableAction::Search => "search",
        BindableAction::SettingsPanel => "settings",
        BindableAction::ThemePicker => "theme-picker",
        BindableAction::Copy => "copy",
        BindableAction::Paste => "paste",
        BindableAction::ScrollPageUp => "scroll-up",
        BindableAction::ScrollPageDown => "scroll-down",
        BindableAction::JumpPromptPrev => "jump-prompt-prev",
        BindableAction::JumpPromptNext => "jump-prompt-next",
        BindableAction::CopyMode => "copy-mode",
        BindableAction::Hints => "hints",
        BindableAction::ClearInput => "clear-input",
        BindableAction::CommandPalette => "command-palette",
        BindableAction::SessionReplay => "session-replay",
        BindableAction::ConnectionManager => "connection-manager",
        BindableAction::ThemeBuilder => "theme-builder",
        BindableAction::SessionAttach => "session-attach",
        BindableAction::NewTab => "new-tab",
        BindableAction::NewWindow => "new-window",
        BindableAction::NextTab => "next-tab",
        BindableAction::PrevTab => "prev-tab",
        BindableAction::CloseTab => "close-tab",
        BindableAction::SplitColumns => "split-columns",
        BindableAction::SplitRows => "split-rows",
        BindableAction::FocusPaneLeft => "focus-pane-left",
        BindableAction::FocusPaneRight => "focus-pane-right",
        BindableAction::FocusPaneUp => "focus-pane-up",
        BindableAction::FocusPaneDown => "focus-pane-down",
        BindableAction::FocusPaneNext => "focus-pane-next",
        BindableAction::ClosePane => "close-pane",
        BindableAction::ZoomPane => "zoom-pane",
        BindableAction::EqualizePanes => "equalize-panes",
    }
}
