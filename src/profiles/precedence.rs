// SPDX-License-Identifier: GPL-3.0-only
//! Deterministic launch-profile precedence resolution.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;

use crate::settings::{
    BLOOM_ENV, CRT_ENV, CURSOR_BLINK_ENV, CURSOR_STYLE_ENV, ConfigValues,
    EXTERNAL_PALETTE_PATH_ENV, EXTERNAL_PALETTE_PROVIDER_ENV, FOLLOW_EXTERNAL_PALETTE_ENV,
    FONT_ENV, FONT_FAMILY_ENV, FONT_SIZE_ENV, FONT_WEIGHT_ENV, RENDER_QUALITY_ENV, RETRO_ENV,
    Settings, THEME_ENV, VISUAL_ENV, resolve_theme_file, theme_dir_path,
};
use crate::text;

use super::schema::{LaunchProfile, ProfileCommand, ProfileError};
use super::store::ProfileCatalog;

/// Explicit CLI launch overrides for one startup.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaunchCliOverrides {
    pub profile_name: Option<String>,
    pub shell: Option<String>,
    pub command: Option<ProfileCommand>,
    pub working_directory: Option<String>,
    pub env: BTreeMap<String, String>,
    pub connection: Option<String>,
    pub layout: Option<String>,
    pub title: Option<String>,
    pub settings: BTreeMap<&'static str, String>,
}

/// Restored workspace/session hints applied after the named profile layer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoredLaunchOverrides {
    pub profile_name: Option<String>,
    pub working_directory: Option<String>,
    pub title: Option<String>,
}

/// Live UI edits sit above every startup-scoped layer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiveLaunchOverrides {
    pub settings: BTreeMap<&'static str, String>,
    pub working_directory: Option<String>,
    pub title: Option<String>,
}

/// Fully resolved launch context after precedence merge.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveLaunch {
    pub settings: Settings,
    pub shell: Option<String>,
    pub command: Option<ProfileCommand>,
    pub working_directory: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub connection: Option<String>,
    pub layout: Option<String>,
    pub title: Option<String>,
    pub profile_name: Option<String>,
    /// The authored theme name the selected profile set (`appearance.theme`),
    /// when a profile is active and set one. `None` when no profile is selected
    /// or the profile inherits the global theme. Lets the launch path record the
    /// resolved theme as per-session state so a global theme sweep does not
    /// flatten the profile tab.
    pub profile_theme: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrecedenceLayer {
    BuiltinDefault,
    GlobalConfig,
    NamedProfile,
    WorkspaceBinding,
    StartupEnvironment,
    RestoredState,
    CliOverride,
    LiveUiEdit,
}

/// Resolve one launch context using the v0.14.0 precedence contract.
///
/// Layer order (lowest to highest priority) matches [`precedence_chain`]:
/// built-in defaults, global config, named profile, workspace named-profile
/// binding (explicit `launch_profile`; not the shipped workspace `default_profile`
/// host alias), startup environment, restored hints, CLI overrides, live UI edits.
pub(crate) fn resolve_effective_launch(
    config: Option<&ConfigValues>,
    env: &HashMap<&'static str, OsString>,
    catalog: &ProfileCatalog,
    cli: &LaunchCliOverrides,
    restored: &RestoredLaunchOverrides,
    live: &LiveLaunchOverrides,
    workspace_launch_profile: Option<&str>,
) -> EffectiveLaunch {
    let mut warnings = catalog.warnings.clone();
    let profile_name = cli
        .profile_name
        .clone()
        .or_else(|| restored.profile_name.clone())
        .or_else(|| {
            workspace_launch_profile
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        });

    let profile = profile_name
        .as_deref()
        .and_then(|name| match catalog.get(name) {
            Some(profile) if profile.applies_on_current_platform() => Some(profile),
            Some(_) => {
                warnings.push(format!(
                    "profile {name:?} does not apply on this platform; using global settings"
                ));
                None
            }
            None => {
                if profile_name.is_some() {
                    warnings.push(format!(
                        "profile {name:?} is missing; using global settings"
                    ));
                }
                None
            }
        });

    let profile_settings = profile.map(profile_settings_overrides).unwrap_or_default();
    let settings = resolve_settings(config, env, &profile_settings, cli, live, &mut warnings);

    EffectiveLaunch {
        settings,
        shell: cli
            .shell
            .clone()
            .or_else(|| profile.and_then(|p| p.launch.shell.clone())),
        command: cli
            .command
            .clone()
            .or_else(|| profile.and_then(|p| p.launch.command.clone())),
        working_directory: pick_launch_path(
            live.working_directory.as_deref(),
            cli.working_directory.as_deref(),
            restored.working_directory.as_deref(),
            profile.and_then(|p| p.launch.working_directory.as_deref()),
        ),
        env: merge_env(profile.map(|p| &p.launch.env), cli),
        connection: cli
            .connection
            .clone()
            .or_else(|| profile.and_then(|p| p.connection.clone())),
        layout: cli
            .layout
            .clone()
            .or_else(|| profile.and_then(|p| p.layout.saved_layout.clone())),
        title: pick_launch_string(
            live.title.as_deref(),
            cli.title.as_deref(),
            restored.title.as_deref(),
            profile.and_then(|p| p.appearance.title.as_deref()),
        ),
        profile_theme: profile
            .and_then(|p| p.appearance.theme.clone())
            .filter(|value| !value.is_empty()),
        profile_name,
        warnings,
    }
}

fn resolve_settings(
    config: Option<&ConfigValues>,
    env: &HashMap<&'static str, OsString>,
    profile: &BTreeMap<&'static str, String>,
    cli: &LaunchCliOverrides,
    live: &LiveLaunchOverrides,
    warnings: &mut Vec<String>,
) -> Settings {
    Settings::from_source(
        |key| {
            live.settings
                .get(key)
                .cloned()
                .or_else(|| cli.settings.get(key).cloned())
                .map(OsString::from)
                .or_else(|| env.get(key).cloned())
                .or_else(|| profile.get(key).cloned().map(OsString::from))
                .or_else(|| config.and_then(|cfg| cfg.get(key).cloned()))
        },
        |message| warnings.push(message.to_owned()),
        |family| {
            text::resolve_font_family(family, &text::font_search_dirs())
                .map(|matched| matched.regular)
        },
        |value| resolve_theme_file(value, theme_dir_path().as_deref()),
    )
}

fn profile_settings_overrides(profile: &LaunchProfile) -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    push_setting(&mut out, THEME_ENV, profile.appearance.theme.as_deref());
    push_setting(&mut out, VISUAL_ENV, profile.appearance.visual.as_deref());
    push_setting(&mut out, FONT_ENV, profile.appearance.font.as_deref());
    push_setting(
        &mut out,
        FONT_FAMILY_ENV,
        profile.appearance.font_family.as_deref(),
    );
    push_setting(
        &mut out,
        FONT_WEIGHT_ENV,
        profile.appearance.font_weight.as_deref(),
    );
    if let Some(size) = profile.appearance.font_size_px {
        out.insert(FONT_SIZE_ENV, size.to_string());
    }
    push_setting(&mut out, CURSOR_STYLE_ENV, profile.cursor.style.as_deref());
    push_setting(&mut out, CURSOR_BLINK_ENV, profile.cursor.blink.as_deref());
    push_setting(
        &mut out,
        RENDER_QUALITY_ENV,
        profile.effects.render_quality.as_deref(),
    );
    push_bool_setting(&mut out, BLOOM_ENV, profile.effects.bloom);
    push_bool_setting(&mut out, CRT_ENV, profile.effects.crt);
    push_bool_setting(&mut out, RETRO_ENV, profile.effects.retro);
    push_bool_setting(
        &mut out,
        FOLLOW_EXTERNAL_PALETTE_ENV,
        profile.appearance.follow_external_palette,
    );
    push_setting(
        &mut out,
        EXTERNAL_PALETTE_PROVIDER_ENV,
        profile.appearance.external_palette_provider.as_deref(),
    );
    push_setting(
        &mut out,
        EXTERNAL_PALETTE_PATH_ENV,
        profile.appearance.external_palette_path.as_deref(),
    );
    out
}

fn push_setting(out: &mut BTreeMap<&'static str, String>, key: &'static str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        out.insert(key, value.to_owned());
    }
}

fn push_bool_setting(
    out: &mut BTreeMap<&'static str, String>,
    key: &'static str,
    value: Option<bool>,
) {
    if let Some(value) = value {
        out.insert(key, if value { "on" } else { "off" }.to_owned());
    }
}

fn pick_launch_string(
    live: Option<&str>,
    cli: Option<&str>,
    restored: Option<&str>,
    profile: Option<&str>,
) -> Option<String> {
    live.filter(|value| !value.is_empty())
        .or(cli.filter(|value| !value.is_empty()))
        .or(restored.filter(|value| !value.is_empty()))
        .or(profile.filter(|value| !value.is_empty()))
        .map(str::to_owned)
}

fn pick_launch_path(
    live: Option<&str>,
    cli: Option<&str>,
    restored: Option<&str>,
    profile: Option<&str>,
) -> Option<PathBuf> {
    pick_launch_string(live, cli, restored, profile).map(PathBuf::from)
}

fn merge_env(
    profile: Option<&BTreeMap<String, String>>,
    cli: &LaunchCliOverrides,
) -> BTreeMap<String, String> {
    let mut out = profile.cloned().unwrap_or_default();
    for (key, value) in &cli.env {
        out.insert(key.clone(), value.clone());
    }
    out
}

/// Documented precedence labels for diagnostics and tests.
pub fn precedence_chain() -> &'static [PrecedenceLayer] {
    &[
        PrecedenceLayer::BuiltinDefault,
        PrecedenceLayer::GlobalConfig,
        PrecedenceLayer::NamedProfile,
        PrecedenceLayer::WorkspaceBinding,
        PrecedenceLayer::StartupEnvironment,
        PrecedenceLayer::RestoredState,
        PrecedenceLayer::CliOverride,
        PrecedenceLayer::LiveUiEdit,
    ]
}

/// Validate that a would-be named profile reference is safe to store.
pub fn validate_named_profile_reference(name: &str) -> Result<String, ProfileError> {
    super::schema::validate_profile_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::LaunchProfile;
    use crate::settings::DEFAULT_THEME;
    use crate::theme::Theme;

    fn empty_catalog() -> ProfileCatalog {
        ProfileCatalog::default()
    }

    #[test]
    fn profile_theme_overrides_config_but_env_wins() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.appearance.theme = Some("plain".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), profile);

        let config = ConfigValues::parse("theme = odyssey\n", |_| {});

        let mut env = HashMap::new();
        env.insert(THEME_ENV, OsString::from("odyssey-noir"));

        let cli = LaunchCliOverrides {
            profile_name: Some("dev".to_owned()),
            ..LaunchCliOverrides::default()
        };
        let effective = resolve_effective_launch(
            Some(&config),
            &env,
            &catalog,
            &cli,
            &RestoredLaunchOverrides::default(),
            &LiveLaunchOverrides::default(),
            None,
        );
        assert_eq!(
            effective.settings.theme,
            Theme::from_name("odyssey-noir").expect("theme")
        );
    }

    #[test]
    fn missing_profile_warns_and_falls_back_to_global_settings() {
        let config = ConfigValues::parse("", |_| {});
        let cli = LaunchCliOverrides {
            profile_name: Some("missing".to_owned()),
            ..LaunchCliOverrides::default()
        };
        let effective = resolve_effective_launch(
            Some(&config),
            &HashMap::new(),
            &empty_catalog(),
            &cli,
            &RestoredLaunchOverrides::default(),
            &LiveLaunchOverrides::default(),
            None,
        );
        assert_eq!(effective.settings.theme, DEFAULT_THEME);
        assert!(
            effective
                .warnings
                .iter()
                .any(|warning| warning.contains("missing"))
        );
    }

    #[test]
    fn cli_overrides_sit_above_profile_launch_fields() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.launch.working_directory = Some("/from/profile".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), profile);

        let cli = LaunchCliOverrides {
            profile_name: Some("dev".to_owned()),
            working_directory: Some("/from/cli".to_owned()),
            ..LaunchCliOverrides::default()
        };
        let effective = resolve_effective_launch(
            None,
            &HashMap::new(),
            &catalog,
            &cli,
            &RestoredLaunchOverrides::default(),
            &LiveLaunchOverrides::default(),
            None,
        );
        assert_eq!(
            effective.working_directory,
            Some(PathBuf::from("/from/cli"))
        );
    }

    #[test]
    fn profile_external_palette_fields_override_global_settings() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.appearance.follow_external_palette = Some(true);
        profile.appearance.external_palette_provider = Some("colors_toml".to_owned());
        profile.appearance.external_palette_path = Some("/tmp/synthetic-palette.toml".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), profile);

        let cli = LaunchCliOverrides {
            profile_name: Some("dev".to_owned()),
            ..LaunchCliOverrides::default()
        };
        let effective = resolve_effective_launch(
            None,
            &HashMap::new(),
            &catalog,
            &cli,
            &RestoredLaunchOverrides::default(),
            &LiveLaunchOverrides::default(),
            None,
        );
        assert!(effective.settings.follow_external_palette);
        assert_eq!(
            effective.settings.external_palette_provider,
            crate::external_palette::ExternalPaletteProvider::ColorsToml
        );
        assert_eq!(
            effective.settings.external_palette_path.as_deref(),
            Some("/tmp/synthetic-palette.toml")
        );
    }

    #[test]
    fn live_ui_working_directory_overrides_cli_and_restored() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.launch.working_directory = Some("/from/profile".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), profile);

        let cli = LaunchCliOverrides {
            profile_name: Some("dev".to_owned()),
            working_directory: Some("/from/cli".to_owned()),
            ..LaunchCliOverrides::default()
        };
        let restored = RestoredLaunchOverrides {
            working_directory: Some("/from/restored".to_owned()),
            ..RestoredLaunchOverrides::default()
        };
        let live = LiveLaunchOverrides {
            working_directory: Some("/from/live".to_owned()),
            ..LiveLaunchOverrides::default()
        };
        let effective = resolve_effective_launch(
            None,
            &HashMap::new(),
            &catalog,
            &cli,
            &restored,
            &live,
            None,
        );
        assert_eq!(
            effective.working_directory,
            Some(PathBuf::from("/from/live"))
        );
    }

    #[test]
    fn workspace_launch_profile_selects_named_profile_when_cli_is_absent() {
        let mut profile = LaunchProfile::new("bound").expect("profile");
        profile.launch.shell = Some("/bin/zsh".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("bound".to_owned(), profile);

        let effective = resolve_effective_launch(
            None,
            &HashMap::new(),
            &catalog,
            &LaunchCliOverrides::default(),
            &RestoredLaunchOverrides::default(),
            &LiveLaunchOverrides::default(),
            Some("bound"),
        );
        assert_eq!(effective.profile_name.as_deref(), Some("bound"));
        assert_eq!(effective.shell.as_deref(), Some("/bin/zsh"));
    }

    #[test]
    fn live_ui_title_overrides_cli_and_restored() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.appearance.title = Some("profile title".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), profile);

        let cli = LaunchCliOverrides {
            profile_name: Some("dev".to_owned()),
            title: Some("cli title".to_owned()),
            ..LaunchCliOverrides::default()
        };
        let restored = RestoredLaunchOverrides {
            title: Some("restored title".to_owned()),
            ..RestoredLaunchOverrides::default()
        };
        let live = LiveLaunchOverrides {
            title: Some("live title".to_owned()),
            ..LiveLaunchOverrides::default()
        };
        let effective = resolve_effective_launch(
            None,
            &HashMap::new(),
            &catalog,
            &cli,
            &restored,
            &live,
            None,
        );
        assert_eq!(effective.title.as_deref(), Some("live title"));
    }
}
