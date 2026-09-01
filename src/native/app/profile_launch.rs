// SPDX-License-Identifier: GPL-3.0-only
//! Native-side named profile resolution and lazy catalog loading.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::native::options::NativeOptions;
use crate::profiles::{
    EffectiveLaunch, LaunchCliOverrides, LiveLaunchOverrides, LocalLaunchPlan, ProfileCatalog,
    RestoredLaunchOverrides, load_catalog_from_dir, precedence, profiles_dir_path,
};
use crate::settings::{ConfigValues, SETTING_ENV_KEYS, Settings};

/// Resolve startup launch inputs for the first window.
///
/// The selected profile is the CLI `--profile` name when present, otherwise the
/// saved global `default_launch_profile`. Returns merged settings, an optional
/// local spawn plan, and any resolver warnings. When neither names a profile
/// the input settings pass through unchanged and no catalog load occurs, so the
/// System Default startup path never scans the profile directory. A configured
/// default performs one bounded local catalog read; a missing or invalid name
/// falls back to the built-in System Default with a warning and never rewrites
/// the saved default.
pub(crate) fn resolve_startup_launch(
    options: &NativeOptions,
    settings: Settings,
) -> (Settings, Option<LocalLaunchPlan>, Vec<String>) {
    let Some(profile_name) = pick_default_profile_name(
        options.profile_name.as_deref(),
        settings.default_launch_profile.as_deref(),
    ) else {
        return (settings, None, Vec::new());
    };
    let catalog = load_profile_catalog();
    let cli = launch_cli_from_options(options, Some(profile_name));
    let effective = resolve_effective_launch(&catalog, &cli, &RestoredLaunchOverrides::default());
    let plan = LocalLaunchPlan::from_effective(&effective);
    (effective.settings, Some(plan), effective.warnings)
}

pub(crate) fn load_profile_catalog() -> ProfileCatalog {
    profiles_dir_path()
        .map(|dir| load_catalog_from_dir(&dir))
        .unwrap_or_default()
}

pub(crate) fn launch_cli_from_options(
    options: &NativeOptions,
    profile_name: Option<&str>,
) -> LaunchCliOverrides {
    LaunchCliOverrides {
        profile_name: profile_name
            .map(str::to_owned)
            .or_else(|| options.profile_name.clone()),
        working_directory: options
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        command: options
            .command
            .as_ref()
            .map(|command| crate::profiles::ProfileCommand {
                program: command.program.to_string_lossy().into_owned(),
                args: command
                    .args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
                preserved: Default::default(),
            }),
        title: Some(options.title.clone()).filter(|title| title != "OdyTTY"),
        ..LaunchCliOverrides::default()
    }
}

pub(crate) fn resolve_effective_launch(
    catalog: &ProfileCatalog,
    cli: &LaunchCliOverrides,
    restored: &RestoredLaunchOverrides,
) -> EffectiveLaunch {
    resolve_effective_launch_with_workspace(catalog, cli, restored, None)
}

pub(crate) fn resolve_effective_launch_with_workspace(
    catalog: &ProfileCatalog,
    cli: &LaunchCliOverrides,
    restored: &RestoredLaunchOverrides,
    workspace_launch_profile: Option<&str>,
) -> EffectiveLaunch {
    let config = load_config_values();
    let env = process_env_for_settings();
    precedence::resolve_effective_launch(
        config.as_ref(),
        &env,
        catalog,
        cli,
        restored,
        &LiveLaunchOverrides::default(),
        workspace_launch_profile,
    )
}

/// Resolve launch context for a new local tab or live auto-switch.
///
/// `inherited_cwd` is the active pane's OSC 7 directory when known. It is a
/// fallback only: profile, restored, and true CLI working-directory overrides
/// outrank it via the precedence resolver.
pub(crate) fn resolve_for_new_local_tab(
    settings: &Settings,
    workspace_launch_profile: Option<&str>,
    inherited_cwd: Option<PathBuf>,
    explicit_profile: Option<&str>,
) -> EffectiveLaunch {
    resolve_local_tab_launch(
        settings,
        &load_profile_catalog(),
        workspace_launch_profile,
        inherited_cwd,
        explicit_profile,
        &RestoredLaunchOverrides::default(),
    )
}

/// Pick the profile name for plain New Tab / New Workspace before catalog load.
pub(crate) fn pick_default_profile_name<'a>(
    workspace_launch_profile: Option<&'a str>,
    global_default: Option<&'a str>,
) -> Option<&'a str> {
    workspace_launch_profile
        .filter(|name| !name.is_empty())
        .or_else(|| global_default.filter(|name| !name.is_empty()))
}

/// Resolve launch context for plain New Tab / New Workspace when the workspace
/// has a `launch_profile` override or the global default is configured.
/// Returns `None` when neither applies (System Default / bare local spawn).
pub(crate) fn resolve_default_launch_for_new_tab(
    settings: &Settings,
    workspace_launch_profile: Option<&str>,
    inherited_cwd: Option<PathBuf>,
) -> Option<EffectiveLaunch> {
    let picked = pick_default_profile_name(
        workspace_launch_profile,
        settings.default_launch_profile.as_deref(),
    )?;
    if workspace_launch_profile.is_some_and(|name| !name.is_empty()) {
        return Some(resolve_for_new_local_tab(
            settings,
            workspace_launch_profile,
            inherited_cwd,
            None,
        ));
    }
    Some(resolve_for_new_local_tab(
        settings,
        None,
        inherited_cwd,
        Some(picked),
    ))
}

/// Resolve launch context when restoring a persisted local leaf with a named
/// profile. Captured cwd travels through [`RestoredLaunchOverrides`] so it
/// outranks the profile's configured starting directory without masquerading
/// as a CLI override.
pub(crate) fn resolve_for_restored_local_leaf(
    settings: &Settings,
    launch_profile: &str,
    restored_cwd: Option<PathBuf>,
) -> EffectiveLaunch {
    let restored = RestoredLaunchOverrides {
        profile_name: Some(launch_profile.to_owned()),
        working_directory: restored_cwd.as_ref().map(|path| path.display().to_string()),
        ..RestoredLaunchOverrides::default()
    };
    resolve_local_tab_launch(
        settings,
        &load_profile_catalog(),
        None,
        None,
        None,
        &restored,
    )
}

pub(crate) fn resolve_local_tab_launch(
    settings: &Settings,
    catalog: &ProfileCatalog,
    workspace_launch_profile: Option<&str>,
    inherited_cwd: Option<PathBuf>,
    explicit_profile: Option<&str>,
    restored: &RestoredLaunchOverrides,
) -> EffectiveLaunch {
    let mut cli = LaunchCliOverrides::default();
    if let Some(name) = explicit_profile.filter(|name| !name.is_empty()) {
        cli.profile_name = Some(name.to_owned());
    }
    let mut effective =
        resolve_effective_launch_with_workspace(catalog, &cli, restored, workspace_launch_profile);
    effective.settings.shell_integration = settings.shell_integration;
    if effective.working_directory.is_none() {
        effective.working_directory = inherited_cwd;
    }
    effective
}

pub(crate) fn connection_profile_rows_for_manager(
    catalog: &ProfileCatalog,
) -> Vec<crate::native::connection_overlay::ConnectionProfileRow> {
    let mut rows: Vec<_> = catalog
        .profiles
        .values()
        .filter(|profile| profile.applies_on_current_platform())
        .map(
            |profile| crate::native::connection_overlay::ConnectionProfileRow {
                name: profile.name.clone(),
                label: profile
                    .display_name
                    .clone()
                    .unwrap_or_else(|| profile.name.clone()),
                connection: profile.connection.clone(),
            },
        )
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

pub(crate) fn profile_display_names(catalog: &ProfileCatalog) -> Vec<(String, String)> {
    let mut names: Vec<_> = catalog
        .profiles
        .values()
        .filter(|profile| profile.applies_on_current_platform())
        .map(|profile| {
            (
                profile.name.clone(),
                profile
                    .display_name
                    .clone()
                    .unwrap_or_else(|| profile.name.clone()),
            )
        })
        .collect();
    names.sort_by(|(a, _), (b, _)| a.cmp(b));
    names
}

pub(crate) fn profile_picker_entries(
    catalog: &ProfileCatalog,
) -> Vec<crate::native::profile_picker::ProfilePickerEntry> {
    profile_display_names(catalog)
        .into_iter()
        .map(|(name, label)| crate::native::profile_picker::ProfilePickerEntry { name, label })
        .collect()
}

pub(crate) fn spawn_restored_local_leaf(
    settings: &Settings,
    grid: crate::core::Dimensions,
    set: &mut crate::native::session::WorkspaceSet,
    leaf: crate::native::session::RestoredLocalLeaf,
) -> Option<crate::native::session::SessionToken> {
    if let Some(name) = leaf.launch_profile.as_deref() {
        let effective = resolve_for_restored_local_leaf(settings, name, leaf.cwd.clone());
        set.insert_restored_session_with_effective(grid, leaf.cwd.clone(), &effective)
            .ok()
    } else {
        set.insert_restored_session(grid, leaf.cwd).ok()
    }
}

fn load_config_values() -> Option<ConfigValues> {
    let path = crate::settings::config_file_path()?;
    let contents = crate::settings::fs_read::read_capped(&path).ok()?;
    Some(ConfigValues::parse(&contents, |_| {}))
}

fn process_env_for_settings() -> HashMap<&'static str, OsString> {
    SETTING_ENV_KEYS
        .iter()
        .filter_map(|&key| std::env::var_os(key).map(|value| (key, value)))
        .collect()
}

#[cfg(test)]
mod tests {
    //! v0.14 Phase A3 final-surface: the connection-manager picker/launch route
    //! data builders. `connection_profile_rows_for_manager` feeds the Connect
    //! picker's profile rows; `profile_display_names` feeds the palette/menu
    //! surfaces. Both must filter by the current platform, sort deterministically
    //! by name, carry the display-name fallback, and surface the connection
    //! reference so the picker can show `-> host` vs `(local)`.
    use super::{
        connection_profile_rows_for_manager, profile_display_names, resolve_local_tab_launch,
    };
    use crate::profiles::{
        LaunchProfile, ProfileCatalog, ProfilePlatform, RestoredLaunchOverrides,
    };
    use crate::settings::Settings;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn catalog() -> ProfileCatalog {
        let mut catalog = ProfileCatalog::default();
        // Out of alphabetical order on insert so the sort is genuinely exercised.
        let mut zeta = LaunchProfile::new("zeta").expect("profile");
        zeta.display_name = Some("Zeta Edge".to_owned());
        zeta.connection = Some("edge-host".to_owned());
        catalog.profiles.insert("zeta".to_owned(), zeta);

        // No display_name -> falls back to the name; no connection -> local.
        let alpha = LaunchProfile::new("alpha").expect("profile");
        catalog.profiles.insert("alpha".to_owned(), alpha);
        catalog
    }

    #[test]
    fn connection_rows_sort_by_name_and_carry_label_and_connection() {
        let rows = connection_profile_rows_for_manager(&catalog());
        assert_eq!(rows.len(), 2);
        // Deterministic sort by profile name regardless of insert order.
        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[1].name, "zeta");
        // display_name fallback: absent -> the profile name itself.
        assert_eq!(rows[0].label, "alpha");
        assert_eq!(rows[0].connection, None);
        // Present display_name + connection reference are surfaced verbatim.
        assert_eq!(rows[1].label, "Zeta Edge");
        assert_eq!(rows[1].connection.as_deref(), Some("edge-host"));
    }

    #[test]
    fn profile_display_names_sort_and_fall_back_to_name() {
        let names = profile_display_names(&catalog());
        assert_eq!(
            names,
            vec![
                ("alpha".to_owned(), "alpha".to_owned()),
                ("zeta".to_owned(), "Zeta Edge".to_owned()),
            ],
        );
    }

    #[test]
    fn platform_scoped_profile_is_excluded_from_both_launch_surfaces() {
        // A profile whose platform set names only the two NON-current platforms
        // must not appear in either launch surface. The set is built by cfg so
        // the fixture is genuinely off-platform on Linux, macOS, and Windows
        // alike (an empty set, by contrast, means "applies everywhere").
        #[cfg(target_os = "linux")]
        let others = [ProfilePlatform::Macos, ProfilePlatform::Windows];
        #[cfg(target_os = "macos")]
        let others = [ProfilePlatform::Linux, ProfilePlatform::Windows];
        #[cfg(windows)]
        let others = [ProfilePlatform::Linux, ProfilePlatform::Macos];

        let mut catalog = ProfileCatalog::default();
        let mut scoped = LaunchProfile::new("scoped").expect("profile");
        scoped.platforms = Some(others.into_iter().collect::<BTreeSet<ProfilePlatform>>());
        catalog.profiles.insert("scoped".to_owned(), scoped);
        let mut always = LaunchProfile::new("always").expect("profile");
        always.platforms = None;
        catalog.profiles.insert("always".to_owned(), always);

        let rows = connection_profile_rows_for_manager(&catalog);
        assert_eq!(rows.len(), 1, "the off-platform profile is filtered out");
        assert_eq!(rows[0].name, "always");

        let names = profile_display_names(&catalog);
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].0, "always");
    }

    fn cwd_test_catalog() -> ProfileCatalog {
        let mut catalog = ProfileCatalog::default();
        let mut with_cwd = LaunchProfile::new("with-cwd").expect("profile");
        with_cwd.launch.working_directory = Some("/from/profile".to_owned());
        catalog.profiles.insert("with-cwd".to_owned(), with_cwd);
        let bare = LaunchProfile::new("bare").expect("profile");
        catalog.profiles.insert("bare".to_owned(), bare);
        catalog
    }

    #[test]
    fn pick_default_profile_prefers_workspace_override() {
        assert_eq!(
            super::pick_default_profile_name(Some("work"), Some("global")),
            Some("work")
        );
    }

    #[test]
    fn pick_default_profile_falls_back_to_global() {
        assert_eq!(
            super::pick_default_profile_name(None, Some("global")),
            Some("global")
        );
    }

    #[test]
    fn pick_default_profile_none_when_unset() {
        assert_eq!(super::pick_default_profile_name(None, None), None);
    }

    #[test]
    fn profile_starting_directory_outranks_inherited_pane_cwd() {
        let settings = Settings::default();
        let catalog = cwd_test_catalog();
        let effective = resolve_local_tab_launch(
            &settings,
            &catalog,
            None,
            Some(PathBuf::from("/from/pane")),
            Some("with-cwd"),
            &RestoredLaunchOverrides::default(),
        );
        assert_eq!(
            effective.working_directory,
            Some(PathBuf::from("/from/profile"))
        );
    }

    #[test]
    fn inherited_pane_cwd_falls_back_when_profile_has_no_starting_directory() {
        let settings = Settings::default();
        let catalog = cwd_test_catalog();
        let effective = resolve_local_tab_launch(
            &settings,
            &catalog,
            None,
            Some(PathBuf::from("/from/pane")),
            Some("bare"),
            &RestoredLaunchOverrides::default(),
        );
        assert_eq!(
            effective.working_directory,
            Some(PathBuf::from("/from/pane"))
        );
    }

    #[test]
    fn restored_cwd_outranks_profile_starting_directory() {
        let settings = Settings::default();
        let catalog = cwd_test_catalog();
        let effective = resolve_local_tab_launch(
            &settings,
            &catalog,
            None,
            None,
            None,
            &RestoredLaunchOverrides {
                profile_name: Some("with-cwd".to_owned()),
                working_directory: Some("/from/restored".to_owned()),
                ..RestoredLaunchOverrides::default()
            },
        );
        assert_eq!(
            effective.working_directory,
            Some(PathBuf::from("/from/restored"))
        );
    }
}
