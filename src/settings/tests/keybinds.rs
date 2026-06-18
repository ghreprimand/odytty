// SPDX-License-Identifier: GPL-3.0-only
//! Key-binding parse / round-trip tests (split from legacy.rs to keep files
//! under the module size cap). Pure: env/config parsing only.

use super::*;

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
fn bindable_action_names_round_trip_through_parse() {
    use BindableAction::*;
    for action in [
        Search,
        SettingsPanel,
        ThemePicker,
        Copy,
        Paste,
        ScrollPageUp,
        ScrollPageDown,
        JumpPromptPrev,
        JumpPromptNext,
        CopyMode,
        Hints,
        ClearInput,
        NewTab,
        NextTab,
        PrevTab,
        CloseTab,
    ] {
        assert_eq!(
            BindableAction::parse(bindable_action_name(action)),
            Some(action),
            "action name did not round-trip: {action:?}"
        );
    }
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
