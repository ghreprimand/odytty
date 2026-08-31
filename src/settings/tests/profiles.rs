// SPDX-License-Identifier: GPL-3.0-only
//! Named launch profile precedence and startup-readiness tests.

use std::collections::HashMap;
use std::ffi::OsString;
use std::time::{Duration, Instant};

use crate::profiles::{
    LaunchCliOverrides, LaunchProfile, LiveLaunchOverrides, MAX_PROFILE_ENTRIES,
    MAX_PROFILE_ENV_ENTRIES, MAX_PROFILE_FILE_BYTES, PrecedenceLayer, ProfileCatalog, ProfileError,
    ProfilePlatform, ProfileStoreError, RestoredLaunchOverrides, catalog_load_count_for_test,
    load_catalog_from_dir, precedence::resolve_effective_launch, precedence_chain,
    profile_path_in_dir, quarantine_malformed_file, read_profile_file,
    reset_catalog_load_count_for_test, validate_profile_name, write_profile_file,
};
use crate::settings::{ConfigValues, DEFAULT_THEME, FONT_FAMILY_ENV, Settings, THEME_ENV};
use crate::theme::Theme;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_profiles_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "odytty-profiles-test-{name}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn precedence_chain_matches_v0_14_contract() {
    assert_eq!(
        precedence_chain(),
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
    );
}

#[test]
fn live_ui_settings_override_env_and_profile() {
    let mut profile = LaunchProfile::new("dev").expect("profile");
    profile.appearance.font_family = Some("Victor Mono".to_owned());
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("dev".to_owned(), profile);

    let config = ConfigValues::parse("", |_| {});
    let mut env = HashMap::new();
    env.insert(FONT_FAMILY_ENV, OsString::from("JetBrains Mono"));

    let live = LiveLaunchOverrides {
        settings: [(FONT_FAMILY_ENV, "Fira Code".to_owned())]
            .into_iter()
            .collect(),
        ..LiveLaunchOverrides::default()
    };

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
        &live,
    );
    assert_eq!(effective.settings.font_family.as_deref(), Some("Fira Code"));
}

#[test]
fn local_catalog_load_is_bounded_and_non_blocking() {
    let dir = temp_profiles_dir("catalog");
    std::fs::create_dir_all(&dir).expect("mkdir");
    for index in 0..3 {
        let path = dir.join(format!("p{index}.profile.json"));
        write_profile_file(
            &path,
            &LaunchProfile::new(format!("p{index}")).expect("profile"),
        )
        .expect("write");
    }
    let start = Instant::now();
    let catalog = load_catalog_from_dir(&dir);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(catalog.profiles.len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bare_launch_without_profile_selection_keeps_default_theme() {
    let effective = resolve_effective_launch(
        None,
        &HashMap::new(),
        &ProfileCatalog::default(),
        &LaunchCliOverrides::default(),
        &RestoredLaunchOverrides::default(),
        &LiveLaunchOverrides::default(),
    );
    assert_eq!(effective.settings.theme, DEFAULT_THEME);
    assert_eq!(effective.profile_name, None);
}

#[test]
fn default_settings_load_does_not_enumerate_profiles() {
    // Observable startup seam: ordinary Settings::from_env (default first-
    // terminal path) must not call load_catalog_from_dir. The Profile Manager
    // is the only intentional catalog enumeration entry.
    reset_catalog_load_count_for_test();
    let before = catalog_load_count_for_test();
    let _ = Settings::from_env();
    let _ = resolve_effective_launch(
        None,
        &HashMap::new(),
        &ProfileCatalog::default(),
        &LaunchCliOverrides::default(),
        &RestoredLaunchOverrides::default(),
        &LiveLaunchOverrides::default(),
    );
    assert_eq!(
        catalog_load_count_for_test(),
        before,
        "default launch must not enumerate named profiles"
    );
}

#[test]
fn restored_named_profile_binding_selects_profile() {
    let mut profile = LaunchProfile::new("edge").expect("profile");
    profile.appearance.theme = Some("plain".to_owned());
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("edge".to_owned(), profile);

    let effective = resolve_effective_launch(
        None,
        &HashMap::new(),
        &catalog,
        &LaunchCliOverrides::default(),
        &RestoredLaunchOverrides {
            profile_name: Some("edge".to_owned()),
            ..RestoredLaunchOverrides::default()
        },
        &LiveLaunchOverrides::default(),
    );
    assert_eq!(effective.profile_name.as_deref(), Some("edge"));
    assert_eq!(
        effective.settings.theme,
        Theme::from_name("plain").expect("plain")
    );
}

// --- Adversarial profile-foundation coverage (v0.14 Phase A1) ---

#[test]
fn unknown_keys_survive_the_full_on_disk_write_reload_cycle() {
    // RISK #1 through the ATOMIC-WRITE path (not just an in-memory serialize).
    // A profile parsed with future keys at every owned object must retain them
    // byte-for-byte after a write to disk and a reload.
    let dir = temp_profiles_dir("unknown-keys-disk");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let text = r#"{
  "schema_version": 1,
  "name": "keeper",
  "future_top": {"a": 1},
  "launch": {"future_launch": true, "shell": "/bin/sh"},
  "appearance": {"future_look": "kept"}
}"#;
    let parsed = LaunchProfile::parse_json(text, Some("keeper")).expect("parse");
    let path = dir.join("keeper.profile.json");
    write_profile_file(&path, &parsed).expect("write");

    let raw = std::fs::read_to_string(&path).expect("read raw");
    for key in ["future_top", "future_launch", "future_look"] {
        assert!(raw.contains(key), "on-disk bytes retain {key}");
    }
    let reloaded = read_profile_file(&path, Some("keeper")).expect("reload");
    assert_eq!(reloaded, parsed, "unknown keys survive a disk round-trip");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn secret_env_value_is_rejected_at_the_write_boundary_leaving_no_bytes() {
    // no-secret / on-disk bytes: a private-key marker in an env VALUE (not just
    // a secret-shaped KEY) must be refused before any file or temp sibling is
    // created. The directory stays empty.
    let dir = temp_profiles_dir("secret-value");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut profile = LaunchProfile::new("leaky").expect("profile");
    let marker = ["-----BEGIN OPENSSH PRIVATE", " KEY-----"].concat();
    profile.launch.env.insert("SAFE_LOOKING".to_owned(), marker);
    let path = dir.join("leaky.profile.json");
    assert!(matches!(
        write_profile_file(&path, &profile),
        Err(ProfileStoreError::Validation(ProfileError::RejectedSecret(
            _
        )))
    ));
    assert!(
        !path.exists(),
        "no profile file written for a rejected secret"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp sibling left behind: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn catalog_load_is_capped_at_the_entry_limit_and_warns() {
    // Bounded catalog load: more than MAX_PROFILE_ENTRIES files on disk load at
    // most the cap, and the overflow is reported as a warning, never silently.
    let dir = temp_profiles_dir("entry-cap");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let over = MAX_PROFILE_ENTRIES + 5;
    for index in 0..over {
        let name = format!("p{index:04}");
        let path = dir.join(format!("{name}.profile.json"));
        write_profile_file(&path, &LaunchProfile::new(name).expect("profile")).expect("write");
    }
    let catalog = load_catalog_from_dir(&dir);
    assert!(
        catalog.profiles.len() <= MAX_PROFILE_ENTRIES,
        "catalog is capped at {MAX_PROFILE_ENTRIES}, got {}",
        catalog.profiles.len()
    );
    assert!(
        catalog.warnings.iter().any(|w| w.contains("exceeds")),
        "overflow is reported: {:?}",
        catalog.warnings
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn oversized_profile_file_is_skipped_without_emptying_the_catalog() {
    // Malformed/atomic: a file exceeding MAX_PROFILE_FILE_BYTES is refused by
    // the capped reader and skipped with a warning; sibling good profiles load.
    let dir = temp_profiles_dir("oversized");
    std::fs::create_dir_all(&dir).expect("mkdir");
    write_profile_file(
        &dir.join("good.profile.json"),
        &LaunchProfile::new("good").expect("good"),
    )
    .expect("write good");
    // A syntactically-plausible but over-cap file (> 1 MiB of padding).
    let padding = "x".repeat((MAX_PROFILE_FILE_BYTES as usize) + 16);
    let huge = format!(r#"{{"schema_version":1,"name":"huge","pad":"{padding}"}}"#);
    std::fs::write(dir.join("huge.profile.json"), huge).expect("write huge");
    let catalog = load_catalog_from_dir(&dir);
    assert!(catalog.profiles.contains_key("good"));
    assert!(
        !catalog.profiles.contains_key("huge"),
        "over-cap file is not loaded"
    );
    assert!(!catalog.warnings.is_empty(), "the skip is reported");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn quarantine_renames_a_malformed_file_beside_the_original() {
    // Malformed recovery: an unparseable file can be quarantined to `.bad` so a
    // subsequent load ignores it; the original name no longer exists.
    let dir = temp_profiles_dir("quarantine");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let bad = dir.join("broken.profile.json");
    std::fs::write(&bad, "{ not valid json").expect("write bad");
    assert!(quarantine_malformed_file(&bad).expect("quarantine"));
    assert!(!bad.exists(), "original malformed file was renamed away");
    assert!(
        dir.join("broken.profile.json.bad").exists(),
        "quarantined copy exists"
    );
    // A second quarantine of the now-missing path is a no-op, not an error.
    assert!(!quarantine_malformed_file(&bad).expect("second quarantine"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_profile_scoped_to_a_foreign_platform_falls_back_with_a_warning() {
    // Restore/platform leg: a profile applicable only on a platform other than
    // the current one must NOT apply its settings; the resolver warns and falls
    // back to global defaults. No cross-platform inference.
    let foreign = match ProfilePlatform::current() {
        ProfilePlatform::Linux => ProfilePlatform::Windows,
        _ => ProfilePlatform::Linux,
    };
    let mut profile = LaunchProfile::new("elsewhere").expect("profile");
    profile.platforms = Some([foreign].into_iter().collect());
    profile.appearance.theme = Some("plain".to_owned());
    assert!(
        !profile.applies_on_current_platform(),
        "profile does not apply on the current platform"
    );
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("elsewhere".to_owned(), profile);

    let effective = resolve_effective_launch(
        None,
        &HashMap::new(),
        &catalog,
        &LaunchCliOverrides {
            profile_name: Some("elsewhere".to_owned()),
            ..LaunchCliOverrides::default()
        },
        &RestoredLaunchOverrides::default(),
        &LiveLaunchOverrides::default(),
    );
    assert_eq!(
        effective.settings.theme, DEFAULT_THEME,
        "foreign-platform profile settings do not apply"
    );
    assert!(
        effective
            .warnings
            .iter()
            .any(|w| w.contains("does not apply")),
        "the platform mismatch is reported: {:?}",
        effective.warnings
    );
}

#[test]
fn a_current_platform_scoped_profile_applies() {
    // The companion positive leg: a profile explicitly scoped to the current
    // platform DOES apply, proving the platform gate is not a blanket reject.
    let mut profile = LaunchProfile::new("here").expect("profile");
    profile.platforms = Some([ProfilePlatform::current()].into_iter().collect());
    profile.appearance.theme = Some("plain".to_owned());
    assert!(profile.applies_on_current_platform());
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("here".to_owned(), profile);

    let effective = resolve_effective_launch(
        None,
        &HashMap::new(),
        &catalog,
        &LaunchCliOverrides {
            profile_name: Some("here".to_owned()),
            ..LaunchCliOverrides::default()
        },
        &RestoredLaunchOverrides::default(),
        &LiveLaunchOverrides::default(),
    );
    assert_eq!(
        effective.settings.theme,
        Theme::from_name("plain").expect("plain")
    );
}

#[test]
fn an_over_cap_env_map_is_rejected_not_written() {
    // Bounds: an env override map exceeding MAX_PROFILE_ENV_ENTRIES is refused
    // at the write boundary rather than persisted whole.
    let dir = temp_profiles_dir("env-cap");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut profile = LaunchProfile::new("busy").expect("profile");
    for index in 0..(MAX_PROFILE_ENV_ENTRIES + 1) {
        profile
            .launch
            .env
            .insert(format!("VAR_{index}"), "value".to_owned());
    }
    let path = dir.join("busy.profile.json");
    assert!(matches!(
        write_profile_file(&path, &profile),
        Err(ProfileStoreError::Validation(ProfileError::LimitExceeded(
            _
        )))
    ));
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_file_name_mismatching_its_document_name_is_rejected() {
    // No reinterpretation: a file whose on-disk name does not match the
    // document's `name` field is rejected, so a renamed file cannot silently
    // masquerade as a different profile.
    let dir = temp_profiles_dir("name-mismatch");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = profile_path_in_dir(&dir, "declared").expect("path");
    // Write a document whose internal name is "declared" to a "declared" file,
    // then read it back demanding a DIFFERENT expected name.
    write_profile_file(&path, &LaunchProfile::new("declared").expect("profile")).expect("write");
    assert!(matches!(
        read_profile_file(&path, Some("someone-else")),
        Err(ProfileStoreError::Validation(ProfileError::InvalidName(_)))
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn write_profile_file_replaces_a_same_named_file_without_a_store_level_guard() {
    // Store contract tripwire: collision policy belongs to callers. The
    // manager and import flow reject same-name collisions, while this atomic
    // persistence primitive intentionally replaces its exact destination.
    // Keeping that distinction explicit prevents a future caller from assuming
    // the store supplies user-facing collision confirmation.
    let dir = temp_profiles_dir("overwrite-guard");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = profile_path_in_dir(&dir, "dev").expect("path");

    let mut first = LaunchProfile::new("dev").expect("profile");
    first.launch.shell = Some("/bin/original".to_owned());
    write_profile_file(&path, &first).expect("write original");

    let mut replacement = LaunchProfile::new("dev").expect("profile");
    replacement.launch.shell = Some("/bin/imported".to_owned());
    // No guard: the second write succeeds and replaces the first.
    write_profile_file(&path, &replacement).expect("write replacement");

    let reloaded = read_profile_file(&path, Some("dev")).expect("reload");
    assert_eq!(
        reloaded.launch.shell.as_deref(),
        Some("/bin/imported"),
        "the store write replaces a same-named profile with no collision guard"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn path_separators_and_traversal_in_a_profile_name_are_rejected() {
    // Path-traversal defense: a profile name is restricted to ASCII
    // [A-Za-z0-9._-], so no name can escape the profiles directory or address
    // an arbitrary file, on either Unix or Windows separator conventions. This
    // guards profile_path_in_dir (which joins dir + name + suffix) against a
    // name sourced from an imported file or a future host-directory switch.
    for bad in [
        "../evil",
        "..\\evil",
        "/etc/passwd",
        "C:\\Windows\\system32",
        "a/b",
        "a\\b",
        "with space",
        "tab\tname",
        "null\0name",
    ] {
        assert!(
            validate_profile_name(bad).is_err(),
            "name {bad:?} must be rejected before it can address a path"
        );
        // The path builder rejects the same names rather than composing a path.
        let dir = std::path::Path::new("/tmp/odytty-nonexistent");
        assert!(
            profile_path_in_dir(dir, bad).is_err(),
            "profile_path_in_dir must refuse {bad:?} rather than join it"
        );
    }
    // Sanity: an ordinary name is accepted and stays inside the directory.
    let ok = profile_path_in_dir(std::path::Path::new("/tmp/odytty-x"), "dev").expect("ok name");
    assert!(ok.ends_with("dev.profile.json"));
}

#[test]
fn a_malformed_import_file_is_rejected_and_never_reaches_the_catalog() {
    // Import egress: the App import path is read_profile_file -> (on Ok)
    // save_overlay_profile. A malformed source file must fail at the read, so
    // no write is ever attempted and the catalog is unaffected.
    let dir = temp_profiles_dir("malformed-import");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = dir.join("import-source.json");
    std::fs::write(&src, b"{ this is not valid json ]").expect("write source");

    let result = read_profile_file(&src, None);
    assert!(
        matches!(result, Err(ProfileStoreError::Validation(_))),
        "a malformed import file must be rejected at read, before any write"
    );
    // Nothing was persisted into the profiles directory beyond the source file.
    let catalog = load_catalog_from_dir(&dir);
    assert!(
        catalog.profiles.is_empty(),
        "a rejected import must not create any profile entry"
    );
    let _ = std::fs::remove_dir_all(dir);
}
