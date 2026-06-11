//! Mouse/focus/title reports and key-binding/key-mapping tests. (M6 mechanical split from native/tests.rs).

use super::*;

#[test]
fn changed_window_title_reports_only_on_core_change() {
    let mut terminal = Terminal::new(10, 2);

    assert_eq!(changed_window_title(&mut terminal, "OdyTTY"), None);

    terminal.advance(b"\x1b]2;build log\x07");
    assert_eq!(
        changed_window_title(&mut terminal, "OdyTTY").as_deref(),
        Some("build log")
    );
    assert_eq!(changed_window_title(&mut terminal, "OdyTTY"), None);

    terminal.advance(b"\x1b]2;\x07");
    assert_eq!(
        changed_window_title(&mut terminal, "OdyTTY").as_deref(),
        Some("")
    );
}

#[test]
fn native_mouse_reports_use_one_based_cells_and_modifiers() {
    let protocol = MouseProtocol {
        tracking: MouseTracking::Normal,
        encoding: crate::core::MouseEncoding::Sgr,
    };
    let point = CellPoint { row: 4, column: 9 };
    let mods = Modifiers {
        shift: true,
        ctrl: true,
        alt: true,
    };

    assert_eq!(
        encode_native_mouse_report(
            protocol,
            point,
            CoreMouseButton::Left,
            MouseEventKind::Press,
            mods,
        )
        .as_deref(),
        Some(b"\x1b[<24;10;5M".as_slice())
    );
}

#[test]
fn any_event_hover_motion_uses_no_button_when_no_button_is_held() {
    let any_event = MouseProtocol {
        tracking: MouseTracking::AnyEvent,
        encoding: crate::core::MouseEncoding::Sgr,
    };
    let button_event = MouseProtocol {
        tracking: MouseTracking::ButtonEvent,
        encoding: crate::core::MouseEncoding::Sgr,
    };

    assert_eq!(
        motion_report_button(any_event, None),
        Some(CoreMouseButton::NoButton)
    );
    assert_eq!(
        motion_report_button(any_event, Some(CoreMouseButton::Left)),
        Some(CoreMouseButton::Left)
    );
    assert_eq!(motion_report_button(button_event, None), None);
}

#[test]
fn native_focus_reports_follow_terminal_focus_mode() {
    let mut terminal = Terminal::new(10, 2);

    assert_eq!(encode_native_focus_report(&terminal, true), None);
    assert_eq!(encode_native_focus_report(&terminal, false), None);

    terminal.advance(b"\x1b[?1004h");
    assert_eq!(
        encode_native_focus_report(&terminal, true).as_deref(),
        Some(b"\x1b[I".as_slice())
    );
    assert_eq!(
        encode_native_focus_report(&terminal, false).as_deref(),
        Some(b"\x1b[O".as_slice())
    );

    terminal.advance(b"\x1b[?1004l");
    assert_eq!(encode_native_focus_report(&terminal, true), None);
}

#[test]
fn maps_winit_mouse_buttons_to_core_buttons() {
    assert_eq!(
        map_winit_mouse_button(WinitMouseButton::Left),
        Some(CoreMouseButton::Left)
    );
    assert_eq!(
        map_winit_mouse_button(WinitMouseButton::Middle),
        Some(CoreMouseButton::Middle)
    );
    assert_eq!(
        map_winit_mouse_button(WinitMouseButton::Right),
        Some(CoreMouseButton::Right)
    );
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Back), None);
}

#[test]
fn wheel_delta_maps_to_mouse_report_buttons() {
    assert_eq!(
        wheel_report_button(MouseScrollDelta::LineDelta(0.0, 1.0)),
        Some(CoreMouseButton::WheelUp)
    );
    assert_eq!(
        wheel_report_button(MouseScrollDelta::LineDelta(0.0, -1.0)),
        Some(CoreMouseButton::WheelDown)
    );
    assert_eq!(
        wheel_report_button(MouseScrollDelta::LineDelta(0.0, 0.0)),
        None
    );
}

#[test]
fn named_keys_map_to_neutral_model() {
    assert_eq!(map_named_key(NamedKey::Enter, false), Some(Key::Enter));
    assert_eq!(map_named_key(NamedKey::ArrowUp, false), Some(Key::Up));
    assert_eq!(
        map_named_key(NamedKey::Backspace, false),
        Some(Key::Backspace)
    );
    // Shift-Tab becomes BackTab; plain Tab stays Tab.
    assert_eq!(map_named_key(NamedKey::Tab, false), Some(Key::Tab));
    assert_eq!(map_named_key(NamedKey::Tab, true), Some(Key::BackTab));
    // Space maps to a char so Ctrl-Space can encode to NUL downstream.
    assert_eq!(map_named_key(NamedKey::Space, false), Some(Key::Char(' ')));
    // Unhandled named keys are dropped.
    assert_eq!(map_named_key(NamedKey::F1, false), None);
}

#[test]
fn keypad_physical_keys_map_to_neutral_model() {
    assert_eq!(
        map_keypad_physical_key(PhysicalKey::Code(KeyCode::Numpad1)),
        Some(Key::KeypadDigit(1))
    );
    assert_eq!(
        map_keypad_physical_key(PhysicalKey::Code(KeyCode::NumpadEnter)),
        Some(Key::KeypadEnter)
    );
    assert_eq!(
        map_keypad_physical_key(PhysicalKey::Code(KeyCode::Digit1)),
        None
    );
}

#[test]
fn space_named_key_encodes_nul_under_ctrl() {
    // Full path: Space named key -> neutral Key -> shared encoder, with Ctrl.
    let key = map_named_key(NamedKey::Space, false).expect("space maps");
    assert_eq!(
        input::encode_key(key, Modifiers::CTRL, input::KeyModes::default()),
        vec![0]
    );
}

#[test]
fn key_modes_from_core_preserves_kitty_keyboard_flags() {
    let modes = key_modes_from_core(CoreKeyboardModes {
        application_cursor: true,
        application_keypad: true,
        kitty_keyboard_flags: 9,
    });

    assert!(modes.application_cursor);
    assert!(modes.application_keypad);
    assert_eq!(modes.kitty_keyboard_flags, 9);
}

#[test]
fn mapped_named_key_release_uses_kitty_event_type_flag() {
    let key = map_named_key(NamedKey::ArrowUp, false).expect("arrow maps");
    let modes = input::KeyModes {
        kitty_keyboard_flags: input::KITTY_REPORT_EVENT_TYPES,
        ..input::KeyModes::default()
    };

    assert_eq!(
        input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Release),
        b"\x1b[1;1:3A"
    );
    assert!(
        input::encode_key_event(
            key,
            Modifiers::NONE,
            input::KeyModes::default(),
            KeyEventType::Release
        )
        .is_empty()
    );
}

#[test]
fn paste_shortcut_requires_ctrl_shift_v() {
    assert!(is_paste_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(is_paste_shortcut(
        &WinitKey::Character("V".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(!is_paste_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers::CTRL
    ));
    assert!(!is_paste_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
        }
    ));
    assert!(!is_paste_shortcut(
        &WinitKey::Named(NamedKey::Enter),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
}

#[test]
fn copy_shortcut_requires_ctrl_shift_c() {
    assert!(is_copy_shortcut(
        &WinitKey::Character("c".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(is_copy_shortcut(
        &WinitKey::Character("C".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(!is_copy_shortcut(
        &WinitKey::Character("c".into()),
        Modifiers::CTRL
    ));
    assert!(!is_copy_shortcut(
        &WinitKey::Character("c".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
        }
    ));
    assert!(!is_copy_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
}

#[test]
fn key_bindings_preserve_default_shortcuts_when_unset() {
    let bindings = KeyBindings::from_overrides(&[]);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };
    let shift = Modifiers {
        ctrl: false,
        shift: true,
        alt: false,
    };

    assert_eq!(
        bindings.action_for(&WinitKey::Character("f".into()), ctrl_shift, false),
        Some(BindableAction::Search)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("c".into()), ctrl_shift, false),
        Some(BindableAction::Copy)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("v".into()), ctrl_shift, false),
        Some(BindableAction::Paste)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::PageUp), shift, false),
        Some(BindableAction::ScrollPageUp)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::PageDown), shift, false),
        Some(BindableAction::ScrollPageDown)
    );
}

#[test]
fn key_bindings_override_only_the_named_action() {
    let override_ = KeyBindingOverride {
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
    };
    let bindings = KeyBindings::from_overrides(&[override_]);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    assert_eq!(
        bindings.action_for(&WinitKey::Character("y".into()), ctrl_shift, false),
        Some(BindableAction::Copy)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("c".into()), ctrl_shift, false),
        None
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("v".into()), ctrl_shift, false),
        Some(BindableAction::Paste)
    );
}

#[test]
fn key_bindings_support_super_modifier_without_pty_modifier_changes() {
    let override_ = KeyBindingOverride {
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
    };
    let bindings = KeyBindings::from_overrides(&[override_]);

    assert_eq!(
        bindings.action_for(&WinitKey::Character("f".into()), Modifiers::default(), true),
        Some(BindableAction::Search)
    );
    assert_eq!(
        bindings.action_for(
            &WinitKey::Character("f".into()),
            Modifiers::default(),
            false
        ),
        None
    );
}

#[test]
fn duplicate_key_binding_chord_uses_last_action() {
    let chord = KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl: true,
            shift: true,
            alt: false,
            super_key: false,
        },
        key: KeyBindingKey::Character('y'),
    };
    let bindings = KeyBindings::from_overrides(&[
        KeyBindingOverride {
            chord,
            action: BindableAction::Copy,
        },
        KeyBindingOverride {
            chord,
            action: BindableAction::Paste,
        },
    ]);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    assert_eq!(
        bindings.action_for(&WinitKey::Character("y".into()), ctrl_shift, false),
        Some(BindableAction::Paste)
    );
}
