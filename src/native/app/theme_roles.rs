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
    /// operator opts out. The fill is the theme `selection` role verbatim; the
    /// foreground is the theme foreground floored over that fill through the
    /// RV1 minimum-contrast machinery, so it stays legible at the active
    /// `min_contrast` (identity at the default 1.0).
    pub(super) fn themed_selection_style(&self) -> Option<SelectionStyle> {
        if !self.themed_ui_roles {
            return None;
        }
        let fill = [
            self.effective_theme.selection.0,
            self.effective_theme.selection.1,
            self.effective_theme.selection.2,
        ];
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
