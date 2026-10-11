// SPDX-License-Identifier: GPL-3.0-only
//! Profile form tests, kept in the form module to access its private buffers.
use super::*;

#[test]
fn form_values_backspace_removes_whole_owners() {
    for field in [
        FormField::Name,
        FormField::DisplayName,
        FormField::Shell,
        FormField::WorkingDirectory,
        FormField::CommandProgram,
        FormField::CommandArg(0),
        FormField::EnvKey(0),
        FormField::EnvValue(0),
        FormField::Theme,
        FormField::ExternalPaletteProvider,
        FormField::ExternalPalettePath,
        FormField::FontFamily,
        FormField::Title,
        FormField::Connection,
        FormField::Font,
        FormField::FontSizePx,
        FormField::SavedLayout,
        FormField::MatchHost(0),
        FormField::MatchDirectory(0),
    ] {
        for owner in [
            "e\u{301}",
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{1f1fa}\u{1f1f8}",
            "\u{915}\u{94d}\u{937}",
            "\u{e01}\u{e33}",
        ] {
            let mut manager = ProfileManager::new();
            manager.view = ManagerView::Form(FormMode::Add);
            manager.draft_command_args.push(String::new());
            manager.draft_env.push((String::new(), String::new()));
            manager.draft_match_hosts.push(String::new());
            manager.draft_match_directories.push(String::new());
            manager.form_focus = manager
                .visible_form_fields()
                .iter()
                .position(|f| *f == field)
                .unwrap();
            manager.edit_active_buffer(String::clear);
            for ch in format!("x{owner}").chars() {
                manager.handle_input(OverlayInput::Char(ch));
            }
            let before = manager.render_signature();
            manager.handle_input(OverlayInput::Backspace);
            manager.edit_active_buffer(|buf| assert_eq!(buf, "x", "{field:?}: {owner:?}"));
            assert_ne!(manager.render_signature(), before);
            for _ in 0..2 {
                manager.handle_input(OverlayInput::Backspace);
            }
            manager.edit_active_buffer(|buf| assert!(buf.is_empty()));
        }
    }
}

use crate::profiles::{ProfileCatalog, ShellKind};

fn catalog_with(names: &[&str]) -> ProfileCatalog {
    let mut catalog = ProfileCatalog::default();
    for name in names {
        catalog.profiles.insert(
            (*name).to_owned(),
            LaunchProfile::new(*name).expect("profile"),
        );
    }
    catalog
}

/// Focus a form field by locating it in the ordered field list, matching
/// the pointer path rather than a hardcoded Down count that field
/// reordering would break.
fn focus_field(manager: &mut ProfileManager, field: FormField) {
    let index = manager
        .visible_form_fields()
        .iter()
        .position(|candidate| *candidate == field)
        .expect("field present");
    manager.form_focus = index;
}

#[test]
fn theme_row_cycles_the_builtin_roster_and_save_rejects_unknown() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    manager.draft_name = "themed".to_owned();
    // The roster is loaded on open and holds the built-ins.
    assert!(!manager.theme_suggestions.is_empty());
    focus_field(&mut manager, FormField::Theme);
    let _ = manager.handle_input(OverlayInput::Right);
    let cycled = manager.draft_theme.clone();
    assert!(
        manager.theme_suggestions.contains(&cycled),
        "Right must land on a roster theme"
    );
    assert!(manager.theme_suggestion_hint().is_some());
    // An unknown theme name is an inline save error, not a silent fallback.
    manager.draft_theme = "no-such-theme-xyz".to_owned();
    assert!(matches!(
        manager.try_save(),
        ProfileManagerOutcome::Consumed
    ));
    assert!(
        manager
            .error
            .as_deref()
            .is_some_and(|e| e.contains("unknown theme")),
        "unknown theme must block save with an inline error"
    );
}

#[test]
fn form_loads_discovered_shell_suggestions_on_add() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&[]), None);
    assert!(matches!(
        manager.open_add(),
        ProfileManagerOutcome::Consumed
    ));
    assert!(
        !manager.discovered_shells.is_empty(),
        "profile form must load cached shell discovery on demand"
    );
}

#[test]
fn form_fields_are_grouped_into_contiguous_sections() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&[]), None);
    manager.open_add();
    let headers: Vec<String> = manager
        .form_all_lines(80)
        .into_iter()
        .filter(|line| line.text.starts_with("-- "))
        .map(|line| line.text)
        .collect();
    assert_eq!(
        headers,
        vec![
            "-- Identity --",
            "-- Launch --",
            "-- Appearance --",
            "-- Cursor --",
            "-- Effects --",
            "-- Layout --",
            "-- Switching --",
            "-- Actions --",
        ],
        "each section header appears exactly once, in order"
    );
}

#[test]
fn command_program_row_is_not_duplicated() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&[]), None);
    manager.open_add();
    let count = manager
        .visible_form_fields()
        .iter()
        .filter(|field| matches!(field, FormField::CommandProgram))
        .count();
    assert_eq!(count, 1, "command program must appear exactly once");
}

#[test]
fn shell_field_right_cycles_discovered_suggestions() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&[]), None);
    manager.open_add();
    focus_field(&mut manager, FormField::Shell);
    let before = manager.draft_shell.clone();
    manager.cycle_shell_suggestion(1);
    let after = manager.draft_shell.clone();
    assert_ne!(
        before, after,
        "cycling must apply the next discovered shell"
    );
    assert!(
        manager.shell_suggestion_hint().is_some(),
        "focused shell field shows a suggestion hint"
    );
}

#[test]
fn structured_shell_suggestion_persists_as_profile_command() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&[]), None);
    manager.open_add();
    manager.draft_name = "wsl".to_owned();
    manager.apply_shell_suggestion(&DiscoveredShell {
        label: "WSL: Ubuntu".to_owned(),
        program: "wsl.exe".to_owned(),
        args: vec!["-d".to_owned(), "Ubuntu".to_owned()],
        kind: ShellKind::Wsl,
    });
    let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
        panic!("expected persist");
    };
    assert_eq!(
        profile
            .launch
            .command
            .as_ref()
            .map(|command| command.program.as_str()),
        Some("wsl.exe")
    );
    let command = profile.launch.command.expect("command");
    assert_eq!(command.args, vec!["-d".to_owned(), "Ubuntu".to_owned()]);
    assert!(profile.launch.shell.is_none());
}

#[test]
fn create_edit_duplicate_rename_and_delete_flows() {
    let mut manager = ProfileManager::new();
    manager.open(catalog_with(&["dev"]), None);

    assert!(matches!(
        manager.open_add(),
        ProfileManagerOutcome::Consumed
    ));
    manager.draft_name = "work".to_owned();
    manager.draft_shell = "/bin/zsh".to_owned();
    let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
        panic!("expected persist");
    };
    assert_eq!(profile.name, "work");
    assert_eq!(profile.launch.shell.as_deref(), Some("/bin/zsh"));
    assert_eq!(replace, None);

    manager.open(catalog_with(&["dev", "work"]), None);
    manager.selected = 0;
    assert!(matches!(
        manager.open_duplicate_selected(),
        ProfileManagerOutcome::Consumed
    ));
    assert_eq!(manager.draft_name, "dev-copy");

    manager.open(catalog_with(&["dev"]), None);
    assert!(matches!(
        manager.open_rename_selected(),
        ProfileManagerOutcome::Consumed
    ));
    manager.draft_name = "edge".to_owned();
    let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
        panic!("expected rename persist");
    };
    assert_eq!(profile.name, "edge");
    assert_eq!(replace.as_deref(), Some("dev"));
}

/// Every form field must change the repaint signature when edited, so
/// typing (or toggling) is visible on the next frame rather than only when
/// the focus moves. Iterates the live field list so a field added later
/// cannot be missed.
#[test]
fn every_form_field_edit_changes_the_render_signature() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    // Materialize one row of every dynamic list so their text fields exist.
    for add in [
        FormField::AddCommandArg,
        FormField::AddEnv,
        FormField::AddMatchHost,
        FormField::AddMatchDirectory,
    ] {
        focus_field(&mut manager, add);
        let _ = manager.activate_form_field(add);
    }
    let fields = manager.visible_form_fields();
    for field in fields {
        if matches!(field, FormField::Save | FormField::Cancel) {
            continue;
        }
        focus_field(&mut manager, field);
        let before = manager.render_signature();
        if field.is_toggle_or_cycle() {
            let _ = manager.handle_form_input(OverlayInput::Char(' '));
        } else if matches!(
            field,
            FormField::AddCommandArg
                | FormField::AddEnv
                | FormField::AddMatchHost
                | FormField::AddMatchDirectory
                | FormField::RemoveCommandArg(_)
                | FormField::RemoveEnv(_)
                | FormField::RemoveMatchHost(_)
                | FormField::RemoveMatchDirectory(_)
        ) {
            // Row add/remove actions change the field list itself; the
            // list length and focus are visible in the rendered lines.
            let _ = manager.handle_form_input(OverlayInput::Activate);
        } else {
            let _ = manager.handle_form_input(OverlayInput::Char('x'));
        }
        assert_ne!(
            before,
            manager.render_signature(),
            "editing {field:?} must invalidate the repaint signature"
        );
    }
}

/// The reported case: Enter on the Add row, then typing into the new key
/// row, must show each character as it is typed.
#[test]
fn typing_into_a_new_env_key_row_repaints_per_character() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    focus_field(&mut manager, FormField::AddEnv);
    let _ = manager.handle_form_input(OverlayInput::Activate);
    assert_eq!(
        manager.visible_form_fields()[manager.form_focus],
        FormField::EnvKey(0),
        "Enter on the Add row lands on the new key row"
    );
    let mut previous = manager.render_signature();
    for ch in "ODY".chars() {
        let _ = manager.handle_form_input(OverlayInput::Char(ch));
        let current = manager.render_signature();
        assert_ne!(previous, current, "typing {ch:?} must repaint");
        previous = current;
    }
    let lines = manager.form_all_lines(80);
    assert!(
        lines
            .iter()
            .any(|line| line.text == "Environment key 1: ODY" && line.focused),
        "typed key text is drawn on the focused row"
    );
}

#[test]
fn env_editor_adds_rows_and_secret_entries_are_rejected_inline_not_dropped() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    manager.draft_name = "dev".to_owned();
    // Add one env row through the same activation the UI drives.
    focus_field(&mut manager, FormField::AddEnv);
    let _ = manager.activate_form_field(FormField::AddEnv);
    assert_eq!(manager.draft_env.len(), 1, "add row appends one env slot");
    manager.draft_env[0] = ("API_TOKEN".to_owned(), "nope".to_owned());

    assert!(matches!(
        manager.try_save(),
        ProfileManagerOutcome::Consumed
    ));
    assert!(
        manager
            .error
            .as_deref()
            .is_some_and(|error| error.contains("secret")),
        "secret env key must surface an inline error"
    );
    assert_eq!(
        manager.draft_env.len(),
        1,
        "the rejected entry stays in the draft; it is never silently dropped"
    );
    assert_eq!(manager.draft_env[0].0, "API_TOKEN");
}

/// OS-invalid env overrides (empty / `=` / NUL) must block save with an
/// inline error and leave the draft row intact.
#[test]
fn env_editor_rejects_os_invalid_keys_and_nul_values_inline() {
    let cases = [
        ("", "value", "empty key"),
        ("FOO=BAR", "1", "equals in key"),
        ("FOO\0BAR", "1", "NUL in key"),
        ("FOO", "a\0b", "NUL in value"),
    ];
    for (key, value, label) in cases {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default(), None);
        manager.open_add();
        manager.draft_name = "dev".to_owned();
        focus_field(&mut manager, FormField::AddEnv);
        let _ = manager.activate_form_field(FormField::AddEnv);
        manager.draft_env[0] = (key.to_owned(), value.to_owned());

        assert!(
            matches!(manager.try_save(), ProfileManagerOutcome::Consumed),
            "{label}: save must not Persist"
        );
        assert!(
            manager.error.as_ref().is_some_and(|error| {
                let lower = error.to_ascii_lowercase();
                lower.contains("environment")
                    || lower.contains("env")
                    || lower.contains("invalid")
                    || lower.contains("malformed")
                    || lower.contains("nul")
                    || lower.contains('=')
                    || lower.contains("required")
                    || lower.contains("empty")
            }),
            "{label}: must surface an inline validation error; got {:?}",
            manager.error
        );
        assert_eq!(
            manager.draft_env.len(),
            1,
            "{label}: rejected row stays in the draft"
        );
        assert_eq!(manager.draft_env[0].0, key);
        assert_eq!(manager.draft_env[0].1, value);
    }
}

#[test]
fn env_editor_preserves_valid_os_env_names_on_save() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    manager.draft_name = "dev".to_owned();
    focus_field(&mut manager, FormField::AddEnv);
    let _ = manager.activate_form_field(FormField::AddEnv);
    manager.draft_env[0] = ("ODY_TEST".to_owned(), "alpha".to_owned());
    let _ = manager.activate_form_field(FormField::AddEnv);
    manager.draft_env[1] = ("_Private".to_owned(), "ok".to_owned());
    let _ = manager.activate_form_field(FormField::AddEnv);
    // Space in the name is valid and must not be over-restricted.
    manager.draft_env[2] = ("A B".to_owned(), "spaced".to_owned());

    let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
        panic!("valid OS env names must save; error={:?}", manager.error);
    };
    assert_eq!(
        profile.launch.env.get("ODY_TEST").map(String::as_str),
        Some("alpha")
    );
    assert_eq!(
        profile.launch.env.get("_Private").map(String::as_str),
        Some("ok")
    );
    assert_eq!(
        profile.launch.env.get("A B").map(String::as_str),
        Some("spaced")
    );
}

#[test]
fn env_editor_stops_at_the_bounded_limit_with_a_visible_message() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    for _ in 0..MAX_PROFILE_ENV_ENTRIES {
        let _ = manager.activate_form_field(FormField::AddEnv);
    }
    assert_eq!(manager.draft_env.len(), MAX_PROFILE_ENV_ENTRIES);
    let _ = manager.activate_form_field(FormField::AddEnv);
    assert_eq!(
        manager.draft_env.len(),
        MAX_PROFILE_ENV_ENTRIES,
        "the limit is not exceeded"
    );
    assert!(
        manager
            .error
            .as_deref()
            .is_some_and(|error| error.contains("limited to")),
        "hitting the limit shows a bounded message"
    );
}

#[test]
fn tristate_toggles_cycle_inherit_on_off_and_persist() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    manager.draft_name = "fx".to_owned();
    assert_eq!(manager.draft_bloom, "inherit");
    let _ = manager.activate_form_field(FormField::Bloom); // -> on
    assert_eq!(manager.draft_bloom, "on");
    let _ = manager.activate_form_field(FormField::Bloom); // -> off
    assert_eq!(manager.draft_bloom, "off");
    let _ = manager.activate_form_field(FormField::Bloom); // -> inherit
    assert_eq!(manager.draft_bloom, "inherit");

    // Toggle crt to on and leave bloom at inherit; saving must not error.
    let _ = manager.activate_form_field(FormField::Crt);
    let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
        panic!("inherit tri-state must save cleanly, not error");
    };
    assert_eq!(profile.effects.bloom, None, "inherit maps to None");
    assert_eq!(profile.effects.crt, Some(true));
}

#[test]
fn enumerated_fields_cycle_valid_values_only() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    manager.open_add();
    manager.draft_name = "enum".to_owned();
    let _ = manager.activate_form_field(FormField::CursorStyle); // inherit -> block
    assert_eq!(manager.draft_cursor_style, "block");
    // Typing into a cycle field is a no-op: valid values only.
    focus_field(&mut manager, FormField::CursorStyle);
    let _ = manager.handle_input(OverlayInput::Char('z'));
    assert_eq!(manager.draft_cursor_style, "block");
    let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
        panic!("persist");
    };
    assert_eq!(profile.cursor.style.as_deref(), Some("block"));
}

#[test]
fn external_palette_appearance_fields_round_trip_through_form() {
    let mut profile = LaunchProfile::new("dev").expect("profile");
    profile.appearance.follow_external_palette = Some(true);
    profile.appearance.external_palette_provider = Some("colors_toml".to_owned());
    profile.appearance.external_palette_path = Some("/tmp/palette.toml".to_owned());
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("dev".to_owned(), profile);
    let mut manager = ProfileManager::new();
    manager.open(catalog, None);
    let _ = manager.open_edit_selected();
    assert_eq!(manager.draft_follow_external_palette, "on");
    assert_eq!(manager.draft_external_palette_provider, "colors_toml");
    assert_eq!(manager.draft_external_palette_path, "/tmp/palette.toml");
    manager.draft_external_palette_path = "/tmp/edited.toml".to_owned();
    let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
        panic!("persist");
    };
    assert_eq!(profile.appearance.follow_external_palette, Some(true));
    assert_eq!(
        profile.appearance.external_palette_provider.as_deref(),
        Some("colors_toml")
    );
    assert_eq!(
        profile.appearance.external_palette_path.as_deref(),
        Some("/tmp/edited.toml")
    );
}

#[test]
fn unknown_keys_survive_edit_draft() {
    let text = r#"{
  "schema_version": 1,
  "name": "dev",
  "future_flag": true,
  "launch": {
"future_launch": 1,
"command": {"program": "echo", "args": ["hi"], "future_command": true}
  },
  "appearance": {"future_appearance": "kept"},
  "cursor": {"future_cursor": false},
  "effects": {"future_effect": 0.5},
  "layout": {"future_layout": [1, 2]}
}"#;
    let profile = LaunchProfile::parse_json(text, Some("dev")).expect("parse");
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("dev".to_owned(), profile);
    let mut manager = ProfileManager::new();
    manager.open(catalog, None);
    let _ = manager.open_edit_selected();
    manager.draft_title = "Dev".to_owned();
    let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
        panic!("persist");
    };
    let serialized = profile.serialize_pretty();
    for key in [
        "future_flag",
        "future_launch",
        "future_command",
        "future_appearance",
        "future_cursor",
        "future_effect",
        "future_layout",
    ] {
        assert!(serialized.contains(&format!("\"{key}\"")), "{key} missing");
    }
    assert!(profile.preserved.contains_key("future_flag"));
    assert!(profile.launch.preserved.contains_key("future_launch"));
    assert!(
        profile
            .launch
            .command
            .as_ref()
            .expect("command")
            .preserved
            .contains_key("future_command")
    );
    assert!(
        profile
            .appearance
            .preserved
            .contains_key("future_appearance")
    );
    assert!(profile.cursor.preserved.contains_key("future_cursor"));
    assert!(profile.effects.preserved.contains_key("future_effect"));
    assert!(profile.layout.preserved.contains_key("future_layout"));
}

#[test]
fn fully_populated_profile_round_trips_byte_for_byte_without_edits() {
    // Every section populated, including a hand-authored two-of-three
    // platform set that the single-token chooser cannot represent. Opening
    // and saving without touching the platform field must not collapse it.
    let text = r#"{
  "schema_version": 1,
  "name": "full",
  "display_name": "Full profile",
  "platforms": ["linux", "macos"],
  "future_top": {"kept": true},
  "launch": {
"working_directory": "/srv/app",
"command": {"program": "wsl.exe", "args": ["-d", "Ubuntu"], "future_cmd": 3},
"env": {"EDITOR": "vi", "PAGER": "less"},
"future_launch": [1, 2]
  },
  "appearance": {
"theme": "odyssey-classic",
"visual": "odyssey",
"font": "berkeley",
"font_family": "Berkeley Mono",
"font_weight": "medium",
"font_size_px": 13.5,
"title": "Full",
"follow_external_palette": false,
"external_palette_provider": "colors_toml",
"external_palette_path": "/tmp/p.toml",
"future_look": "kept"
  },
  "cursor": {"style": "beam", "blink": "on", "future_cursor": 1},
  "effects": {"render_quality": "high", "bloom": true, "crt": false, "retro": true},
  "layout": {"saved_layout": "grid", "future_layout": 9},
  "connection": "prod-host",
  "switch": {"match_hosts": ["prod-*"], "match_directories": ["/srv/*"]}
}"#;
    let original = LaunchProfile::parse_json(text, Some("full")).expect("parse");
    let original_json = original.serialize_pretty();
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("full".to_owned(), original.clone());

    let mut manager = ProfileManager::new();
    manager.open(catalog, None);
    let _ = manager.open_edit_selected();

    // Every field is populated in the draft.
    assert_eq!(manager.draft_display_name, "Full profile");
    assert_eq!(manager.draft_working_directory, "/srv/app");
    assert_eq!(manager.draft_command_program, "wsl.exe");
    assert_eq!(manager.draft_command_args, vec!["-d", "Ubuntu"]);
    assert_eq!(manager.draft_env.len(), 2);
    assert_eq!(manager.draft_visual, "odyssey");
    assert_eq!(manager.draft_font_weight, "medium");
    assert_eq!(manager.draft_cursor_style, "beam");
    assert_eq!(manager.draft_render_quality, "high");
    assert_eq!(manager.draft_retro, "on");
    assert_eq!(manager.draft_saved_layout, "grid");
    assert_eq!(manager.draft_connection, "prod-host");
    assert_eq!(manager.draft_match_hosts, vec!["prod-*"]);

    // Save with no edits: the persisted JSON is byte-identical.
    let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
        panic!("persist");
    };
    assert_eq!(replace, None);
    assert_eq!(
        profile.serialize_pretty(),
        original_json,
        "an unedited save must reproduce the source document byte for byte"
    );
    // The two-of-three platform set survived intact.
    let platforms = profile.platforms.expect("platforms preserved");
    assert!(platforms.contains(&ProfilePlatform::Linux));
    assert!(platforms.contains(&ProfilePlatform::Macos));
    assert!(!platforms.contains(&ProfilePlatform::Windows));
}

#[test]
fn invalid_name_surfaces_error_without_persist() {
    let mut manager = ProfileManager::new();
    manager.open(ProfileCatalog::default(), None);
    let _ = manager.open_add();
    manager.draft_name = "bad name!".to_owned();
    assert!(matches!(
        manager.try_save(),
        ProfileManagerOutcome::Consumed
    ));
    assert!(manager.error.is_some());
}

#[test]
fn load_edit_save_reload_keeps_every_nested_preserved_map() {
    use crate::profiles::{profile_path_in_dir, read_profile_file, write_profile_file};
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "odytty-profile-ui-preserved-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let text = r#"{
  "schema_version": 1,
  "name": "dev",
  "future_flag": true,
  "launch": {
"shell": "/bin/zsh",
"future_launch": {"kept": true},
"command": {"program": "echo", "args": [], "future_command": 7}
  },
  "appearance": {"theme": "plain", "future_appearance": "kept"},
  "cursor": {"style": "block", "future_cursor": false},
  "effects": {"bloom": true, "future_effect": 0.25},
  "layout": {"saved_layout": "two", "future_layout": [9]}
}"#;
    let parsed = LaunchProfile::parse_json(text, Some("dev")).expect("parse");
    let path = profile_path_in_dir(&dir, "dev").expect("path");
    write_profile_file(&path, &parsed).expect("seed");

    let loaded = read_profile_file(&path, Some("dev")).expect("load");
    let mut catalog = ProfileCatalog::default();
    catalog.profiles.insert("dev".to_owned(), loaded);
    let mut manager = ProfileManager::new();
    manager.open(catalog, None);
    let _ = manager.open_edit_selected();
    manager.draft_title = "Edited".to_owned();
    let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
        panic!("persist");
    };
    assert_eq!(replace, None);
    write_profile_file(&path, &profile).expect("save");
    let reloaded = read_profile_file(&path, Some("dev")).expect("reload");
    assert_eq!(reloaded.appearance.title.as_deref(), Some("Edited"));
    assert!(reloaded.preserved.contains_key("future_flag"));
    assert!(reloaded.launch.preserved.contains_key("future_launch"));
    assert!(
        reloaded
            .launch
            .command
            .as_ref()
            .expect("command")
            .preserved
            .contains_key("future_command")
    );
    assert!(
        reloaded
            .appearance
            .preserved
            .contains_key("future_appearance")
    );
    assert!(reloaded.cursor.preserved.contains_key("future_cursor"));
    assert!(reloaded.effects.preserved.contains_key("future_effect"));
    assert!(reloaded.layout.preserved.contains_key("future_layout"));
    let _ = std::fs::remove_dir_all(dir);
}
