// SPDX-License-Identifier: GPL-3.0-only
//! VE5 plain-bypass pixel proof (L2): `render_quality = plain` collapses to the
//! minimal renderer, proven BYTE-FOR-BYTE in the headless CPU compositor.
//!
//! The plain profile is a hard fast path: it derives neutralized *effective*
//! values (`stem_darken = 0.0`, `min_contrast = 1.0`, `focus_dim = 0.0`, bloom
//! and CRT off) at the settings layer, so an all-extras-ON config rendered under
//! plain must be pixel-identical to the genuine minimal config. The visual-gate
//! standing rule requires this fast path be *tested as such*; a passing
//! structural/precedence test is not a proof of live output (the same gap that
//! let the bloom SIGABRT through a green precedence test).
//!
//! ## What this layer proves, and why pixels
//!
//! `focus_dim` is the one neutralized value the CPU compositor can drive
//! end-to-end as a pure parameter (no process global): it flows through the real
//! `grid::build_cell_vertices_with_focus_dim_into` seam via
//! [`composite_focus_dim`]. So this layer rasterizes the SAME content twice and
//! asserts byte-identity:
//!
//! * **plain** — an all-HOT [`Settings`] (`stem=0.5`, `min_contrast=4.5`,
//!   `focus_dim=0.6`, bloom+crt ON) with `render_quality = Plain`, its
//!   `focus_dim` taken through the LIVE [`Settings::effective_focus_dim`]
//!   accessor (which must collapse the hot `0.6` to `0.0`). The hot config is
//!   the teeth: it proves plain *overrides* live-enabled extras, not merely that
//!   "0.0 in yields 0.0 out".
//! * **minimal** — the standard focused composite (the pre-feature renderer).
//!
//! A genuine unfocused frame (a hot **Balanced** config, whose
//! `effective_focus_dim()` stays `0.6`) is the **control**: it MUST differ, or
//! the byte-identity assertion would be vacuous (a test that cannot fail is
//! worthless).
//!
//! `stem` (process global, atlas raster) and the post passes (bloom/crt) are out
//! of this compositor's reach; they are proven in `stem_raster_smoke` (L1) and
//! the settings/gpu structural units (L3) respectively. This layer is
//! deliberately **global-free** so it stays parallel-safe inside the shared
//! pixel suite — it mutates no `MIN_CONTRAST`/stem global.

use crate::harness::{composite, composite_focus_dim, frames_match, row_snapshot, setup};
use odytty::core::CursorStyle;
use odytty::settings::{RenderQuality, Settings};
use odytty::text;

/// An all-extras-HOT config: every optional visual knob is turned up, so the
/// only thing that can neutralize them is the `render_quality` profile under
/// test. `render_quality` is supplied by the caller.
fn all_hot(quality: RenderQuality) -> Settings {
    Settings {
        render_quality: quality,
        stem_darken: 0.5,
        min_contrast: 4.5,
        focus_dim: 0.6,
        bloom: true,
        crt: true,
        ..Settings::default()
    }
}

/// L2 — VE5 plain bypass, proven in composited pixels.
///
/// `render_quality = Plain` must render an all-extras-ON config pixel-identical
/// to the minimal renderer. Driven through the live `effective_focus_dim()`
/// accessor (not a hardcoded `0.0`), with a hot-Balanced control proving the
/// equality assertion can actually fail.
#[test]
fn plain_render_quality_is_pixel_identical_to_minimal() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };

    // Global-free guarantee: the shared suite runs at the identity floor, and
    // this layer must not perturb it. Assert it so a future global-mutating test
    // added here trips this guard instead of silently coupling the suites.
    assert_eq!(
        text::min_contrast(),
        1.0,
        "L2 must run at the 1.0 identity floor (global-free)"
    );

    let snapshot = row_snapshot(4, "Mix!");

    // The minimal renderer: the standard focused composite (pre-feature path).
    let minimal = composite(&snapshot, &atlas, CursorStyle::Block);

    // Plain: an all-HOT config, but its focus-dim taken through the LIVE plain
    // derivation. `effective_focus_dim()` must collapse the hot 0.6 to 0.0.
    let hot_plain = all_hot(RenderQuality::Plain);
    assert_eq!(
        hot_plain.effective_focus_dim(),
        0.0,
        "plain must neutralize a hot focus_dim to 0.0 at the effective layer"
    );
    let plain = composite_focus_dim(
        &snapshot,
        &atlas,
        CursorStyle::Block,
        hot_plain.effective_focus_dim(),
    );

    assert!(
        frames_match(&minimal, &plain),
        "render_quality=plain must be byte-identical to the minimal renderer"
    );

    // CONTROL — proves the assertion above can fail. A hot Balanced config keeps
    // its focus_dim live (effective == 0.6), so its unfocused frame MUST differ
    // from the minimal/focused render. Without this, byte-identity is vacuous.
    let hot_balanced = all_hot(RenderQuality::Balanced);
    assert_eq!(
        hot_balanced.effective_focus_dim(),
        0.6,
        "balanced must preserve the live focus_dim"
    );
    let unfocused = composite_focus_dim(
        &snapshot,
        &atlas,
        CursorStyle::Block,
        hot_balanced.effective_focus_dim(),
    );
    assert!(
        !frames_match(&minimal, &unfocused),
        "control: a live (balanced) focus_dim must change pixels, or the \
         byte-identity proof above is vacuous"
    );
}
