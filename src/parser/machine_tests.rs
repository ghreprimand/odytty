// SPDX-License-Identifier: GPL-3.0-only
//! Component-level tests for [`super::machine`] - the byte classifier and the
//! state machine in isolation. Parser golden/self-consistency tests exercise the
//! broader corpus; these tests pin specific high-value cells so a component-
//! level regression points at the right module.

use super::action::Action;
use super::machine::{ByteClass, Machine, State, classify};

#[test]
fn classify_covers_canonical_byte_ranges() {
    assert_eq!(classify(0x00), ByteClass::C0Execute);
    assert_eq!(classify(0x06), ByteClass::C0Execute);
    assert_eq!(classify(0x07), ByteClass::C0Bel);
    assert_eq!(classify(0x08), ByteClass::C0Execute);
    assert_eq!(classify(0x17), ByteClass::C0Execute);
    assert_eq!(classify(0x18), ByteClass::Cancel);
    assert_eq!(classify(0x19), ByteClass::C0Execute);
    assert_eq!(classify(0x1A), ByteClass::Cancel);
    assert_eq!(classify(0x1B), ByteClass::Esc);
    assert_eq!(classify(0x1F), ByteClass::C0Execute);
    assert_eq!(classify(0x20), ByteClass::Intermediate);
    assert_eq!(classify(0x2F), ByteClass::Intermediate);
    assert_eq!(classify(0x30), ByteClass::Digit);
    assert_eq!(classify(0x39), ByteClass::Digit);
    assert_eq!(classify(0x3A), ByteClass::SubParamSep);
    assert_eq!(classify(0x3B), ByteClass::ParamSep);
    assert_eq!(classify(0x3C), ByteClass::ParamMarker);
    assert_eq!(classify(0x3F), ByteClass::ParamMarker);
    assert_eq!(classify(0x40), ByteClass::Final);
    assert_eq!(classify(0x7E), ByteClass::Final);
    assert_eq!(classify(0x7F), ByteClass::Del);
    assert_eq!(classify(0x80), ByteClass::Other);
    assert_eq!(classify(0x9C), ByteClass::StringTerm8);
    assert_eq!(classify(0xFF), ByteClass::Other);
}

#[test]
fn machine_starts_in_ground() {
    let m = Machine::new();
    assert_eq!(m.state, State::Ground);
}

#[test]
fn csi_param_digits_accumulate_and_dispatch() {
    let mut m = Machine::new();
    m.state = State::Escape;
    assert_eq!(m.step(b'['), Action::None);
    assert_eq!(m.state, State::CsiEntry);
    assert_eq!(m.step(b'5'), Action::None);
    assert_eq!(m.state, State::CsiParam);
    assert_eq!(m.step(b'A'), Action::CsiDispatch(b'A'));
    assert_eq!(m.state, State::Ground);
}

#[test]
fn osc_bel_ends_with_bell_true() {
    let mut m = Machine::new();
    m.state = State::OscString;
    assert_eq!(m.step(0x07), Action::OscEnd { bell: true });
    assert_eq!(m.state, State::Ground);
}

#[test]
fn osc_cancel_emits_cancel_then_execute() {
    let mut m = Machine::new();
    m.state = State::OscString;
    assert_eq!(m.step(0x18), Action::OscCancelExecute(0x18));
    assert_eq!(m.state, State::Ground);
}

#[test]
fn dcs_passthrough_keeps_a_raw_st8_byte_as_payload() {
    let mut m = Machine::new();
    m.state = State::DcsPassthrough;
    assert_eq!(m.step(b'a'), Action::DcsPut(b'a'));
    assert_eq!(m.step(0x9C), Action::DcsPut(0x9C));
    assert_eq!(m.state, State::DcsPassthrough);
    assert_eq!(m.step(0x1B), Action::DcsUnhook, "ESC still ends the string");
    assert_eq!(m.state, State::Escape);
}

#[test]
fn apc_payload_then_st_emits_apc_end() {
    let mut m = Machine::new();
    m.state = State::ApcString;
    assert_eq!(m.step(b'G'), Action::ApcPut(b'G'));
    assert_eq!(m.step(0x1B), Action::ApcEnd);
    assert_eq!(m.state, State::Escape);
}

#[test]
fn cancel_anywhere_executes_and_returns_ground() {
    let starting = [
        State::Escape,
        State::EscapeIntermediate,
        State::CsiEntry,
        State::CsiParam,
        State::CsiIntermediate,
        State::CsiIgnore,
        State::DcsEntry,
        State::DcsParam,
        State::DcsIntermediate,
        State::DcsIgnore,
    ];
    for s in starting {
        let mut m = Machine::new();
        m.state = s;
        let a = m.step(0x18);
        assert!(matches!(a, Action::Execute(0x18)), "state {s:?}");
        assert_eq!(m.state, State::Ground, "state {s:?}");
    }
}

/// One representative byte per [`ByteClass`], in the column order of
/// [`expected_row`].
const CLASS_BYTES: [(ByteClass, u8); 13] = [
    (ByteClass::C0Execute, 0x01),
    (ByteClass::C0Bel, 0x07),
    (ByteClass::Cancel, 0x18),
    (ByteClass::Esc, 0x1B),
    (ByteClass::Intermediate, 0x20),
    (ByteClass::Digit, b'5'),
    (ByteClass::SubParamSep, b':'),
    (ByteClass::ParamSep, b';'),
    (ByteClass::ParamMarker, b'?'),
    (ByteClass::Final, b'm'),
    (ByteClass::Del, 0x7F),
    (ByteClass::StringTerm8, 0x9C),
    (ByteClass::Other, 0x80),
];

/// The expected `(next state, action name)` for every byte class in `state`,
/// in [`CLASS_BYTES`] order. This is the canonical VT500-family transition
/// table with OdyTTY's choices: raw 0x9C is never a terminator, OSC/APC/DCS
/// strings end only with `ESC \` (or BEL for OSC), and CAN/SUB cancel any
/// sequence in flight.
fn expected_row(state: State) -> [(State, &'static str); 13] {
    use State as S;
    // Shorthands for the columns that behave the same in many states.
    let cancel = (S::Ground, "Execute");
    let to_escape = (S::Escape, "None");
    match state {
        S::Escape => [
            (S::Escape, "Execute"),
            (S::Escape, "Execute"),
            cancel,
            to_escape,
            (S::EscapeIntermediate, "None"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::Escape, "None"),
            (S::Escape, "None"),
            (S::Escape, "None"),
        ],
        S::EscapeIntermediate => [
            (S::EscapeIntermediate, "Execute"),
            (S::EscapeIntermediate, "Execute"),
            cancel,
            to_escape,
            (S::EscapeIntermediate, "None"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::Ground, "EscDispatch"),
            (S::EscapeIntermediate, "None"),
            (S::EscapeIntermediate, "None"),
            (S::EscapeIntermediate, "None"),
        ],
        S::CsiEntry => [
            (S::CsiEntry, "Execute"),
            (S::CsiEntry, "Execute"),
            cancel,
            to_escape,
            (S::CsiIntermediate, "None"),
            (S::CsiParam, "None"),
            (S::CsiParam, "None"),
            (S::CsiParam, "None"),
            (S::CsiParam, "None"),
            (S::Ground, "CsiDispatch"),
            (S::CsiEntry, "None"),
            (S::CsiEntry, "None"),
            (S::CsiEntry, "None"),
        ],
        S::CsiParam => [
            (S::CsiParam, "Execute"),
            (S::CsiParam, "Execute"),
            cancel,
            to_escape,
            (S::CsiIntermediate, "None"),
            (S::CsiParam, "None"),
            (S::CsiParam, "None"),
            (S::CsiParam, "None"),
            (S::CsiIgnore, "None"),
            (S::Ground, "CsiDispatch"),
            (S::CsiParam, "None"),
            (S::CsiParam, "None"),
            (S::CsiParam, "None"),
        ],
        S::CsiIntermediate => [
            (S::CsiIntermediate, "Execute"),
            (S::CsiIntermediate, "Execute"),
            cancel,
            to_escape,
            (S::CsiIntermediate, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::Ground, "CsiDispatch"),
            (S::CsiIntermediate, "None"),
            (S::CsiIntermediate, "None"),
            (S::CsiIntermediate, "None"),
        ],
        S::CsiIgnore => [
            (S::CsiIgnore, "Execute"),
            (S::CsiIgnore, "Execute"),
            cancel,
            to_escape,
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::Ground, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
            (S::CsiIgnore, "None"),
        ],
        S::DcsEntry => [
            (S::DcsEntry, "None"),
            (S::DcsEntry, "None"),
            cancel,
            to_escape,
            (S::DcsIntermediate, "None"),
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
            (S::DcsPassthrough, "DcsHook"),
            (S::DcsEntry, "None"),
            (S::DcsEntry, "None"),
            (S::DcsEntry, "None"),
        ],
        S::DcsParam => [
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
            cancel,
            to_escape,
            (S::DcsIntermediate, "None"),
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsPassthrough, "DcsHook"),
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
            (S::DcsParam, "None"),
        ],
        S::DcsIntermediate => [
            (S::DcsIntermediate, "None"),
            (S::DcsIntermediate, "None"),
            cancel,
            to_escape,
            (S::DcsIntermediate, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsPassthrough, "DcsHook"),
            (S::DcsIntermediate, "None"),
            (S::DcsIntermediate, "None"),
            (S::DcsIntermediate, "None"),
        ],
        S::DcsPassthrough => [
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "DcsPut"),
            (S::Ground, "DcsCancelExecute"),
            (S::Escape, "DcsUnhook"),
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "None"),
            (S::DcsPassthrough, "DcsPut"),
            (S::DcsPassthrough, "DcsPut"),
        ],
        S::DcsIgnore => [
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            cancel,
            to_escape,
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
            (S::DcsIgnore, "None"),
        ],
        S::OscString => [
            (S::OscString, "None"),
            (S::Ground, "OscEnd"),
            (S::Ground, "OscCancelExecute"),
            (S::Escape, "OscEnd"),
            (S::OscString, "OscPut"),
            (S::OscString, "OscPut"),
            (S::OscString, "OscPut"),
            (S::OscString, "OscParamBoundary"),
            (S::OscString, "OscPut"),
            (S::OscString, "OscPut"),
            (S::OscString, "OscPut"),
            (S::OscString, "OscPut"),
            (S::OscString, "OscPut"),
        ],
        S::ApcString => [
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            cancel,
            (S::Escape, "ApcEnd"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
            (S::ApcString, "ApcPut"),
        ],
        S::DiscardString => [
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            cancel,
            to_escape,
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
            (S::DiscardString, "None"),
        ],
        S::Ground => unreachable!("Ground is handled by the segmenter, not the table"),
    }
}

/// The variant name of an action, without its payload.
fn action_name(action: &Action) -> String {
    format!("{action:?}")
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

/// Every non-Ground state times every byte class lands in the expected next
/// state with the expected action, so a changed cell shows up here by name.
#[test]
fn every_state_and_class_pair_follows_the_transition_matrix() {
    let states = [
        State::Escape,
        State::EscapeIntermediate,
        State::CsiEntry,
        State::CsiParam,
        State::CsiIntermediate,
        State::CsiIgnore,
        State::DcsEntry,
        State::DcsParam,
        State::DcsIntermediate,
        State::DcsPassthrough,
        State::DcsIgnore,
        State::OscString,
        State::ApcString,
        State::DiscardString,
    ];
    for state in states {
        let row = expected_row(state);
        for (index, (class, byte)) in CLASS_BYTES.into_iter().enumerate() {
            assert_eq!(classify(byte), class, "fixture byte {byte:#04x}");
            let mut machine = Machine::new();
            machine.state = state;
            let action = machine.step(byte);
            let (next, name) = row[index];
            assert_eq!(
                (machine.state, action_name(&action).as_str()),
                (next, name),
                "state {state:?}, class {class:?}"
            );
        }
    }
}

/// ESC inside a sequence restarts it: the new escape starts clean, so a stale
/// parameter or intermediate from the abandoned sequence cannot leak.
#[test]
fn esc_reentry_starts_a_fresh_escape_sequence() {
    let mut machine = Machine::new();
    machine.state = State::Escape;
    for byte in *b"[1;2\x1b" {
        machine.step(byte);
    }
    assert_eq!(machine.state, State::Escape);
    assert_eq!(machine.step(b'['), Action::None);
    assert_eq!(machine.step(b'H'), Action::CsiDispatch(b'H'));
    assert_eq!(machine.state, State::Ground);
    assert!(
        !machine
            .params
            .iter()
            .any(|group| group.contains(&1) || group.contains(&2)),
        "the abandoned parameters are gone"
    );

    // ESC ESC stays in Escape and still dispatches the next final.
    let mut machine = Machine::new();
    machine.state = State::Escape;
    assert_eq!(machine.step(0x1B), Action::None);
    assert_eq!(machine.state, State::Escape);
    assert_eq!(machine.step(b'7'), Action::EscDispatch(b'7'));
}
