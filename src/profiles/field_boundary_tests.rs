// SPDX-License-Identifier: GPL-3.0-only
//! Profile field boundaries: names, launch text, font size, optional arrays,
//! switch-rule typos, secret-key messages and case-only save collisions.

use std::fs;
use std::path::{Path, PathBuf};

use super::schema::{LaunchProfile, ProfileError, validate_profile_name};
use super::{
    LocalLaunchPlan, LocalSpawnKind, ProfileCommand, ProfileStoreError, load_catalog_from_dir,
    spawn_local_plan, write_profile_file,
};

fn profile_text(extra: &str) -> String {
    format!("{{\"schema_version\": 1, \"name\": \"dev\"{extra}}}")
}

fn parse(extra: &str) -> Result<LaunchProfile, ProfileError> {
    LaunchProfile::parse_json(&profile_text(extra), Some("dev"))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("odytty-profile-field-"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn windows_device_names_are_not_profile_names_on_any_platform() {
    for reserved in [
        "con", "CON", "prn", "Aux", "nul", "com0", "COM1", "com9", "lpt0", "LPT9", "nul.txt",
        "con.dev",
    ] {
        assert!(
            matches!(
                validate_profile_name(reserved),
                Err(ProfileError::InvalidName(_))
            ),
            "{reserved}"
        );
    }
    for fine in [
        "console",
        "com10",
        "com",
        "lpt",
        "auxiliary",
        "nulled",
        "dev.con",
        "com1x",
    ] {
        assert!(validate_profile_name(fine).is_ok(), "{fine}");
    }
}

#[test]
fn launch_text_with_nul_or_an_empty_program_is_refused_at_the_boundary() {
    for extra in [
        ", \"launch\": {\"shell\": \"a\\u0000b\"}",
        ", \"launch\": {\"shell\": \"\"}",
        ", \"launch\": {\"working_directory\": \"/a\\u0000\"}",
        ", \"launch\": {\"command\": {\"program\": \"x\\u0000\"}}",
        ", \"launch\": {\"command\": {\"program\": \"\"}}",
        ", \"launch\": {\"command\": {\"program\": \"x\", \"args\": [\"a\\u0000\"]}}",
    ] {
        assert!(
            matches!(parse(extra), Err(ProfileError::Malformed(_))),
            "{extra}"
        );
    }
    // Other control characters in an argument stay legal.
    assert!(
        parse(", \"launch\": {\"command\": {\"program\": \"x\", \"args\": [\"a\\tb\\n\"]}}")
            .is_ok()
    );
}

#[test]
fn the_local_spawn_repeats_the_launch_text_gate_for_plans_built_without_the_parser() {
    let settings = crate::settings::Settings::default();
    for spawn in [
        LocalSpawnKind::Shell {
            program: "x\0y".to_owned(),
            args: Vec::new(),
        },
        LocalSpawnKind::Shell {
            program: String::new(),
            args: Vec::new(),
        },
        LocalSpawnKind::Exec(ProfileCommand {
            program: "x".to_owned(),
            args: vec!["a\0".to_owned()],
            preserved: Default::default(),
        }),
    ] {
        let plan = LocalLaunchPlan {
            settings: settings.clone(),
            working_directory: None,
            working_directory_source: crate::cwd::DirectorySource::Derived,
            env: Default::default(),
            spawn,
        };
        let Err(error) = spawn_local_plan(crate::core::Dimensions::new(80, 24), &plan) else {
            panic!("an invalid launch must be refused");
        };
        assert!(
            error.to_string().contains("rejected before launch"),
            "{error}"
        );
    }
}

#[test]
fn font_size_must_be_positive_and_fit_f32() {
    for bad in ["0", "-3", "-0.5", "1e300"] {
        assert!(
            matches!(
                parse(&format!(", \"appearance\": {{\"font_size_px\": {bad}}}")),
                Err(ProfileError::LimitExceeded(_))
            ),
            "{bad}"
        );
    }
    let profile = parse(", \"appearance\": {\"font_size_px\": 13.3}").expect("13.3 is valid");
    assert_eq!(profile.appearance.font_size_px, Some(13.3));
}

#[test]
fn a_font_size_is_saved_as_the_decimal_that_was_written() {
    let profile = parse(", \"appearance\": {\"font_size_px\": 13.3}").expect("parse");
    let text = profile.serialize_pretty();
    assert!(text.contains("\"font_size_px\": 13.3\n"), "{text}");
    let reparsed = LaunchProfile::parse_json(&text, Some("dev")).expect("reparse");
    assert_eq!(reparsed, profile);
}

#[test]
fn an_explicit_null_switch_array_is_absent_like_every_other_optional_field() {
    let profile = parse(", \"switch\": {\"match_hosts\": null, \"match_directories\": null}")
        .expect("null arrays load");
    assert!(profile.switch.match_hosts.is_empty());
    assert!(profile.switch.match_directories.is_empty());
    assert!(parse(", \"switch\": {\"match_hosts\": 3}").is_err());
}

#[test]
fn a_secret_shaped_key_error_names_the_matching_rule() {
    let error = parse(", \"launch\": {\"env\": {\"MY_API_TOKEN\": \"x\"}}")
        .expect_err("secret key refused");
    let ProfileError::RejectedSecret(message) = error else {
        panic!("expected a secret rejection, got {error:?}");
    };
    assert!(message.contains("MY_API_TOKEN"), "{message}");
    assert!(message.contains("\"token\""), "{message}");
}

#[test]
fn switch_rules_that_can_never_match_are_reported_when_the_catalog_loads() {
    let _count_guard = crate::test_lock::catalog_count_lock();
    let dir = TempDir::new();
    fs::write(
        dir.path().join("dev.profile.json"),
        profile_text(
            ", \"switch\": {\"match_hosts\": [\"Dev Box\", \"ok-host\"], \
             \"match_directories\": [\"~/work\", \"work/project\", \"/work/../etc\", \"/work/ok\"]}",
        ),
    )
    .expect("write profile");
    let catalog = load_catalog_from_dir(dir.path());
    assert!(
        catalog.profiles.contains_key("dev"),
        "the profile still loads"
    );
    let warnings = catalog.warnings.join("\n");
    for typo in ["Dev Box", "~/work", "work/project", "/work/../etc"] {
        assert!(warnings.contains(typo), "{typo} in {warnings}");
    }
    assert!(!warnings.contains("ok-host"), "{warnings}");
    assert!(!warnings.contains("/work/ok"), "{warnings}");
}

#[test]
fn a_save_that_differs_from_an_existing_profile_only_by_case_is_refused() {
    let dir = TempDir::new();
    let existing = LaunchProfile::new("Dev").expect("profile");
    write_profile_file(&dir.path().join("Dev.profile.json"), &existing).expect("first save");
    let clash = LaunchProfile::new("dev").expect("profile");
    let result = write_profile_file(&dir.path().join("dev.profile.json"), &clash);
    match result {
        Err(ProfileStoreError::Validation(ProfileError::InvalidName(message))) => {
            assert!(message.contains("letter case"), "{message}");
        }
        other => panic!("expected a case collision, got {other:?}"),
    }
    // Saving the same name again stays allowed, and so does a distinct name.
    write_profile_file(&dir.path().join("Dev.profile.json"), &existing).expect("same name");
    let other = LaunchProfile::new("other").expect("profile");
    write_profile_file(&dir.path().join("other.profile.json"), &other).expect("distinct name");
}
