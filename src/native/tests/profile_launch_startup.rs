// SPDX-License-Identifier: GPL-3.0-only
//! Startup isolation: the ordinary default launch must not read the profile
//! catalog. Drives the real native startup resolver headlessly
//! and observes catalog loads only on the resolver's calling test thread.

use crate::native::app::profile_launch::resolve_startup_launch;
use crate::native::options::NativeOptions;
use crate::profiles::{catalog_load_count_for_test, reset_catalog_load_count_for_test};
use crate::settings::Settings;

/// Run `f` with the configuration and state folders redirected into a scratch
/// folder, under the shared environment and catalog-count locks, so the startup
/// resolver never enumerates or parses the real profile directory.
fn redirected<R>(f: impl FnOnce() -> R) -> R {
    let scratch = crate::test_dirs::ScratchDir::new("ottps");
    super::config_env::with_config_base(scratch.path(), true, f)
}

#[test]
fn default_startup_launch_reads_no_profile_catalog() {
    redirected(|| {
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
    });
}

#[test]
fn explicit_profile_selection_loads_the_catalog_exactly_once() {
    redirected(|| {
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
    });
}

#[test]
fn global_default_profile_applies_to_the_first_window_with_one_catalog_load() {
    redirected(|| {
        reset_catalog_load_count_for_test();
        let before = catalog_load_count_for_test();

        let options = NativeOptions::from_settings(&Settings::default());
        assert!(options.profile_name.is_none());
        let settings = Settings {
            default_launch_profile: Some("missing-a3-default-fixture".to_owned()),
            ..Settings::default()
        };

        let (_settings, plan, warnings) = resolve_startup_launch(&options, settings);

        assert_eq!(
            catalog_load_count_for_test(),
            before + 1,
            "a configured global default performs exactly one bounded catalog load"
        );
        assert!(
            plan.is_some(),
            "the global default selects a profile-resolved startup plan"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("missing-a3-default-fixture")),
            "a missing default falls back with a bounded warning naming it, got {warnings:?}"
        );
    });
}

#[test]
fn cli_profile_outranks_the_global_default_at_startup() {
    redirected(|| {
        reset_catalog_load_count_for_test();

        let mut options = NativeOptions::from_settings(&Settings::default());
        options.profile_name = Some("cli-a3-fixture".to_owned());
        let settings = Settings {
            default_launch_profile: Some("global-a3-fixture".to_owned()),
            ..Settings::default()
        };

        let (_settings, _plan, warnings) = resolve_startup_launch(&options, settings);

        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("cli-a3-fixture")),
            "the CLI selection is the one resolved, got {warnings:?}"
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("global-a3-fixture")),
            "the global default is not consulted when --profile is given, got {warnings:?}"
        );
    });
}
