// SPDX-License-Identifier: GPL-3.0-only
//! Built-in theme library.
//!
//! Every built-in theme is authored as a `.theme` file in the TH2 file format
//! (see [`crate::theme::ThemeSpec`]) under `src/theme/builtins/`, embedded at
//! compile time with [`include_str!`], and loaded at runtime through the same
//! [`ThemeSpec::parse`] → project path that user theme files use — there is no
//! bespoke per-theme construction. The canonical `&'static` name comes from the
//! [`REGISTRY`] table (so it survives projection without a [`Theme::from_name`]
//! lookup), and the parsed color payload is projected via
//! [`ThemeSpec::to_theme_with_name`].
//!
//! The library is built once, lazily, into [`all`]. [`Theme::from_name`] and the
//! `ODYTTY_THEME` resolver consult it, so every built-in resolves by name.
//!
//! ## Roster
//!
//! Odyssey identity: `plain` (the baseline; palette byte-identical to the
//! historical xterm table), `odyssey`, `odyssey-noir`, `odyssey-light` (light),
//! `odyssey-aurora` (high-contrast), `odyssey-deepspace`, `odyssey-nebula`,
//! `odyssey-solar`, `odyssey-abyss`, `odyssey-ember`, `odyssey-glacier`,
//! `odyssey-meridian`, `odyssey-voyager`, `odyssey-pulsar`,
//! `odyssey-dawn-light` (light), `odyssey-sandstone-light` (light), and
//! `odyssey-graphite`, `odyssey-fathom`, `odyssey-harbor`, `odyssey-ion`,
//! `odyssey-orchard`, `odyssey-volcanic`, `odyssey-cloud-light` (light),
//! `odyssey-coral-light` (light), `odyssey-mist-light` (light),
//! `odyssey-twilight`, `odyssey-verdant`, `odyssey-quasar`,
//! `odyssey-meadow-light` (light), `odyssey-parchment-light` (light),
//! `odyssey-tidepool`, `odyssey-moss`, `odyssey-rosewood`, `odyssey-slate`,
//! `odyssey-blossom-light` (light), `odyssey-linen-light` (light),
//! `odyssey-garnet`, `odyssey-sepia`, `odyssey-cobalt`,
//! `odyssey-lilac-light` (light), `odyssey-pearl-light` (light),
//! `odyssey-apricot-light` (light), `odyssey-chartreuse`, `odyssey-violet`,
//! `odyssey-fuchsia`, `odyssey-butter-light` (light), `odyssey-sage-light`
//! (light), `odyssey-slate-light` (light), `odyssey-amber`, `odyssey-default`
//! (the shipped default; formerly `odyssey-jungle`, still a working alias),
//! `odyssey-orchid`, `odyssey-seafoam-light` (light), `odyssey-indigo`,
//! `odyssey-raspberry`, `odyssey-citrus-light` (light),
//! `odyssey-mauve-light` (light), `odyssey-terracotta`, `odyssey-harvest`,
//! `odyssey-lagoon`, `odyssey-clover-light` (light), `odyssey-midnight`,
//! `odyssey-sienna-light` (light), `odyssey-periwinkle-light` (light),
//! and `odyssey-pine`.
//!
//! Extended Odyssey originals: `odyssey-eclipse`, `odyssey-comet`,
//! `odyssey-obsidian`, `odyssey-jade`, `odyssey-basalt`, `odyssey-fjord`,
//! `odyssey-mangrove`, `odyssey-nocturne`, `odyssey-quartz-light` (light),
//! `odyssey-marble-light` (light), `odyssey-dune-light` (light), and
//! `odyssey-fern-light` (light).
//!
//! Community palettes (published values): `solarized-dark`, `gruvbox-dark`,
//! `nord`, `dracula`, `tokyo-night`, `catppuccin-mocha`, `one-dark`, `monokai`,
//! `everforest-dark`, `kanagawa`, `rose-pine`, `ayu-mirage`, `night-owl`,
//! `palenight`, `github-dark`, `zenburn`, `oceanic-next`, and `iceberg-dark`.
//! Light palettes: `solarized-light`, `catppuccin-latte`, `github-light`,
//! `gruvbox-light`, `one-light`, `ayu-light`, `rose-pine-dawn`,
//! `tokyo-night-day`, `papercolor-light`, and `everforest-light`.
//! Retro / phosphor palettes: `green-phosphor`, `amber-crt`, `ibm-5151`,
//! `dos-cga`, `apple-ii-green`, `commodore-64`, `hercules-amber`, and
//! `vt220-green`.

use std::sync::OnceLock;

use super::{Theme, ThemeSpec};

/// The built-in roster: `(canonical name, embedded .theme source)`. The name is
/// the authoritative `&'static` identifier `ODYTTY_THEME` matches and the name
/// projected into the runtime [`Theme`]; the file's own `name =` line is for
/// human authoring and round-trip fidelity.
const REGISTRY: &[(&str, &str)] = &[
    ("plain", include_str!("builtins/plain.theme")),
    ("odyssey", include_str!("builtins/odyssey.theme")),
    ("odyssey-noir", include_str!("builtins/odyssey-noir.theme")),
    (
        "odyssey-light",
        include_str!("builtins/odyssey-light.theme"),
    ),
    (
        "odyssey-aurora",
        include_str!("builtins/odyssey-aurora.theme"),
    ),
    (
        "odyssey-deepspace",
        include_str!("builtins/odyssey-deepspace.theme"),
    ),
    (
        "odyssey-nebula",
        include_str!("builtins/odyssey-nebula.theme"),
    ),
    (
        "odyssey-solar",
        include_str!("builtins/odyssey-solar.theme"),
    ),
    (
        "odyssey-abyss",
        include_str!("builtins/odyssey-abyss.theme"),
    ),
    (
        "odyssey-ember",
        include_str!("builtins/odyssey-ember.theme"),
    ),
    (
        "odyssey-glacier",
        include_str!("builtins/odyssey-glacier.theme"),
    ),
    (
        "odyssey-meridian",
        include_str!("builtins/odyssey-meridian.theme"),
    ),
    (
        "odyssey-voyager",
        include_str!("builtins/odyssey-voyager.theme"),
    ),
    (
        "odyssey-pulsar",
        include_str!("builtins/odyssey-pulsar.theme"),
    ),
    (
        "odyssey-dawn-light",
        include_str!("builtins/odyssey-dawn-light.theme"),
    ),
    (
        "odyssey-sandstone-light",
        include_str!("builtins/odyssey-sandstone-light.theme"),
    ),
    (
        "odyssey-graphite",
        include_str!("builtins/odyssey-graphite.theme"),
    ),
    (
        "odyssey-fathom",
        include_str!("builtins/odyssey-fathom.theme"),
    ),
    (
        "odyssey-harbor",
        include_str!("builtins/odyssey-harbor.theme"),
    ),
    ("odyssey-ion", include_str!("builtins/odyssey-ion.theme")),
    (
        "odyssey-orchard",
        include_str!("builtins/odyssey-orchard.theme"),
    ),
    (
        "odyssey-volcanic",
        include_str!("builtins/odyssey-volcanic.theme"),
    ),
    (
        "odyssey-cloud-light",
        include_str!("builtins/odyssey-cloud-light.theme"),
    ),
    (
        "odyssey-coral-light",
        include_str!("builtins/odyssey-coral-light.theme"),
    ),
    (
        "odyssey-mist-light",
        include_str!("builtins/odyssey-mist-light.theme"),
    ),
    (
        "odyssey-twilight",
        include_str!("builtins/odyssey-twilight.theme"),
    ),
    (
        "odyssey-verdant",
        include_str!("builtins/odyssey-verdant.theme"),
    ),
    (
        "odyssey-quasar",
        include_str!("builtins/odyssey-quasar.theme"),
    ),
    (
        "odyssey-meadow-light",
        include_str!("builtins/odyssey-meadow-light.theme"),
    ),
    (
        "odyssey-parchment-light",
        include_str!("builtins/odyssey-parchment-light.theme"),
    ),
    (
        "odyssey-tidepool",
        include_str!("builtins/odyssey-tidepool.theme"),
    ),
    ("odyssey-moss", include_str!("builtins/odyssey-moss.theme")),
    (
        "odyssey-rosewood",
        include_str!("builtins/odyssey-rosewood.theme"),
    ),
    (
        "odyssey-slate",
        include_str!("builtins/odyssey-slate.theme"),
    ),
    (
        "odyssey-blossom-light",
        include_str!("builtins/odyssey-blossom-light.theme"),
    ),
    (
        "odyssey-linen-light",
        include_str!("builtins/odyssey-linen-light.theme"),
    ),
    (
        "odyssey-garnet",
        include_str!("builtins/odyssey-garnet.theme"),
    ),
    (
        "odyssey-sepia",
        include_str!("builtins/odyssey-sepia.theme"),
    ),
    (
        "odyssey-cobalt",
        include_str!("builtins/odyssey-cobalt.theme"),
    ),
    (
        "odyssey-lilac-light",
        include_str!("builtins/odyssey-lilac-light.theme"),
    ),
    (
        "odyssey-pearl-light",
        include_str!("builtins/odyssey-pearl-light.theme"),
    ),
    (
        "odyssey-apricot-light",
        include_str!("builtins/odyssey-apricot-light.theme"),
    ),
    (
        "odyssey-chartreuse",
        include_str!("builtins/odyssey-chartreuse.theme"),
    ),
    (
        "odyssey-violet",
        include_str!("builtins/odyssey-violet.theme"),
    ),
    (
        "odyssey-fuchsia",
        include_str!("builtins/odyssey-fuchsia.theme"),
    ),
    (
        "odyssey-butter-light",
        include_str!("builtins/odyssey-butter-light.theme"),
    ),
    (
        "odyssey-sage-light",
        include_str!("builtins/odyssey-sage-light.theme"),
    ),
    (
        "odyssey-slate-light",
        include_str!("builtins/odyssey-slate-light.theme"),
    ),
    (
        "odyssey-amber",
        include_str!("builtins/odyssey-amber.theme"),
    ),
    (
        "odyssey-default",
        include_str!("builtins/odyssey-default.theme"),
    ),
    (
        "odyssey-orchid",
        include_str!("builtins/odyssey-orchid.theme"),
    ),
    (
        "odyssey-seafoam-light",
        include_str!("builtins/odyssey-seafoam-light.theme"),
    ),
    (
        "odyssey-indigo",
        include_str!("builtins/odyssey-indigo.theme"),
    ),
    (
        "odyssey-raspberry",
        include_str!("builtins/odyssey-raspberry.theme"),
    ),
    (
        "odyssey-citrus-light",
        include_str!("builtins/odyssey-citrus-light.theme"),
    ),
    (
        "odyssey-mauve-light",
        include_str!("builtins/odyssey-mauve-light.theme"),
    ),
    (
        "odyssey-terracotta",
        include_str!("builtins/odyssey-terracotta.theme"),
    ),
    (
        "odyssey-harvest",
        include_str!("builtins/odyssey-harvest.theme"),
    ),
    (
        "odyssey-lagoon",
        include_str!("builtins/odyssey-lagoon.theme"),
    ),
    (
        "odyssey-clover-light",
        include_str!("builtins/odyssey-clover-light.theme"),
    ),
    (
        "odyssey-midnight",
        include_str!("builtins/odyssey-midnight.theme"),
    ),
    (
        "odyssey-sienna-light",
        include_str!("builtins/odyssey-sienna-light.theme"),
    ),
    (
        "odyssey-periwinkle-light",
        include_str!("builtins/odyssey-periwinkle-light.theme"),
    ),
    ("odyssey-pine", include_str!("builtins/odyssey-pine.theme")),
    // Extended Odyssey originals (celestial phenomena, minerals, waters,
    // botanicals, times-of-day).
    (
        "odyssey-eclipse",
        include_str!("builtins/odyssey-eclipse.theme"),
    ),
    (
        "odyssey-comet",
        include_str!("builtins/odyssey-comet.theme"),
    ),
    (
        "odyssey-obsidian",
        include_str!("builtins/odyssey-obsidian.theme"),
    ),
    ("odyssey-jade", include_str!("builtins/odyssey-jade.theme")),
    (
        "odyssey-basalt",
        include_str!("builtins/odyssey-basalt.theme"),
    ),
    (
        "odyssey-fjord",
        include_str!("builtins/odyssey-fjord.theme"),
    ),
    (
        "odyssey-mangrove",
        include_str!("builtins/odyssey-mangrove.theme"),
    ),
    (
        "odyssey-nocturne",
        include_str!("builtins/odyssey-nocturne.theme"),
    ),
    (
        "odyssey-quartz-light",
        include_str!("builtins/odyssey-quartz-light.theme"),
    ),
    (
        "odyssey-marble-light",
        include_str!("builtins/odyssey-marble-light.theme"),
    ),
    (
        "odyssey-dune-light",
        include_str!("builtins/odyssey-dune-light.theme"),
    ),
    (
        "odyssey-fern-light",
        include_str!("builtins/odyssey-fern-light.theme"),
    ),
    // Community palettes (published values).
    (
        "solarized-dark",
        include_str!("builtins/solarized-dark.theme"),
    ),
    ("gruvbox-dark", include_str!("builtins/gruvbox-dark.theme")),
    ("nord", include_str!("builtins/nord.theme")),
    ("dracula", include_str!("builtins/dracula.theme")),
    ("tokyo-night", include_str!("builtins/tokyo-night.theme")),
    (
        "catppuccin-mocha",
        include_str!("builtins/catppuccin-mocha.theme"),
    ),
    ("one-dark", include_str!("builtins/one-dark.theme")),
    ("monokai", include_str!("builtins/monokai.theme")),
    (
        "everforest-dark",
        include_str!("builtins/everforest-dark.theme"),
    ),
    ("kanagawa", include_str!("builtins/kanagawa.theme")),
    ("rose-pine", include_str!("builtins/rose-pine.theme")),
    ("ayu-mirage", include_str!("builtins/ayu-mirage.theme")),
    ("night-owl", include_str!("builtins/night-owl.theme")),
    ("palenight", include_str!("builtins/palenight.theme")),
    ("github-dark", include_str!("builtins/github-dark.theme")),
    ("zenburn", include_str!("builtins/zenburn.theme")),
    ("oceanic-next", include_str!("builtins/oceanic-next.theme")),
    ("iceberg-dark", include_str!("builtins/iceberg-dark.theme")),
    // Light palettes.
    (
        "solarized-light",
        include_str!("builtins/solarized-light.theme"),
    ),
    (
        "catppuccin-latte",
        include_str!("builtins/catppuccin-latte.theme"),
    ),
    ("github-light", include_str!("builtins/github-light.theme")),
    (
        "gruvbox-light",
        include_str!("builtins/gruvbox-light.theme"),
    ),
    ("one-light", include_str!("builtins/one-light.theme")),
    ("ayu-light", include_str!("builtins/ayu-light.theme")),
    (
        "rose-pine-dawn",
        include_str!("builtins/rose-pine-dawn.theme"),
    ),
    (
        "tokyo-night-day",
        include_str!("builtins/tokyo-night-day.theme"),
    ),
    (
        "papercolor-light",
        include_str!("builtins/papercolor-light.theme"),
    ),
    (
        "everforest-light",
        include_str!("builtins/everforest-light.theme"),
    ),
    // Retro / phosphor palettes.
    (
        "green-phosphor",
        include_str!("builtins/green-phosphor.theme"),
    ),
    ("amber-crt", include_str!("builtins/amber-crt.theme")),
    ("ibm-5151", include_str!("builtins/ibm-5151.theme")),
    ("dos-cga", include_str!("builtins/dos-cga.theme")),
    (
        "apple-ii-green",
        include_str!("builtins/apple-ii-green.theme"),
    ),
    ("commodore-64", include_str!("builtins/commodore-64.theme")),
    (
        "hercules-amber",
        include_str!("builtins/hercules-amber.theme"),
    ),
    ("vt220-green", include_str!("builtins/vt220-green.theme")),
];

static LIBRARY: OnceLock<Vec<Theme>> = OnceLock::new();

/// Parse every embedded built-in through the shared [`ThemeSpec`] path and
/// project it with its canonical registry name. Built-ins are vetted to parse
/// cleanly; any parse warning here is an authoring bug, surfaced as a panic in
/// debug builds (tests cover the same with `panic`-on-warn) and otherwise
/// ignored so a release build never aborts over a built-in.
fn build_library() -> Vec<Theme> {
    REGISTRY
        .iter()
        .map(|&(name, source)| {
            let spec = ThemeSpec::parse(source, |message| {
                debug_assert!(false, "built-in theme {name:?} parse warning: {message}");
                let _ = message;
            });
            spec.to_theme_with_name(name)
        })
        .collect()
}

/// All built-in themes, in roster order (Odyssey identity first, then community
/// palettes). Built once and cached.
pub fn all() -> &'static [Theme] {
    LIBRARY.get_or_init(build_library).as_slice()
}

/// The canonical names of every built-in, in roster order. Forward-compat for
/// `CFG1 --list-themes`.
pub fn names() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|&(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::super::contrast::contrast_ratio;
    use super::super::{Appearance, MIN_CONTRAST};
    use super::*;

    /// Re-parse a built-in's embedded source into its spec (for tests that need
    /// the authoring-only fields, e.g. the `appearance` flag).
    fn spec_for(name: &str) -> ThemeSpec {
        let (_, source) = REGISTRY
            .iter()
            .find(|&&(n, _)| n == name)
            .unwrap_or_else(|| panic!("no built-in named {name:?}"));
        ThemeSpec::parse(source, |m| panic!("built-in {name:?} parse warning: {m}"))
    }

    #[test]
    fn library_has_the_full_roster() {
        assert_eq!(all().len(), REGISTRY.len());
        assert_eq!(all().len(), 112, "roster size changed — update docs + this");
    }

    #[test]
    fn every_builtin_parses_without_warnings() {
        for &(name, source) in REGISTRY {
            ThemeSpec::parse(source, |m| panic!("built-in {name:?} warned: {m}"));
        }
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for name in names() {
            assert!(seen.insert(name), "duplicate built-in name {name:?}");
        }
    }

    #[test]
    fn projected_name_matches_the_registry_name() {
        for (theme, name) in all().iter().zip(names()) {
            assert_eq!(theme.name, name);
        }
    }

    #[test]
    fn from_name_resolves_every_builtin() {
        for name in names() {
            assert_eq!(
                Theme::from_name(name).map(|t| t.name),
                Some(name),
                "ODYTTY_THEME={name:?} must resolve"
            );
        }
    }

    #[test]
    fn parsed_core_themes_match_their_consts() {
        // The const baselines (used as the parse default and the fallback) must
        // stay byte-identical to their embedded authoring source.
        assert_eq!(Theme::from_name("plain"), Some(Theme::PLAIN));
        assert_eq!(Theme::from_name("odyssey"), Some(Theme::ODYSSEY));
        assert_eq!(Theme::from_name("odyssey-noir"), Some(Theme::ODYSSEY_NOIR));
        assert_eq!(
            Theme::from_name("odyssey-default"),
            Some(Theme::ODYSSEY_DEFAULT)
        );
    }

    #[test]
    fn odyssey_jungle_alias_resolves_to_the_default_palette() {
        // `odyssey-jungle` is the pre-rename alias for the flagship default
        // palette (v0.6.0), kept working so existing configs don't break.
        assert_eq!(
            Theme::from_name("odyssey-jungle"),
            Some(Theme::ODYSSEY_DEFAULT)
        );
        assert_eq!(
            Theme::from_name("odyssey-default"),
            Theme::from_name("odyssey-jungle")
        );
    }

    #[test]
    fn plain_palette_is_byte_identical_to_historical_table() {
        // The pixel-identity guarantee survives the embed → parse round-trip.
        let plain = Theme::from_name("plain").unwrap();
        assert_eq!(plain.palette, crate::text::DEFAULT_ANSI_SRGB);
        assert_eq!(plain.foreground, crate::text::DEFAULT_FG_SRGB);
        assert_eq!(plain.background, crate::text::DEFAULT_BG_SRGB);
    }

    #[test]
    fn every_builtin_meets_minimum_default_contrast() {
        // The readability floor (seeds RV1): every built-in's default fg/bg must
        // clear MIN_CONTRAST. If a new theme fails, fix its fg/bg (or, for a
        // borderline community palette, document the exception) rather than
        // lowering the floor.
        for theme in all() {
            let ratio = contrast_ratio(theme.foreground, theme.background);
            assert!(
                ratio >= MIN_CONTRAST,
                "{} default contrast {ratio:.2} < {MIN_CONTRAST}",
                theme.name
            );
        }
    }

    #[test]
    fn appearance_flag_matches_background_luminance() {
        // A light theme must actually have a light background and vice versa —
        // catches a mis-set `appearance =` line. The midpoint split is generous
        // (0.18 linear ≈ mid-gray) so only a clearly-wrong flag trips it.
        use super::super::contrast::relative_luminance;
        for name in names() {
            let spec = spec_for(name);
            let theme = Theme::from_name(name).unwrap();
            let bg = relative_luminance(theme.background);
            match spec.appearance {
                Appearance::Light => {
                    assert!(bg > 0.18, "{name}: light theme but dark background");
                }
                Appearance::Dark => {
                    assert!(bg < 0.18, "{name}: dark theme but light background");
                }
            }
        }
    }

    #[test]
    fn bright_row_differs_from_normal_row() {
        for theme in all() {
            assert_ne!(
                &theme.palette[0..8],
                &theme.palette[8..16],
                "{} bright row duplicates normal row",
                theme.name
            );
        }
    }
}
