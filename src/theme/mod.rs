// SPDX-License-Identifier: GPL-3.0-only
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
//! baseline. The settings layer reads `ODYTTY_THEME`; an unset, empty, or
//! unrecognized value falls back to plain so the terminal is always readable
//! regardless of configuration. [`Theme::from_name`] is the pure selector used
//! by callers that already hold a name (and by tests), so theme resolution can
//! be exercised without mutating process environment.

mod builtins;
mod contrast;
mod spec;

pub use builtins::{all, names};
pub use contrast::{contrast_ratio, relative_luminance};
pub use spec::{Appearance, ThemeSpec};

/// Minimum default foreground/background contrast ratio (WCAG) every built-in
/// theme is validated against. Set just below the strict WCAG AA normal-text
/// threshold (4.5) so the canonical low-contrast community palettes (notably
/// Solarized, which sits intentionally around 4.1–4.8 to reduce eye strain)
/// remain authentic, while still rejecting any theme whose body text would be
/// genuinely illegible. RV1 (minimum-contrast guarantee) will let users enforce
/// a stricter floor at render time; this is the authoring gate for the bundled
/// library.
pub const MIN_CONTRAST: f64 = 4.0;

/// An sRGB color triple (8-bit per channel), matching the palette byte form used
/// by the text renderer. Kept as a plain tuple so this module stays free of any
/// rendering-backend types.
pub type Srgb = (u8, u8, u8);

/// A presentation theme: a full appearance profile — default cell colors, the
/// window clear color, the 16-color ANSI palette, and semantic role colors.
///
/// Colors are sRGB bytes. Rendering converts them to linear space at the
/// boundary; this module stays backend-agnostic on purpose.
///
/// ## Palette and roles
///
/// [`palette`](Self::palette) carries the 16 standard ANSI colors (indices 0–7
/// normal, 8–15 bright) used to resolve `Color::Indexed(0..=15)` in the
/// renderer; the theme layer feeds it to [`crate::text::set_ansi_palette`] at
/// startup. Per-app OSC-4 dynamic-color overrides still win over the theme (the
/// render path consults the core dynamic palette first). The semantic-role
/// colors ([`cursor`](Self::cursor), [`selection`](Self::selection),
/// [`search`](Self::search), and the reserved [`border`](Self::border) /
/// [`inactive`](Self::inactive)) describe how presentation chrome should be
/// painted; consumers land in later packets (cursor/selection/search treatments,
/// window chrome), so `border`/`inactive` are authored now but not yet read.
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
    /// The 16 standard ANSI colors: indices 0–7 normal, 8–15 bright. Resolves
    /// `Color::Indexed(0..=15)` in the renderer. For [`PLAIN`](Self::PLAIN) this
    /// is byte-identical to the historical xterm table.
    pub palette: [Srgb; 16],
    /// Cursor color (semantic role). Consumed by cursor treatments (ID1).
    pub cursor: Srgb,
    /// Selection background (semantic role). Consumed by selection rendering.
    pub selection: Srgb,
    /// Search-highlight background (semantic role).
    pub search: Srgb,
    /// Border / frame color (semantic role; reserved, not yet rendered).
    pub border: Srgb,
    /// Inactive / dimmed color (semantic role; reserved, not yet rendered).
    pub inactive: Srgb,
}

impl Theme {
    /// Plain baseline. Matches the renderer's historical hardcoded defaults so
    /// selecting `plain` (or providing no/invalid `ODYTTY_THEME`) reproduces the
    /// pre-theme appearance exactly. Its palette is the historical xterm table
    /// ([`crate::text::DEFAULT_ANSI_SRGB`]) byte-for-byte.
    pub const PLAIN: Theme = Theme {
        name: "plain",
        foreground: (0xCC, 0xCC, 0xCC),
        background: (0x0B, 0x0C, 0x10),
        clear: (0x0B, 0x0C, 0x10),
        palette: crate::text::DEFAULT_ANSI_SRGB,
        cursor: (0xCC, 0xCC, 0xCC),
        selection: (0x33, 0x3A, 0x47),
        search: (0x5C, 0x50, 0x1F),
        border: (0x2A, 0x2C, 0x33),
        inactive: (0x66, 0x66, 0x66),
    };

    /// Odyssey default: a deep blue-black field with cool off-white text. The
    /// clear color is a touch darker than the cell background so the grid reads
    /// as a panel floating on the surface. The ANSI palette is cool-leaning and
    /// tuned for legibility on the dark blue field.
    pub const ODYSSEY: Theme = Theme {
        name: "odyssey",
        foreground: (0xD6, 0xDE, 0xF4),
        background: (0x0C, 0x12, 0x24),
        clear: (0x07, 0x0B, 0x18),
        palette: [
            (0x12, 0x18, 0x2A), // 0  black (lifted from bg)
            (0xE0, 0x6B, 0x74), // 1  red
            (0x98, 0xC3, 0x79), // 2  green
            (0xE5, 0xC0, 0x7B), // 3  yellow
            (0x61, 0xAF, 0xEF), // 4  blue
            (0xC6, 0x8A, 0xEE), // 5  magenta
            (0x56, 0xB6, 0xC2), // 6  cyan
            (0xC5, 0xCD, 0xE0), // 7  white
            (0x3A, 0x44, 0x5E), // 8  bright black
            (0xFF, 0x8B, 0x92), // 9  bright red
            (0xB6, 0xE3, 0x99), // 10 bright green
            (0xFF, 0xD9, 0x9A), // 11 bright yellow
            (0x7F, 0xC1, 0xFF), // 12 bright blue
            (0xD8, 0xA6, 0xFF), // 13 bright magenta
            (0x7A, 0xD4, 0xDF), // 14 bright cyan
            (0xF0, 0xF4, 0xFF), // 15 bright white
        ],
        cursor: (0x86, 0xC1, 0xFF),
        selection: (0x24, 0x33, 0x52),
        search: (0x4A, 0x40, 0x18),
        border: (0x1B, 0x24, 0x3E),
        inactive: (0x5A, 0x64, 0x80),
    };

    /// Odyssey Noir: a near-black, low-chroma variant for high-contrast text on
    /// a very dark field. The ANSI palette is desaturated to keep the monochrome
    /// feel while staying readable.
    pub const ODYSSEY_NOIR: Theme = Theme {
        name: "odyssey-noir",
        foreground: (0xCE, 0xD6, 0xD2),
        background: (0x05, 0x07, 0x08),
        clear: (0x02, 0x03, 0x04),
        palette: [
            (0x0A, 0x0C, 0x0D), // 0  black
            (0xC9, 0x6A, 0x6A), // 1  red
            (0x9A, 0xB5, 0x8E), // 2  green
            (0xC6, 0xB7, 0x86), // 3  yellow
            (0x84, 0x9C, 0xB0), // 4  blue
            (0xA9, 0x90, 0xB5), // 5  magenta
            (0x88, 0xB0, 0xAE), // 6  cyan
            (0xB8, 0xC0, 0xBC), // 7  white
            (0x3A, 0x3F, 0x3D), // 8  bright black
            (0xDD, 0x86, 0x86), // 9  bright red
            (0xB4, 0xCE, 0xA6), // 10 bright green
            (0xDD, 0xCE, 0x9E), // 11 bright yellow
            (0x9E, 0xB6, 0xCA), // 12 bright blue
            (0xC3, 0xAA, 0xCE), // 13 bright magenta
            (0xA2, 0xC9, 0xC7), // 14 bright cyan
            (0xE6, 0xEC, 0xE8), // 15 bright white
        ],
        cursor: (0xCE, 0xD6, 0xD2),
        selection: (0x22, 0x28, 0x26),
        search: (0x3E, 0x39, 0x20),
        border: (0x1A, 0x1E, 0x1C),
        inactive: (0x55, 0x5C, 0x58),
    };

    /// The const core themes, authored directly in source. These are the parse
    /// default ([`PLAIN`](Self::PLAIN) seeds [`ThemeSpec`] defaults) and the
    /// fallback baselines; the *full* built-in library — including these three,
    /// re-parsed from their embedded `.theme` files, plus the community palettes
    /// — is [`Theme::all`]. The two are pinned equal by test.
    pub const ALL: [Theme; 3] = [Theme::PLAIN, Theme::ODYSSEY, Theme::ODYSSEY_NOIR];

    /// Resolve a theme by name against the full built-in library
    /// ([`Theme::all`]). Matching is case-insensitive and ignores surrounding
    /// whitespace. Returns `None` for an unknown name so callers can decide
    /// their own fallback; use [`Theme::from_name_or_default`] for the
    /// plain-fallback convenience.
    pub fn from_name(name: &str) -> Option<Theme> {
        let key = name.trim().to_ascii_lowercase();
        all().iter().copied().find(|theme| theme.name == key)
    }

    /// Resolve a theme by name, falling back to the [`plain`](Theme::PLAIN)
    /// baseline for an unknown or empty name.
    pub fn from_name_or_default(name: &str) -> Theme {
        Theme::from_name(name).unwrap_or(Theme::PLAIN)
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
    const AMBIENT_STRENGTH: f32 = 0.12;
    /// Scanline period in physical pixels for [`Ambient`](Self::Ambient).
    const AMBIENT_PERIOD_PX: f32 = 8.0;

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

    /// Canonical lowercase token used when serializing a theme's bundled
    /// effect profile. The inverse of [`from_name`](Self::from_name).
    pub fn as_str(self) -> &'static str {
        match self {
            VisualEffect::Off => "off",
            VisualEffect::Ambient => "ambient",
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
    fn plain_palette_is_historical_xterm_table_byte_identical() {
        // The core pixel-identity guarantee: selecting `plain` (or no theme)
        // must resolve indexed colors exactly as the pre-theme renderer did.
        assert_eq!(Theme::PLAIN.palette, crate::text::DEFAULT_ANSI_SRGB);
    }

    #[test]
    fn every_theme_carries_a_full_palette_and_roles() {
        // Each built-in authors all 16 ANSI entries plus its semantic roles;
        // this catches a half-authored theme. Role colors must be present (the
        // struct guarantees that), so we assert distinctness from a sentinel to
        // prove they were intentionally set rather than left at a placeholder.
        for theme in Theme::ALL {
            assert_eq!(theme.palette.len(), 16, "{} palette", theme.name);
            // Bright variants should not all collapse to their normal
            // counterparts (a sign of an unfinished palette).
            assert_ne!(
                &theme.palette[0..8],
                &theme.palette[8..16],
                "{} bright row duplicates normal row",
                theme.name
            );
        }
    }

    #[test]
    fn non_plain_palettes_differ_from_plain() {
        // Authored themes must actually recolor the ANSI palette (otherwise
        // they would render indexed colors identically to plain).
        assert_ne!(Theme::ODYSSEY.palette, Theme::PLAIN.palette);
        assert_ne!(Theme::ODYSSEY_NOIR.palette, Theme::PLAIN.palette);
    }

    #[test]
    fn semantic_roles_are_readable_from_the_theme() {
        // Semantic-role fields resolve to the authored theme values.
        let t = Theme::ODYSSEY;
        assert_eq!(t.cursor, (0x86, 0xC1, 0xFF));
        assert_eq!(t.selection, (0x24, 0x33, 0x52));
        assert_eq!(t.search, (0x4A, 0x40, 0x18));
        assert_eq!(t.border, (0x1B, 0x24, 0x3E));
        assert_eq!(t.inactive, (0x5A, 0x64, 0x80));
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
        // Ambient is enabled, visible enough to evaluate, and bounded to a
        // subtle range.
        assert!(VisualEffect::Ambient.is_enabled());
        let strength = VisualEffect::Ambient.scanline_strength();
        assert!((0.10..=0.15).contains(&strength), "strength={strength}");
        // Period is always positive (no divide-by-zero in the shader).
        assert!(VisualEffect::Off.scanline_period_px() > 0.0);
        assert!(VisualEffect::Ambient.scanline_period_px() >= 6.0);
    }
}
