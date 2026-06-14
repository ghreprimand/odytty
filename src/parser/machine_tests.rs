// SPDX-License-Identifier: GPL-3.0-only
//! Component-level tests for [`super::machine`] — the byte classifier and the
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
fn osc_cancel_emits_end_then_execute() {
    let mut m = Machine::new();
    m.state = State::OscString;
    assert_eq!(
        m.step(0x18),
        Action::OscEndExecute {
            bell: false,
            byte: 0x18
        }
    );
    assert_eq!(m.state, State::Ground);
}

#[test]
fn dcs_passthrough_unhook_on_st8() {
    let mut m = Machine::new();
    m.state = State::DcsPassthrough;
    assert_eq!(m.step(b'a'), Action::DcsPut(b'a'));
    assert_eq!(m.step(0x9C), Action::DcsUnhook);
    assert_eq!(m.state, State::Ground);
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
