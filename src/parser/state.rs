//! The OdyTTY-owned VT parser: a DEC ANSI escape-sequence state machine.
//!
//! `OdyParser` consumes raw PTY bytes and drives a [`VtDispatch`] implementation
//! (the terminal core), following the canonical DEC ANSI parser state diagram
//! (<https://vt100.net/emu/dec_ansi_parser>). It is byte-for-byte compatible
//! with the `vte` crate at the dispatch layer — verified by the differential
//! oracle in the core tests — with one deliberate, Screen-invisible extension:
//! it surfaces APC (`ESC _ … ST`) payloads via [`VtDispatch::apc_dispatch`],
//! which `vte` discards. That seam is what later enables the Kitty graphics
//! protocol on OdyTTY-owned plumbing.
//!
//! UTF-8 is decoded only in the Ground state; multi-byte codepoints split across
//! `advance()` calls are completed via [`PartialUtf8`]. All other states process
//! raw bytes, matching the canonical parser.
//!
//! ## C1 / UTF-8 precedence (PA2 decision)
//!
//! OdyTTY is a UTF-8 terminal: **UTF-8 decoding takes precedence and 8-bit C1
//! sequence introduction is not supported**, matching vte/xterm UTF-8 mode. A
//! lone `0x80..=0x9F` byte (an invalid UTF-8 lead) `execute`s as a C1 control;
//! it does **not** introduce a sequence (`0x9B` is not CSI, `0x9D` not OSC,
//! `0x9F` not APC, `0x9C` not ST). The 8-bit introducers exist only in legacy
//! 8-bit / `S7C1T` modes OdyTTY never enters. A C1 codepoint arriving as valid
//! 2-byte UTF-8 (`0xC2 0x8x`) follows the canonical rule (see [`Self::advance`]
//! below): a continuation-path scalar `print`s, a whole-buffer Ground scalar
//! `execute`s — both parsers agree, verified at every byte split.
//!
//! ## DCS / APC payload buffer policy (PA2 decision)
//!
//! **DCS is unbuffered streaming passthrough**: payload bytes flow
//! `hook → put → unhook` straight to the consumer, so the parser holds no DCS
//! buffer and imposes no DCS-side cap (buffering and limits are the consumer's
//! responsibility). **APC is buffered** because [`VtDispatch::apc_dispatch`]
//! delivers the whole payload at once (the Kitty-graphics landing pad); its
//! buffer is bounded by [`MAX_APC_RAW`]. On overflow the parser stops buffering,
//! marks the string overflowed, and **drops** the APC rather than dispatching a
//! truncated payload. SOS/PM strings are discarded as in vte.

use super::VtDispatch;
use super::params::Params;
use super::utf8::{PartialResult, PartialUtf8};

/// Maximum collected intermediate bytes before a sequence is flagged `ignore`.
const MAX_INTERMEDIATES: usize = 2;
/// Maximum OSC parameters (semicolon-separated fields) retained.
const MAX_OSC_PARAMS: usize = 16;
/// Soft cap on the OSC payload buffer, matching the canonical parser's default.
const MAX_OSC_RAW: usize = 1024;
/// Hard cap on a single APC string's buffered payload. APC is the only string
/// the parser buffers whole (to deliver via [`VtDispatch::apc_dispatch`]); this
/// bounds the memory an unterminated or hostile APC can consume. The cap is
/// generous enough for realistic single APC payloads; the Kitty graphics
/// protocol additionally chunks large transfers (`m=1`) into small per-APC
/// pieces, so this is a DoS guard, not a per-image limit. An APC exceeding it is
/// dropped, not truncated-and-dispatched (a corrupt partial payload is worse
/// than none). Revisit when the graphics layer lands.
const MAX_APC_RAW: usize = 1 << 20; // 1 MiB

/// Parser states from the canonical DEC ANSI diagram.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Printable text + C0/C1 controls.
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
    /// OSC payload accumulation.
    OscString,
    /// SOS/PM/APC string; payload discarded except APC, which OdyTTY surfaces.
    SosPmApcString,
}

/// The OdyTTY VT parser. Construct with [`OdyParser::new`], then feed bytes with
/// [`OdyParser::advance`], supplying the [`VtDispatch`] sink.
#[derive(Debug, Clone)]
pub struct OdyParser {
    state: State,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_idx: usize,
    params: Params,
    /// The digit run currently being accumulated (not yet pushed to `params`).
    param: u16,
    /// Whether the current sequence overflowed a limit and must be ignored.
    ignoring: bool,
    /// Raw OSC payload bytes (before splitting on `;`).
    osc_raw: Vec<u8>,
    /// `(start, end)` byte ranges into `osc_raw`, one per OSC parameter.
    osc_params: [(usize, usize); MAX_OSC_PARAMS],
    osc_num_params: usize,
    /// Captured APC payload (only while in an APC string).
    apc_raw: Vec<u8>,
    /// Whether the active SOS/PM/APC string is specifically an APC string.
    apc_active: bool,
    /// Set when the active APC payload exceeded [`MAX_APC_RAW`]; the string is
    /// then dropped on terminate rather than dispatched truncated.
    apc_overflow: bool,
    /// Carryover bytes of a UTF-8 codepoint split across `advance()` calls.
    partial: PartialUtf8,
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
            state: State::Ground,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_idx: 0,
            params: Params::new(),
            param: 0,
            ignoring: false,
            osc_raw: Vec::new(),
            osc_params: [(0, 0); MAX_OSC_PARAMS],
            osc_num_params: 0,
            apc_raw: Vec::new(),
            apc_active: false,
            apc_overflow: false,
            partial: PartialUtf8::default(),
        }
    }

    /// Feed `bytes` to the parser, dispatching actions to `sink`.
    pub fn advance<D: VtDispatch>(&mut self, sink: &mut D, bytes: &[u8]) {
        let mut i = 0;

        // Finish any UTF-8 codepoint left partial by the previous call.
        if self.partial.is_pending() {
            i += self.advance_partial(sink, bytes);
        }

        while i != bytes.len() {
            if self.state == State::Ground {
                i += self.advance_ground(sink, &bytes[i..]);
            } else {
                self.change_state(sink, bytes[i]);
                i += 1;
            }
        }
    }

    /// Complete a pending partial codepoint using the head of `bytes`.
    fn advance_partial<D: VtDispatch>(&mut self, sink: &mut D, bytes: &[u8]) -> usize {
        match self.partial.advance(bytes) {
            // Matches the canonical parser: a completed codepoint always prints,
            // even for the C1 range (`U+0080..=U+009F`), which differs from a
            // C1 that arrives whole in Ground (that path executes). The oracle
            // proves both parsers agree because both follow this rule.
            PartialResult::Char { ch, consumed } => {
                sink.print(ch);
                consumed
            }
            PartialResult::Invalid { consumed } => {
                sink.print('\u{FFFD}');
                consumed
            }
            PartialResult::NeedMore { consumed } => consumed,
        }
    }

    /// Dispatch one byte in any non-Ground state.
    fn change_state<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match self.state {
            State::Ground => unreachable!("ground handled by advance_ground"),
            State::Escape => self.advance_escape(sink, byte),
            State::EscapeIntermediate => self.advance_escape_intermediate(sink, byte),
            State::CsiEntry => self.advance_csi_entry(sink, byte),
            State::CsiParam => self.advance_csi_param(sink, byte),
            State::CsiIntermediate => self.advance_csi_intermediate(sink, byte),
            State::CsiIgnore => self.advance_csi_ignore(sink, byte),
            State::DcsEntry => self.advance_dcs_entry(sink, byte),
            State::DcsParam => self.advance_dcs_param(sink, byte),
            State::DcsIntermediate => self.advance_dcs_intermediate(sink, byte),
            State::DcsPassthrough => self.advance_dcs_passthrough(sink, byte),
            State::DcsIgnore => self.anywhere(sink, byte),
            State::OscString => self.advance_osc_string(sink, byte),
            State::SosPmApcString => self.advance_sos_pm_apc(sink, byte),
        }
    }

    // ----- Ground: UTF-8 text + C0/C1 controls -----

    /// Consume a run of Ground bytes up to the next `ESC`, decoding UTF-8.
    /// Returns the number of bytes consumed.
    fn advance_ground<D: VtDispatch>(&mut self, sink: &mut D, bytes: &[u8]) -> usize {
        let num_bytes = bytes.len();
        let plain = bytes.iter().position(|&b| b == 0x1B).unwrap_or(num_bytes);

        // An immediate ESC: switch to Escape and consume it.
        if plain == 0 {
            self.enter_escape();
            return 1;
        }

        match std::str::from_utf8(&bytes[..plain]) {
            Ok(text) => {
                Self::ground_dispatch(sink, text);
                let mut processed = plain;
                if processed < num_bytes {
                    // The byte at `plain` is ESC (the only thing the scan finds).
                    self.enter_escape();
                    processed += 1;
                }
                processed
            }
            Err(err) => {
                let valid = err.valid_up_to();
                // Dispatch the valid prefix first.
                let prefix = std::str::from_utf8(&bytes[..valid]).expect("valid prefix");
                Self::ground_dispatch(sink, prefix);

                match err.error_len() {
                    Some(len) => {
                        // Definitively invalid: a lone C1 executes, else U+FFFD.
                        if len == 1 && bytes[valid] <= 0x9F {
                            sink.execute(bytes[valid]);
                        } else {
                            sink.print('\u{FFFD}');
                        }
                        valid + len
                    }
                    None => {
                        // Incomplete codepoint at the end of this Ground run.
                        if plain < num_bytes {
                            // An ESC interrupted it: drop it and enter Escape.
                            sink.print('\u{FFFD}');
                            self.enter_escape();
                            plain + 1
                        } else {
                            // Genuine buffer end: stash the partial bytes.
                            self.partial.stash(&bytes[valid..plain]);
                            num_bytes
                        }
                    }
                }
            }
        }
    }

    /// Print/execute each scalar of a decoded Ground run. C0 (`0x00..=0x1F`) and
    /// C1 (`0x80..=0x9F`) controls execute; everything else prints.
    fn ground_dispatch<D: VtDispatch>(sink: &mut D, text: &str) {
        for ch in text.chars() {
            match ch {
                '\u{00}'..='\u{1F}' | '\u{80}'..='\u{9F}' => sink.execute(ch as u8),
                _ => sink.print(ch),
            }
        }
    }

    // ----- Escape -----

    fn advance_escape<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => sink.execute(byte),
            0x20..=0x2F => {
                self.collect(byte);
                self.state = State::EscapeIntermediate;
            }
            0x30..=0x4F => {
                sink.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground;
            }
            0x50 => {
                self.reset_params();
                self.state = State::DcsEntry;
            }
            0x51..=0x57 => {
                sink.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground;
            }
            0x58 => self.enter_string(false),
            0x59..=0x5A => {
                sink.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground;
            }
            0x5B => {
                self.reset_params();
                self.state = State::CsiEntry;
            }
            0x5C => {
                sink.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground;
            }
            0x5D => {
                self.osc_raw.clear();
                self.osc_num_params = 0;
                self.state = State::OscString;
            }
            // PM (`^`) and APC (`_`): only APC (`0x5F`) is surfaced.
            0x5E => self.enter_string(false),
            0x5F => self.enter_string(true),
            0x60..=0x7E => {
                sink.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground;
            }
            0x18 | 0x1A => {
                sink.execute(byte);
                self.state = State::Ground;
            }
            0x1B => {}
            _ => {}
        }
    }

    fn advance_escape_intermediate<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => sink.execute(byte),
            0x20..=0x2F => self.collect(byte),
            0x30..=0x7E => {
                sink.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground;
            }
            0x7F => {}
            _ => self.anywhere(sink, byte),
        }
    }

    // ----- CSI -----

    fn advance_csi_entry<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => sink.execute(byte),
            0x20..=0x2F => {
                self.collect(byte);
                self.state = State::CsiIntermediate;
            }
            0x30..=0x39 => {
                self.param_next(byte);
                self.state = State::CsiParam;
            }
            0x3A => {
                self.subparam();
                self.state = State::CsiParam;
            }
            0x3B => {
                self.param_sep();
                self.state = State::CsiParam;
            }
            0x3C..=0x3F => {
                self.collect(byte);
                self.state = State::CsiParam;
            }
            0x40..=0x7E => self.csi_dispatch(sink, byte),
            _ => self.anywhere(sink, byte),
        }
    }

    fn advance_csi_param<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => sink.execute(byte),
            0x20..=0x2F => {
                self.collect(byte);
                self.state = State::CsiIntermediate;
            }
            0x30..=0x39 => self.param_next(byte),
            0x3A => self.subparam(),
            0x3B => self.param_sep(),
            0x3C..=0x3F => self.state = State::CsiIgnore,
            0x40..=0x7E => self.csi_dispatch(sink, byte),
            0x7F => {}
            _ => self.anywhere(sink, byte),
        }
    }

    fn advance_csi_intermediate<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => sink.execute(byte),
            0x20..=0x2F => self.collect(byte),
            0x30..=0x3F => self.state = State::CsiIgnore,
            0x40..=0x7E => self.csi_dispatch(sink, byte),
            _ => self.anywhere(sink, byte),
        }
    }

    fn advance_csi_ignore<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => sink.execute(byte),
            0x20..=0x3F => {}
            0x40..=0x7E => self.state = State::Ground,
            0x7F => {}
            _ => self.anywhere(sink, byte),
        }
    }

    // ----- DCS -----

    fn advance_dcs_entry<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => {}
            0x20..=0x2F => {
                self.collect(byte);
                self.state = State::DcsIntermediate;
            }
            0x30..=0x39 => {
                self.param_next(byte);
                self.state = State::DcsParam;
            }
            0x3A => {
                self.subparam();
                self.state = State::DcsParam;
            }
            0x3B => {
                self.param_sep();
                self.state = State::DcsParam;
            }
            0x3C..=0x3F => {
                self.collect(byte);
                self.state = State::DcsParam;
            }
            0x40..=0x7E => self.hook(sink, byte),
            0x7F => {}
            _ => self.anywhere(sink, byte),
        }
    }

    fn advance_dcs_param<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => {}
            0x20..=0x2F => {
                self.collect(byte);
                self.state = State::DcsIntermediate;
            }
            0x30..=0x39 => self.param_next(byte),
            0x3A => self.subparam(),
            0x3B => self.param_sep(),
            0x3C..=0x3F => self.state = State::DcsIgnore,
            0x40..=0x7E => self.hook(sink, byte),
            0x7F => {}
            _ => self.anywhere(sink, byte),
        }
    }

    fn advance_dcs_intermediate<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => {}
            0x20..=0x2F => self.collect(byte),
            0x30..=0x3F => self.state = State::DcsIgnore,
            0x40..=0x7E => self.hook(sink, byte),
            0x7F => {}
            _ => self.anywhere(sink, byte),
        }
    }

    fn advance_dcs_passthrough<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x7E => sink.put(byte),
            0x18 | 0x1A => {
                sink.unhook();
                sink.execute(byte);
                self.state = State::Ground;
            }
            0x1B => {
                sink.unhook();
                self.reset_params();
                self.state = State::Escape;
            }
            0x7F => {}
            0x9C => {
                sink.unhook();
                self.state = State::Ground;
            }
            _ => {}
        }
    }

    // ----- OSC -----

    fn advance_osc_string<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1C..=0x1F => {}
            0x07 => {
                self.osc_end(sink, byte);
                self.state = State::Ground;
            }
            0x18 | 0x1A => {
                self.osc_end(sink, byte);
                sink.execute(byte);
                self.state = State::Ground;
            }
            0x1B => {
                self.osc_end(sink, byte);
                self.reset_params();
                self.state = State::Escape;
            }
            0x3B => self.osc_put_param(),
            _ => self.osc_put(byte),
        }
    }

    // ----- SOS / PM / APC -----

    fn advance_sos_pm_apc<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x18 | 0x1A => {
                // Aborted: discard the string (matches the canonical parser).
                sink.execute(byte);
                self.state = State::Ground;
            }
            0x1B => {
                // ST start: surface a captured APC payload (unless it overflowed
                // the cap, in which case it is dropped), then resume Escape so the
                // trailing `\` dispatches exactly as the canonical parser.
                if self.apc_active && !self.apc_overflow {
                    sink.apc_dispatch(&self.apc_raw);
                }
                self.reset_params();
                self.state = State::Escape;
            }
            _ => {
                if self.apc_active && !self.apc_overflow {
                    if self.apc_raw.len() == MAX_APC_RAW {
                        // Bound the buffer and drop (do not dispatch) this APC.
                        self.apc_overflow = true;
                        self.apc_raw = Vec::new();
                    } else {
                        self.apc_raw.push(byte);
                    }
                }
            }
        }
    }

    // ----- shared actions -----

    fn anywhere<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        match byte {
            0x18 | 0x1A => {
                sink.execute(byte);
                self.state = State::Ground;
            }
            0x1B => self.enter_escape(),
            _ => {}
        }
    }

    fn csi_dispatch<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
        }
        sink.csi_dispatch(
            &self.params,
            self.intermediates(),
            self.ignoring,
            byte as char,
        );
        self.state = State::Ground;
    }

    fn hook<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
        }
        sink.hook(
            &self.params,
            self.intermediates(),
            self.ignoring,
            byte as char,
        );
        self.state = State::DcsPassthrough;
    }

    fn collect(&mut self, byte: u8) {
        if self.intermediate_idx == MAX_INTERMEDIATES {
            self.ignoring = true;
        } else {
            self.intermediates[self.intermediate_idx] = byte;
            self.intermediate_idx += 1;
        }
    }

    /// `:` — extend the current parameter with another subparameter.
    fn subparam(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.extend(self.param);
            self.param = 0;
        }
    }

    /// `;` — finish the current parameter and open the next.
    fn param_sep(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
            self.param = 0;
        }
    }

    /// A digit — accumulate into the current parameter with saturating math.
    fn param_next(&mut self, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.param = self.param.saturating_mul(10);
            self.param = self.param.saturating_add((byte - b'0') as u16);
        }
    }

    fn osc_put(&mut self, byte: u8) {
        if self.osc_raw.len() == MAX_OSC_RAW {
            return;
        }
        self.osc_raw.push(byte);
    }

    fn osc_put_param(&mut self) {
        let idx = self.osc_raw.len();
        let param_idx = self.osc_num_params;
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

    fn osc_end<D: VtDispatch>(&mut self, sink: &mut D, byte: u8) {
        self.osc_put_param();
        self.osc_dispatch(sink, byte);
        self.osc_raw.clear();
        self.osc_num_params = 0;
    }

    fn osc_dispatch<D: VtDispatch>(&self, sink: &mut D, byte: u8) {
        let mut slices: Vec<&[u8]> = Vec::with_capacity(self.osc_num_params);
        for &(start, end) in self.osc_params.iter().take(self.osc_num_params) {
            slices.push(&self.osc_raw[start..end]);
        }
        sink.osc_dispatch(&slices, byte == 0x07);
    }

    /// Enter Escape, clearing the per-sequence state.
    fn enter_escape(&mut self) {
        self.reset_params();
        self.state = State::Escape;
    }

    /// Enter a SOS/PM/APC string. `apc` selects APC, the only kind OdyTTY
    /// surfaces; SOS and PM payloads are discarded as in the canonical parser.
    fn enter_string(&mut self, apc: bool) {
        self.apc_active = apc;
        self.apc_overflow = false;
        self.apc_raw.clear();
        self.state = State::SosPmApcString;
    }

    fn reset_params(&mut self) {
        self.intermediate_idx = 0;
        self.ignoring = false;
        self.param = 0;
        self.params.clear();
    }

    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.intermediate_idx]
    }
}
