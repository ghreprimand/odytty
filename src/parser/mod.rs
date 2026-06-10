//! OdyTTY-owned VT parser (Stage 4.5 Foundation Ownership).
//!
//! This module is the OdyTTY-owned replacement for the `vte` parser dependency.
//! It contains:
//!
//! - [`Params`] — an owned CSI/DCS parameter container mirroring the shape the
//!   terminal core consumes.
//! - [`VtDispatch`] — the OdyTTY-owned dispatch trait the parser drives,
//!   mirroring the callback shape the core already implements for `vte`, plus a
//!   first-class [`VtDispatch::apc_dispatch`] hook (APC is the capability `vte`
//!   never surfaces and the reason OdyTTY owns its byte path).
//! - [`OdyParser`] — the DEC ANSI escape-sequence state machine.
//!
//! ## Migration status
//!
//! During PA1 the parser ships **dark**: `vte` remains the live production
//! parser driving [`crate::core::Terminal`], and `OdyParser` is exercised only
//! by a differential oracle that feeds identical byte streams to both parsers
//! against cloned [`crate::core::Screen`]s and asserts byte-identical terminal
//! state. Later Foundation-Ownership packets harden the edge cases (PA2) and
//! retire `vte` entirely (PA3).

mod params;
mod state;
mod utf8;

#[cfg(test)]
mod params_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod utf8_tests;

pub use params::{Params, ParamsIter};
pub use state::OdyParser;

/// The OdyTTY-owned terminal dispatch trait, driven by [`OdyParser`].
///
/// Each method corresponds to an action in the canonical DEC ANSI parser state
/// diagram (<https://vt100.net/emu/dec_ansi_parser>). The shape deliberately
/// mirrors `vte::Perform` — so the core's existing dispatch logic transfers
/// near-verbatim — with one addition: [`Self::apc_dispatch`], which surfaces
/// Application Program Command payloads that `vte` discards.
///
/// All methods default to no-ops, so an implementor overrides only the actions
/// it cares about (the terminal core ignores DCS and APC today; those are wired
/// up in later packets).
pub trait VtDispatch {
    /// Draw a character to the screen.
    fn print(&mut self, _c: char) {}

    /// Execute a C0 or C1 control function.
    fn execute(&mut self, _byte: u8) {}

    /// A final byte arrived for a CSI sequence.
    ///
    /// `params` are the numeric parameters, `intermediates` the collected
    /// intermediate bytes, `ignore` is set when a limit overflowed (parameters
    /// or intermediates) and trailing bytes were dropped, and `action` is the
    /// final byte as a `char`.
    fn csi_dispatch(
        &mut self,
        _params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
    }

    /// The final byte of an `ESC` (non-CSI) escape sequence arrived.
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}

    /// Dispatch an Operating System Command. `params` are the semicolon-split
    /// fields of the payload; `bell_terminated` distinguishes a `BEL` terminator
    /// from a String Terminator.
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    /// The final byte of a Device Control String introducer arrived; subsequent
    /// payload bytes flow to [`Self::put`] until [`Self::unhook`].
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}

    /// A Device Control String payload byte.
    fn put(&mut self, _byte: u8) {}

    /// The active Device Control String terminated.
    fn unhook(&mut self) {}

    /// An Application Program Command (`ESC _ … ST`) payload was received.
    ///
    /// This is the OdyTTY-owned extension over `vte`, which discards APC
    /// strings. The terminal core ignores it today; the graphics-protocol work
    /// (Kitty) consumes it on owned plumbing in a later packet.
    fn apc_dispatch(&mut self, _data: &[u8]) {}
}
