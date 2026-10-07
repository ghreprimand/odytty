// SPDX-License-Identifier: GPL-3.0-only
//! Pane-prefix table and engine tests: shadowed overrides, the timeout
//! boundary, shifted prefixes, and prefix passthrough bytes.

use super::*;

fn over(chord: KeyChord, action: BindableAction) -> KeyBindingOverride {
    KeyBindingOverride { chord, action }
}

fn key(ch: char) -> KeyChord {
    char_chord(ch, false, false, false, false)
}

fn shifted(ch: char) -> KeyChord {
    char_chord(ch, false, true, false, false)
}

fn engine(prefix: KeyChord, overrides: &[KeyBindingOverride]) -> PrefixEngine {
    PrefixEngine::new(Some(prefix), overrides, Duration::from_millis(1000))
}

fn ctrl_b() -> KeyChord {
    char_chord('b', true, false, false, false)
}

#[test]
fn a_shifted_override_shadows_the_default_on_the_same_key_everywhere() {
    // Close Pane moved to Shift+o: the engine folds Shift, so `o` now closes
    // the pane and Focus Next Pane can no longer fire.
    let overrides = [over(shifted('o'), BindableAction::ClosePane)];
    let mut engine = engine(ctrl_b(), &overrides);
    let t0 = Instant::now();
    assert_eq!(engine.on_chord(ctrl_b(), t0), PrefixOutcome::Entered);
    assert_eq!(
        engine.on_chord(key('o'), t0),
        PrefixOutcome::Action(BindableAction::ClosePane)
    );
    assert_eq!(engine.chord_for_action(BindableAction::FocusPaneNext), None);

    let view = PanePrefixBindings::from_overrides(&overrides);
    assert_eq!(
        view.chord_for_action(BindableAction::FocusPaneNext),
        None,
        "the shadowed action reads as unbound"
    );
    assert_eq!(
        view.action_for_chord(key('o')),
        Some(BindableAction::ClosePane)
    );
    assert_eq!(
        view.action_for_chord(shifted('o')),
        Some(BindableAction::ClosePane)
    );
    assert_eq!(
        view.chord_for_action(BindableAction::ClosePane),
        Some(shifted('o')),
        "the display keeps the chord as authored"
    );
}

#[test]
fn two_pane_overrides_on_one_folded_key_keep_only_the_later() {
    let overrides = [
        over(key('q'), BindableAction::ZoomPane),
        over(shifted('q'), BindableAction::ClosePane),
    ];
    let view = PanePrefixBindings::from_overrides(&overrides);
    assert_eq!(view.chord_for_action(BindableAction::ZoomPane), None);
    assert_eq!(
        view.action_for_chord(key('q')),
        Some(BindableAction::ClosePane)
    );
    let engine = engine(ctrl_b(), &overrides);
    assert_eq!(engine.chord_for_action(BindableAction::ZoomPane), None);
}

#[test]
fn reassigning_a_default_pane_key_unbinds_the_previous_action() {
    // Prefix `x` moved from Close Pane to Zoom Pane.
    let overrides = [over(key('x'), BindableAction::ZoomPane)];
    let view = PanePrefixBindings::from_overrides(&overrides);
    assert_eq!(view.chord_for_action(BindableAction::ClosePane), None);
    assert_eq!(
        view.chord_for_action(BindableAction::ZoomPane),
        Some(key('x'))
    );
    let engine = engine(ctrl_b(), &overrides);
    assert_eq!(engine.chord_for_action(BindableAction::ClosePane), None);
}

#[test]
fn a_second_key_at_the_exact_deadline_is_fresh_input_on_both_paths() {
    let t0 = Instant::now();
    let mut without_timer = engine(ctrl_b(), &[]);
    without_timer.on_chord(ctrl_b(), t0);
    let deadline = without_timer.pending_deadline().expect("pending");
    assert_eq!(
        without_timer.on_chord(key('x'), deadline),
        PrefixOutcome::Inactive,
        "a key at the deadline does not close the pane"
    );

    let mut with_timer = engine(ctrl_b(), &[]);
    with_timer.on_chord(ctrl_b(), t0);
    assert!(with_timer.expire_pending(deadline), "the timer expires it");
    assert_eq!(
        with_timer.on_chord(key('x'), deadline),
        PrefixOutcome::Inactive
    );
}

#[test]
fn a_shifted_prefix_matches_its_produced_or_base_character() {
    let prefix = char_chord('5', true, true, false, false);
    let produced = char_chord('%', true, true, false, false);
    let t0 = Instant::now();
    let mut engine = engine(prefix, &[]);
    assert_eq!(
        engine.on_key(produced, Some(prefix), t0),
        PrefixOutcome::Entered,
        "Ctrl+Shift+5 reported as % still enters the prefix"
    );
    assert_eq!(
        engine.on_key(produced, Some(prefix), t0),
        PrefixOutcome::Passthrough,
        "doubling the shifted prefix passes it through"
    );
    // A produced `%` second key still splits, as before.
    assert_eq!(
        engine.on_key(produced, Some(prefix), t0),
        PrefixOutcome::Entered
    );
    assert_eq!(
        engine.on_key(char_chord('%', false, true, false, false), None, t0),
        PrefixOutcome::Action(BindableAction::SplitColumns)
    );
}

#[test]
fn ctrl_space_prefix_passes_through_as_nul() {
    let prefix = named_chord(KeyBindingNamedKey::Space, true, false, false, false);
    assert_eq!(engine(prefix, &[]).passthrough_bytes(), vec![0x00]);
    assert_eq!(engine(ctrl_b(), &[]).passthrough_bytes(), vec![0x02]);
    let f5 = named_chord(KeyBindingNamedKey::F(5), true, false, false, false);
    assert!(
        engine(f5, &[]).passthrough_bytes().is_empty(),
        "a prefix with no single-byte literal sends nothing"
    );
}

#[test]
fn default_global_bindings_never_take_a_typing_key() {
    for (chord, action) in default_key_bindings() {
        assert!(
            !chord.is_unmodified_typing_key(),
            "{action:?} defaults to an unmodified typing key {chord:?}"
        );
    }
}
