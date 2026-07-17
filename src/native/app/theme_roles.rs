// SPDX-License-Identifier: GPL-3.0-only
//! Theme-role-derived render styling for the native app.
//!
//! Mechanically split out of `app/mod.rs` to keep that file under the
//! source-size cap; no behavior or API change. These `App` methods turn the
//! active (effective) theme's roles — `selection`, `search`, and the
//! scroll-indicator foreground — into the concrete colors the renderer draws,
//! flooring foregrounds over their fills through the RV1 minimum-contrast
//! machinery so they stay legible at the active `min_contrast` (identity at the
//! default 1.0). They live in a child module so they can reach `App`'s private
//! fields and the sibling free helpers directly; callers in `app/mod.rs` reach
//! them through `pub(super)`.

use super::*;

/// How far the active search match is brightened toward white (OKLab mix) from
/// the `search` role, so it reads as distinct from non-active matches.
const SEARCH_ACTIVE_BRIGHTEN: f32 = 0.35;

impl App {
    pub(super) fn scroll_indicator_color(&self) -> [f32; 4] {
        let (r, g, b) = self.effective_theme.foreground;
        let mut color = text::foreground_linear(Color::Rgb(r, g, b));
        color[3] = 0.62;
        color
    }

    /// ID1: themed selection treatment, or `None` (today's inverse) when the
    /// operator opts out. The stored `fill` is the theme `selection` role
    /// COMPOSITED over the theme background at `selection_opacity` (design §3),
    /// so the painted selection background already carries the tint strength;
    /// the foreground is the theme foreground floored over that same effective
    /// fill through the RV1 minimum-contrast machinery, so it stays legible at
    /// the active `min_contrast` (identity at the default 1.0) even as the fill
    /// goes translucent.
    ///
    /// Baking the composite here (rather than as a surface-alpha scale in the
    /// GPU builder) keeps the selected cell on the SAME transparency plane as
    /// surrounding content: the vertex builder draws this fill at the ordinary
    /// content opacity, so the selection reads the same against its surround at
    /// any window opacity instead of coupling inversely to it. The floor
    /// reference and the painted color are now the identical effective fill, so
    /// legibility is judged against the exact pixels drawn. At
    /// `selection_opacity == 1.0` the effective fill equals the opaque role
    /// color (the `composite_over` endpoint is exact), so the whole style — fill
    /// and floored fg alike — is byte-identical to a fully-opaque selection.
    pub(super) fn themed_selection_style(&self) -> Option<SelectionStyle> {
        if !self.themed_ui_roles {
            return None;
        }
        let role_fill = [
            self.effective_theme.selection.0,
            self.effective_theme.selection.1,
            self.effective_theme.selection.2,
        ];
        let fill = effective_selection_fill(
            role_fill,
            self.effective_theme.background,
            self.settings.selection_opacity,
        );
        let fg = floor_fg_over(
            self.effective_theme.foreground,
            fill,
            self.settings.effective_min_contrast(),
        );
        Some(SelectionStyle { fill, fg })
    }

    /// ID1: themed search-highlight treatment, or `None` (today's inverse /
    /// black-on-yellow) when the operator opts out. Non-active matches use the
    /// theme `search` role; the active match uses a brightened OKLab derivative
    /// of it. Both foregrounds are RV1-floored over their fills.
    pub(super) fn themed_search_style(&self) -> Option<SearchStyle> {
        if !self.themed_ui_roles {
            return None;
        }
        let fill = [
            self.effective_theme.search.0,
            self.effective_theme.search.1,
            self.effective_theme.search.2,
        ];
        let fill_lin = srgb_tuple_to_linear(self.effective_theme.search);
        let active_fill_lin =
            crate::color::mix_oklab(fill_lin, [1.0, 1.0, 1.0], SEARCH_ACTIVE_BRIGHTEN);
        let active_fill = linear_to_srgb_tuple(active_fill_lin);
        let fg = floor_fg_over(
            self.effective_theme.foreground,
            fill,
            self.settings.effective_min_contrast(),
        );
        let active_fg = floor_fg_over(
            self.effective_theme.foreground,
            active_fill,
            self.settings.effective_min_contrast(),
        );
        Some(SearchStyle {
            fill,
            fg,
            active_fill,
            active_fg,
        })
    }

    /// HINTS label-badge treatment: a brightened `search`-role derivative so the
    /// badge reads as distinct from a passive search highlight, with the label
    /// foreground RV1-floored over the badge fill (trap #4). Returns `None` when
    /// themed UI roles are off, preserving the high-contrast default badge.
    pub(super) fn themed_hint_style(&self) -> Option<super::hints_ui::HintStyle> {
        if !self.themed_ui_roles {
            return None;
        }
        let fill_lin = srgb_tuple_to_linear(self.effective_theme.search);
        let badge_fill_lin =
            crate::color::mix_oklab(fill_lin, [1.0, 1.0, 1.0], SEARCH_ACTIVE_BRIGHTEN);
        let fill = linear_to_srgb_tuple(badge_fill_lin);
        let fg = floor_fg_over(
            self.effective_theme.foreground,
            fill,
            self.settings.effective_min_contrast(),
        );
        Some(super::hints_ui::HintStyle { fill, fg })
    }
}

fn srgb_tuple_to_linear(color: (u8, u8, u8)) -> crate::color::LinearRgb {
    [
        crate::color::srgb_to_linear(color.0),
        crate::color::srgb_to_linear(color.1),
        crate::color::srgb_to_linear(color.2),
    ]
}

fn linear_to_srgb_tuple(linear: crate::color::LinearRgb) -> [u8; 3] {
    [
        crate::color::linear_to_srgb_u8(linear[0]),
        crate::color::linear_to_srgb_u8(linear[1]),
        crate::color::linear_to_srgb_u8(linear[2]),
    ]
}

/// SELECTION-OPACITY: the effective color a translucent selection fill presents
/// to the eye — the opaque `fill` composited over the theme `backdrop` at
/// `selection_opacity`, in linear light (design §3). The RV1 floor references
/// this so foreground legibility holds as the fill goes translucent. The theme
/// background is the deterministic, worst-case-for-legibility backdrop; under
/// window transparency the true backdrop is the desktop and is unknowable, so
/// flooring over the theme background is the conservative choice. At
/// `selection_opacity == 1.0` this returns `fill` bit-exactly (the
/// `composite_over` endpoint), keeping the floored fg byte-identical.
fn effective_selection_fill(
    fill: [u8; 3],
    backdrop: (u8, u8, u8),
    selection_opacity: f32,
) -> [u8; 3] {
    let fill_lin = srgb_tuple_to_linear((fill[0], fill[1], fill[2]));
    let backdrop_lin = srgb_tuple_to_linear(backdrop);
    linear_to_srgb_tuple(crate::color::composite_over(
        fill_lin,
        backdrop_lin,
        selection_opacity,
    ))
}

/// Floor a foreground over a fill so it meets `ratio` WCAG contrast (RV1).
/// Identity at `ratio <= 1.0` (the default `min_contrast`).
fn floor_fg_over(fg: (u8, u8, u8), bg: [u8; 3], ratio: f32) -> [u8; 3] {
    let fg_lin = srgb_tuple_to_linear(fg);
    let bg_lin = [
        crate::color::srgb_to_linear(bg[0]),
        crate::color::srgb_to_linear(bg[1]),
        crate::color::srgb_to_linear(bg[2]),
    ];
    linear_to_srgb_tuple(crate::color::enforce_min_contrast(fg_lin, bg_lin, ratio))
}

#[cfg(test)]
mod selection_fill_tests {
    use super::effective_selection_fill;

    #[test]
    fn opacity_one_returns_the_opaque_fill_exactly() {
        // Byte-identity underwriting: at full opacity the floor reference is the
        // opaque fill itself, so the themed fg is unchanged from before.
        let fill = [40, 90, 200];
        let backdrop = (12, 12, 16);
        assert_eq!(effective_selection_fill(fill, backdrop, 1.0), fill);
    }

    #[test]
    fn opacity_zero_returns_the_backdrop_exactly() {
        let fill = [40, 90, 200];
        let backdrop = (12, 12, 16);
        assert_eq!(
            effective_selection_fill(fill, backdrop, 0.0),
            [backdrop.0, backdrop.1, backdrop.2]
        );
    }

    #[test]
    fn partial_opacity_recedes_toward_the_backdrop() {
        // A translucent fill over a darker backdrop composites strictly between
        // the two per channel, so the floor sees the dimmer color the eye reads.
        let fill = [220, 220, 220];
        let backdrop = (0, 0, 0);
        let eff = effective_selection_fill(fill, backdrop, 0.5);
        for c in eff {
            assert!(
                c > 0 && c < 220,
                "channel {c} must recede toward the backdrop"
            );
        }
    }

    #[test]
    fn opacity_is_clamped() {
        let fill = [40, 90, 200];
        let backdrop = (12, 12, 16);
        assert_eq!(effective_selection_fill(fill, backdrop, 2.0), fill);
        assert_eq!(
            effective_selection_fill(fill, backdrop, -1.0),
            [backdrop.0, backdrop.1, backdrop.2]
        );
    }
}
