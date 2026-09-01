// SPDX-License-Identifier: GPL-3.0-only
//! Adversarial coverage for the named-profile manager overlay (v0.14 A2).
//!
//! Test-only. These drive `ProfileManager` through its real public overlay
//! surface (`open` / `handle_input` / `handle_pointer_press` / `visible_lines`
//! / `title`) rather than poking private draft fields, so they exercise the
//! same focus/navigation/edit-buffer path a keyboard or pointer user hits.
//! They complement the in-module unit tests, which reach private fields the
//! external overlay contract does not expose.

use crate::native::overlay::OverlayInput;
use crate::native::profile_manager::{ProfileManager, ProfileManagerOutcome};
use crate::profiles::{LaunchProfile, ProfileCatalog};

fn catalog_with(names: &[&str]) -> ProfileCatalog {
    let mut catalog = ProfileCatalog::default();
    for name in names {
        catalog.profiles.insert(
            (*name).to_owned(),
            LaunchProfile::new(*name).expect("valid profile name"),
        );
    }
    catalog
}

/// Every rendered line's text, for asserting on visible state without reaching
/// into the manager's private fields.
fn line_texts(manager: &ProfileManager) -> Vec<String> {
    manager
        .visible_lines(80, 24)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

fn feed_chars(manager: &mut ProfileManager, text: &str) {
    for ch in text.chars() {
        let _ = manager.handle_input(OverlayInput::Char(ch));
    }
}

// --- Section 4: create / rename collision guards (fail-closed, no clobber) ---

#[test]
fn create_onto_an_existing_name_is_rejected_not_persisted() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev"]), None);

    // Tab opens the Add form; type a name that already exists.
    let _ = manager.handle_input(OverlayInput::Tab);
    feed_chars(&mut manager, "dev");

    // Click [Save] (Add form field index 11) via the pointer path.
    let outcome = manager.handle_pointer_press(80, 24, 11, 0);

    assert!(
        matches!(outcome, ProfileManagerOutcome::Consumed),
        "collision save must be consumed, not persisted"
    );
    assert_eq!(manager.title(), "Add profile", "must stay on the Add form");
    assert!(
        line_texts(&manager).iter().any(|t| t.contains("exists")),
        "an explicit collision error must be shown"
    );
}

#[test]
fn rename_onto_an_existing_name_does_not_clobber_the_other_profile() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev", "work"]), None);
    // Filtered order is BTreeMap-sorted: index 0 == "dev".

    // 'r' opens the rename form preloaded with "dev".
    let _ = manager.handle_input(OverlayInput::Char('r'));
    assert_eq!(manager.title(), "Rename profile");

    // Erase "dev" and retype the colliding name "work".
    for _ in 0..3 {
        let _ = manager.handle_input(OverlayInput::Backspace);
    }
    feed_chars(&mut manager, "work");

    // Rename form shows Name / [Save] / [Cancel]; Save is field index 1.
    let outcome = manager.handle_pointer_press(80, 24, 1, 0);

    assert!(
        matches!(outcome, ProfileManagerOutcome::Consumed),
        "rename onto an existing name must not emit a Persist that would clobber"
    );
    assert_eq!(manager.title(), "Rename profile", "must stay on the form");
    assert!(
        line_texts(&manager).iter().any(|t| t.contains("exists")),
        "an explicit collision error must be shown"
    );
}

// --- Section 3: destructive delete confirmation / cancel ---

#[test]
fn delete_cancel_is_a_true_no_op_and_returns_to_the_catalog() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev"]), None);

    // 'x' opens the confirm-delete prompt for the selected profile.
    let _ = manager.handle_input(OverlayInput::Char('x'));
    assert_eq!(manager.title(), "Delete profile dev?");

    // 'n' cancels: no Delete outcome, back to the catalog list.
    let outcome = manager.handle_input(OverlayInput::Char('n'));
    assert!(
        matches!(outcome, ProfileManagerOutcome::Consumed),
        "cancel must be a no-op, never a Delete"
    );
    assert!(
        manager.title().contains("Named Profiles"),
        "cancel must return to the catalog view"
    );
}

#[test]
fn delete_confirm_emits_delete_for_exactly_the_selected_profile() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev", "keep"]), None);
    // Sorted order: index 0 == "dev". Select it and confirm delete.

    let _ = manager.handle_input(OverlayInput::Char('x'));
    let outcome = manager.handle_input(OverlayInput::Activate);

    match outcome {
        ProfileManagerOutcome::Delete(name) => {
            assert_eq!(name, "dev", "delete must target only the selected profile");
        }
        other => panic!("expected Delete(\"dev\"), got {other:?}"),
    }
}

#[test]
fn delete_key_on_an_empty_catalog_is_a_safe_no_op() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    // Empty catalog focuses the add-row; there is no selected profile.

    let outcome = manager.handle_input(OverlayInput::Char('x'));

    assert!(
        matches!(outcome, ProfileManagerOutcome::Consumed),
        "delete key with no selection must never emit a Delete"
    );
    assert!(
        manager.title().contains("Named Profiles"),
        "must remain on the catalog view, never enter a confirm prompt"
    );
}

// --- Section 1: unknown-key retention through a fully UI-driven edit ---

#[test]
fn unknown_future_keys_survive_a_ui_driven_edit_and_save() {
    let text = r#"{
  "schema_version": 1,
  "name": "dev",
  "future_flag": true,
  "launch": { "future_launch": 1 },
  "appearance": { "future_appearance": "kept" }
}"#;
    let profile = LaunchProfile::parse_json(text, Some("dev")).expect("parse");
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("dev".to_owned(), profile);

    let mut manager = ProfileManager::new();
    manager.open(catalog, None);

    // Enter edits the selected profile.
    let _ = manager.handle_input(OverlayInput::Activate);
    assert_eq!(manager.title(), "Edit profile");

    // Navigate to the Title field (index 9) and type through the edit buffer.
    for _ in 0..9 {
        let _ = manager.handle_input(OverlayInput::Down);
    }
    feed_chars(&mut manager, "Dev");

    // Down twice more to [Save] (index 11), then activate.
    let _ = manager.handle_input(OverlayInput::Down);
    let _ = manager.handle_input(OverlayInput::Down);
    let outcome = manager.handle_input(OverlayInput::Activate);

    let ProfileManagerOutcome::Persist { profile, .. } = outcome else {
        panic!("expected a Persist outcome from the edit");
    };
    let serialized = profile.serialize_pretty();
    assert!(
        serialized.contains("future_flag"),
        "top-level unknown key must survive a UI edit round-trip"
    );
    assert!(
        serialized.contains("future_launch"),
        "nested launch unknown key must survive"
    );
    assert!(
        serialized.contains("future_appearance"),
        "nested appearance unknown key must survive"
    );
    assert!(
        serialized.contains("Dev"),
        "the edited title must be applied alongside preserved keys"
    );
}

// --- Section 2 / 7: the form exposes no environment field ---

#[test]
fn the_add_form_exposes_no_environment_field_so_ui_cannot_introduce_secrets() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);
    assert_eq!(manager.title(), "Add profile");

    let has_env_field = line_texts(&manager).iter().any(|t| {
        let lower = t.to_ascii_lowercase();
        lower.starts_with("env") || lower.contains("environment")
    });
    assert!(
        !has_env_field,
        "the manager form must not offer an env editor; env only rides opaquely \
         on an imported/edited base and is rejected at the write boundary"
    );
}

// --- Section 4: external palette appearance fields ---

#[test]
fn external_palette_fields_are_editable_through_the_form() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);

    feed_chars(&mut manager, "palette-dev");

    // Follow external palette (index 5).
    for _ in 0..5 {
        let _ = manager.handle_input(OverlayInput::Down);
    }
    feed_chars(&mut manager, "on");

    // Provider (index 6).
    let _ = manager.handle_input(OverlayInput::Down);
    feed_chars(&mut manager, "colors_toml");

    // Path (index 7).
    let _ = manager.handle_input(OverlayInput::Down);
    feed_chars(&mut manager, "/tmp/synthetic-palette.toml");

    for _ in 0..4 {
        let _ = manager.handle_input(OverlayInput::Down);
    }
    let outcome = manager.handle_input(OverlayInput::Activate);
    let ProfileManagerOutcome::Persist { profile, .. } = outcome else {
        panic!("expected persist");
    };
    assert_eq!(profile.appearance.follow_external_palette, Some(true));
    assert_eq!(
        profile.appearance.external_palette_provider.as_deref(),
        Some("colors_toml")
    );
    assert_eq!(
        profile.appearance.external_palette_path.as_deref(),
        Some("/tmp/synthetic-palette.toml")
    );
}

// --- Section 4: duplicate never targets an existing name ---

#[test]
fn duplicate_produces_a_non_colliding_suggested_name() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev", "dev-copy"]), None);
    // Select "dev" (index 0) and duplicate it.

    let _ = manager.handle_input(OverlayInput::Char('d'));
    assert_eq!(manager.title(), "Duplicate profile");

    // The suggested name must dodge the already-present "dev-copy".
    let name_line = line_texts(&manager)
        .into_iter()
        .find(|t| t.starts_with("Name:"))
        .expect("the duplicate form shows a Name field");
    assert_ne!(
        name_line.trim(),
        "Name: dev-copy",
        "duplicate must not reuse the existing dev-copy name verbatim"
    );
    assert!(
        name_line.contains("dev-copy"),
        "duplicate suggestion is derived from the source name"
    );
}
