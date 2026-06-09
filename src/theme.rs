//! Odyssey theme system (presentation only).
//!
//! A theme is a tiny, source-agnostic bundle of *presentation* colors: the
//! default foreground/background used when a cell carries `Color::Default`, plus
//! the window clear color. Themes deliberately do **not** touch terminal
//! semantics — escape-sequence parsing, the cell grid, and SGR attributes are
//! all theme-unaware. Swapping themes can only change how `Color::Default` and
//! the empty surface are painted; it can never change what the terminal core
//! considers the screen contents to be.
//!
//! ## Selection
//!
//! The active theme is chosen by name, defaulting to the [`plain`](Theme::PLAIN)
//! baseline. [`Theme::from_env`] reads the `ODYTTY_THEME` environment variable;
//! an unset, empty, or unrecognized value falls back to plain so the terminal is
//! always readable regardless of configuration. [`Theme::from_name`] is the pure
//! selector used by callers that already hold a name (and by tests), so theme
//! resolution can be exercised without mutating process environment.

/// An sRGB color triple (8-bit per channel), matching the palette byte form used
/// by the text renderer. Kept as a plain tuple so this module stays free of any
/// rendering-backend types.
pub type Srgb = (u8, u8, u8);

/// A presentation theme: default cell colors plus the window clear color.
///
/// Colors are sRGB bytes. Rendering converts them to linear space at the
/// boundary; this module stays backend-agnostic on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Stable identifier, also the `ODYTTY_THEME` value that selects it.
    pub name: &'static str,
    /// Default foreground for cells whose attribute is `Color::Default`.
    pub foreground: Srgb,
    /// Default background for cells whose attribute is `Color::Default`.
    pub background: Srgb,
    /// Color the window surface is cleared to before cells are drawn. Usually
    /// equal to `background`, but kept separate so a theme can frame the grid.
    pub clear: Srgb,
}

impl Theme {
    /// Plain baseline. Matches the renderer's historical hardcoded defaults so
    /// selecting `plain` (or providing no/invalid `ODYTTY_THEME`) reproduces the
    /// pre-theme appearance exactly.
    pub const PLAIN: Theme = Theme {
        name: "plain",
        foreground: (0xCC, 0xCC, 0xCC),
        background: (0x0B, 0x0C, 0x10),
        clear: (0x0B, 0x0C, 0x10),
    };

    /// Odyssey default: a deep blue-black field with cool off-white text. The
    /// clear color is a touch darker than the cell background so the grid reads
    /// as a panel floating on the surface.
    pub const ODYSSEY: Theme = Theme {
        name: "odyssey",
        foreground: (0xD6, 0xDE, 0xF4),
        background: (0x0C, 0x12, 0x24),
        clear: (0x07, 0x0B, 0x18),
    };

    /// Odyssey Noir: a near-black, low-chroma variant for high-contrast text on
    /// a very dark field.
    pub const ODYSSEY_NOIR: Theme = Theme {
        name: "odyssey-noir",
        foreground: (0xCE, 0xD6, 0xD2),
        background: (0x05, 0x07, 0x08),
        clear: (0x02, 0x03, 0x04),
    };

    /// Every built-in theme, in selection-listing order (baseline first).
    pub const ALL: [Theme; 3] = [Theme::PLAIN, Theme::ODYSSEY, Theme::ODYSSEY_NOIR];

    /// Resolve a theme by name. Matching is case-insensitive and ignores
    /// surrounding whitespace. Returns `None` for an unknown name so callers can
    /// decide their own fallback; use [`Theme::from_name_or_default`] for the
    /// plain-fallback convenience.
    pub fn from_name(name: &str) -> Option<Theme> {
        let key = name.trim().to_ascii_lowercase();
        Theme::ALL.into_iter().find(|theme| theme.name == key)
    }

    /// Resolve a theme by name, falling back to the [`plain`](Theme::PLAIN)
    /// baseline for an unknown or empty name.
    pub fn from_name_or_default(name: &str) -> Theme {
        Theme::from_name(name).unwrap_or(Theme::PLAIN)
    }

    /// Select the theme named by the `ODYTTY_THEME` environment variable,
    /// defaulting to the plain baseline when unset, empty, or unrecognized.
    pub fn from_env() -> Theme {
        match std::env::var("ODYTTY_THEME") {
            Ok(value) => Theme::from_name_or_default(&value),
            Err(_) => Theme::PLAIN,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::PLAIN
    }
}

/// Optional Odyssey visual treatment, selected by `ODYTTY_VISUAL`.
///
/// This is a presentation-only effect layered on top of the rendered grid. It
/// never changes terminal cell contents or attributes — the core is unaware it
/// exists — and it is fully disableable. The default is [`Off`](Self::Off),
/// which produces output pixel-identical to having no effect at all.
///
/// [`Ambient`](Self::Ambient) applies a faint static scanline wash to cell
/// *backgrounds only* (glyph coverage is untouched, so text stays crisp). It is
/// static (no animation), cheap (a few ALU ops per fragment), and subtle by
/// design so readability is never compromised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisualEffect {
    /// No visual treatment. Rendering is identical to the pre-effect path.
    #[default]
    Off,
    /// Faint static scanline wash over backgrounds.
    Ambient,
}

impl VisualEffect {
    /// Scanline darkening strength for [`Ambient`](Self::Ambient): the maximum
    /// fraction by which a background fragment is darkened at a scanline trough.
    /// Kept small so the effect is felt, not read. `Off` is always `0.0`, which
    /// makes the shader a no-op.
    const AMBIENT_STRENGTH: f32 = 0.06;
    /// Scanline period in physical pixels for [`Ambient`](Self::Ambient).
    const AMBIENT_PERIOD_PX: f32 = 3.0;

    /// Resolve an effect by name (case-insensitive, whitespace-trimmed).
    ///
    /// `off`/`none`/`plain` map to [`Off`](Self::Off); `ambient`/`scanlines`
    /// map to [`Ambient`](Self::Ambient). Returns `None` for anything else so
    /// callers choose their own fallback.
    pub fn from_name(name: &str) -> Option<VisualEffect> {
        match name.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "plain" => Some(VisualEffect::Off),
            "ambient" | "scanlines" => Some(VisualEffect::Ambient),
            _ => None,
        }
    }

    /// Resolve an effect by name, falling back to [`Off`](Self::Off) for an
    /// unknown or empty name.
    pub fn from_name_or_default(name: &str) -> VisualEffect {
        VisualEffect::from_name(name).unwrap_or(VisualEffect::Off)
    }

    /// Select the effect named by `ODYTTY_VISUAL`, defaulting to off when unset,
    /// empty, or unrecognized.
    pub fn from_env() -> VisualEffect {
        match std::env::var("ODYTTY_VISUAL") {
            Ok(value) => VisualEffect::from_name_or_default(&value),
            Err(_) => VisualEffect::Off,
        }
    }

    /// Whether any visual treatment is active.
    pub fn is_enabled(self) -> bool {
        !matches!(self, VisualEffect::Off)
    }

    /// Scanline strength fed to the renderer (`0.0` when off → shader no-op).
    pub fn scanline_strength(self) -> f32 {
        match self {
            VisualEffect::Off => 0.0,
            VisualEffect::Ambient => Self::AMBIENT_STRENGTH,
        }
    }

    /// Scanline period in physical pixels fed to the renderer.
    pub fn scanline_period_px(self) -> f32 {
        match self {
            // Period is irrelevant when off, but keep it positive so the shader
            // never risks a divide-by-zero regardless of branch.
            VisualEffect::Off => Self::AMBIENT_PERIOD_PX,
            VisualEffect::Ambient => Self::AMBIENT_PERIOD_PX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_plain_baseline() {
        assert_eq!(Theme::default(), Theme::PLAIN);
        assert_eq!(Theme::default().name, "plain");
    }

    #[test]
    fn from_name_resolves_known_themes() {
        assert_eq!(Theme::from_name("plain"), Some(Theme::PLAIN));
        assert_eq!(Theme::from_name("odyssey"), Some(Theme::ODYSSEY));
        assert_eq!(Theme::from_name("odyssey-noir"), Some(Theme::ODYSSEY_NOIR));
    }

    #[test]
    fn from_name_is_case_and_whitespace_insensitive() {
        assert_eq!(Theme::from_name("  ODYSSEY  "), Some(Theme::ODYSSEY));
        assert_eq!(Theme::from_name("Odyssey-Noir"), Some(Theme::ODYSSEY_NOIR));
    }

    #[test]
    fn from_name_rejects_unknown() {
        assert_eq!(Theme::from_name("nope"), None);
        assert_eq!(Theme::from_name(""), None);
    }

    #[test]
    fn from_name_or_default_falls_back_to_plain() {
        assert_eq!(Theme::from_name_or_default("nope"), Theme::PLAIN);
        assert_eq!(Theme::from_name_or_default(""), Theme::PLAIN);
        // A valid name still resolves to the requested theme.
        assert_eq!(Theme::from_name_or_default("odyssey"), Theme::ODYSSEY);
    }

    #[test]
    fn theme_names_are_unique_and_match_listing() {
        for theme in Theme::ALL {
            assert_eq!(
                Theme::from_name(theme.name),
                Some(theme),
                "{} must resolve to itself",
                theme.name
            );
        }
    }

    #[test]
    fn plain_matches_renderer_historical_defaults() {
        // Guards against drift between the plain baseline and the text
        // renderer's documented defaults.
        assert_eq!(Theme::PLAIN.foreground, crate::text::DEFAULT_FG_SRGB);
        assert_eq!(Theme::PLAIN.background, crate::text::DEFAULT_BG_SRGB);
    }

    #[test]
    fn visual_effect_defaults_to_off() {
        assert_eq!(VisualEffect::default(), VisualEffect::Off);
        assert!(!VisualEffect::default().is_enabled());
    }

    #[test]
    fn visual_effect_from_name_resolves_known_values() {
        assert_eq!(VisualEffect::from_name("off"), Some(VisualEffect::Off));
        assert_eq!(VisualEffect::from_name("none"), Some(VisualEffect::Off));
        assert_eq!(VisualEffect::from_name("plain"), Some(VisualEffect::Off));
        assert_eq!(
            VisualEffect::from_name("ambient"),
            Some(VisualEffect::Ambient)
        );
        assert_eq!(
            VisualEffect::from_name("scanlines"),
            Some(VisualEffect::Ambient)
        );
    }

    #[test]
    fn visual_effect_from_name_is_case_and_whitespace_insensitive() {
        assert_eq!(
            VisualEffect::from_name("  AMBIENT  "),
            Some(VisualEffect::Ambient)
        );
        assert_eq!(VisualEffect::from_name("Off"), Some(VisualEffect::Off));
    }

    #[test]
    fn visual_effect_invalid_falls_back_to_off() {
        assert_eq!(VisualEffect::from_name("sparkles"), None);
        assert_eq!(VisualEffect::from_name(""), None);
        assert_eq!(
            VisualEffect::from_name_or_default("sparkles"),
            VisualEffect::Off
        );
        assert_eq!(VisualEffect::from_name_or_default(""), VisualEffect::Off);
        // A valid name still resolves.
        assert_eq!(
            VisualEffect::from_name_or_default("ambient"),
            VisualEffect::Ambient
        );
    }

    #[test]
    fn off_effect_has_zero_strength_and_ambient_is_subtle() {
        // Off must be a true no-op (zero strength → shader identity).
        assert_eq!(VisualEffect::Off.scanline_strength(), 0.0);
        assert!(!VisualEffect::Off.is_enabled());
        // Ambient is enabled, positive, and bounded to a subtle range.
        assert!(VisualEffect::Ambient.is_enabled());
        let strength = VisualEffect::Ambient.scanline_strength();
        assert!(strength > 0.0 && strength <= 0.15, "strength={strength}");
        // Period is always positive (no divide-by-zero in the shader).
        assert!(VisualEffect::Off.scanline_period_px() > 0.0);
        assert!(VisualEffect::Ambient.scanline_period_px() > 0.0);
    }
}
