//! Theme file format: a dependency-free, owned serialization model.
//!
//! [`ThemeSpec`] is the on-disk / authoring representation of a theme. It is a
//! *superset* of the runtime [`Theme`](super::Theme): it carries the same color
//! payload (default fg/bg/clear, the full 16-color ANSI palette, and the TH1
//! semantic roles) plus authoring-only metadata that the runtime `Theme` does
//! not need yet — a light/dark [`Appearance`] flag, optional font hints, and an
//! optional bundled visual-effect profile. Those extra fields are parsed,
//! serialized, and round-tripped now (forward-compat) but only the color
//! payload is projected into the runtime `Theme` via [`ThemeSpec::to_theme`];
//! the rest is consumed by later packets (settings panel, theme picker).
//!
//! ## File format
//!
//! Line-oriented `key = value`, mirroring `odytty.conf` exactly so users learn
//! one syntax:
//!
//! * `#` starts a comment to end of line; blank lines are ignored.
//! * Keys are case-insensitive and punctuation-insensitive (`color0`,
//!   `Color_0`, and `COLOR 0` are the same key).
//! * Unknown keys are **ignored with a warning**, never fatal, so a theme
//!   written for a newer OdyTTY still loads on an older one.
//! * A malformed value (bad hex, bad number) warns and leaves that one field at
//!   its default — a single bad line never aborts the whole theme.
//!
//! Colors are `#RRGGBB` or `#RGB` (a leading `#` is optional). Recognized keys:
//!
//! ```text
//! name        = My Theme          # display name
//! appearance  = dark              # dark | light
//! foreground  = #d6def4           # alias: fg
//! background  = #0c1224           # alias: bg
//! clear       = #070b18           # window clear; defaults to background
//! cursor      = #86c1ff           # semantic roles ↓
//! selection   = #243352
//! search      = #4a4018
//! border      = #1b243e
//! inactive    = #5a6480
//! color0      = #12182a           # ANSI palette, color0..color15
//! ...                             #   (alias: palette0..palette15)
//! color15     = #f0f4ff
//! font_family = JetBrains Mono     # optional font hint (forward-compat)
//! font_size   = 14                 # optional font hint (forward-compat)
//! visual      = ambient            # bundled effect: off | ambient | scanlines
//! ```

use super::{Srgb, Theme, VisualEffect};

/// Light/dark classification of a theme. Authored in the file as
/// `appearance = dark|light`; defaults to [`Dark`](Self::Dark). Stored for
/// forward-compat (auto contrast handling, light/dark-aware effects); not yet
/// projected into the runtime [`Theme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    /// A dark theme (light text on a dark field). The OdyTTY default.
    #[default]
    Dark,
    /// A light theme (dark text on a light field).
    Light,
}

impl Appearance {
    /// Resolve an appearance by name (case-insensitive, whitespace-trimmed).
    /// Returns `None` for anything unrecognized so the parser can warn.
    pub fn from_name(name: &str) -> Option<Appearance> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Appearance::Dark),
            "light" => Some(Appearance::Light),
            _ => None,
        }
    }

    /// Canonical lowercase token used when serializing.
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }
}

/// The authoring/serialization model of a theme (see module docs for the file
/// format). Built-ins and user theme files share this one parse → project path.
///
/// `PartialEq` is exact, which makes the parse → serialize → parse round-trip
/// directly testable as a fixed point.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeSpec {
    /// Display name (also the `name = ` field). Defaults to `"custom"`.
    pub name: String,
    /// Light/dark classification.
    pub appearance: Appearance,
    /// Default foreground for `Color::Default` cells.
    pub foreground: Srgb,
    /// Default background for `Color::Default` cells.
    pub background: Srgb,
    /// Window clear color (defaults to `background` when omitted).
    pub clear: Srgb,
    /// The 16 ANSI colors (0–7 normal, 8–15 bright).
    pub palette: [Srgb; 16],
    /// Cursor semantic role.
    pub cursor: Srgb,
    /// Selection background semantic role.
    pub selection: Srgb,
    /// Search-highlight background semantic role.
    pub search: Srgb,
    /// Border/frame semantic role.
    pub border: Srgb,
    /// Inactive/dim semantic role.
    pub inactive: Srgb,
    /// Optional font-family hint (forward-compat; not yet applied at runtime).
    pub font_family: Option<String>,
    /// Optional font-size hint in px (forward-compat; not yet applied).
    pub font_size: Option<f32>,
    /// Bundled visual-effect profile (forward-compat; not yet auto-applied to
    /// the live `ODYTTY_VISUAL` setting).
    pub visual: VisualEffect,
}

impl Default for ThemeSpec {
    fn default() -> Self {
        ThemeSpec::from_theme(&Theme::PLAIN)
    }
}

impl ThemeSpec {
    /// Build a spec from a runtime [`Theme`], copying its color payload. The
    /// authoring-only fields take their defaults (dark appearance, no font
    /// hints, effect off). This is the serialization entry point used to render
    /// a built-in (or a live-edited theme) back to file text.
    pub fn from_theme(theme: &Theme) -> ThemeSpec {
        ThemeSpec {
            name: theme.name.to_string(),
            appearance: Appearance::Dark,
            foreground: theme.foreground,
            background: theme.background,
            clear: theme.clear,
            palette: theme.palette,
            cursor: theme.cursor,
            selection: theme.selection,
            search: theme.search,
            border: theme.border,
            inactive: theme.inactive,
            font_family: None,
            font_size: None,
            visual: VisualEffect::Off,
        }
    }

    /// Project this spec into the runtime [`Theme`] consumed by the renderer.
    ///
    /// Only the color payload crosses over; authoring-only fields (appearance,
    /// font hints, effect profile) stay in the spec for later packets. The
    /// runtime `Theme::name` is `&'static`, so a spec whose name matches a
    /// built-in keeps that static name; any other (user) theme projects to the
    /// static placeholder `"custom"`.
    pub fn to_theme(&self) -> Theme {
        let name = Theme::from_name(&self.name)
            .map(|builtin| builtin.name)
            .unwrap_or("custom");
        self.to_theme_with_name(name)
    }

    /// Project this spec's color payload into a [`Theme`] with an explicit
    /// `&'static` name, bypassing the [`Theme::from_name`] lookup that
    /// [`to_theme`](Self::to_theme) uses to derive the name.
    ///
    /// This is the projection the built-in registry uses: it owns the canonical
    /// name for each embedded theme, and resolving it through `from_name` while
    /// the registry is still being built would re-enter the lazy initializer.
    /// Taking the name as a parameter keeps the registry's parse → project path
    /// free of that cycle while sharing the exact same color projection.
    pub fn to_theme_with_name(&self, name: &'static str) -> Theme {
        Theme {
            name,
            foreground: self.foreground,
            background: self.background,
            clear: self.clear,
            palette: self.palette,
            cursor: self.cursor,
            selection: self.selection,
            search: self.search,
            border: self.border,
            inactive: self.inactive,
        }
    }

    /// Parse a theme file. Missing keys keep the [`plain`](Theme::PLAIN)
    /// default for that slot, so partial theme files are valid. Unknown keys
    /// and malformed values warn (via `warn`) but never abort — the worst case
    /// is a theme that falls back to plain defaults for the offending fields.
    pub fn parse(contents: &str, mut warn: impl FnMut(String)) -> ThemeSpec {
        // Start from plain defaults but with no name yet; a file without an
        // explicit `name` projects to the `"custom"` placeholder.
        let mut spec = ThemeSpec {
            name: "custom".to_string(),
            ..ThemeSpec::default()
        };

        for (line_index, line) in contents.lines().enumerate() {
            let line_number = line_index + 1;
            let trimmed = line.trim();
            // A `#` only starts a comment at the start of a line (full-line
            // comment); colors begin with `#`, so a value-leading `#` is never
            // a comment. Inline trailing comments still work because they are
            // preceded by whitespace (see `strip_inline_comment`).
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key_raw, value_raw)) = trimmed.split_once('=') else {
                warn(format!(
                    "line {line_number}: expected key = value; skipping"
                ));
                continue;
            };
            let key = normalize_key(key_raw);
            let value = strip_inline_comment(value_raw.trim()).trim();
            if key.is_empty() {
                warn(format!("line {line_number}: empty key; skipping"));
                continue;
            }

            // Palette entries: color0..color15 / palette0..palette15.
            if let Some(index) = palette_index(&key) {
                match parse_hex(value) {
                    Some(color) => spec.palette[index] = color,
                    None => warn(format!(
                        "line {line_number}: invalid color {value:?} for color{index}; ignoring"
                    )),
                }
                continue;
            }

            match key.as_str() {
                "name" => spec.name = value.to_string(),
                "appearance" => match Appearance::from_name(value) {
                    Some(appearance) => spec.appearance = appearance,
                    None => warn(format!(
                        "line {line_number}: unknown appearance {value:?}; keeping {}",
                        spec.appearance.as_str()
                    )),
                },
                "foreground" | "fg" => set_color(
                    &mut spec.foreground,
                    value,
                    "foreground",
                    line_number,
                    &mut warn,
                ),
                "background" | "bg" => set_color(
                    &mut spec.background,
                    value,
                    "background",
                    line_number,
                    &mut warn,
                ),
                "clear" => set_color(&mut spec.clear, value, "clear", line_number, &mut warn),
                "cursor" => set_color(&mut spec.cursor, value, "cursor", line_number, &mut warn),
                "selection" => set_color(
                    &mut spec.selection,
                    value,
                    "selection",
                    line_number,
                    &mut warn,
                ),
                "search" => set_color(&mut spec.search, value, "search", line_number, &mut warn),
                "border" => set_color(&mut spec.border, value, "border", line_number, &mut warn),
                "inactive" => set_color(
                    &mut spec.inactive,
                    value,
                    "inactive",
                    line_number,
                    &mut warn,
                ),
                "fontfamily" => {
                    spec.font_family = Some(value.to_string()).filter(|s| !s.is_empty());
                }
                "fontsize" => match value.parse::<f32>() {
                    Ok(size) if size.is_finite() && size > 0.0 => spec.font_size = Some(size),
                    _ => warn(format!(
                        "line {line_number}: invalid font_size {value:?}; ignoring"
                    )),
                },
                "visual" => match VisualEffect::from_name(value) {
                    Some(effect) => spec.visual = effect,
                    None => warn(format!(
                        "line {line_number}: unknown visual {value:?}; keeping off"
                    )),
                },
                other => warn(format!(
                    "line {line_number}: unknown key {other:?}; ignoring"
                )),
            }
        }

        spec
    }

    /// Serialize to canonical theme-file text. The output re-parses to an equal
    /// [`ThemeSpec`] (round-trip fixed point). Optional fields are emitted only
    /// when present.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str("# OdyTTY theme\n");
        out.push_str(&format!("name = {}\n", self.name));
        out.push_str(&format!("appearance = {}\n", self.appearance.as_str()));
        out.push('\n');
        out.push_str(&format!("foreground = {}\n", hex(self.foreground)));
        out.push_str(&format!("background = {}\n", hex(self.background)));
        out.push_str(&format!("clear = {}\n", hex(self.clear)));
        out.push('\n');
        out.push_str(&format!("cursor = {}\n", hex(self.cursor)));
        out.push_str(&format!("selection = {}\n", hex(self.selection)));
        out.push_str(&format!("search = {}\n", hex(self.search)));
        out.push_str(&format!("border = {}\n", hex(self.border)));
        out.push_str(&format!("inactive = {}\n", hex(self.inactive)));
        out.push('\n');
        for (index, color) in self.palette.iter().enumerate() {
            out.push_str(&format!("color{index} = {}\n", hex(*color)));
        }
        if self.font_family.is_some() || self.font_size.is_some() || self.visual.is_enabled() {
            out.push('\n');
        }
        if let Some(family) = &self.font_family {
            out.push_str(&format!("font_family = {family}\n"));
        }
        if let Some(size) = self.font_size {
            out.push_str(&format!("font_size = {size}\n"));
        }
        if self.visual.is_enabled() {
            out.push_str(&format!("visual = {}\n", self.visual.as_str()));
        }
        out
    }
}

/// Strip an inline trailing `#` comment from an already-trimmed value. A `#`
/// is only a comment when preceded by whitespace, so a value-leading hex color
/// (`#rrggbb`) is preserved while `#rrggbb  # note` drops the note.
fn strip_inline_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' && i > 0 && bytes[i - 1].is_ascii_whitespace() {
            return &value[..i];
        }
    }
    value
}

/// Normalize a key: lowercase, drop all non-alphanumeric characters. So
/// `color_0`, `Color 0`, and `COLOR0` all collapse to `color0`.
fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// If `key` is `colorN` or `paletteN` for `N` in `0..=15`, return `N`.
fn palette_index(key: &str) -> Option<usize> {
    let digits = key
        .strip_prefix("color")
        .or_else(|| key.strip_prefix("palette"))?;
    let index: usize = digits.parse().ok()?;
    (index < 16).then_some(index)
}

/// Parse `#RRGGBB`, `RRGGBB`, `#RGB`, or `RGB` into an sRGB triple.
fn parse_hex(value: &str) -> Option<Srgb> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((r, g, b))
        }
        3 => {
            // #abc -> #aabbcc
            let expand = |c: &str| u8::from_str_radix(c, 16).ok().map(|v| v * 17);
            let r = expand(&hex[0..1])?;
            let g = expand(&hex[1..2])?;
            let b = expand(&hex[2..3])?;
            Some((r, g, b))
        }
        _ => None,
    }
}

/// Canonical lowercase `#rrggbb` form used by [`ThemeSpec::serialize`].
fn hex((r, g, b): Srgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Parse a color value into `slot`, warning (and leaving `slot` unchanged) on a
/// malformed value.
fn set_color(
    slot: &mut Srgb,
    value: &str,
    field: &str,
    line_number: usize,
    warn: &mut impl FnMut(String),
) {
    match parse_hex(value) {
        Some(color) => *slot = color,
        None => warn(format!(
            "line {line_number}: invalid color {value:?} for {field}; ignoring"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(contents: &str) -> ThemeSpec {
        let mut warnings = Vec::new();
        let spec = ThemeSpec::parse(contents, |m| warnings.push(m));
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        spec
    }

    #[test]
    fn hex_parses_six_and_three_digit_forms() {
        assert_eq!(parse_hex("#0c1224"), Some((0x0C, 0x12, 0x24)));
        assert_eq!(parse_hex("0c1224"), Some((0x0C, 0x12, 0x24)));
        assert_eq!(parse_hex("#abc"), Some((0xAA, 0xBB, 0xCC)));
        assert_eq!(parse_hex("fff"), Some((0xFF, 0xFF, 0xFF)));
        assert_eq!(parse_hex("#000000"), Some((0, 0, 0)));
    }

    #[test]
    fn hex_rejects_malformed() {
        assert_eq!(parse_hex("#12"), None);
        assert_eq!(parse_hex("nothex"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn hex_serialization_is_lowercase_six_digit() {
        assert_eq!(hex((0x0C, 0x12, 0x24)), "#0c1224");
        assert_eq!(hex((0xFF, 0xFF, 0xFF)), "#ffffff");
    }

    #[test]
    fn normalize_key_collapses_case_and_punctuation() {
        assert_eq!(normalize_key("Color_0"), "color0");
        assert_eq!(normalize_key("  COLOR 0 "), "color0");
        assert_eq!(normalize_key("font-family"), "fontfamily");
    }

    #[test]
    fn palette_index_recognizes_color_and_palette_keys() {
        assert_eq!(palette_index("color0"), Some(0));
        assert_eq!(palette_index("color15"), Some(15));
        assert_eq!(palette_index("palette7"), Some(7));
        assert_eq!(palette_index("color16"), None);
        assert_eq!(palette_index("colorx"), None);
        assert_eq!(palette_index("foreground"), None);
    }

    #[test]
    fn parse_reads_full_color_payload() {
        let spec = parse_ok(
            "name = Test\n\
             appearance = light\n\
             foreground = #112233\n\
             background = #445566\n\
             clear = #010203\n\
             cursor = #aabbcc\n\
             selection = #ddeeff\n\
             search = #102030\n\
             border = #405060\n\
             inactive = #708090\n\
             color0 = #000000\n\
             color15 = #ffffff\n",
        );
        assert_eq!(spec.name, "Test");
        assert_eq!(spec.appearance, Appearance::Light);
        assert_eq!(spec.foreground, (0x11, 0x22, 0x33));
        assert_eq!(spec.background, (0x44, 0x55, 0x66));
        assert_eq!(spec.clear, (0x01, 0x02, 0x03));
        assert_eq!(spec.cursor, (0xAA, 0xBB, 0xCC));
        assert_eq!(spec.selection, (0xDD, 0xEE, 0xFF));
        assert_eq!(spec.search, (0x10, 0x20, 0x30));
        assert_eq!(spec.border, (0x40, 0x50, 0x60));
        assert_eq!(spec.inactive, (0x70, 0x80, 0x90));
        assert_eq!(spec.palette[0], (0, 0, 0));
        assert_eq!(spec.palette[15], (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn parse_accepts_aliases_and_comments() {
        let spec = parse_ok(
            "# a comment\n\
             fg = #111111  # trailing comment\n\
             bg = #222222\n\
             palette3 = #333333\n",
        );
        assert_eq!(spec.foreground, (0x11, 0x11, 0x11));
        assert_eq!(spec.background, (0x22, 0x22, 0x22));
        assert_eq!(spec.palette[3], (0x33, 0x33, 0x33));
    }

    #[test]
    fn parse_missing_keys_keep_plain_defaults() {
        let spec = parse_ok("background = #010101\n");
        // Only background changed; everything else stays at the plain baseline.
        assert_eq!(spec.background, (0x01, 0x01, 0x01));
        assert_eq!(spec.foreground, Theme::PLAIN.foreground);
        assert_eq!(spec.palette, Theme::PLAIN.palette);
        assert_eq!(spec.cursor, Theme::PLAIN.cursor);
    }

    #[test]
    fn parse_unknown_keys_warn_but_do_not_abort() {
        let mut warnings = Vec::new();
        let spec = ThemeSpec::parse(
            "background = #010101\n\
             future_feature = wow\n\
             color0 = #020202\n",
            |m| warnings.push(m),
        );
        // The valid lines still applied around the unknown one.
        assert_eq!(spec.background, (0x01, 0x01, 0x01));
        assert_eq!(spec.palette[0], (0x02, 0x02, 0x02));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown key"));
    }

    #[test]
    fn parse_bad_value_warns_and_keeps_default() {
        let mut warnings = Vec::new();
        let spec = ThemeSpec::parse(
            "foreground = not-a-color\n\
             font_size = huge\n",
            |m| warnings.push(m),
        );
        assert_eq!(spec.foreground, Theme::PLAIN.foreground);
        assert_eq!(spec.font_size, None);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn parse_optional_font_and_visual_hints() {
        let spec = parse_ok(
            "font_family = JetBrains Mono\n\
             font_size = 14\n\
             visual = ambient\n",
        );
        assert_eq!(spec.font_family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(spec.font_size, Some(14.0));
        assert_eq!(spec.visual, VisualEffect::Ambient);
    }

    #[test]
    fn empty_font_family_value_is_none() {
        let spec = parse_ok("font_family =\n");
        assert_eq!(spec.font_family, None);
    }

    #[test]
    fn round_trip_is_a_fixed_point_for_each_builtin() {
        for theme in Theme::ALL {
            let spec = ThemeSpec::from_theme(&theme);
            let text = spec.serialize();
            let reparsed = ThemeSpec::parse(&text, |m| panic!("warn: {m}"));
            assert_eq!(reparsed, spec, "round-trip changed {}", theme.name);
            // Serializing the reparse is byte-stable too.
            assert_eq!(
                reparsed.serialize(),
                text,
                "serialize not stable for {}",
                theme.name
            );
        }
    }

    #[test]
    fn round_trip_preserves_optional_fields() {
        let mut spec = ThemeSpec::from_theme(&Theme::ODYSSEY);
        spec.name = "My Odyssey".to_string();
        spec.appearance = Appearance::Light;
        spec.font_family = Some("Iosevka".to_string());
        spec.font_size = Some(13.5);
        spec.visual = VisualEffect::Ambient;
        let reparsed = ThemeSpec::parse(&spec.serialize(), |m| panic!("warn: {m}"));
        assert_eq!(reparsed, spec);
    }

    #[test]
    fn to_theme_projects_color_payload() {
        let spec = ThemeSpec::from_theme(&Theme::ODYSSEY);
        let theme = spec.to_theme();
        assert_eq!(theme.foreground, Theme::ODYSSEY.foreground);
        assert_eq!(theme.palette, Theme::ODYSSEY.palette);
        assert_eq!(theme.cursor, Theme::ODYSSEY.cursor);
        // A spec named after a built-in keeps that static name.
        assert_eq!(theme.name, "odyssey");
    }

    #[test]
    fn to_theme_user_name_projects_to_custom() {
        let spec = parse_ok("name = My Personal Theme\nbackground = #010101\n");
        let theme = spec.to_theme();
        assert_eq!(theme.name, "custom");
        assert_eq!(theme.background, (0x01, 0x01, 0x01));
    }

    #[test]
    fn builtins_survive_the_format_with_identical_colors() {
        // The shared-code-path guarantee: every built-in is fully expressible
        // in the theme file format and re-projects to its exact color payload.
        for theme in Theme::ALL {
            let text = ThemeSpec::from_theme(&theme).serialize();
            let projected = ThemeSpec::parse(&text, |m| panic!("warn: {m}")).to_theme();
            assert_eq!(projected, theme, "{} full projection", theme.name);
        }
    }
}
