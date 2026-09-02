// SPDX-License-Identifier: GPL-3.0-only
//! Mouse/focus/title reports and key-binding/key-mapping tests. (M6 mechanical split from native/tests.rs).

use super::*;

#[derive(Clone, Default)]
struct KeyRecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for KeyRecordingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .expect("key bytes")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

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
    // F1..F12 reach the PTY encoder; F13+ stay chord-only.
    assert_eq!(map_named_key(NamedKey::F1, false), Some(Key::F(1)));
    // Unhandled named keys are dropped.
    assert_eq!(map_named_key(NamedKey::F13, false), None);
}

#[test]
fn fedora_wayland_ctrl_backspace_reaches_the_pty_through_the_real_key_path() {
    let dimensions = Dimensions::new(80, 24);
    let recorder = KeyRecordingWriter::default();
    let recorded = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        dimensions,
        Settings::default(),
        writer,
    );

    let backspace = WinitKey::Named(NamedKey::Backspace);
    app.drive_raw_key_event_for_test(
        backspace.clone(),
        backspace.clone(),
        PhysicalKey::Code(KeyCode::Backspace),
        Modifiers::CTRL,
        KeyEventType::Press,
    );
    app.drive_raw_key_event_for_test(
        backspace.clone(),
        backspace.clone(),
        PhysicalKey::Code(KeyCode::Backspace),
        Modifiers::CTRL,
        KeyEventType::Release,
    );
    assert_eq!(&*recorded.lock().expect("legacy bytes"), b"\x08");

    recorded.lock().expect("clear legacy bytes").clear();
    terminal.lock().expect("terminal").advance(b"\x1b[=1;2u");
    app.drive_raw_key_event_for_test(
        backspace.clone(),
        backspace.clone(),
        PhysicalKey::Code(KeyCode::Backspace),
        Modifiers::CTRL,
        KeyEventType::Press,
    );
    app.drive_raw_key_event_for_test(
        backspace.clone(),
        backspace,
        PhysicalKey::Code(KeyCode::Backspace),
        Modifiers::CTRL,
        KeyEventType::Release,
    );
    assert_eq!(&*recorded.lock().expect("kitty bytes"), b"\x1b[127;5u");

    terminal.lock().expect("terminal").advance(b"\x1b[=1;3u");
    assert_eq!(
        terminal
            .lock()
            .expect("terminal")
            .keyboard_modes()
            .kitty_keyboard_flags,
        0,
        "the Bash PS0 removal must restore legacy mode before a child runs"
    );
}

#[test]
fn plain_tab_cycling_punctuation_reaches_the_pty_in_legacy_and_kitty_modes() {
    let dimensions = Dimensions::new(80, 24);
    let recorder = KeyRecordingWriter::default();
    let recorded = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        dimensions,
        Settings::default(),
        writer,
    );

    let punctuation_cases = [
        (";", ";", KeyCode::Semicolon, Modifiers::NONE),
        ("'", "'", KeyCode::Quote, Modifiers::NONE),
        (
            ":",
            ";",
            KeyCode::Semicolon,
            Modifiers {
                ctrl: false,
                shift: true,
                alt: false,
            },
        ),
        (
            "\"",
            "'",
            KeyCode::Quote,
            Modifiers {
                ctrl: false,
                shift: true,
                alt: false,
            },
        ),
    ];
    for (logical_character, binding_character, code, modifiers) in punctuation_cases {
        let logical = WinitKey::Character(logical_character.into());
        let binding_key = WinitKey::Character(binding_character.into());
        app.drive_raw_key_event_for_test(
            logical.clone(),
            binding_key.clone(),
            PhysicalKey::Code(code),
            modifiers,
            KeyEventType::Press,
        );
        app.drive_raw_key_event_for_test(
            logical,
            binding_key,
            PhysicalKey::Code(code),
            modifiers,
            KeyEventType::Release,
        );
    }
    assert_eq!(
        &*recorded.lock().expect("legacy punctuation bytes"),
        b";':\"",
        "plain and shifted punctuation must not be intercepted by tab cycling"
    );

    recorded
        .lock()
        .expect("clear legacy punctuation bytes")
        .clear();
    // Kitty's report-all-keys flag makes printable characters use explicit
    // protocol sequences; default Kitty disambiguation deliberately leaves
    // them as raw bytes, which the legacy assertion above already covers.
    terminal.lock().expect("terminal").advance(b"\x1b[=8u");
    for (logical_character, binding_character, code, modifiers) in punctuation_cases {
        let logical = WinitKey::Character(logical_character.into());
        let binding_key = WinitKey::Character(binding_character.into());
        app.drive_raw_key_event_for_test(
            logical.clone(),
            binding_key.clone(),
            PhysicalKey::Code(code),
            modifiers,
            KeyEventType::Press,
        );
        app.drive_raw_key_event_for_test(
            logical,
            binding_key,
            PhysicalKey::Code(code),
            modifiers,
            KeyEventType::Release,
        );
    }
    assert_eq!(
        &*recorded.lock().expect("kitty punctuation bytes"),
        b"\x1b[59u\x1b[39u\x1b[59;2u\x1b[39;2u",
        "plain and shifted punctuation keep their Kitty protocol encodings"
    );
}

#[test]
fn tab_cycling_punctuation_does_not_bind_when_the_layout_base_key_is_a_letter() {
    let bindings = KeyBindings::default();
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    // On layouts where the physical punctuation position reports a letter from
    // `key_without_modifiers()` (for example a German-style layout), that
    // letter is not the documented semicolon base key and must reach the PTY.
    assert_eq!(
        bindings.action_for(&WinitKey::Character("ö".into()), ctrl_shift, false),
        None
    );
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
fn physical_editing_keys_normalize_compositor_specific_logical_forms() {
    let cases = [
        (KeyCode::Backspace, "\u{8}", NamedKey::Backspace),
        (KeyCode::NumpadBackspace, "\u{7f}", NamedKey::Backspace),
        (KeyCode::Tab, "\t", NamedKey::Tab),
        (KeyCode::Enter, "\r", NamedKey::Enter),
        (KeyCode::Escape, "\u{1b}", NamedKey::Escape),
        (KeyCode::Delete, "\u{7f}", NamedKey::Delete),
    ];
    for (physical, reported, expected) in cases {
        assert_eq!(
            normalize_winit_editing_key(
                WinitKey::Character(reported.into()),
                PhysicalKey::Code(physical),
            ),
            WinitKey::Named(expected),
            "physical {physical:?} must override compositor logical {reported:?}"
        );
    }

    // A control character from a non-editing physical key is not rewritten at
    // this layer; the shared encoder retains its control-text fallback policy.
    let ctrl_h = WinitKey::Character("\u{8}".into());
    assert_eq!(
        normalize_winit_editing_key(ctrl_h.clone(), PhysicalKey::Code(KeyCode::KeyH)),
        ctrl_h
    );
    // Keep keypad Enter distinct for DECKPAM/DECKPNM handling.
    assert_eq!(
        normalize_winit_editing_key(
            WinitKey::Named(NamedKey::Enter),
            PhysicalKey::Code(KeyCode::NumpadEnter),
        ),
        WinitKey::Named(NamedKey::Enter)
    );
    assert_eq!(
        normalize_winit_editing_key(
            WinitKey::Named(NamedKey::Backspace),
            PhysicalKey::Code(KeyCode::Enter),
        ),
        WinitKey::Named(NamedKey::Backspace),
        "an already-named logical key remains authoritative"
    );
}

#[test]
fn win32_physical_mapping_distinguishes_ctrl_backspace() {
    let plain = map_win32_key_event(
        PhysicalKey::Code(KeyCode::Backspace),
        &WinitKey::Named(NamedKey::Backspace),
        &WinitKey::Named(NamedKey::Backspace),
        Modifiers::NONE,
        KeyEventType::Press,
    )
    .expect("Backspace maps");
    let ctrl = map_win32_key_event(
        PhysicalKey::Code(KeyCode::Backspace),
        &WinitKey::Named(NamedKey::Backspace),
        &WinitKey::Named(NamedKey::Backspace),
        Modifiers::CTRL,
        KeyEventType::Press,
    )
    .expect("Ctrl+Backspace maps");

    assert_eq!(
        input::encode_win32_key_event(plain, KeyEventType::Press),
        b"\x1b[8;14;8;1;0;1_"
    );
    assert_eq!(
        input::encode_win32_key_event(ctrl, KeyEventType::Press),
        b"\x1b[8;14;8;1;8;1_"
    );
}

#[test]
fn win32_physical_mapping_covers_shift_enter_char_and_modifier_release() {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let enter = map_win32_key_event(
        PhysicalKey::Code(KeyCode::Enter),
        &WinitKey::Named(NamedKey::Enter),
        &WinitKey::Named(NamedKey::Enter),
        shift,
        KeyEventType::Press,
    )
    .expect("Enter maps");
    assert_eq!(
        input::encode_win32_key_event(enter, KeyEventType::Press),
        b"\x1b[13;28;13;1;16;1_"
    );

    let letter = map_win32_key_event(
        PhysicalKey::Code(KeyCode::KeyA),
        &WinitKey::Character("a".into()),
        &WinitKey::Character("a".into()),
        Modifiers::NONE,
        KeyEventType::Press,
    )
    .expect("A maps");
    assert_eq!(
        input::encode_win32_key_event(letter, KeyEventType::Press),
        b"\x1b[65;30;97;1;0;1_"
    );

    let ctrl_up = map_win32_key_event(
        PhysicalKey::Code(KeyCode::ControlRight),
        &WinitKey::Named(NamedKey::Control),
        &WinitKey::Named(NamedKey::Control),
        Modifiers::CTRL,
        KeyEventType::Release,
    )
    .expect("right Control maps");
    assert_eq!(
        input::encode_win32_key_event(ctrl_up, KeyEventType::Release),
        b"\x1b[17;29;0;0;256;1_"
    );
}

#[test]
fn win32_mapping_keeps_layout_virtual_key_separate_from_physical_scan() {
    // AZERTY's physical KeyQ position produces logical A: Windows reports
    // VK_A with the KeyQ/set-1 scan code (0x10).
    let event = map_win32_key_event(
        PhysicalKey::Code(KeyCode::KeyQ),
        &WinitKey::Character("a".into()),
        &WinitKey::Character("a".into()),
        Modifiers::NONE,
        KeyEventType::Press,
    )
    .expect("layout-remapped key maps");
    assert_eq!(event.virtual_key, 0x41);
    assert_eq!(event.scan_code, 0x10);
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
        win32_input: true,
        kitty_keyboard_flags: 9,
        modify_other_keys: 2,
    });

    assert!(modes.application_cursor);
    assert!(modes.application_keypad);
    assert_eq!(modes.win32_input, cfg!(windows));
    assert_eq!(modes.kitty_keyboard_flags, 9);
    assert_eq!(modes.modify_other_keys, 2);
}

#[test]
fn overlay_shortcut_is_ctrl_shift_comma_only() {
    let key = WinitKey::Character(",".into());
    let mods = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    assert!(is_overlay_shortcut(&key, mods, false));
    assert!(!is_overlay_shortcut(&key, mods, true));
    assert!(!is_overlay_shortcut(
        &WinitKey::Character(".".into()),
        mods,
        false
    ));
    assert!(!is_overlay_shortcut(
        &key,
        Modifiers {
            ctrl: true,
            shift: false,
            alt: false,
        },
        false
    ));
}

#[test]
fn theme_picker_shortcut_is_ctrl_shift_h_only() {
    let key = WinitKey::Character("h".into());
    let mods = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    assert!(is_theme_picker_shortcut(&key, mods, false));
    assert!(!is_theme_picker_shortcut(&key, mods, true));
    assert!(!is_theme_picker_shortcut(
        &WinitKey::Character("t".into()),
        mods,
        false
    ));
    assert!(!is_theme_picker_shortcut(
        &key,
        Modifiers {
            ctrl: true,
            shift: false,
            alt: false,
        },
        false
    ));
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
        bindings.action_for(&WinitKey::Character("<".into()), ctrl_shift, false),
        None
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character(",".into()), ctrl_shift, false),
        Some(BindableAction::SettingsPanel)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("h".into()), ctrl_shift, false),
        Some(BindableAction::ThemePicker)
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
fn key_bindings_default_prompt_copymode_and_hints_chords() {
    let bindings = KeyBindings::from_overrides(&[]);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    // Prompt navigation (v0.3.1): the arrow chords are the sole default
    // bindings. The `Ctrl+Shift+P` / `Ctrl+Shift+N` letter fallbacks were
    // reclaimed — P now opens the command palette, N is unbound.
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::ArrowUp), ctrl_shift, false),
        Some(BindableAction::JumpPromptPrev)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("p".into()), ctrl_shift, false),
        Some(BindableAction::CommandPalette),
        "Ctrl+Shift+P now opens the command palette, not prompt-jump"
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::ArrowDown), ctrl_shift, false),
        Some(BindableAction::JumpPromptNext)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("n".into()), ctrl_shift, false),
        Some(BindableAction::NewWindow),
        "Ctrl+Shift+N opens a new window (F1)"
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::Space), ctrl_shift, false),
        Some(BindableAction::CopyMode)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("l".into()), ctrl_shift, false),
        Some(BindableAction::Hints)
    );
    // IN1: Ctrl+Shift+K clears the shell input line.
    assert_eq!(
        bindings.action_for(&WinitKey::Character("k".into()), ctrl_shift, false),
        Some(BindableAction::ClearInput)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("t".into()), ctrl_shift, false),
        Some(BindableAction::NewTab)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("w".into()), ctrl_shift, false),
        Some(BindableAction::CloseTab)
    );
    assert_eq!(
        bindings.action_for(
            &WinitKey::Named(NamedKey::PageDown),
            Modifiers {
                ctrl: true,
                shift: false,
                alt: false,
            },
            false
        ),
        Some(BindableAction::NextTab)
    );
    assert_eq!(
        bindings.action_for(
            &WinitKey::Named(NamedKey::PageUp),
            Modifiers {
                ctrl: true,
                shift: false,
                alt: false,
            },
            false
        ),
        Some(BindableAction::PrevTab)
    );
    // The live event path supplies `key_without_modifiers()` to this lookup, so
    // the stored physical base keys resolve even though Ctrl+Shift+; and
    // Ctrl+Shift+' produce `:` and `"` as their logical characters.
    assert_eq!(
        bindings.action_for(&WinitKey::Character(";".into()), ctrl_shift, false),
        Some(BindableAction::PrevTab)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("'".into()), ctrl_shift, false),
        Some(BindableAction::NextTab)
    );
    // Workspace cycling sits one modifier above tab cycling: Ctrl+Shift+Page*.
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::PageDown), ctrl_shift, false),
        Some(BindableAction::NextWorkspace)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::PageUp), ctrl_shift, false),
        Some(BindableAction::PrevWorkspace)
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

/// The two Win32 mappers must agree on key identity where their domains meet.
///
/// Windows records are built by the physical mapper here, while synthesized
/// keys (alternate scroll, click-to-position) and every non-Windows front end
/// go through the neutral mapper in `input`. Two independent tables describing
/// the same hardware is the sibling-path shape this codebase drifts on, so the
/// overlap is pinned rather than assumed.
///
/// Ctrl comparisons are limited to character keys. Named and keypad Ctrl
/// records carry key-specific Windows semantics beyond this shared overlap and
/// remain outside this cross-mapper assertion.
#[test]
fn win32_physical_and_neutral_mappers_agree_on_shared_key_identities() {
    let modes = input::KeyModes {
        win32_input: true,
        ..input::KeyModes::default()
    };
    let cases: &[(KeyCode, WinitKey, input::Key)] = &[
        (
            KeyCode::KeyA,
            WinitKey::Character("a".into()),
            input::Key::Char('a'),
        ),
        (
            KeyCode::KeyM,
            WinitKey::Character("m".into()),
            input::Key::Char('m'),
        ),
        (
            KeyCode::KeyZ,
            WinitKey::Character("z".into()),
            input::Key::Char('z'),
        ),
        (
            KeyCode::Digit0,
            WinitKey::Character("0".into()),
            input::Key::Char('0'),
        ),
        (
            KeyCode::Digit5,
            WinitKey::Character("5".into()),
            input::Key::Char('5'),
        ),
        (
            KeyCode::Digit9,
            WinitKey::Character("9".into()),
            input::Key::Char('9'),
        ),
        (
            KeyCode::Numpad0,
            WinitKey::Character("0".into()),
            input::Key::KeypadDigit(0),
        ),
        (
            KeyCode::Numpad1,
            WinitKey::Character("1".into()),
            input::Key::KeypadDigit(1),
        ),
        (
            KeyCode::NumpadDivide,
            WinitKey::Character("/".into()),
            input::Key::KeypadDivide,
        ),
        (
            KeyCode::Space,
            WinitKey::Named(NamedKey::Space),
            input::Key::Char(' '),
        ),
        (
            KeyCode::Enter,
            WinitKey::Named(NamedKey::Enter),
            input::Key::Enter,
        ),
        (
            KeyCode::Tab,
            WinitKey::Named(NamedKey::Tab),
            input::Key::Tab,
        ),
        (
            KeyCode::Backspace,
            WinitKey::Named(NamedKey::Backspace),
            input::Key::Backspace,
        ),
        (
            KeyCode::Escape,
            WinitKey::Named(NamedKey::Escape),
            input::Key::Esc,
        ),
        (KeyCode::F1, WinitKey::Named(NamedKey::F1), input::Key::F(1)),
        (
            KeyCode::F10,
            WinitKey::Named(NamedKey::F10),
            input::Key::F(10),
        ),
        (
            KeyCode::F11,
            WinitKey::Named(NamedKey::F11),
            input::Key::F(11),
        ),
        (
            KeyCode::F12,
            WinitKey::Named(NamedKey::F12),
            input::Key::F(12),
        ),
        (
            KeyCode::ArrowLeft,
            WinitKey::Named(NamedKey::ArrowLeft),
            input::Key::Left,
        ),
        (
            KeyCode::ArrowRight,
            WinitKey::Named(NamedKey::ArrowRight),
            input::Key::Right,
        ),
        (
            KeyCode::ArrowUp,
            WinitKey::Named(NamedKey::ArrowUp),
            input::Key::Up,
        ),
        (
            KeyCode::ArrowDown,
            WinitKey::Named(NamedKey::ArrowDown),
            input::Key::Down,
        ),
        (
            KeyCode::Home,
            WinitKey::Named(NamedKey::Home),
            input::Key::Home,
        ),
        (
            KeyCode::End,
            WinitKey::Named(NamedKey::End),
            input::Key::End,
        ),
        (
            KeyCode::PageUp,
            WinitKey::Named(NamedKey::PageUp),
            input::Key::PageUp,
        ),
        (
            KeyCode::PageDown,
            WinitKey::Named(NamedKey::PageDown),
            input::Key::PageDown,
        ),
        (
            KeyCode::Insert,
            WinitKey::Named(NamedKey::Insert),
            input::Key::Insert,
        ),
        (
            KeyCode::Delete,
            WinitKey::Named(NamedKey::Delete),
            input::Key::Delete,
        ),
    ];

    for (code, logical, neutral) in cases {
        for mods in [Modifiers::NONE, Modifiers::CTRL] {
            // The shared Ctrl policy applies to character keys. Keypad and
            // named-key Ctrl records are deliberately outside this comparison.
            if mods.ctrl && !matches!(neutral, input::Key::Char(_)) {
                continue;
            }
            let physical = map_win32_key_event(
                PhysicalKey::Code(*code),
                logical,
                logical,
                mods,
                KeyEventType::Press,
            )
            .expect("physical mapping exists for a shared key");
            assert_eq!(
                input::encode_win32_key_event(physical, KeyEventType::Press),
                input::encode_key_event(*neutral, mods, modes, KeyEventType::Press),
                "physical and neutral Win32 records disagree for {code:?} with {mods:?}"
            );
        }
    }
}

#[test]
fn win32_physical_mapper_preserves_keypad_virtual_key_identity() {
    let cases = [
        (KeyCode::Numpad0, "0", b"\x1b[96;82;48;1;0;1_".as_slice()),
        (KeyCode::Numpad1, "1", b"\x1b[97;79;49;1;0;1_".as_slice()),
        (
            KeyCode::NumpadDivide,
            "/",
            b"\x1b[111;53;47;1;256;1_".as_slice(),
        ),
    ];

    for (code, logical, expected) in cases {
        let logical = WinitKey::Character(logical.into());
        let event = map_win32_key_event(
            PhysicalKey::Code(code),
            &logical,
            &logical,
            Modifiers::NONE,
            KeyEventType::Press,
        )
        .expect("keypad key maps");
        assert_eq!(
            input::encode_win32_key_event(event, KeyEventType::Press),
            expected,
            "physical mapping lost the keypad identity for {code:?}"
        );
    }
}

#[test]
fn win32_physical_mapper_matches_neutral_ctrl_unicode_units() {
    let cases = [
        (
            KeyCode::Space,
            WinitKey::Named(NamedKey::Space),
            b"\x1b[32;57;0;1;8;1_".as_slice(),
        ),
        (
            KeyCode::Digit5,
            WinitKey::Character("5".into()),
            b"\x1b[53;6;0;1;8;1_".as_slice(),
        ),
    ];

    for (code, logical, expected) in cases {
        let event = map_win32_key_event(
            PhysicalKey::Code(code),
            &logical,
            &logical,
            Modifiers::CTRL,
            KeyEventType::Press,
        )
        .expect("Ctrl key maps");
        assert_eq!(
            input::encode_win32_key_event(event, KeyEventType::Press),
            expected,
            "physical mapping reported the wrong Ctrl Unicode unit for {code:?}"
        );
    }
}
