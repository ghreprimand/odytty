// SPDX-License-Identifier: GPL-3.0-only
//! v0.14 Phase A3 startup-isolation seam: the ordinary default launch must not
//! read the profile catalog. Drives the real native startup resolver headlessly
//! and asserts the shared catalog-load counter does not move.

use crate::native::app::profile_launch::resolve_startup_launch;
use crate::native::options::NativeOptions;
use crate::profiles::{catalog_load_count_for_test, reset_catalog_load_count_for_test};
use crate::settings::Settings;

#[test]
fn default_startup_launch_reads_no_profile_catalog() {
    reset_catalog_load_count_for_test();
    let before = catalog_load_count_for_test();

    // A default launch carries no --profile selection.
    let options = NativeOptions::from_settings(&Settings::default());
    assert!(
        options.profile_name.is_none(),
        "precondition: a default launch selects no named profile"
    );

    let (_settings, plan, warnings) = resolve_startup_launch(&options, Settings::default());

    assert_eq!(
        catalog_load_count_for_test(),
        before,
        "the default startup path must not enumerate or load the profile catalog"
    );
    assert!(
        plan.is_none(),
        "no profile selected means no profile-specific spawn plan"
    );
    assert!(
        warnings.is_empty(),
        "a clean default launch produces no resolver warnings, got {warnings:?}"
    );
}

#[test]
fn explicit_profile_selection_loads_the_catalog_exactly_once() {
    reset_catalog_load_count_for_test();
    let before = catalog_load_count_for_test();

    let mut options = NativeOptions::from_settings(&Settings::default());
    options.profile_name = Some("nonexistent-a3-fixture".to_owned());

    // The profile does not exist on disk, but selecting one is what authorizes a
    // catalog load: the count moves exactly once for the single resolution.
    let (_settings, _plan, _warnings) = resolve_startup_launch(&options, Settings::default());

    assert_eq!(
        catalog_load_count_for_test(),
        before + 1,
        "an explicit profile selection must trigger exactly one catalog load"
    );
}
