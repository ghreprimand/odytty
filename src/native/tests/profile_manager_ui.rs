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
        .visible_lines(80, 400)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

/// Return the body row on which `needle` is actually rendered.  Pointer
/// regressions use this rather than duplicating layout arithmetic: the press
/// must operate on the row the operator can see, including dynamically
/// inserted form help.
fn rendered_row(manager: &ProfileManager, needle: &str) -> usize {
    manager
        .visible_lines(80, 400)
        .iter()
        .position(|line| line.text.contains(needle))
        .unwrap_or_else(|| panic!("missing rendered row containing {needle:?}"))
}

fn feed_chars(manager: &mut ProfileManager, text: &str) {
    for ch in text.chars() {
        let _ = manager.handle_input(OverlayInput::Char(ch));
    }
}

/// Move form focus to the field whose rendered label starts with `prefix` by
/// pressing the row the operator sees, then return that row index. This mirrors
/// the pointer path and stays robust to section reordering, unlike a hardcoded
/// Down count.
fn focus_form_row(manager: &mut ProfileManager, prefix: &str) -> usize {
    let row = rendered_row(manager, prefix);
    let _ = manager.handle_pointer_press(80, 400, row, 0);
    row
}

#[test]
fn keyboard_text_entry_appends_each_character_exactly_once() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);

    // Name is initially focused. The text buffer must receive each physical
    // keypress once; this is also the basis of keyboard-only editing for every
    // other text field in the form.
    feed_chars(&mut manager, "dev");

    assert!(
        line_texts(&manager).iter().any(|line| line == "Name: dev"),
        "three keypresses must render exactly `dev`, not duplicated input"
    );
}

#[test]
fn form_renders_one_save_and_one_cancel_action() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);

    let lines = line_texts(&manager);
    let save_count = lines
        .iter()
        .filter(|line| line.as_str() == "[Save]")
        .count();
    let cancel_count = lines
        .iter()
        .filter(|line| line.as_str() == "[Cancel]")
        .count();

    assert_eq!(save_count, 1, "the form has exactly one Save action");
    assert_eq!(
        cancel_count, 1,
        "the form has exactly one Cancel action; duplicated rows make the rendered action map ambiguous"
    );
}

#[test]
fn empty_environment_key_is_rejected_inline_instead_of_being_dropped_on_save() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);
    feed_chars(&mut manager, "env-empty");

    let add_row = rendered_row(&manager, "[Add environment override]");
    let _ = manager.handle_pointer_press(80, 400, add_row, 0);
    let value_row = focus_form_row(&mut manager, "Environment value 1:");
    assert!(value_row > add_row, "the new environment row is rendered");
    feed_chars(&mut manager, "present-value");

    let save_row = rendered_row(&manager, "[Save]");
    let outcome = manager.handle_pointer_press(80, 400, save_row, 0);

    assert!(
        matches!(outcome, ProfileManagerOutcome::Consumed),
        "an incomplete environment override must block persistence"
    );
    assert!(
        line_texts(&manager)
            .iter()
            .any(|line| line.contains("environment key")),
        "the empty key must remain visible with an inline validation error"
    );
}

#[test]
fn free_text_and_list_fields_round_trip_a_literal_inherit_word() {
    // The `inherit` word is a sentinel only for closed-vocabulary fields; a
    // free-text scalar or list entry must persist it verbatim rather than
    // collapsing it to "unset".
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);
    feed_chars(&mut manager, "literal");

    let title_row = focus_form_row(&mut manager, "Title:");
    assert!(title_row > 0, "title field is rendered");
    feed_chars(&mut manager, "Inherit");

    let host_add = rendered_row(&manager, "[Add host match]");
    let _ = manager.handle_pointer_press(80, 400, host_add, 0);
    let _ = focus_form_row(&mut manager, "Host match 1:");
    feed_chars(&mut manager, "inherit");

    let save_row = rendered_row(&manager, "[Save]");
    let outcome = manager.handle_pointer_press(80, 400, save_row, 0);
    let ProfileManagerOutcome::Persist { profile, .. } = outcome else {
        panic!("literal inherit strings must save, not error");
    };
    assert_eq!(profile.appearance.title.as_deref(), Some("Inherit"));
    assert_eq!(profile.switch.match_hosts, vec!["inherit".to_owned()]);
}

#[test]
fn space_types_a_literal_space_into_free_text_fields() {
    // Regression: space used to be routed to field activation for every
    // field, so free-text fields silently swallowed the space bar. It must
    // now type into free-text buffers while still cycling tri-states.
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);
    feed_chars(&mut manager, "spaced");

    let _ = focus_form_row(&mut manager, "Font family:");
    feed_chars(&mut manager, "Fira Code");
    assert!(
        line_texts(&manager)
            .iter()
            .any(|line| line == "Font family: Fira Code"),
        "a space must type into the font family free-text field"
    );

    // Space on a tri-state still cycles it rather than typing. Focusing Bloom
    // by pointer already activates it once (inherit -> on), so a following
    // space must advance the cycle again (on -> off), never append a literal
    // space to the value.
    let bloom_row = focus_form_row(&mut manager, "Bloom:");
    assert!(bloom_row > 0, "bloom tri-state is rendered");
    assert!(
        line_texts(&manager).iter().any(|line| line == "Bloom: on"),
        "clicking the tri-state cycles inherit -> on"
    );
    let _ = manager.handle_input(OverlayInput::Char(' '));
    assert!(
        line_texts(&manager).iter().any(|line| line == "Bloom: off"),
        "space on a tri-state advances the cycle (on -> off) rather than typing a space"
    );
}

#[test]
fn pointer_uses_the_rendered_add_row_in_an_empty_catalog() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);

    let add_row = rendered_row(&manager, "+ Add profile");
    assert!(matches!(
        manager.handle_pointer_press(80, 400, add_row, 0),
        ProfileManagerOutcome::Consumed
    ));
    assert_eq!(manager.title(), "Add profile");

    // The empty-state line is visible but has no action.
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let empty_row = rendered_row(&manager, "No profiles yet.");
    assert!(matches!(
        manager.handle_pointer_press(80, 400, empty_row, 0),
        ProfileManagerOutcome::Consumed
    ));
    assert!(manager.title().contains("Named Profiles"));
}

#[test]
fn pointer_cancel_uses_the_rendered_form_row_after_shell_suggestion() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let add_row = rendered_row(&manager, "+ Add profile");
    let _ = manager.handle_pointer_press(80, 400, add_row, 0);

    // Focus Shell and choose a discovered suggestion; this inserts the
    // suggestion line immediately after Shell and used to shift Cancel's
    // arithmetic hit-test target.
    let _ = focus_form_row(&mut manager, "Shell:");
    let _ = manager.handle_input(OverlayInput::Right);
    assert!(
        line_texts(&manager)
            .iter()
            .any(|line| line.contains("Shell suggestions"))
    );
    let cancel_row = rendered_row(&manager, "[Cancel]");
    assert!(matches!(
        manager.handle_pointer_press(80, 400, cancel_row, 0),
        ProfileManagerOutcome::Consumed
    ));
    assert!(manager.title().contains("Named Profiles"));
}

#[test]
fn pointer_delete_confirmation_uses_the_rendered_button_spans() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev"]), None);
    let _ = manager.handle_input(OverlayInput::Char('x'));
    let action_row = rendered_row(&manager, "[Enter] Delete");
    assert!(matches!(
        manager.handle_pointer_press(80, 400, action_row, 0),
        ProfileManagerOutcome::Delete(name) if name == "dev"
    ));

    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev"]), None);
    let _ = manager.handle_input(OverlayInput::Char('x'));
    let action_row = rendered_row(&manager, "[Enter] Delete");
    let cancel_col = "[Enter] Delete    ".chars().count();
    assert!(matches!(
        manager.handle_pointer_press(80, 400, action_row, cancel_col),
        ProfileManagerOutcome::Consumed
    ));
    assert!(manager.title().contains("Named Profiles"));
}

// --- Section 4: create / rename collision guards (fail-closed, no clobber) ---

#[test]
fn create_onto_an_existing_name_is_rejected_not_persisted() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev"]), None);

    // Tab opens the Add form; type a name that already exists.
    let _ = manager.handle_input(OverlayInput::Tab);
    feed_chars(&mut manager, "dev");

    // Click the rendered [Save] row via the pointer path.
    let save_row = rendered_row(&manager, "[Save]");
    let outcome = manager.handle_pointer_press(80, 400, save_row, 0);

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

    // Rename form shows Identity / Name / Actions / [Save] / [Cancel]; click
    // the rendered [Save] row rather than a fixed index.
    let save_row = rendered_row(&manager, "[Save]");
    let outcome = manager.handle_pointer_press(80, 400, save_row, 0);

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

    // Focus the Title row via the pointer path and type through the edit
    // buffer, then click the rendered [Save] row.
    let _ = focus_form_row(&mut manager, "Title:");
    feed_chars(&mut manager, "Dev");
    let save_row = rendered_row(&manager, "[Save]");
    let outcome = manager.handle_pointer_press(80, 400, save_row, 0);

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

// --- Section 2 / 7: the env editor exists and rejects secrets inline ---

#[test]
fn the_add_form_exposes_an_env_editor_that_rejects_secrets_at_the_write_boundary() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);
    assert_eq!(manager.title(), "Add profile");
    feed_chars(&mut manager, "envdev");

    // The Launch section offers an explicit add-environment row.
    let add_env_row = rendered_row(&manager, "[Add environment override]");
    let _ = manager.handle_pointer_press(80, 400, add_env_row, 0);

    // One rendered env key/value pair now exists; type a secret-shaped key.
    let key_row = focus_form_row(&mut manager, "Environment key 1:");
    feed_chars(&mut manager, "API_TOKEN");
    let value_row = rendered_row(&manager, "Environment value 1:");
    assert!(value_row > key_row, "value row follows its key row");
    let _ = manager.handle_pointer_press(80, 400, value_row, 0);
    feed_chars(&mut manager, "nope");

    // Saving is rejected inline; the entry is never silently dropped.
    let save_row = rendered_row(&manager, "[Save]");
    let outcome = manager.handle_pointer_press(80, 400, save_row, 0);
    assert!(
        matches!(outcome, ProfileManagerOutcome::Consumed),
        "a secret env entry must not persist"
    );
    assert!(
        line_texts(&manager).iter().any(|t| t.contains("secret")),
        "an explicit secret-rejection error must be shown inline"
    );
    assert!(
        line_texts(&manager)
            .iter()
            .any(|t| t.contains("Environment key 1: API_TOKEN")),
        "the rejected env entry stays visible in the draft"
    );
}

// --- Pointer row/render lockstep ------------------------------------------

#[test]
fn pointer_press_on_the_rendered_add_row_opens_the_empty_catalog_form() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);

    let add_row = rendered_row(&manager, "+ Add profile");
    let outcome = manager.handle_pointer_press(80, 400, add_row, 0);

    assert!(matches!(outcome, ProfileManagerOutcome::Consumed));
    assert_eq!(
        manager.title(),
        "Add profile",
        "a press where the empty catalog draws its add row must open the form"
    );
}

#[test]
fn pointer_press_on_rendered_cancel_after_shell_suggestion_returns_to_catalog() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);

    // Select a discovered shell. The focused Shell row inserts a visible
    // suggestion line, which must not displace the rows the pointer handler
    // accepts below it.
    let _ = focus_form_row(&mut manager, "Shell:");
    let _ = manager.handle_input(OverlayInput::Right);
    assert!(
        line_texts(&manager)
            .iter()
            .any(|line| line.contains("Shell suggestions")),
        "precondition: selecting a shell renders its suggestion row"
    );

    let cancel_row = rendered_row(&manager, "[Cancel]");
    let outcome = manager.handle_pointer_press(80, 400, cancel_row, 0);

    assert!(matches!(outcome, ProfileManagerOutcome::Consumed));
    assert!(
        manager.title().contains("Named Profiles"),
        "a press where Cancel is drawn must return to the catalog even with a shell hint"
    );
}

#[test]
fn pointer_press_on_rendered_cancel_without_shell_suggestion_returns_to_catalog() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);
    assert!(
        !line_texts(&manager)
            .iter()
            .any(|line| line.contains("Shell suggestions")),
        "control form has no injected shell-suggestion row"
    );

    let cancel_row = rendered_row(&manager, "[Cancel]");
    let outcome = manager.handle_pointer_press(80, 400, cancel_row, 0);

    assert!(matches!(outcome, ProfileManagerOutcome::Consumed));
    assert!(manager.title().contains("Named Profiles"));
}

// --- Section 4: external palette appearance fields ---

#[test]
fn external_palette_fields_are_editable_through_the_form() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.handle_input(OverlayInput::Tab);

    feed_chars(&mut manager, "palette-dev");

    // Follow external palette is a tri-state toggle: clicking its rendered row
    // both focuses and cycles it inherit -> on.
    let _ = focus_form_row(&mut manager, "Follow external palette:");
    assert!(
        line_texts(&manager)
            .iter()
            .any(|line| line.contains("Follow external palette: on")),
        "the toggle row shows on after one activation"
    );

    // Provider and path are text fields; focus each rendered row and type.
    let _ = focus_form_row(&mut manager, "External palette provider:");
    feed_chars(&mut manager, "colors_toml");
    let _ = focus_form_row(&mut manager, "External palette path:");
    feed_chars(&mut manager, "/tmp/synthetic-palette.toml");

    let save_row = rendered_row(&manager, "[Save]");
    let outcome = manager.handle_pointer_press(80, 400, save_row, 0);
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
