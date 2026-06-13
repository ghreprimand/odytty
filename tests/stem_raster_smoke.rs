//! Live stem-darkening (RV5) atlas-raster proof.
//!
//! RV5 ships *default-on* (a conservative coverage boost so light-on-dark body
//! text holds weight); `stem_darken = 0.0` is the opt-out that restores the
//! classic raster byte-for-byte. The pure-function behaviour of the boost is
//! pinned by `src/atlas/tests/stem_darken.rs`; this binary proves the boost is
//! actually *wired through the live atlas raster* — i.e. that moving the global
//! `set_stem_darken` strength changes the bytes a real `GlyphAtlas::build`
//! produces, in the expected direction, with the endpoints pinned.
//!
//! ## Why a dedicated binary
//!
//! `set_stem_darken` is a *process-global* atomic read at raster time. The
//! `pixel_smoke` binary drives glyph style through *per-atlas* methods
//! (`set_synthetic_styles`, `set_geometric_boxdraw`), so it has no
//! global-serialization seam — toggling this global there could perturb sibling
//! tests that raster concurrently and assert byte-identity. Running the proof in
//! its own integration binary gives it a private process: this is the only code
//! that touches the global here, so there is no cross-test interference and no
//! flakiness. Tests still take a local lock and restore the global to the `0.0`
//! "off" sentinel on exit, so they are robust even when run in parallel within
//! this binary.

use std::sync::Mutex;

use odytty::atlas::{GlyphAtlas, set_stem_darken};
use odytty::settings::DEFAULT_STEM_DARKEN;
use odytty::text;

/// Build size for the proof atlas — large enough that printable ASCII produces
/// plenty of anti-aliased edge/stem coverage to observe, small enough to stay
/// fast.
const PX: f32 = 28.0;

/// Serializes the process-global `set_stem_darken` writes across this binary's
/// tests, so a non-zero window in one test cannot bleed into another's build.
static STEM_LOCK: Mutex<()> = Mutex::new(());

/// Build a fresh grayscale atlas at the given stem-darken strength and return a
/// snapshot of its coverage bytes. The global is restored to the `0.0` off
/// sentinel before the lock is released, so the process is always left in the
/// default (classic) state.
fn raster_at(strength: f32) -> Vec<u8> {
    let font = text::load_font().expect("system font available (caller gates on load)");
    let _guard = STEM_LOCK.lock().unwrap();
    set_stem_darken(strength);
    let atlas = GlyphAtlas::build(&font, PX);
    let data = atlas.data.clone();
    set_stem_darken(0.0); // leave the global at the off sentinel
    data
}

/// Skip the whole proof when the host has no loadable font (CI images without
/// one), matching the `pixel_smoke` harness convention.
fn font_available() -> bool {
    text::load_font().is_ok()
}

#[test]
fn default_strength_boosts_midtone_coverage_through_live_raster() {
    if !font_available() {
        eprintln!("skipping: no system font");
        return;
    }
    // (a) The shipped default (`DEFAULT_STEM_DARKEN`) must visibly change the
    // *live* atlas raster versus the classic 0.0 baseline — proving the
    // default-on appearance flows through the real raster, not just the pure
    // function. The boost is monotone (never reduces coverage), so every byte
    // that differs must have risen, and some must rise strictly.
    let baseline = raster_at(0.0);
    let boosted = raster_at(DEFAULT_STEM_DARKEN);
    assert_eq!(
        baseline.len(),
        boosted.len(),
        "atlas geometry must be identical regardless of stem strength"
    );
    assert_ne!(
        baseline, boosted,
        "default-on stem darkening must change the live raster vs the 0.0 baseline"
    );

    let mut raised = 0usize;
    for (i, (&b, &s)) in baseline.iter().zip(boosted.iter()).enumerate() {
        assert!(
            s >= b,
            "byte {i}: stem darkening must not reduce coverage ({b} -> {s})"
        );
        if s > b {
            raised += 1;
        }
    }
    assert!(
        raised > 0,
        "default-on stem darkening must raise midtone coverage on at least one sample"
    );
}

#[test]
fn opt_out_zero_restores_classic_raster() {
    if !font_available() {
        eprintln!("skipping: no system font");
        return;
    }
    // (b) The opt-out: building at 0.0 must reproduce the classic, pre-feature
    // raster byte-for-byte, even after a non-zero build has run. raster_at()
    // restores the global to 0.0, so this exercises the real off path.
    let baseline = raster_at(0.0);
    let _ = raster_at(DEFAULT_STEM_DARKEN); // dirty the global, then restore
    let opt_out = raster_at(0.0);
    assert_eq!(
        baseline, opt_out,
        "stem_darken=0.0 must be byte-identical to the pre-feature raster"
    );
}

#[test]
fn endpoints_stay_pinned_at_default_strength() {
    if !font_available() {
        eprintln!("skipping: no system font");
        return;
    }
    // (c) Fully-uncovered (0) and fully-covered (255) samples never move at any
    // strength — only intermediate (anti-aliased edge / thin stem) coverage is
    // boosted. So glyph ink bounds and solid interiors are unchanged.
    let baseline = raster_at(0.0);
    let boosted = raster_at(DEFAULT_STEM_DARKEN);
    for (i, (&b, &s)) in baseline.iter().zip(boosted.iter()).enumerate() {
        if b == 0 {
            assert_eq!(s, 0, "byte {i}: uncovered pixel must stay uncovered");
        } else if b == 255 {
            assert_eq!(s, 255, "byte {i}: fully-covered pixel must stay covered");
        }
    }
}
