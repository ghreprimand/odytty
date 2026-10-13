// SPDX-License-Identifier: GPL-3.0-only
//! Gradient and vignette modify cell backgrounds before foreground contrast
//! adjustment. Image treatment uses its own pass. Final brightness and overlay
//! tints can change the resulting contrast. No background treatment captures
//! input; inactive treatment leaves the overlay signature inert.

use crate::grid::{self, BackgroundTreatmentParams};
use crate::settings::{BackgroundTreatment as SettingTreatment, Settings};

use super::*;

/// Treatment strength applied before foreground contrast adjustment.
const DEFAULT_BG_TREATMENT_STRENGTH: f32 = 0.7;

/// Pure mapping from the active settings to the grid treatment params. Returns
/// the identity (inactive) when the knob is `off` or the renderer profile is
/// plain. Free function so it is testable without constructing an `App` (which
/// requires a live PTY).
fn treatment_params_for(settings: &Settings) -> BackgroundTreatmentParams {
    let kind = match settings.effective_background_treatment() {
        SettingTreatment::Off => return BackgroundTreatmentParams::default(),
        SettingTreatment::Gradient => grid::BackgroundTreatment::Gradient,
        SettingTreatment::Vignette => grid::BackgroundTreatment::Vignette,
        // T1: the image treatment lives on its own GPU pass and does NOT modulate
        // per-cell background colours. It MUST return the identity params so the
        // grid cell-vertex apply block is skipped - falling through to the
        // gradient/vignette path would double-treat the background. Readability
        // is handled by the readability scrim + `cell_bg_opacity`, not here.
        SettingTreatment::Image => return BackgroundTreatmentParams::default(),
    };
    BackgroundTreatmentParams {
        kind,
        strength: DEFAULT_BG_TREATMENT_STRENGTH,
    }
}

/// Hash the selected image settings, with quantized floating-point values.
/// The overlay fragment uses only the low 16 bits; applying settings also
/// invalidates presentation independently.
fn image_signature_for(settings: &Settings) -> Option<u64> {
    if settings.effective_background_treatment() != SettingTreatment::Image {
        return None;
    }
    let path = settings.background_image.as_ref()?;
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    settings.background_blur_radius.hash(&mut hasher);
    // Quantize the floats so noise never thrashes the cache, then fold them in.
    let opacity_q = (settings.cell_bg_opacity.clamp(0.0, 1.0) * 1000.0).round() as u32;
    opacity_q.hash(&mut hasher);
    let scrim_q = settings
        .background_image_scrim
        .map(|s| (s.clamp(0.0, 1.0) * 1000.0).round() as i32)
        .unwrap_or(-1);
    scrim_q.hash(&mut hasher);
    Some(hasher.finish())
}

/// Background cache fragment: inactive, quantized treatment strength, or
/// a 16-bit fragment of the image settings hash.
fn overlay_signature_for(settings: &Settings) -> OverlayFragment {
    let params = treatment_params_for(settings);
    if !params.active() {
        // Image paints separately despite inactive cell treatment parameters.
        // Its hash is narrowed here; settings apply also invalidates the frame.
        if let Some(sig) = image_signature_for(settings) {
            let scrim_q = (sig & 0xFFFF) as u16;
            return OverlayFragment::Background { scrim_q, treat: 3 };
        }
        return OverlayFragment::Inert;
    }
    let treat = match params.kind {
        grid::BackgroundTreatment::None => return OverlayFragment::Inert,
        grid::BackgroundTreatment::Gradient => 1,
        grid::BackgroundTreatment::Vignette => 2,
    };
    // Quantize strength to a stable integer bucket so floating-point noise can
    // never thrash the cache; constant in v1 but keyed so a future strength knob
    // invalidates correctly.
    let scrim_q = (params.strength.clamp(0.0, 1.0) * 1000.0).round() as u16;
    OverlayFragment::Background { scrim_q, treat }
}

impl App {
    /// Resolve the active background-treatment parameters for the grid
    /// cell-vertex path. Identity (inactive) when the knob is `off` or the
    /// renderer profile is plain, keeping the fast path byte-identical.
    pub(super) fn background_treatment_params(&self) -> BackgroundTreatmentParams {
        treatment_params_for(&self.settings)
    }

    /// background cache fragment. `Inert` when the treatment is off (so
    /// the composite signature stays a frame-to-frame constant and the plain
    /// path is byte-identical); otherwise a [`OverlayFragment::Background`].
    pub(super) fn background_overlay_signature(&self) -> OverlayFragment {
        overlay_signature_for(&self.settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(t: SettingTreatment) -> Settings {
        Settings {
            background_treatment: t,
            ..Default::default()
        }
    }

    /// KILL-SHOT (trap 1): the `off`/`color` treatment produces the identity
    /// params, so the grid apply block is skipped and frames are byte-identical.
    /// (Since v0.6.0 the shipped default is `image`; this guards the opt-out
    /// fast path - `background_treatment = color` - that disables the bundled
    /// background.)
    #[test]
    fn off_treatment_is_inactive_identity() {
        let s = settings_with(SettingTreatment::Off);
        let params = treatment_params_for(&s);
        assert!(!params.active(), "off must be inactive");
        assert_eq!(params, BackgroundTreatmentParams::default());
        assert_eq!(overlay_signature_for(&s), OverlayFragment::Inert);

        // And the shipped default now selects the image treatment.
        assert_eq!(
            Settings::default().background_treatment,
            SettingTreatment::Image
        );
    }

    /// Trap 3: signature is `Inert` off, `Background` on, with a discriminant
    /// that distinguishes the two treatments (so switching kind repaints).
    #[test]
    fn signature_is_inert_off_and_keyed_on() {
        assert_eq!(
            overlay_signature_for(&settings_with(SettingTreatment::Off)),
            OverlayFragment::Inert
        );
        let gs = overlay_signature_for(&settings_with(SettingTreatment::Gradient));
        let vs = overlay_signature_for(&settings_with(SettingTreatment::Vignette));
        assert_ne!(gs, OverlayFragment::Inert);
        assert_ne!(vs, OverlayFragment::Inert);
        assert_ne!(gs, vs, "different treatments must key differently");
    }

    /// Active treatments yield active params with the matching grid kind.
    #[test]
    fn active_treatments_map_to_grid_kind() {
        let p = treatment_params_for(&settings_with(SettingTreatment::Gradient));
        assert!(p.active());
        assert_eq!(p.kind, grid::BackgroundTreatment::Gradient);

        let p = treatment_params_for(&settings_with(SettingTreatment::Vignette));
        assert!(p.active());
        assert_eq!(p.kind, grid::BackgroundTreatment::Vignette);
    }

    /// Trap 6 / plain-path guarantee: under the plain renderer profile the
    /// treatment is forced off even when the knob is set, so the fast path stays
    /// pixel-identical.
    #[test]
    fn plain_profile_forces_treatment_off() {
        let mut s = settings_with(SettingTreatment::Vignette);
        // Balanced default leaves it active.
        assert!(treatment_params_for(&s).active());
        s.render_quality = crate::settings::RenderQuality::Plain;
        assert!(
            !treatment_params_for(&s).active(),
            "plain profile must neutralize the treatment"
        );
        assert_eq!(overlay_signature_for(&s), OverlayFragment::Inert);
    }
}
