//! The OdyTTY VT parser driver: stitches the [`Segmenter`] (Layer 1) and the
//! [`Machine`] (Layer 2) together and adapts their outputs to the
//! [`VtDispatch`] sink that the terminal core implements.
//!
//! Public surface: [`OdyParser::new`] + [`OdyParser::advance`]. The shape is
//! unchanged from the first-generation parser — `Screen` (the core dispatch
//! consumer) and parser fixtures exercise the internals through the same seam.
//!
//! ## Pipeline
//!
//! ```text
//! advance(bytes)
//!     │
//!     ├─ machine.state == Ground? ──► Layer 1 (Segmenter::run_ground)
//!     │      │                            │   ASCII bulk-print
//!     │      │                            │   per-scalar UTF-8 decode
//!     │      │                            │   C1-via-UTF-8 uniform execute
//!     │      │                            └─► sink.print / sink.execute
//!     │      └─ on SawEsc → reset machine → Escape state
//!     │
//!     └─ else ──► Layer 2 (Machine::step)
//!            │       state-class transition table
//!            │       internal params / intermediates / ignore
//!            └─► Action ─► driver adapter
//!                              │
//!                              ├─ direct sink calls
//!                              └─ OSC / APC payload buffers (this module)
//! ```
//!
//! ## OSC / APC buffering (PA2-r caps)
//!
//! - **OSC**: [`MAX_OSC_RAW`] = 128 KiB. Payload accumulated; semicolons split
//!   into params; terminators BEL / ST. Over-cap → drop further payload bytes
//!   (the OSC still dispatches on terminator with the in-cap prefix, matching
//!   the first-generation observable behavior). The cap is sized for
//!   realistic OSC 52 (clipboard base64) and OSC 8 (hyperlinks).
//! - **APC**: [`MAX_APC_RAW`] = 1 MiB. Buffered because `apc_dispatch` delivers
//!   the whole payload. Over-cap → mark overflow and **drop** (do not dispatch
//!   a truncated payload — a corrupt partial base64 image is worse than none).

use super::VtDispatch;
use super::action::Action;
use super::machine::{Machine, State};
use super::segmenter::{GroundResult, Segmenter};

/// Maximum OSC parameters (semicolon-separated fields) retained.
const MAX_OSC_PARAMS: usize = 16;

/// Cap on the OSC payload buffer. Raised to 128 KiB from the first-generation
/// 1024-byte cap to cover realistic OSC 52 clipboard base64 transfers and OSC
/// 8 hyperlinks; over-cap payload bytes are dropped and the OSC still
/// dispatches with the in-cap prefix.
const MAX_OSC_RAW: usize = 128 * 1024;

/// Cap on the APC payload buffer. Bounds the memory a single APC string can
/// consume; over-cap drops (does not dispatch) the entire APC.
const MAX_APC_RAW: usize = 1 << 20; // 1 MiB

/// The OdyTTY VT parser. Construct with [`OdyParser::new`], then feed bytes
/// with [`OdyParser::advance`], supplying the [`VtDispatch`] sink.
#[derive(Debug, Clone)]
pub struct OdyParser {
    segmenter: Segmenter,
    machine: Machine,
    /// Raw OSC payload bytes (before splitting on `;`).
    osc_raw: Vec<u8>,
    /// `(start, end)` byte ranges into `osc_raw`, one per OSC parameter.
    osc_params: [(usize, usize); MAX_OSC_PARAMS],
    osc_num_params: u8,
    /// Captured APC payload (only while in an APC string).
    apc_raw: Vec<u8>,
    /// Set when the active APC payload exceeded [`MAX_APC_RAW`]; the string is
    /// then dropped on terminate rather than dispatched truncated.
    apc_overflow: bool,
}

impl Default for OdyParser {
    fn default() -> Self {
        Self::new()
    }
}

impl OdyParser {
    /// Create a fresh parser in the Ground state.
    pub fn new() -> Self {
        Self {
            segmenter: Segmenter::new(),
            machine: Machine::new(),
            osc_raw: Vec::new(),
            osc_params: [(0, 0); MAX_OSC_PARAMS],
            osc_num_params: 0,
            apc_raw: Vec::new(),
            apc_overflow: false,
        }
    }

    /// Feed `bytes` to the parser, dispatching actions to `sink`.
    pub fn advance<D: VtDispatch>(&mut self, sink: &mut D, bytes: &[u8]) {
        let mut i = 0;
        let n = bytes.len();

        while i < n {
            if self.machine.state == State::Ground {
                let (result, consumed) = self.segmenter.run_ground(sink, &bytes[i..]);
                i += consumed;
                if result == GroundResult::SawEsc {
                    // ESC consumed by segmenter; transition into Escape.
                    self.machine.reset_seq();
                    self.machine.state = State::Escape;
                    self.segmenter.reset();
                }
            } else {
                let byte = bytes[i];
                let was_apc = self.machine.state == State::ApcString;
                let action = self.machine.step(byte);
                // Action::None is the hot case (every param digit / collect /
                // ignored byte returns it). Skipping the action-adapter match
                // for None keeps the inner loop a single comparison + state
                // check, matching the first-generation hot path's cost.
                if !matches!(action, Action::None) {
                    self.apply(sink, action);
                }
                i += 1;
                // Only the APC cancel path leaves stale data behind — every
                // OSC exit and every ApcEnd dispatches+clears in `apply`. So
                // the per-byte cleanup is one boolean check.
                if was_apc && self.machine.state != State::ApcString {
                    self.apc_raw.clear();
                    self.apc_overflow = false;
                }
            }
        }
    }

    // ----- action adapter -----

    #[inline]
    fn apply<D: VtDispatch>(&mut self, sink: &mut D, action: Action) {
        match action {
            Action::None => {}
            Action::Execute(byte) => sink.execute(byte),
            Action::CsiDispatch(byte) => sink.csi_dispatch(
                &self.machine.params,
                self.machine.intermediates(),
                self.machine.ignoring,
                byte as char,
            ),
            Action::EscDispatch(byte) => {
                sink.esc_dispatch(self.machine.intermediates(), self.machine.ignoring, byte)
            }
            Action::DcsHook(byte) => sink.hook(
                &self.machine.params,
                self.machine.intermediates(),
                self.machine.ignoring,
                byte as char,
            ),
            Action::DcsPut(byte) => sink.put(byte),
            Action::DcsUnhook => sink.unhook(),
            Action::DcsUnhookExecute(byte) => {
                sink.unhook();
                sink.execute(byte);
            }
            Action::OscPut(byte) => self.osc_put(byte),
            Action::OscParamBoundary => self.osc_snapshot_param(),
            Action::OscEnd { bell } => {
                self.osc_dispatch_now(sink, bell);
            }
            Action::OscEndExecute { bell, byte } => {
                self.osc_dispatch_now(sink, bell);
                sink.execute(byte);
            }
            Action::ApcPut(byte) => self.apc_put(byte),
            Action::ApcEnd => {
                if !self.apc_overflow {
                    sink.apc_dispatch(&self.apc_raw);
                }
                self.apc_raw.clear();
                self.apc_overflow = false;
            }
        }
    }

    // The advance loop above handles APC-cancel buffer cleanup inline; every
    // OSC exit dispatches+clears via `osc_dispatch_now` (OscEnd / OscEndExecute
    // actions), so no separate OSC transition handler is needed.

    // ----- OSC helpers -----

    fn osc_put(&mut self, byte: u8) {
        if self.osc_raw.len() == MAX_OSC_RAW {
            return;
        }
        self.osc_raw.push(byte);
    }

    fn osc_snapshot_param(&mut self) {
        let idx = self.osc_raw.len();
        let param_idx = self.osc_num_params as usize;
        match param_idx {
            0 => self.osc_params[param_idx] = (0, idx),
            MAX_OSC_PARAMS => return,
            _ => {
                let begin = self.osc_params[param_idx - 1].1;
                self.osc_params[param_idx] = (begin, idx);
            }
        }
        self.osc_num_params += 1;
    }

    fn osc_dispatch_now<D: VtDispatch>(&mut self, sink: &mut D, bell: bool) {
        // Snapshot the trailing implicit param (the bytes between the last `;`
        // and the terminator).
        self.osc_snapshot_param();
        let count = self.osc_num_params as usize;
        let mut slices: Vec<&[u8]> = Vec::with_capacity(count);
        for &(start, end) in self.osc_params.iter().take(count) {
            slices.push(&self.osc_raw[start..end]);
        }
        sink.osc_dispatch(&slices, bell);
        self.osc_raw.clear();
        self.osc_num_params = 0;
    }

    // ----- APC helpers -----

    fn apc_put(&mut self, byte: u8) {
        if self.apc_overflow {
            return;
        }
        if self.apc_raw.len() == MAX_APC_RAW {
            // Bound the buffer; drop further bytes and mark overflow so the
            // terminator drops the whole APC.
            self.apc_overflow = true;
            self.apc_raw = Vec::new();
            return;
        }
        self.apc_raw.push(byte);
    }
}
