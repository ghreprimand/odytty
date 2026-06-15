// SPDX-License-Identifier: GPL-3.0-only
//! Headless CLI introspection helpers.
//!
//! These commands print stable, script-friendly snapshots and exit before the
//! native window or PTY paths start.

use std::path::Path;
use std::time::Duration;

use odytty::atlas::SubpixelMode;
use odytty::core::CursorStyle;
use odytty::settings::{
    BindableAction, KeyBindingKey, KeyBindingNamedKey, KeyBindingOverride, KeyChord, Settings,
};
use odytty::text::{self, FontInventoryEntry};
use odytty::theme::{self, Theme, VisualEffect, relative_luminance};

/// Return stdout for a supported CLI introspection flag.
pub fn output_for_args(args: &[String]) -> Option<String> {
    match args.first().map(String::as_str) {
        Some("--list-fonts") => Some(list_fonts_output()),
        Some("--list-themes") => Some(list_themes_output()),
        Some("--show-config") => Some(show_config_output(&Settings::from_env())),
        _ => None,
    }
}

/// Machine-friendly system font inventory.
pub fn list_fonts_output() -> String {
    list_fonts_output_for_entries(text::font_inventory())
}

/// Machine-friendly font inventory for an explicit entry set.
pub fn list_fonts_output_for_entries(entries: Vec<FontInventoryEntry>) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!(
            "path={}\tname={}\tmonospace={}\n",
            path_value(&entry.path),
            entry.name,
            bool_value(entry.monospace)
        ));
    }
    out
}

/// Machine-friendly built-in theme inventory.
pub fn list_themes_output() -> String {
    let mut themes = theme::all().iter().copied().collect::<Vec<_>>();
    themes.sort_by_key(|theme| theme.name);

    let mut out = String::new();
    for theme in themes {
        out.push_str(&format!(
            "name={}\tappearance={}\tfamily={}\n",
            theme.name,
            appearance(theme),
            theme_family(theme.name)
        ));
    }
    out
}

/// Stable effective settings dump.
pub fn show_config_output(settings: &Settings) -> String {
    let mut rows = vec![
        ("bloom", bool_value(settings.bloom).to_owned()),
        ("bloom_intensity", float_value(settings.bloom_intensity)),
        ("bloom_radius", float_value(settings.bloom_radius)),
        ("bloom_threshold", float_value(settings.bloom_threshold)),
        ("crt", bool_value(settings.crt).to_owned()),
        (
            "crt_scanline_intensity",
            float_value(settings.crt_scanline_intensity),
        ),
        (
            "crt_scanline_period",
            float_value(settings.crt_scanline_period),
        ),
        (
            "crt_vignette_strength",
            float_value(settings.crt_vignette_strength),
        ),
        ("cursor_blink", settings.cursor_blink.as_str().to_owned()),
        (
            "cursor_style",
            cursor_style_value(settings.cursor_style).to_owned(),
        ),
        (
            "font",
            settings
                .font_path
                .as_deref()
                .map(path_value)
                .unwrap_or_default(),
        ),
        (
            "font_family",
            settings.font_family.clone().unwrap_or_default(),
        ),
        ("font_size", float_value(settings.font_size_px)),
        ("keybinds", key_bindings_value(&settings.key_bindings)),
        (
            "native_autoclose_ms",
            settings
                .native_autoclose
                .map(duration_millis_value)
                .unwrap_or_default(),
        ),
        ("osc52_read", bool_value(settings.osc52_read).to_owned()),
        (
            "render_quality",
            settings.render_quality.as_str().to_owned(),
        ),
        ("stem_darken", float_value(settings.stem_darken)),
        ("subpixel", subpixel_value(settings.subpixel).to_owned()),
        (
            "synthetic_styles",
            bool_value(settings.synthetic_styles).to_owned(),
        ),
        ("text_gamma", float_value(settings.text_gamma)),
        ("theme", settings.theme.name.to_owned()),
        ("visual", visual_value(settings.visual).to_owned()),
        ("window_padding", float_value(settings.window_padding_px)),
    ];
    rows.sort_by_key(|(key, _)| *key);

    let mut out = String::new();
    for (key, value) in rows {
        out.push_str(key);
        out.push('=');
        out.push_str(&value);
        out.push('\n');
    }
    out
}

fn appearance(theme: Theme) -> &'static str {
    if relative_luminance(theme.background) > 0.18 {
        "light"
    } else {
        "dark"
    }
}

fn theme_family(name: &str) -> &'static str {
    if name == "plain" {
        "baseline"
    } else if name.starts_with("odyssey") {
        "odyssey"
    } else {
        "community"
    }
}

fn float_value(value: f32) -> String {
    let formatted = format!("{value:.2}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn bool_value(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn path_value(path: &Path) -> String {
    path.display().to_string()
}

fn duration_millis_value(duration: Duration) -> String {
    duration.as_millis().to_string()
}

fn visual_value(value: VisualEffect) -> &'static str {
    value.as_str()
}

fn subpixel_value(value: SubpixelMode) -> &'static str {
    match value {
        SubpixelMode::Off => "off",
        SubpixelMode::Rgb => "rgb",
        SubpixelMode::Bgr => "bgr",
    }
}

fn cursor_style_value(value: CursorStyle) -> &'static str {
    match value {
        CursorStyle::Block => "block",
        CursorStyle::Underline => "underline",
        CursorStyle::Bar => "bar",
    }
}

fn key_bindings_value(bindings: &[KeyBindingOverride]) -> String {
    bindings
        .iter()
        .map(key_binding_value)
        .collect::<Vec<_>>()
        .join(";")
}

fn key_binding_value(binding: &KeyBindingOverride) -> String {
    format!(
        "{}={}",
        chord_value(binding.chord),
        action_value(binding.action)
    )
}

fn chord_value(chord: KeyChord) -> String {
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
    parts.push(key_value(chord.key));
    parts.join("+")
}

fn key_value(key: KeyBindingKey) -> String {
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

fn action_value(action: BindableAction) -> &'static str {
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
    }
}
