// SPDX-License-Identifier: GPL-3.0-only
//! Native CVD theme-wiring layer (U4): turn the authored presentation theme
//! into the *effective* theme that is actually published to the renderer when a
//! colour-vision-deficiency mode is active.
//!
//! The pure adaptation maths lives in [`crate::cvd`]; this module is the thin
//! native seam that decides *when* to apply it and caches the result so it is
//! computed once per change rather than per frame.
//!
//! ## What it adapts (palette scope)
//!
//! [`effective_theme`] adapts the palette payload only — the 16 ANSI colours
//! plus the cursor / selection / search role colours — via
//! [`crate::cvd::adapt_palette`], which also re-floors the result so it stays
//! readable. Indexed-256 colours and application truecolour are **not** remapped
//! (a per-cell output lens is a deferred horizon, recorded so it is not lost);
//! the structural background / foreground / chrome are held by `adapt_palette`.
//!
//! ## Off is the pixel-identical baseline
//!
//! When the mode is [`CvdMode::Off`] — the default — or the strength is `<= 0`,
//! [`effective_theme`] returns the authored theme **unchanged** (a `Copy`), so
//! every downstream publish sees byte-identical colours and the plain path is
//! preserved exactly. The off short-circuit (not [`crate::cvd::adapt_palette`]'s
//! `strength = 0` passthrough) is the guarantee, because `adapt_palette`
//! re-floors unconditionally and so is not itself an identity at zero strength.
//!
//! ## Appearance inference (not the spec default)
//!
//! [`crate::theme::ThemeSpec::from_theme`] defaults `appearance` to `Dark`, but
//! the re-floor's bg-side neutral exemption depends on the real appearance. We
//! infer it from the background luminance (mirroring the theme builder's own
//! threshold) so a light theme classifies correctly and its near-background
//! neutrals stay structural rather than being lifted to the legible floor.
//!
//! ## Builder bypass seam (available, not yet routed)
//!
//! [`effective_theme`] is exposed so the theme builder's live preview can be
//! routed *around* adaptation in a later step — keeping authoring WYSIWYG-to-file
//! by publishing the authored theme directly. That bypass is not wired yet: today
//! the builder preview flows through the normal settings-apply path like any
//! other theme application, so with a CVD mode active a preview is adapted too.
//! Closing that is a small cross-layer follow-up (a dedicated preview source that
//! skips this compute); until then the only effect is on previews while a CVD
//! mode is on, which is off by default.

use crate::cvd::{self, CvdType};
use crate::settings::CvdMode;
use crate::theme::{Appearance, Srgb, Theme, ThemeSpec, relative_luminance};

/// Background luminance above which a theme is treated as light. Mirrors the
/// theme builder's own appearance inference (`> 0.18`) so the colour modules
/// agree on where the dark/light boundary sits.
const LIGHT_APPEARANCE_THRESHOLD: f64 = 0.18;

/// Map a settings [`CvdMode`] to the pure-core [`CvdType`]. `Off` has no model
/// (the caller short-circuits to the authored theme).
fn cvd_type(mode: CvdMode) -> Option<CvdType> {
    match mode {
        CvdMode::Off => None,
        CvdMode::Protan => Some(CvdType::Protan),
        CvdMode::Deutan => Some(CvdType::Deutan),
        CvdMode::Tritan => Some(CvdType::Tritan),
    }
}

/// Classify a theme's appearance from its background luminance (D-U4-2): do not
/// trust [`ThemeSpec::from_theme`]'s `Dark` default, which would mis-exempt a
/// light theme's neutrals during the re-floor.
fn appearance_from_background(background: Srgb) -> Appearance {
    if relative_luminance(background) > LIGHT_APPEARANCE_THRESHOLD {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

/// Compute the effective (CVD-adapted) theme for `base` under `mode`/`strength`.
///
/// Returns `base` unchanged when the mode is [`CvdMode::Off`] or the strength is
/// `<= 0` (the pixel-identical baseline). Otherwise it projects the theme to a
/// [`ThemeSpec`], fixes the appearance from the real background luminance, runs
/// [`crate::cvd::adapt_palette`] (which daltonises the palette/roles and
/// re-floors them readable), and projects back to a [`Theme`] keeping the
/// original name.
///
/// Pure and deterministic: identical inputs yield identical output. This is the
/// seam a later step can route the theme builder's preview *around* for WYSIWYG
/// authoring (not yet wired — preview currently flows through adaptation).
pub(crate) fn effective_theme(base: &Theme, mode: CvdMode, strength: f32) -> Theme {
    let Some(ty) = cvd_type(mode) else {
        return *base;
    };
    if strength <= 0.0 {
        return *base;
    }
    let mut spec = ThemeSpec::from_theme(base);
    spec.appearance = appearance_from_background(base.background);
    cvd::adapt_palette(&spec, ty, strength).to_theme()
}

/// One-entry cache for the effective theme (D-U4-4).
///
/// [`effective_theme`] re-floors the whole palette, which is non-trivial, and
/// `apply_settings` can fire repeatedly (e.g. while a slider is dragged). The
/// cache keys on the exact `(authored theme, mode, strength)` triple and only
/// recomputes when it changes, so steady-state applies are a cheap clone. The
/// key is the authored [`Theme`] itself (it is `Copy` + `Eq`), so the match is
/// exact rather than a hash that could collide.
#[derive(Default)]
pub(crate) struct CvdThemeCache {
    cached: Option<(CacheKey, Theme)>,
}

/// `(authored theme, mode, strength bits)`. Strength is keyed by its bit pattern
/// so the lookup is a total equality (no float `Eq`), matching the determinism
/// of [`effective_theme`].
type CacheKey = (Theme, CvdMode, u32);

impl CvdThemeCache {
    /// Resolve the effective theme for `base`/`mode`/`strength`, reusing the
    /// last result when the inputs are unchanged.
    pub(crate) fn resolve(&mut self, base: &Theme, mode: CvdMode, strength: f32) -> Theme {
        let key: CacheKey = (*base, mode, strength.to_bits());
        if let Some((cached_key, cached_value)) = &self.cached
            && *cached_key == key
        {
            return *cached_value;
        }
        let value = effective_theme(base, mode, strength);
        self.cached = Some((key, value));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A confusable dark theme: ANSI red (1) and green (2) that a deutan viewer
    /// collapses, over a dark background.
    fn confusable_dark() -> Theme {
        let mut theme = Theme::PLAIN;
        theme.background = (0x10, 0x10, 0x10);
        theme.foreground = (0xE0, 0xE0, 0xE0);
        theme.palette[1] = (0xC0, 0x30, 0x30); // red
        theme.palette[2] = (0x30, 0xA0, 0x30); // green
        theme
    }

    #[test]
    fn off_mode_returns_the_authored_theme_bitwise() {
        let base = confusable_dark();
        // Every strength is irrelevant while off: the authored theme is returned
        // unchanged, so the published colours are byte-identical to the plain
        // path.
        assert_eq!(effective_theme(&base, CvdMode::Off, 1.0), base);
        assert_eq!(effective_theme(&base, CvdMode::Off, 0.0), base);
        assert_eq!(effective_theme(&base, CvdMode::Off, 0.5), base);
    }

    #[test]
    fn zero_strength_is_an_exact_passthrough_even_when_a_mode_is_set() {
        let base = confusable_dark();
        // The second net: a mode is selected but strength is 0 → no adaptation,
        // bypassing adapt_palette's unconditional re-floor entirely.
        assert_eq!(effective_theme(&base, CvdMode::Deutan, 0.0), base);
        assert_eq!(effective_theme(&base, CvdMode::Protan, -1.0), base);
    }

    #[test]
    fn active_mode_changes_the_confusable_palette() {
        let base = confusable_dark();
        let adapted = effective_theme(&base, CvdMode::Deutan, 1.0);
        // Adaptation must move at least one of the confusable slots so a deutan
        // viewer can separate them; the name is preserved.
        assert_ne!(adapted, base);
        assert!(
            adapted.palette[1] != base.palette[1] || adapted.palette[2] != base.palette[2],
            "deutan adaptation should move red and/or green off the lost axis"
        );
        assert_eq!(adapted.name, base.name);
    }

    #[test]
    fn appearance_is_inferred_from_background_not_the_dark_default() {
        // A light theme must classify as Light so the re-floor exempts the
        // light-side neutrals (7/15) rather than the dark-side (0/8). We assert
        // the classifier directly, since the exemption is internal to cvd.
        let light_bg = (0xF5, 0xF5, 0xF0);
        let dark_bg = (0x0B, 0x0C, 0x10);
        assert_eq!(appearance_from_background(light_bg), Appearance::Light);
        assert_eq!(appearance_from_background(dark_bg), Appearance::Dark);
    }

    #[test]
    fn cache_returns_equal_result_and_recomputes_on_key_change() {
        let base = confusable_dark();
        let mut cache = CvdThemeCache::default();

        let direct = effective_theme(&base, CvdMode::Deutan, 1.0);
        let first = cache.resolve(&base, CvdMode::Deutan, 1.0);
        assert_eq!(first, direct, "cache result matches the direct compute");

        // Same key → same value (served from cache).
        let second = cache.resolve(&base, CvdMode::Deutan, 1.0);
        assert_eq!(second, first);

        // Changing the mode is a new key → recompute, and off returns the
        // authored theme.
        let off = cache.resolve(&base, CvdMode::Off, 1.0);
        assert_eq!(off, base);

        // Changing strength is a new key as well.
        let weaker = cache.resolve(&base, CvdMode::Deutan, 0.5);
        assert_eq!(weaker, effective_theme(&base, CvdMode::Deutan, 0.5));
    }
}
