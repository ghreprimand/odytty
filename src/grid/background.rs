// SPDX-License-Identifier: GPL-3.0-only
//! Readability-bounded background color treatments.

/// Maximum background-luminance attenuation an ID3/U5 treatment may apply, at
/// full strength and the farthest falloff point. The cap keeps the effect
/// subtle by construction; the RV1 floor (applied immediately after, on the
/// treated background) is the hard readability guarantee regardless.
pub const MAX_BG_TREATMENT_DARKEN: f32 = 0.55;

/// Which ID3/U5 background treatment is active. [`BackgroundTreatment::None`]
/// (the default) is the identity — the treatment block is skipped entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundTreatment {
    /// No treatment — background drawn exactly as resolved (default).
    #[default]
    None,
    /// Vertical gradient: the background darkens smoothly toward the bottom rows.
    Gradient,
    /// Radial vignette: the background darkens toward the edges and corners.
    Vignette,
}

/// ID3/U5 readability-safe background-treatment parameters, applied per cell to
/// the resolved background color in [`build_cell_vertices_with_focus_dim_and_origin_into`].
///
/// The treatment runs **before** the RV1 minimum-contrast floor, so the floor
/// sees the treated per-cell background and re-lifts the foreground to keep
/// contrast above the configured ratio — readability is preserved by
/// construction, per cell. The [`Default`] is the identity (`kind = None`,
/// `strength = 0.0`), for which [`Self::active`] is `false`, the apply block is
/// skipped, and the rendered frame is byte-identical to the pre-feature
/// renderer. Lives here (not in the native overlay registry) because
/// [`build_cell_vertices_with_focus_dim_and_origin_into`] — a `crate::grid`
/// function — must name it to apply the fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundTreatmentParams {
    /// The spatial treatment function.
    pub kind: BackgroundTreatment,
    /// Treatment strength in `0.0..=1.0`. Scales the maximum attenuation
    /// ([`MAX_BG_TREATMENT_DARKEN`]). `0.0` ⇒ inactive (identity).
    pub strength: f32,
}

impl Default for BackgroundTreatmentParams {
    fn default() -> Self {
        Self {
            kind: BackgroundTreatment::None,
            strength: 0.0,
        }
    }
}

impl BackgroundTreatmentParams {
    /// True when the treatment will actually modify some cell. The identity
    /// (`None` kind, or zero/negative strength) is `false`, so the per-cell
    /// apply block is skipped and the frame stays byte-identical.
    pub fn active(&self) -> bool {
        !matches!(self.kind, BackgroundTreatment::None) && self.strength > 0.0
    }

    /// Per-cell attenuation factor in `0.0..=1.0` (0 = unchanged, 1 = farthest
    /// point of the treatment). Pure and total.
    fn falloff(&self, row: usize, col: usize, rows: usize, cols: usize) -> f32 {
        match self.kind {
            BackgroundTreatment::None => 0.0,
            BackgroundTreatment::Gradient => {
                if rows <= 1 {
                    0.0
                } else {
                    (row as f32 / (rows - 1) as f32).clamp(0.0, 1.0)
                }
            }
            BackgroundTreatment::Vignette => {
                // Normalized radial distance from the grid center: 0 at the
                // center cell, 1 at the farthest corner.
                let cx = (cols as f32 - 1.0) * 0.5;
                let cy = (rows as f32 - 1.0) * 0.5;
                let dx = col as f32 - cx;
                let dy = row as f32 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let maxd = (cx * cx + cy * cy).sqrt();
                if maxd <= 0.0 {
                    0.0
                } else {
                    (dist / maxd).clamp(0.0, 1.0)
                }
            }
        }
    }

    /// Apply the treatment to a linear-RGBA background color at grid position
    /// `(row, col)`. Pure; returns `bg` unchanged when inactive or at a
    /// zero-falloff cell. Only luminance is attenuated; alpha is preserved.
    pub fn apply_to(
        &self,
        bg: [f32; 4],
        row: usize,
        col: usize,
        rows: usize,
        cols: usize,
    ) -> [f32; 4] {
        if !self.active() {
            return bg;
        }
        let f = self.falloff(row, col, rows, cols);
        if f <= 0.0 {
            return bg;
        }
        let atten = 1.0 - (self.strength.clamp(0.0, 1.0) * MAX_BG_TREATMENT_DARKEN * f);
        [bg[0] * atten, bg[1] * atten, bg[2] * atten, bg[3]]
    }
}
