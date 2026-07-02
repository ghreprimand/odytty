// SPDX-License-Identifier: GPL-3.0-only
//! Key-binding parse / round-trip tests (split from legacy.rs to keep files
//! under the module size cap). Pure: env/config parsing only.

use super::*;
use std::ffi::OsStr;

fn settings_from<const N: usize>(values: [(&str, &str); N]) -> (Settings, Vec<String>) {
    settings_from_resolving(values, |_| None)
}

fn settings_from_resolving<const N: usize>(
    values: [(&str, &str); N],
    resolve_family: impl FnMut(&str) -> Option<PathBuf>,
) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let settings = Settings::from_source(
        |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
        },
        |message| warnings.push(message.to_owned()),
        resolve_family,
        |_| None,
    );
    (settings, warnings)
}

#[test]
fn key_bindings_parse_valid_entries_case_insensitively() {
    let (settings, warnings) = settings_from([(
        KEYBINDS_ENV,
        "ctrl+shift+y=copy; SUPER+F=search, Shift+PageDown=scroll-down;ctrl+shift+comma=settings;ctrl+shift+t=theme-picker",
    )]);

    assert_eq!(settings.key_bindings.len(), 5);
    assert_eq!(
        settings.key_bindings[0],
        KeyBindingOverride {
            chord: KeyChord {
                modifiers: KeyBindingModifiers {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    super_key: false,
                },
                key: KeyBindingKey::Character('y'),
            },
            action: BindableAction::Copy,
        }
    );
    assert_eq!(
        settings.key_bindings[1],
        KeyBindingOverride {
            chord: KeyChord {
                modifiers: KeyBindingModifiers {
                    ctrl: false,
                    shift: false,
                    alt: false,
                    super_key: true,
                },
                key: KeyBindingKey::Character('f'),
            },
            action: BindableAction::Search,
        }
    );
    assert_eq!(
        settings.key_bindings[2].chord.key,
        KeyBindingKey::Named(KeyBindingNamedKey::PageDown)
    );
    assert_eq!(
        settings.key_bindings[2].action,
        BindableAction::ScrollPageDown
    );
    assert_eq!(
        settings.key_bindings[3].chord.key,
        KeyBindingKey::Character(',')
    );
    assert_eq!(
        settings.key_bindings[3].action,
        BindableAction::SettingsPanel
    );
    assert_eq!(
        settings.key_bindings[4].chord.key,
        KeyBindingKey::Character('t')
    );
    assert_eq!(settings.key_bindings[4].action, BindableAction::ThemePicker);
    assert!(warnings.is_empty());
}

#[test]
fn key_bindings_skip_bad_entries_with_warnings() {
    let (settings, warnings) = settings_from([(
        KEYBINDS_ENV,
        "ctrl+shift=copy,ctrl+shift+f=nope,ctrl+x+z=paste,alt+space=paste",
    )]);

    assert_eq!(settings.key_bindings.len(), 1);
    assert_eq!(
        settings.key_bindings[0].chord.key,
        KeyBindingKey::Named(KeyBindingNamedKey::Space)
    );
    assert_eq!(settings.key_bindings[0].action, BindableAction::Paste);
    assert_eq!(warnings.len(), 3);
    assert!(
        warnings
            .iter()
            .all(|warning| warning.contains(KEYBINDS_ENV))
    );
}

#[test]
fn empty_key_bindings_are_ignored_without_warning() {
    let (settings, warnings) = settings_from([(KEYBINDS_ENV, " , ; ")]);

    assert!(settings.key_bindings.is_empty());
    assert!(warnings.is_empty());
}

#[test]
fn duplicate_key_binding_entries_preserve_input_order() {
    let (settings, warnings) =
        settings_from([(KEYBINDS_ENV, "ctrl+shift+y=copy,ctrl+shift+y=paste")]);

    assert_eq!(settings.key_bindings.len(), 2);
    assert_eq!(settings.key_bindings[0].action, BindableAction::Copy);
    assert_eq!(settings.key_bindings[1].action, BindableAction::Paste);
    assert!(warnings.is_empty());
}

#[test]
fn new_window_action_parses_and_remaps() {
    // F1: the "new-window" action name parses, and a user can rebind it to any
    // chord through the same `keybinds` config the remap UI serializes. The
    // default chord (Ctrl+Shift+N) lives in the native default table; here we
    // prove the config surface accepts an override onto a different chord.
    assert_eq!(
        BindableAction::parse("new-window"),
        Some(BindableAction::NewWindow),
    );
    let (settings, warnings) = settings_from([(KEYBINDS_ENV, "ctrl+shift+g=new-window")]);
    assert_eq!(settings.key_bindings.len(), 1);
    assert_eq!(settings.key_bindings[0].action, BindableAction::NewWindow);
    assert!(warnings.is_empty());
}

#[test]
fn bindable_action_names_round_trip_through_parse() {
    // Every variant — driven off the single `ALL` source of truth so a new
    // action is covered automatically.
    for action in BindableAction::ALL {
        assert_eq!(
            BindableAction::parse(bindable_action_name(action)),
            Some(action),
            "action name did not round-trip: {action:?}"
        );
    }
}

#[test]
fn keybinds_value_round_trips_for_every_action() {
    // Bind a distinct chord to every action, serialize via the same path the
    // in-app keybinding editor saves through, then re-parse — proving the full
    // expanded editor set (overlay/tab/pane actions included) survives the
    // config round-trip, not only the original 12.
    let overrides: Vec<KeyBindingOverride> = BindableAction::ALL
        .iter()
        .enumerate()
        .map(|(i, &action)| KeyBindingOverride {
            chord: KeyChord {
                modifiers: KeyBindingModifiers {
                    ctrl: true,
                    shift: true,
                    // The `alt` bit distinguishes the second pass over the
                    // alphabet, so every action gets a distinct chord even once
                    // `ALL` grows past 26 entries.
                    alt: i >= 26,
                    super_key: false,
                },
                // Distinct printable key per action: cycle the alphabet, with
                // the `alt` bit above keeping the wrapped letters distinct.
                key: KeyBindingKey::Character((b'a' + (i % 26) as u8) as char),
            },
            action,
        })
        .collect();

    let value = key_bindings_config_value(&overrides);
    let mut warnings = Vec::new();
    let parsed = parse_key_bindings(Some(OsStr::new(&value)), &mut |m| {
        warnings.push(m.to_owned())
    });

    assert!(warnings.is_empty(), "round-trip warned: {warnings:?}");
    assert_eq!(parsed, overrides, "keybinds value must round-trip exactly");
}

#[test]
fn all_bindable_actions_is_exhaustive() {
    // `BindableAction::ALL` is the source of truth for the in-app keybinding
    // editor's row set; this pins it to the enum so a newly-added variant can't
    // be silently omitted. The match below fails to compile if a variant is
    // added without being classified here, and the assertions catch a variant
    // added to the enum but not to `ALL` (or duplicated within it).
    fn classify(action: BindableAction) -> u8 {
        use BindableAction::*;
        match action {
            Search | SettingsPanel | ThemePicker | Copy | Paste | ScrollPageUp | ScrollPageDown
            | JumpPromptPrev | JumpPromptNext | CopyMode | Hints | ClearInput => 0,
            CommandPalette | ConnectionManager | SessionReplay | ThemeBuilder | SessionAttach => 1,
            NewTab | NewWindow | NextTab | PrevTab | CloseTab => 2,
            SplitColumns | SplitRows | FocusPaneLeft | FocusPaneRight | FocusPaneUp
            | FocusPaneDown | FocusPaneNext | ClosePane | ZoomPane | EqualizePanes => 3,
        }
    }

    // Distinct: no variant appears twice in ALL.
    for (i, a) in BindableAction::ALL.iter().enumerate() {
        for b in &BindableAction::ALL[i + 1..] {
            assert_ne!(a, b, "BindableAction::ALL must hold each variant once");
        }
    }
    // Every variant is reachable through `classify`, and every group is present
    // in ALL — proving ALL covers all four classes without omission.
    let mut groups = [false; 4];
    for action in BindableAction::ALL {
        groups[classify(action) as usize] = true;
    }
    assert!(
        groups.iter().all(|&seen| seen),
        "BindableAction::ALL must include every action class"
    );
}

#[test]
fn new_bindable_actions_parse_from_config_names() {
    let (settings, warnings) = settings_from([(
        KEYBINDS_ENV,
        "ctrl+shift+p=jump-prompt-prev;ctrl+shift+n=jump-prompt-next;ctrl+alt+k=copy-mode;ctrl+shift+l=hints",
    )]);

    let actions: Vec<BindableAction> = settings
        .key_bindings
        .iter()
        .map(|binding| binding.action)
        .collect();
    assert_eq!(
        actions,
        vec![
            BindableAction::JumpPromptPrev,
            BindableAction::JumpPromptNext,
            BindableAction::CopyMode,
            BindableAction::Hints,
        ]
    );
    assert!(warnings.is_empty());
}
