// SPDX-License-Identifier: GPL-3.0-only
//! Profile-aware local PTY spawn planning from [`EffectiveLaunch`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::pty::{CommandBuilder, PtySession};
use crate::settings::Settings;

use super::precedence::EffectiveLaunch;
use super::schema::ProfileCommand;

/// Resolved local spawn inputs for one profile-aware session insert.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalLaunchPlan {
    pub settings: Settings,
    pub working_directory: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub spawn: LocalSpawnKind,
}

/// How the local PTY child should be started.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalSpawnKind {
    DefaultShell,
    Shell { program: String, args: Vec<String> },
    Exec(ProfileCommand),
}

impl LocalLaunchPlan {
    pub fn from_effective(effective: &EffectiveLaunch) -> Self {
        let spawn = if let Some(command) = effective.command.clone() {
            LocalSpawnKind::Exec(command)
        } else if let Some(shell) = effective.shell.clone() {
            LocalSpawnKind::Shell {
                program: shell,
                args: Vec::new(),
            }
        } else {
            LocalSpawnKind::DefaultShell
        };
        Self {
            settings: effective.settings.clone(),
            working_directory: effective.working_directory.clone(),
            env: effective.env.clone(),
            spawn,
        }
    }
}

/// Build the platform [`CommandBuilder`] for a non-default [`LocalLaunchPlan`].
pub fn build_local_command(plan: &LocalLaunchPlan) -> CommandBuilder {
    let mut command = match &plan.spawn {
        LocalSpawnKind::DefaultShell => unreachable!("default shell uses dedicated spawn path"),
        LocalSpawnKind::Shell { program, args } => {
            let mut builder = CommandBuilder::new(OsString::from(program.clone()));
            for arg in args {
                builder.arg(OsString::from(arg.clone()));
            }
            builder
        }
        LocalSpawnKind::Exec(profile_command) => {
            let mut builder = CommandBuilder::new(OsString::from(profile_command.program.clone()));
            for arg in &profile_command.args {
                builder.arg(OsString::from(arg.clone()));
            }
            builder
        }
    };
    match &plan.spawn {
        LocalSpawnKind::Exec(_) => {
            command.apply_standard_exec_env(&plan.env);
        }
        _ => {
            command.apply_standard_interactive_shell_env(&plan.settings);
            for (key, value) in &plan.env {
                command.env(key.clone(), value.clone());
            }
        }
    }
    if let Some(path) = plan.working_directory.as_ref() {
        command.current_dir(path.clone());
    }
    command
}

/// Spawn a local PTY from a resolved [`LocalLaunchPlan`].
pub fn spawn_local_plan(
    grid: crate::core::Dimensions,
    plan: &LocalLaunchPlan,
) -> Result<PtySession, anyhow::Error> {
    if matches!(plan.spawn, LocalSpawnKind::DefaultShell) {
        return PtySession::spawn_default_shell_in_with_settings(
            grid,
            plan.working_directory.clone(),
            &plan.settings,
        );
    }
    let command = build_local_command(plan);
    PtySession::spawn_command(grid, command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::precedence::{
        LaunchCliOverrides, LiveLaunchOverrides, RestoredLaunchOverrides, resolve_effective_launch,
    };
    use crate::profiles::{LaunchProfile, ProfileCatalog, ProfileCommand};
    use std::collections::HashMap;

    #[test]
    fn profile_shell_overrides_default_shell_plan() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.launch.shell = Some("/bin/zsh".to_owned());
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
        let plan = LocalLaunchPlan::from_effective(&effective);
        assert!(matches!(
            plan.spawn,
            LocalSpawnKind::Shell { ref program, .. } if program == "/bin/zsh"
        ));
    }

    #[test]
    fn profile_command_wins_over_shell_in_plan() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.launch.shell = Some("/bin/zsh".to_owned());
        profile.launch.command = Some(ProfileCommand {
            program: "echo".to_owned(),
            args: vec!["hi".to_owned()],
            preserved: Default::default(),
        });
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
        let plan = LocalLaunchPlan::from_effective(&effective);
        assert!(matches!(plan.spawn, LocalSpawnKind::Exec(_)));
    }

    // ---- v0.14 Phase A3 adversarial: plan carries env/cwd, default fallback ----

    #[test]
    fn a3_plan_carries_env_and_working_directory_from_profile() {
        // A launch surface's resolved env and working directory must survive into
        // the spawn plan verbatim, so the child starts in the profile's context.
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.launch.shell = Some("/bin/zsh".to_owned());
        profile.launch.working_directory = Some("/work/project".to_owned());
        profile
            .launch
            .env
            .insert("PROJECT".to_owned(), "odytty".to_owned());
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
        let plan = LocalLaunchPlan::from_effective(&effective);
        assert_eq!(
            plan.working_directory.as_deref(),
            Some(std::path::Path::new("/work/project"))
        );
        assert_eq!(plan.env.get("PROJECT").map(String::as_str), Some("odytty"));
    }

    #[test]
    fn a3_default_shell_plan_when_profile_sets_neither_shell_nor_command() {
        // A named profile that customizes only appearance/env must still spawn the
        // platform default shell, not an empty or fabricated command.
        let mut profile = LaunchProfile::new("themed").expect("profile");
        profile.appearance.font_family = Some("Victor Mono".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("themed".to_owned(), profile);
        let cli = LaunchCliOverrides {
            profile_name: Some("themed".to_owned()),
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
        let plan = LocalLaunchPlan::from_effective(&effective);
        assert_eq!(plan.spawn, LocalSpawnKind::DefaultShell);
    }

    #[test]
    fn a3_shell_spawn_applies_structured_args() {
        let plan = LocalLaunchPlan {
            settings: Settings::default(),
            working_directory: None,
            env: BTreeMap::new(),
            spawn: LocalSpawnKind::Shell {
                program: "wsl.exe".to_owned(),
                args: vec!["-d".to_owned(), "Ubuntu".to_owned()],
            },
        };
        let command = build_local_command(&plan);
        assert_eq!(
            command
                .args_for_test()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["-d".to_owned(), "Ubuntu".to_owned()]
        );
    }
}
