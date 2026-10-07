// SPDX-License-Identifier: GPL-3.0-only
//! Key-binding editor tests for the list window, message wrapping, chord
//! spaces, pane-key reassignment and typing-key refusal.

use super::*;
use crate::settings::KeyBindingModifiers;

const WIDTH: usize = 60;

fn ui() -> KeyRemapUi {
    let mut ui = KeyRemapUi::new(&Settings::default());
    ui.open(&Settings::default());
    ui
}

fn chord(ctrl: bool, key: KeyBindingKey) -> KeyChord {
    KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl,
            ..KeyBindingModifiers::default()
        },
        key,
    }
}

fn ch(ctrl: bool, c: char) -> KeyChord {
    chord(ctrl, KeyBindingKey::Character(c))
}

fn select(ui: &mut KeyRemapUi, action: BindableAction) {
    let index = ACTIONS.iter().position(|a| *a == action).expect("listed");
    ui.set_selection(index);
}

/// The action rows of a render, as action indices, read back through
/// `row_at` so the click map and the painter are checked together.
fn rendered_actions(ui: &KeyRemapUi, height: usize) -> Vec<usize> {
    let lines = ui.visible_lines(WIDTH, height);
    (0..lines.len())
        .filter_map(|row| ui.row_at(row, height))
        .collect()
}

fn focused_is_rendered(ui: &KeyRemapUi, height: usize) -> bool {
    let lines = ui.visible_lines(WIDTH, height);
    lines.iter().any(|line| line.focused)
}

#[test]
fn keyboard_navigation_keeps_the_selected_row_on_screen() {
    let mut ui = ui();
    let height = 10;
    ui.visible_lines(WIDTH, height);
    for step in 0..40 {
        ui.handle_input(OverlayInput::Down);
        assert!(focused_is_rendered(&ui, height), "Down {step}");
        assert!(rendered_actions(&ui, height).contains(&ui.selected));
    }
    ui.handle_input(OverlayInput::End);
    assert!(focused_is_rendered(&ui, height), "End");
    assert_eq!(
        rendered_actions(&ui, height).last(),
        Some(&(ACTIONS.len() - 1))
    );
    ui.handle_input(OverlayInput::Home);
    assert!(focused_is_rendered(&ui, height), "Home");
    assert_eq!(rendered_actions(&ui, height).first(), Some(&0));
    ui.handle_input(OverlayInput::PageDown);
    assert!(focused_is_rendered(&ui, height), "PageDown");
}

#[test]
fn arming_capture_on_the_bottom_row_keeps_it_on_screen() {
    let mut ui = ui();
    let height = 8;
    ui.visible_lines(WIDTH, height);
    for _ in 0..20 {
        ui.handle_input(OverlayInput::Down);
        ui.visible_lines(WIDTH, height);
    }
    // Capture adds a message line above the list.
    ui.handle_input(OverlayInput::Activate);
    assert!(ui.is_capturing_chord());
    assert!(focused_is_rendered(&ui, height));
}

#[test]
fn scroll_arrows_match_the_rendered_window() {
    let mut ui = ui();
    let height = 10;
    for delta in [0, 3, 10, 30, 60, -5, -60] {
        ui.visible_lines(WIDTH, height);
        ui.scroll_lines(delta);
        let shown = rendered_actions(&ui, height);
        let (above, below) = ui.scroll_indicator(height);
        assert_eq!(
            above,
            shown.first().is_some_and(|first| *first > 0),
            "{delta}"
        );
        assert_eq!(
            below,
            shown.last().is_some_and(|last| *last + 1 < ACTIONS.len()),
            "after scrolling {delta}: rows {shown:?}"
        );
        assert!(
            focused_is_rendered(&ui, height),
            "the selection follows the wheel"
        );
    }
}

#[test]
fn messages_wrap_to_the_body_width_and_keep_their_key_hints() {
    let ui = ui();
    let width = 40;
    let lines = ui.visible_lines(width, 20);
    let message: Vec<&str> = lines
        .iter()
        .map(|line| line.text.as_str())
        .take_while(|text| !text.starts_with("> ") && !text.starts_with("  "))
        .collect();
    assert!(message.len() > 1, "the open hint wraps: {message:?}");
    assert!(message.iter().all(|line| line.chars().count() <= width));
    assert!(message.join(" ").ends_with("Esc closes."));
    // The first click below the message selects the first shown action.
    assert_eq!(ui.row_at(message.len() - 1, 20), None);
    assert_eq!(ui.row_at(message.len(), 20), Some(0));

    let mut dirty = ui.clone();
    dirty.handle_input(OverlayInput::Activate);
    dirty.deliver_chord(Some(ch(true, 'j')));
    dirty.handle_input(OverlayInput::Close);
    assert!(dirty.pending_close_prompt);
    let text: Vec<String> = dirty
        .visible_lines(30, 20)
        .into_iter()
        .map(|line| line.text)
        .take_while(|text| !text.starts_with("> ") && !text.starts_with("  "))
        .collect();
    assert!(
        text.join(" ").contains("[D] discard [C] keep editing")
            || text.join(" ").contains("[D] discard  [C] keep editing"),
        "{text:?}"
    );
    assert!(text.iter().all(|line| line.chars().count() <= 30));
}

#[test]
fn a_global_binding_never_drops_a_pane_override_on_the_same_chord() {
    for pane_first in [true, false] {
        let mut ui = ui();
        let shared = ch(true, 'q');
        let order = if pane_first {
            [BindableAction::ZoomPane, BindableAction::Copy]
        } else {
            [BindableAction::Copy, BindableAction::ZoomPane]
        };
        for action in order {
            select(&mut ui, action);
            ui.handle_input(OverlayInput::Activate);
            assert!(matches!(
                ui.deliver_chord(Some(shared)),
                KeyRemapOutcome::Preview(_)
            ));
        }
        for action in order {
            assert!(
                ui.overrides
                    .iter()
                    .any(|o| o.action == action && o.chord == shared),
                "{action:?} lost its override (pane first: {pane_first})"
            );
        }
    }
}

#[test]
fn confirming_a_pane_key_reassignment_shows_the_old_action_unbound() {
    let mut ui = ui();
    select(&mut ui, BindableAction::ZoomPane);
    ui.handle_input(OverlayInput::Activate);
    ui.deliver_chord(Some(ch(false, 'x')));
    assert!(ui.conflict.is_some(), "x belongs to Close Pane");
    let out = ui.deliver_chord(Some(chord(
        false,
        KeyBindingKey::Named(KeyBindingNamedKey::Enter),
    )));
    assert!(matches!(out, KeyRemapOutcome::Preview(_)));
    assert_eq!(
        ui.pane_action_chord_text(BindableAction::ClosePane),
        "(unbound)"
    );
    assert!(
        ui.pane_action_chord_text(BindableAction::ZoomPane)
            .ends_with(" then x")
    );
}

#[test]
fn a_global_action_refuses_an_unmodified_typing_key() {
    let mut ui = ui();
    select(&mut ui, BindableAction::Copy);
    ui.handle_input(OverlayInput::Activate);
    for refused in [
        ch(false, 'x'),
        chord(false, KeyBindingKey::Named(KeyBindingNamedKey::Space)),
        chord(false, KeyBindingKey::Named(KeyBindingNamedKey::Tab)),
        chord(false, KeyBindingKey::Named(KeyBindingNamedKey::Backspace)),
    ] {
        assert_eq!(ui.deliver_chord(Some(refused)), KeyRemapOutcome::Consumed);
        assert!(
            ui.is_capturing_chord(),
            "capture stays armed for {refused:?}"
        );
        assert!(ui.overrides.is_empty());
        assert!(
            ui.message
                .as_deref()
                .is_some_and(|m| m.contains("types text"))
        );
    }
    // A bare non-text key is still accepted.
    assert!(matches!(
        ui.deliver_chord(Some(chord(
            false,
            KeyBindingKey::Named(KeyBindingNamedKey::F(9))
        ))),
        KeyRemapOutcome::Preview(_)
    ));
    // Pane second keys stay bare by design.
    select(&mut ui, BindableAction::ZoomPane);
    ui.handle_input(OverlayInput::Activate);
    assert!(matches!(
        ui.deliver_chord(Some(ch(false, 'q'))),
        KeyRemapOutcome::Preview(_)
    ));
}
