//! OdyTTY-owned VT parser (Stage 4.5 Foundation Ownership).
//!
//! This module is the OdyTTY-owned VT parser — the replacement for the `vte`
//! dependency on the byte path from PTY to Screen. After PA3 retires `vte`
//! entirely it becomes the sole parser; during PA2-r it ships dark behind the
//! differential oracle while the live production path remains `vte`.
//!
//! ## Two-layer pipeline (PA2-r clean-room design)
//!
//! The PA2-r rebuild splits the parser into two clean layers along the
//! text/control boundary, an OdyTTY-original structural choice taken from the
//! primary specifications (vt100.net DEC ANSI parser diagram, ECMA-48, xterm
//! `ctlseqs`). Neither `vte` nor Ghostty source was consulted during this
//! rebuild; `vte` is retained strictly as a black-box behavioral oracle.
//!
//! ```text
//! ┌───────────── Layer 1 (segmenter.rs) ─────────────┐
//! │ Ground-state text/control sweep                  │
//! │   • bulk ESC scan + bulk UTF-8 validation        │
//! │   • per-codepoint dispatch via chars()           │
//! │   • partial-codepoint carry across advance()     │
//! │   • C1-via-UTF-8 uniform execute (policy)        │
//! │   • direct sink.print / sink.execute              │
//! └──────────────────────┬───────────────────────────┘
//!                        │ ESC → enter Escape
//! ┌──────────────────────▼ Layer 2 (machine.rs) ────┐
//! │ Byte-class control state machine                 │
//! │   • classify(byte) → ByteClass (~13 classes)     │
//! │   • flat match (state, class) → Action           │
//! │   • internal params / intermediates / ignore     │
//! └──────────────────────┬───────────────────────────┘
//!                        │ Action (pure value)
//! ┌──────────────────────▼ driver.rs ────────────────┐
//! │ Action → VtDispatch adapter                      │
//! │ OSC / APC payload buffers (caps + dispatch)      │
//! │ State-transition cleanup                         │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! ## Originality boundary
//!
//! Each axis of the design is a deliberate OdyTTY choice, documented in the
//! submodule headers:
//!
//! - **State transitions** — flat `match (state, byte-class)` returning an
//!   [`action::Action`], NOT per-state methods (the first-generation shape)
//!   and NOT the Williams `[state][256]` table (canonical reference).
//! - **UTF-8 strategy** — lifted entirely out of the state machine into the
//!   Layer-1 [`segmenter`]; partial-codepoint carry lives in one place.
//! - **Dispatch shape** — hybrid pure-action core + thin driver adapter;
//!   [`VtDispatch`] survives unchanged as the output contract.
//! - **Params storage** — inline `[u16; 32]` + `u32` boundary bitmap (no heap,
//!   no parallel array); see [`params`] module docs.
//! - **String buffering** — OSC bounded at 128 KiB drop-on-overflow; DCS
//!   streaming passthrough (no parser buffer); APC bounded at 1 MiB
//!   drop-not-truncate (the Kitty graphics landing pad).
//!
//! ## Divergence ledger
//!
//! The differential oracle stays byte-identical to `vte` driving the same
//! [`crate::core::Screen`] at every chunk-split EXCEPT these two
//! operator-approved divergences, ledgered with oracle filters (see
//! `core::parser_oracle_tests`):
//!
//! 1. **C1-via-UTF-8 uniform execute** — a validly-decoded C1 scalar
//!    (`U+0080..=U+009F` via `0xC2 0x8x`) **executes** regardless of how its
//!    bytes split across `advance()` calls. The canonical/`vte` behavior
//!    prints on the partial-completion path and executes on the whole-buffer
//!    path; OdyTTY makes the rule uniform. Real-world impact is negligible
//!    (apps send NEL `U+0085` and friends as raw `0x85`, not as UTF-8). The
//!    oracle filter skips split points falling between `0xC2` and `0x80..=0x9F`.
//! 2. **OSC cap window** — `vte` caps OSC at 1024 bytes; OdyTTY at 128 KiB.
//!    Payloads between the two caps differ in dispatch outcome. The corpus
//!    and fuzzers avoid the 1024..128 KiB window so the oracle stays clean in
//!    practice; the policy gap is documented but no input lands in it.
//!
//! ## APC surfacing (Screen-invisible)
//!
//! OdyParser also surfaces APC strings via [`VtDispatch::apc_dispatch`] (`vte`
//! discards them) — but `Screen` ignores `apc_dispatch`, so the oracle holds
//! without filtering. A dedicated test asserts this invisibility.

mod action;
mod driver;
mod machine;
mod params;
mod segmenter;

#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod machine_tests;
#[cfg(test)]
mod params_tests;
#[cfg(test)]
mod segmenter_tests;

pub use driver::OdyParser;
pub use params::{Params, ParamsIter};

/// The OdyTTY-owned terminal dispatch trait, driven by [`OdyParser`].
///
/// Each method corresponds to an action in the canonical DEC ANSI parser state
/// diagram (<https://vt100.net/emu/dec_ansi_parser>) with one OdyTTY-owned
/// extension: [`Self::apc_dispatch`], which surfaces Application Program
/// Command payloads that `vte` discards. The trait shape (method signatures
/// and semantics) is preserved verbatim from the first-generation parser so
/// `Screen` and the differential oracle observe the same surface.
///
/// All methods default to no-ops, so an implementor overrides only the actions
/// it cares about (the terminal core ignores DCS and APC today; those are
/// wired up in later packets).
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
