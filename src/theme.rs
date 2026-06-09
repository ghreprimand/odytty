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
}
