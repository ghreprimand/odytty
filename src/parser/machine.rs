//! Layer-2 control state machine — byte-class–driven transitions.
//!
//! After Layer 1 (the [`super::segmenter`]) has lifted UTF-8 text and
//! C1-via-UTF-8 executes out of the byte stream, what remains is a clean
//! 8-bit-clean control automaton. This module is that automaton.
//!
//! ## Design (PA2-r originality)
//!
//! Each input byte is first mapped to one of the ~13 [`ByteClass`] values via
//! the small [`classify`] table; the [`Machine`] then advances state and emits
//! a single [`Action`] from one flat `match (state, class)` discriminator. This
//! shape differs structurally from the canonical references:
//!
//! - It is NOT the per-state-method decomposition of inline-callback parsers
//!   (each state a separate `advance_<state>` function with its own per-byte
//!   match) — the first-generation OdyTTY core followed that shape and the
//!   operator ruled it too derivative. Here, transitions are one flat table.
//! - It is NOT the Williams `[state][256]` data table either. Our match has
//!   ~13 byte-class columns, not 256; the action vocabulary is OdyTTY-designed
//!   ([`Action`]); and the state machine is byte-only — UTF-8 lives in Layer 1.
//! - The "anywhere edges" (ECMA-48: a control byte that means the same thing
//!   regardless of current state) are encoded inline at each state's
//!   `Cancel` / `Esc` arms rather than as separate pre-match shortcuts; the
//!   flat match is the single source of truth, and the compiler lowers it to a
//!   jump table.
//!
//! ## What lives where
//!
//! The machine owns the per-sequence bookkeeping a CSI/DCS dispatch needs to
//! materialise: [`Params`], the running param-digit accumulator, the
//! intermediate-byte buffer, and the `ignoring` flag (set on parameter or
//! intermediate overflow). It does NOT own OSC/APC payload buffers — those are
//! the driver's because their caps and lifecycle differ. Layer 1 (the
//! segmenter) owns the UTF-8 partial-codepoint carry.
//!
//! ## C1-via-UTF-8 policy
//!
//! Per the operator-approved divergence ledger, a validly-decoded C1 scalar
//! (`U+0080..=U+009F` via `0xC2 0x8x`) **executes uniformly**, regardless of
//! how its bytes are split across `advance()` calls. This is enforced in Layer
//! 1 (the segmenter), not here — by the time bytes reach this machine, they
//! are already in a non-Ground control state, and raw `0x80..=0x9F` bytes here
//! are NEVER C1 sequence introducers (8-bit C1 introduction is not supported;
//! see [`super`] module docs and the divergence ledger in `mod.rs`).

use super::action::Action;
use super::params::Params;

/// Maximum collected intermediate bytes before a sequence is flagged `ignore`.
pub(crate) const MAX_INTERMEDIATES: usize = 2;

/// Canonical DEC ANSI parser states, minus Ground (handled by Layer 1).
///
/// Two extras: `ApcString` (the OdyTTY-surfaced APC payload state) and
/// `DiscardString` (SOS and PM; canonical "ignore until terminator"). Splitting
/// the canonical `SosPmApc` state along the APC/non-APC boundary keeps the
/// driver from threading an `apc_active` bool through every transition.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// Printable text + C0/C1 controls. Handled by Layer 1; never reached here.
    #[default]
    Ground,
    /// Saw `ESC`; awaiting the sequence type.
    Escape,
    /// `ESC` + intermediate byte(s); awaiting the final byte.
    EscapeIntermediate,
    /// `ESC [`; first byte of a CSI sequence.
    CsiEntry,
    /// Collecting CSI parameter digits / separators.
    CsiParam,
    /// Collecting CSI intermediate bytes.
    CsiIntermediate,
    /// A malformed CSI; consume to the final byte then drop.
    CsiIgnore,
    /// `ESC P`; first byte of a DCS sequence.
    DcsEntry,
    /// Collecting DCS parameter digits / separators.
    DcsParam,
    /// Collecting DCS intermediate bytes.
    DcsIntermediate,
    /// DCS payload; bytes flow to `put` until terminated.
    DcsPassthrough,
    /// A malformed DCS; consume to the terminator then drop.
    DcsIgnore,
    /// OSC payload accumulation (driver owns the buffer).
    OscString,
    /// APC payload accumulation (driver owns the buffer).
    ApcString,
    /// SOS / PM: payload silently discarded until terminator.
    DiscardString,
}

/// One byte's compact classification for the state-transition match. The
/// alphabet here is OdyTTY's chosen vocabulary; each variant corresponds to a
/// distinct cell in the canonical transition table that the control machine
/// treats uniformly across multiple states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteClass {
    /// C0 controls that execute (0x00..=0x06, 0x08..=0x17, 0x19, 0x1C..=0x1F).
    C0Execute,
    /// BEL (0x07) — separated because OSC treats it as a string terminator.
    C0Bel,
    /// Cancel/Substitute (0x18, 0x1A) — abort any in-flight sequence.
    Cancel,
    /// Escape (0x1B) — begins a new escape sequence (anywhere).
    Esc,
    /// Intermediate / collect range (0x20..=0x2F).
    Intermediate,
    /// ASCII digit (0x30..=0x39) — parameter accumulator.
    Digit,
    /// `:` — colon subparameter separator (0x3A).
    SubParamSep,
    /// `;` — semicolon parameter separator (0x3B).
    ParamSep,
    /// CSI parameter marker / private-mode introducer (`<=>?` — 0x3C..=0x3F).
    ParamMarker,
    /// Final / dispatch range (0x40..=0x7E).
    Final,
    /// DEL (0x7F).
    Del,
    /// 8-bit ST (0x9C). Acts as a string terminator in DCS passthrough; in
    /// other states it is treated as an inert byte (raw C1 introducers are
    /// disabled — see `super` module docs).
    StringTerm8,
    /// Anything else (other 0x80..=0xFF bytes that aren't 0x9C).
    Other,
}

/// Map a raw byte to its [`ByteClass`].
#[inline]
pub(crate) fn classify(byte: u8) -> ByteClass {
    match byte {
        0x07 => ByteClass::C0Bel,
        0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1C..=0x1F => ByteClass::C0Execute,
        0x18 | 0x1A => ByteClass::Cancel,
        0x1B => ByteClass::Esc,
        0x20..=0x2F => ByteClass::Intermediate,
        0x30..=0x39 => ByteClass::Digit,
        0x3A => ByteClass::SubParamSep,
        0x3B => ByteClass::ParamSep,
        0x3C..=0x3F => ByteClass::ParamMarker,
        0x40..=0x7E => ByteClass::Final,
        0x7F => ByteClass::Del,
        0x9C => ByteClass::StringTerm8,
        _ => ByteClass::Other,
    }
}

/// The control state machine. Holds per-sequence bookkeeping the driver needs
/// to assemble a CSI/DCS dispatch.
#[derive(Debug, Clone)]
pub(crate) struct Machine {
    pub(crate) state: State,
    pub(crate) params: Params,
    /// Running digit accumulator; pushed/extended on `;` / `:` / final byte.
    pub(crate) param_accum: u16,
    pub(crate) intermediates: [u8; MAX_INTERMEDIATES],
    pub(crate) intermediate_idx: u8,
    /// Set when a parameter or intermediate cap overflowed; the sequence
    /// dispatches with `ignore=true`.
    pub(crate) ignoring: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            params: Params::new(),
            param_accum: 0,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_idx: 0,
            ignoring: false,
        }
    }

    /// Borrow the active intermediate bytes for dispatch.
    pub(crate) fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.intermediate_idx as usize]
    }

    /// Reset per-sequence state. Called when entering a new escape sequence.
    pub(crate) fn reset_seq(&mut self) {
        self.params.clear();
        self.param_accum = 0;
        self.intermediate_idx = 0;
        self.ignoring = false;
    }

    #[inline]
    fn collect(&mut self, byte: u8) {
        if (self.intermediate_idx as usize) == MAX_INTERMEDIATES {
            self.ignoring = true;
        } else {
            self.intermediates[self.intermediate_idx as usize] = byte;
            self.intermediate_idx += 1;
        }
    }

    /// `;` — finalise current param accumulator and open a new param.
    #[inline]
    fn param_sep(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param_accum);
            self.param_accum = 0;
        }
    }

    /// `:` — extend current param with another subparam.
    #[inline]
    fn subparam_sep(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.extend(self.param_accum);
            self.param_accum = 0;
        }
    }

    /// Digit — accumulate into the running param value (saturating).
    #[inline]
    fn param_digit(&mut self, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.param_accum = self.param_accum.saturating_mul(10);
            self.param_accum = self.param_accum.saturating_add((byte - b'0') as u16);
        }
    }

    /// Finalise the accumulator with one last push, used at every sequence's
    /// final byte. Mirrors the canonical parser's terminating push.
    #[inline]
    fn finish_params(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param_accum);
        }
    }

    /// Advance one control byte. Updates internal state and returns the
    /// observable side effect, if any.
    ///
    /// The compiler will not inline the full `(state, class)` match (it has
    /// >100 arms), but heavy CSI workloads spend most of their byte budget in
    /// the `CsiParam` state running digits/separators with a `None` outcome —
    /// so this wrapper peels that path off into a tight inlineable shape and
    /// falls through to [`Self::step_cold`] for everything else.
    #[inline]
    pub(crate) fn step(&mut self, byte: u8) -> Action {
        if self.state == State::CsiParam {
            match byte {
                0x30..=0x39 => {
                    self.param_digit(byte);
                    return Action::None;
                }
                0x3B => {
                    self.param_sep();
                    return Action::None;
                }
                0x3A => {
                    self.subparam_sep();
                    return Action::None;
                }
                0x40..=0x7E => {
                    self.finish_params();
                    self.state = State::Ground;
                    return Action::CsiDispatch(byte);
                }
                _ => {}
            }
        }
        self.step_cold(byte)
    }

    /// Cold-path transition table. The full byte-class state-machine match.
    fn step_cold(&mut self, byte: u8) -> Action {
        use ByteClass as C;
        use State as S;

        let class = classify(byte);

        match (self.state, class) {
            // ---------------- Escape (after raw ESC) ----------------
            (S::Escape, C::C0Bel | C::C0Execute) => Action::Execute(byte),
            (S::Escape, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::Escape, C::Esc) => {
                self.reset_seq();
                Action::None
            }
            (S::Escape, C::Intermediate) => {
                self.collect(byte);
                self.state = S::EscapeIntermediate;
                Action::None
            }
            // 0x30..=0x3F in Escape all dispatch as ESC final.
            (S::Escape, C::Digit | C::SubParamSep | C::ParamSep | C::ParamMarker) => {
                self.state = S::Ground;
                Action::EscDispatch(byte)
            }
            (S::Escape, C::Final) => {
                // 0x40..=0x7E: a few are sequence-introducers; rest dispatch.
                match byte {
                    0x50 => {
                        self.state = S::DcsEntry;
                        Action::None
                    }
                    0x58 => {
                        self.state = S::DiscardString;
                        Action::None
                    }
                    0x5B => {
                        self.state = S::CsiEntry;
                        Action::None
                    }
                    0x5D => {
                        self.state = S::OscString;
                        Action::None
                    }
                    0x5E => {
                        self.state = S::DiscardString;
                        Action::None
                    }
                    0x5F => {
                        self.state = S::ApcString;
                        Action::None
                    }
                    // 0x5C (`\`) is the trailing half of a 7-bit ST — dispatch
                    // as a normal ESC final (canonical), with state → Ground.
                    _ => {
                        self.state = S::Ground;
                        Action::EscDispatch(byte)
                    }
                }
            }
            (S::Escape, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- EscapeIntermediate ----------------
            (S::EscapeIntermediate, C::C0Bel | C::C0Execute) => Action::Execute(byte),
            (S::EscapeIntermediate, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::EscapeIntermediate, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::EscapeIntermediate, C::Intermediate) => {
                self.collect(byte);
                Action::None
            }
            (
                S::EscapeIntermediate,
                C::Digit | C::SubParamSep | C::ParamSep | C::ParamMarker | C::Final,
            ) => {
                self.state = S::Ground;
                Action::EscDispatch(byte)
            }
            (S::EscapeIntermediate, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- CsiEntry ----------------
            (S::CsiEntry, C::C0Bel | C::C0Execute) => Action::Execute(byte),
            (S::CsiEntry, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::CsiEntry, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::CsiEntry, C::Intermediate) => {
                self.collect(byte);
                self.state = S::CsiIntermediate;
                Action::None
            }
            (S::CsiEntry, C::Digit) => {
                self.param_digit(byte);
                self.state = S::CsiParam;
                Action::None
            }
            (S::CsiEntry, C::SubParamSep) => {
                self.subparam_sep();
                self.state = S::CsiParam;
                Action::None
            }
            (S::CsiEntry, C::ParamSep) => {
                self.param_sep();
                self.state = S::CsiParam;
                Action::None
            }
            (S::CsiEntry, C::ParamMarker) => {
                self.collect(byte);
                self.state = S::CsiParam;
                Action::None
            }
            (S::CsiEntry, C::Final) => {
                self.finish_params();
                self.state = S::Ground;
                Action::CsiDispatch(byte)
            }
            (S::CsiEntry, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- CsiParam ----------------
            (S::CsiParam, C::C0Bel | C::C0Execute) => Action::Execute(byte),
            (S::CsiParam, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::CsiParam, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::CsiParam, C::Intermediate) => {
                self.collect(byte);
                self.state = S::CsiIntermediate;
                Action::None
            }
            (S::CsiParam, C::Digit) => {
                self.param_digit(byte);
                Action::None
            }
            (S::CsiParam, C::SubParamSep) => {
                self.subparam_sep();
                Action::None
            }
            (S::CsiParam, C::ParamSep) => {
                self.param_sep();
                Action::None
            }
            (S::CsiParam, C::ParamMarker) => {
                // Per canonical: a ParamMarker once inside CsiParam → ignore.
                self.state = S::CsiIgnore;
                Action::None
            }
            (S::CsiParam, C::Final) => {
                self.finish_params();
                self.state = S::Ground;
                Action::CsiDispatch(byte)
            }
            (S::CsiParam, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- CsiIntermediate ----------------
            (S::CsiIntermediate, C::C0Bel | C::C0Execute) => Action::Execute(byte),
            (S::CsiIntermediate, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::CsiIntermediate, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::CsiIntermediate, C::Intermediate) => {
                self.collect(byte);
                Action::None
            }
            (S::CsiIntermediate, C::Digit | C::SubParamSep | C::ParamSep | C::ParamMarker) => {
                // Any param-range byte inside CsiIntermediate → ignore.
                self.state = S::CsiIgnore;
                Action::None
            }
            (S::CsiIntermediate, C::Final) => {
                self.finish_params();
                self.state = S::Ground;
                Action::CsiDispatch(byte)
            }
            (S::CsiIntermediate, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- CsiIgnore ----------------
            (S::CsiIgnore, C::C0Bel | C::C0Execute) => Action::Execute(byte),
            (S::CsiIgnore, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::CsiIgnore, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::CsiIgnore, C::Final) => {
                // 0x40..=0x7E in CsiIgnore: consume the final byte and drop.
                self.state = S::Ground;
                Action::None
            }
            (S::CsiIgnore, _) => Action::None,

            // ---------------- DcsEntry ----------------
            (S::DcsEntry, C::C0Bel | C::C0Execute) => Action::None,
            (S::DcsEntry, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::DcsEntry, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::DcsEntry, C::Intermediate) => {
                self.collect(byte);
                self.state = S::DcsIntermediate;
                Action::None
            }
            (S::DcsEntry, C::Digit) => {
                self.param_digit(byte);
                self.state = S::DcsParam;
                Action::None
            }
            (S::DcsEntry, C::SubParamSep) => {
                self.subparam_sep();
                self.state = S::DcsParam;
                Action::None
            }
            (S::DcsEntry, C::ParamSep) => {
                self.param_sep();
                self.state = S::DcsParam;
                Action::None
            }
            (S::DcsEntry, C::ParamMarker) => {
                self.collect(byte);
                self.state = S::DcsParam;
                Action::None
            }
            (S::DcsEntry, C::Final) => {
                self.finish_params();
                self.state = S::DcsPassthrough;
                Action::DcsHook(byte)
            }
            (S::DcsEntry, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- DcsParam ----------------
            (S::DcsParam, C::C0Bel | C::C0Execute) => Action::None,
            (S::DcsParam, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::DcsParam, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::DcsParam, C::Intermediate) => {
                self.collect(byte);
                self.state = S::DcsIntermediate;
                Action::None
            }
            (S::DcsParam, C::Digit) => {
                self.param_digit(byte);
                Action::None
            }
            (S::DcsParam, C::SubParamSep) => {
                self.subparam_sep();
                Action::None
            }
            (S::DcsParam, C::ParamSep) => {
                self.param_sep();
                Action::None
            }
            (S::DcsParam, C::ParamMarker) => {
                self.state = S::DcsIgnore;
                Action::None
            }
            (S::DcsParam, C::Final) => {
                self.finish_params();
                self.state = S::DcsPassthrough;
                Action::DcsHook(byte)
            }
            (S::DcsParam, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- DcsIntermediate ----------------
            (S::DcsIntermediate, C::C0Bel | C::C0Execute) => Action::None,
            (S::DcsIntermediate, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::DcsIntermediate, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::DcsIntermediate, C::Intermediate) => {
                self.collect(byte);
                Action::None
            }
            (S::DcsIntermediate, C::Digit | C::SubParamSep | C::ParamSep | C::ParamMarker) => {
                self.state = S::DcsIgnore;
                Action::None
            }
            (S::DcsIntermediate, C::Final) => {
                self.finish_params();
                self.state = S::DcsPassthrough;
                Action::DcsHook(byte)
            }
            (S::DcsIntermediate, C::Del | C::StringTerm8 | C::Other) => Action::None,

            // ---------------- DcsPassthrough ----------------
            (S::DcsPassthrough, C::Cancel) => {
                self.state = S::Ground;
                Action::DcsUnhookExecute(byte)
            }
            (S::DcsPassthrough, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::DcsUnhook
            }
            (S::DcsPassthrough, C::StringTerm8) => {
                self.state = S::Ground;
                Action::DcsUnhook
            }
            (S::DcsPassthrough, C::Del) => Action::None,
            // All other bytes (C0, BEL, Intermediate, Digit, SubParamSep,
            // ParamSep, ParamMarker, Final, Other) stream through put.
            (S::DcsPassthrough, _) => Action::DcsPut(byte),

            // ---------------- DcsIgnore ----------------
            (S::DcsIgnore, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::DcsIgnore, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::DcsIgnore, _) => Action::None,

            // ---------------- OscString ----------------
            (S::OscString, C::C0Bel) => {
                self.state = S::Ground;
                Action::OscEnd { bell: true }
            }
            (S::OscString, C::Cancel) => {
                self.state = S::Ground;
                Action::OscEndExecute { bell: false, byte }
            }
            (S::OscString, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::OscEnd { bell: false }
            }
            (S::OscString, C::ParamSep) => Action::OscParamBoundary,
            // C0Execute silently ignored (canonical).
            (S::OscString, C::C0Execute) => Action::None,
            // Everything else (Intermediate, Digit, SubParamSep, ParamMarker,
            // Final, Del, StringTerm8, Other) is OSC payload — raw 0x9C is
            // treated as OSC payload not terminator (matches the differential
            // oracle behavior; xterm UTF-8 mode disables the 8-bit ST edge in
            // the OSC state).
            (S::OscString, _) => Action::OscPut(byte),

            // ---------------- ApcString ----------------
            (S::ApcString, C::Cancel) => {
                self.state = S::Ground;
                // Driver clears the APC buffer on the state transition out of
                // ApcString; the cancel byte itself still executes.
                Action::Execute(byte)
            }
            (S::ApcString, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::ApcEnd
            }
            // Every other byte is APC payload.
            (S::ApcString, _) => Action::ApcPut(byte),

            // ---------------- DiscardString (SOS / PM) ----------------
            (S::DiscardString, C::Cancel) => {
                self.state = S::Ground;
                Action::Execute(byte)
            }
            (S::DiscardString, C::Esc) => {
                self.reset_seq();
                self.state = S::Escape;
                Action::None
            }
            (S::DiscardString, _) => Action::None,

            // Ground is handled by Layer 1 — never reached here.
            (S::Ground, _) => Action::None,
        }
    }
}
