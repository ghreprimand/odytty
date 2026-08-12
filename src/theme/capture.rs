// SPDX-License-Identifier: GPL-3.0-only
//! Capture a theme draft from a pane's live colors.
//!
//! A running pane's colors are not always the theme's colors. Any program can
//! repaint the terminal at runtime with the dynamic-color escape sequences —
//! `OSC 4` overrides one of the 256 palette slots, `OSC 10`/`11`/`12` override
//! the default foreground, background, and cursor. Shell prompts, `vim`
//! colorschemes, `tmux` configs, and `dircolors` setups all do this. The result
//! is a look the user can see but cannot save, because it exists only as live
//! terminal state.
//!
//! This module closes that gap: it turns the **effective** color state of a
//! pane — the colors actually on screen, live override first and the
//! theme-seeded value where no override exists — into a [`ThemeSpec`] draft
//! that the theme editor can name, tweak, and save.
//!
//! ## What is captured directly
//!
//! Foreground, background, cursor, and the 16 ANSI palette entries come
//! straight from the pane. Nothing is inferred for them: whatever the pane is
//! displaying is what lands in the draft.
//!
//! ## What is derived, and how
//!
//! A theme carries five roles the terminal protocol has no way to express, so
//! no live value exists to capture: `clear`, `selection`, `search`, `border`,
//! and `inactive`. They are derived from the captured colors with the
//! luminance-based heuristics below. Each is a *starting point*: the flow hands
//! the draft to the theme editor, where every role is editable before saving,
//! so the heuristics need to be explainable and sane rather than perfect.
//!
//! Every heuristic works in the same direction — "away from the background" for
//! something that must be visible against it, "toward the background" for
//! something that must recede — using [`relative_luminance`] to decide which
//! way that is. A light background darkens; a dark background lightens. This is
//! why they behave correctly for light and dark themes without a separate
//! branch per case.
//!
//! | Role | Derivation |
//! |------|------------|
//! | `clear` | The captured background, unchanged. The window surface matches the cell field unless a theme deliberately frames the grid, and a capture has no way to know a frame was wanted. |
//! | `selection` | The background moved 22% toward the foreground. Enough to read as a highlight band, not enough to fight the text drawn over it. |
//! | `search` | The captured yellow (palette index 3) blended 45% into the background. Search highlights read as a distinct warm band in every major terminal; blending toward the background keeps it from overpowering the field. |
//! | `border` | The background moved 12% toward the foreground — a low-contrast structural line, visible as an edge without drawing the eye. |
//! | `inactive` | The midpoint between foreground and background. Legible but clearly recessive, which is what inactive tab/workspace chrome needs. |
//!
//! Appearance (light or dark) is classified from the background's relative
//! luminance against the same 0.18 threshold the theme editor already uses when
//! cloning a theme, so a captured draft and a cloned draft agree.
//!
//! ## Platform surface
//!
//! Platform-neutral. This is pure color arithmetic over values the terminal
//! core already holds; it touches no filesystem, process, or window-system
//! state, and behaves identically on Linux, macOS, and Windows.

use super::{Appearance, Srgb, ThemeSpec, VisualEffect, relative_luminance};

/// Background relative-luminance above which a captured draft is classified as
/// a light theme. Matches the theme editor's clone path so the two entry points
/// agree on the same colors.
const LIGHT_APPEARANCE_LUMINANCE: f64 = 0.18;

/// How far `selection` moves from the background toward the foreground.
const SELECTION_MIX: f32 = 0.22;

/// How far `border` moves from the background toward the foreground.
const BORDER_MIX: f32 = 0.12;

/// How far the captured yellow is blended into the background for `search`.
const SEARCH_MIX: f32 = 0.45;

/// How far `inactive` sits between background (0.0) and foreground (1.0).
const INACTIVE_MIX: f32 = 0.5;

/// ANSI palette index the `search` heuristic samples. Index 3 is yellow in
/// every ANSI palette, which is the conventional search-highlight hue.
const YELLOW_INDEX: usize = 3;

/// The effective color state of one pane: what it is actually displaying.
///
/// "Effective" is the whole point. Each field must already have live dynamic
/// overrides resolved against theme-seeded values by the caller — a captured
/// draft that reproduced the *theme* rather than the *screen* would defeat the
/// feature. [`crate::core::Terminal::effective_colors`] produces exactly this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveColors {
    /// Effective default foreground (`OSC 10` override, else theme-seeded).
    pub foreground: Srgb,
    /// Effective default background (`OSC 11` override, else theme-seeded).
    pub background: Srgb,
    /// Effective cursor color (`OSC 12` override, else theme-seeded).
    pub cursor: Srgb,
    /// The effective 16 ANSI colors (`OSC 4` overrides, else theme-seeded).
    pub palette: [Srgb; 16],
}

/// Build a theme draft from a pane's live colors.
///
/// `name` is the draft's starting display name; the theme editor prompts for
/// the final name before saving, so this only has to be a reasonable default.
/// The returned spec is a plain value — capturing writes nothing and changes no
/// live state.
pub fn capture_spec(live: &LiveColors, name: &str) -> ThemeSpec {
    let foreground = live.foreground;
    let background = live.background;

    ThemeSpec {
        name: name.to_owned(),
        appearance: if relative_luminance(background) > LIGHT_APPEARANCE_LUMINANCE {
            Appearance::Light
        } else {
            Appearance::Dark
        },
        foreground,
        background,
        // Captured directly, no inference: see the module docs.
        clear: background,
        palette: live.palette,
        cursor: live.cursor,
        selection: mix(background, foreground, SELECTION_MIX),
        search: mix(live.palette[YELLOW_INDEX], background, SEARCH_MIX),
        border: mix(background, foreground, BORDER_MIX),
        inactive: mix(background, foreground, INACTIVE_MIX),
        font_family: None,
        font_size: None,
        visual: VisualEffect::default(),
    }
}

/// Linear interpolation between two sRGB colors, `t` of the way from `from` to
/// `to`. Deliberately mixes in sRGB byte space rather than a perceptual space:
/// the derived roles are starting points a user immediately edits, and a plain
/// mix is the version whose result someone can predict from the two inputs.
/// `t` is clamped, so a caller cannot push a channel out of range.
fn mix(from: Srgb, to: Srgb, t: f32) -> Srgb {
    let t = t.clamp(0.0, 1.0);
    (
        mix_channel(from.0, to.0, t),
        mix_channel(from.1, to.1, t),
        mix_channel(from.2, to.2, t),
    )
}

fn mix_channel(from: u8, to: u8, t: f32) -> u8 {
    let value = f32::from(from) + (f32::from(to) - f32::from(from)) * t;
    value.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn live_from(theme: &Theme) -> LiveColors {
        LiveColors {
            foreground: theme.foreground,
            background: theme.background,
            cursor: theme.cursor,
            palette: theme.palette,
        }
    }

    #[test]
    fn captured_colors_are_copied_verbatim() {
        let live = LiveColors {
            foreground: (0x11, 0x22, 0x33),
            background: (0x44, 0x55, 0x66),
            cursor: (0x77, 0x88, 0x99),
            palette: [(0xAB, 0xCD, 0xEF); 16],
        };
        let spec = capture_spec(&live, "captured");
        assert_eq!(spec.name, "captured");
        assert_eq!(spec.foreground, live.foreground);
        assert_eq!(spec.background, live.background);
        assert_eq!(spec.cursor, live.cursor);
        assert_eq!(spec.palette, live.palette);
        // `clear` is captured, not derived.
        assert_eq!(spec.clear, live.background);
    }

    #[test]
    fn derived_roles_sit_between_background_and_foreground() {
        let spec = capture_spec(&live_from(&Theme::ODYSSEY), "draft");
        let bg = relative_luminance(spec.background);
        let fg = relative_luminance(spec.foreground);
        for (role, color) in [
            ("selection", spec.selection),
            ("border", spec.border),
            ("inactive", spec.inactive),
        ] {
            let luminance = relative_luminance(color);
            assert!(
                luminance >= bg - 1e-6 && luminance <= fg + 1e-6,
                "{role} luminance {luminance} escaped the bg..fg range {bg}..{fg}"
            );
        }
    }

    #[test]
    fn derived_roles_are_ordered_by_prominence() {
        // border is the most recessive structural line, selection reads as a
        // band, inactive sits midway to the foreground. On a dark theme that
        // means strictly increasing luminance.
        let spec = capture_spec(&live_from(&Theme::ODYSSEY), "draft");
        let border = relative_luminance(spec.border);
        let selection = relative_luminance(spec.selection);
        let inactive = relative_luminance(spec.inactive);
        assert!(
            border < selection,
            "border {border} >= selection {selection}"
        );
        assert!(
            selection < inactive,
            "selection {selection} >= inactive {inactive}"
        );
    }

    #[test]
    fn heuristics_invert_for_a_light_background() {
        // The same code must move roles the other way when the field is light;
        // there is no light/dark branch, the direction falls out of mixing
        // toward the foreground.
        let live = LiveColors {
            foreground: (0x20, 0x20, 0x20),
            background: (0xF5, 0xF5, 0xF5),
            cursor: (0x20, 0x20, 0x20),
            palette: [(0x80, 0x80, 0x80); 16],
        };
        let spec = capture_spec(&live, "light");
        assert_eq!(spec.appearance, Appearance::Light);
        assert!(relative_luminance(spec.selection) < relative_luminance(spec.background));
        assert!(relative_luminance(spec.border) > relative_luminance(spec.selection));
        assert!(relative_luminance(spec.inactive) < relative_luminance(spec.selection));
    }

    #[test]
    fn appearance_follows_background_luminance() {
        let dark = capture_spec(&live_from(&Theme::ODYSSEY), "d");
        assert_eq!(dark.appearance, Appearance::Dark);

        let live = LiveColors {
            foreground: (0x00, 0x00, 0x00),
            background: (0xFF, 0xFF, 0xFF),
            cursor: (0x00, 0x00, 0x00),
            palette: [(0x40, 0x40, 0x40); 16],
        };
        assert_eq!(capture_spec(&live, "l").appearance, Appearance::Light);
    }

    #[test]
    fn search_is_pulled_from_the_captured_yellow() {
        let mut live = live_from(&Theme::ODYSSEY);
        live.palette[YELLOW_INDEX] = (0xFF, 0xFF, 0x00);
        let spec = capture_spec(&live, "draft");
        // Blended toward the background, so it is neither the raw yellow nor
        // the background itself.
        assert_ne!(spec.search, (0xFF, 0xFF, 0x00));
        assert_ne!(spec.search, spec.background);
        // The red and green channels still dominate blue: it is recognizably
        // the captured yellow.
        assert!(spec.search.0 > spec.search.2 && spec.search.1 > spec.search.2);
    }

    #[test]
    fn capture_is_deterministic_and_pure() {
        let live = live_from(&Theme::ODYSSEY);
        assert_eq!(capture_spec(&live, "a"), capture_spec(&live, "a"));
    }

    #[test]
    fn identical_foreground_and_background_do_not_panic() {
        let flat = (0x808080u32 >> 16) as u8;
        let live = LiveColors {
            foreground: (flat, flat, flat),
            background: (flat, flat, flat),
            cursor: (flat, flat, flat),
            palette: [(flat, flat, flat); 16],
        };
        let spec = capture_spec(&live, "flat");
        assert_eq!(spec.selection, spec.background);
        assert_eq!(spec.border, spec.background);
        assert_eq!(spec.inactive, spec.background);
    }

    #[test]
    fn extreme_channel_values_stay_in_gamut() {
        let live = LiveColors {
            foreground: (0xFF, 0xFF, 0xFF),
            background: (0x00, 0x00, 0x00),
            cursor: (0xFF, 0x00, 0xFF),
            palette: [(0xFF, 0xFF, 0xFF); 16],
        };
        // Every channel is a u8 by construction; the assertion is that the
        // round-and-clamp path produces the expected endpoints rather than
        // wrapping.
        let spec = capture_spec(&live, "extreme");
        assert_eq!(spec.inactive, (0x80, 0x80, 0x80));
        assert_eq!(spec.clear, (0x00, 0x00, 0x00));
    }
}
