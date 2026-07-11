// SPDX-License-Identifier: GPL-3.0-only
//! Layer 1 — Ground-state text/control segmenter (UTF-8 lives here).
//!
//! The segmenter walks input bytes in **Ground** state and splits them into
//! (i) maximal runs of **printable text scalars** (validated UTF-8 that is not
//! C0/C1) and (ii) **single control scalars/bytes** that drive the Layer-2
//! state machine. UTF-8 decoding — including the partial-codepoint carry across
//! `advance()` boundaries that the PTY's arbitrary chunk sizes force — lives
//! entirely in this module; by the time bytes reach Layer 2 they are
//! 8-bit-clean control bytes.
//!
//! ## Design (PA2-r originality)
//!
//! This layer differs structurally from the first-generation OdyTTY parser
//! (which folded UTF-8 into the Ground state of its main state machine):
//! lifting UTF-8 out makes Layer 2 a clean byte-only control automaton, and
//! shrinks the partial-codepoint logic to one focused module instead of
//! threading it through every state's hot path. The bulk ASCII fast-path
//! (a single linear scan + one `from_utf8_unchecked` per maximal run) gives
//! plain text its zero-branch route through the parser; non-ASCII scalars
//! take a per-scalar careful decode.
//!
//! ## C1-via-UTF-8 uniform-execute
//!
//! A validly-decoded C1 scalar (`U+0080..=U+009F` via `0xC2 0x8x`) **executes
//! uniformly** regardless of how its bytes are split across `advance()` calls.
//! That removes the canonical "split prints, whole executes" quirk so the
//! observable behavior is the same whether the lead byte and continuation
//! arrive together or across two calls.
//!
//! ## Partial-completion no-byte-loss policy
//!
//! A pending partial UTF-8 scalar may complete at the head of a later `advance()`
//! chunk that also contains following printable bytes. OdyTTY consumes only the
//! bytes needed for the completed scalar, then lets the normal Ground sweep
//! process the remaining bytes. This preserves arbitrary PTY chunk-boundary
//! behavior: splitting `caféA` after `0xC3` still prints both `é` and `A`.
//!
//! ## Malformed UTF-8 policy
//!
//! Invalid UTF-8 emits **one `U+FFFD`** per maximal invalid subpart (the
//! Unicode 3.9 guideline that `std::str::from_utf8`'s `valid_up_to` /
//! `error_len` already implement). The exception: a lone invalid byte in
//! `0x80..=0x9F` executes as a C1 control (matching the "no 8-bit C1
//! introduction" UTF-8-mode policy — see `mod.rs` divergence ledger). A
//! truncated trailing codepoint at the end of a chunk is carried, not emitted,
//! until the next chunk arrives; a truncated codepoint then interrupted by ESC
//! emits `U+FFFD` and processes the control.

use super::VtDispatch;

/// Outcome of one [`Segmenter::run_ground`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroundResult {
    /// All bytes consumed; possibly with a partial codepoint stashed for the
    /// next call. The driver remains in Ground (control machine untouched).
    Drained,
    /// Saw `ESC` at position `consumed - 1` (already included in the returned
    /// count). The driver resets per-sequence state and transitions to Escape.
    SawEsc,
}

/// The Ground-state segmenter.
///
/// Stateless except for the partial-codepoint carry buffer (≤3 bytes leftover
/// when a multi-byte UTF-8 scalar straddled an `advance()` boundary). Common
/// case (all bytes printable ASCII, no carry): zero state held between calls.
#[derive(Debug, Default, Clone)]
pub(crate) struct Segmenter {
    /// Up to 3 leading bytes of an incomplete UTF-8 codepoint from a previous
    /// call. `partial_len == 0` ⇔ no carry.
    partial_buf: [u8; 4],
    partial_len: u8,
}

impl Segmenter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Clear any pending partial-codepoint carry.
    pub(crate) fn reset(&mut self) {
        self.partial_len = 0;
    }

    /// Whether a partial codepoint is pending from a previous call.
    #[inline]
    pub(crate) fn has_partial(&self) -> bool {
        self.partial_len != 0
    }

    /// Process Ground-state bytes from `bytes`, dispatching printable text and
    /// C0/C1 controls to `sink`. Returns the outcome plus the number of bytes
    /// consumed from `bytes`.
    pub(crate) fn run_ground<D: VtDispatch>(
        &mut self,
        sink: &mut D,
        bytes: &[u8],
    ) -> (GroundResult, usize) {
        let mut i = 0usize;
        let n = bytes.len();

        // -------- Complete any pending partial codepoint --------
        if self.has_partial() {
            match self.advance_partial(sink, bytes) {
                PartialOutcome::Consumed(c) => i += c,
                PartialOutcome::NeedMore(c) => {
                    return (GroundResult::Drained, c);
                }
            }
        }

        // -------- Bulk-validation main sweep --------
        //
        // Each iteration: find the next ESC, bulk-validate UTF-8 over the
        // pre-ESC chunk in one call, and bulk-dispatch its chars via
        // `dispatch_run`. Invalid subparts and trailing partials are handled
        // by the `Err` arm in-place. This is the path PA1 used and it pairs
        // well with `from_utf8`'s SIMD-friendly inner loop; the two-layer
        // architecture (segmenter / control machine) is preserved because the
        // ESC byte still hands the buffer off to Layer 2 at chunk boundaries.
        while i < n {
            let rel = bytes[i..].iter().position(|&b| b == 0x1B);
            let chunk_end = rel.map(|p| i + p).unwrap_or(n);
            let chunk_has_esc = rel.is_some();
            let chunk = &bytes[i..chunk_end];

            match std::str::from_utf8(chunk) {
                Ok(text) => {
                    dispatch_run(sink, text);
                    i = chunk_end;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if valid > 0 {
                        // SAFETY: `valid_up_to` is `from_utf8`'s contract.
                        let prefix = unsafe { std::str::from_utf8_unchecked(&chunk[..valid]) };
                        dispatch_run(sink, prefix);
                    }
                    match err.error_len() {
                        Some(len) => {
                            // Definitively invalid subpart. A lone invalid
                            // byte in `0x80..=0x9F` is a C1 control execute;
                            // everything else emits one U+FFFD.
                            let b0 = chunk[valid];
                            if len == 1 && (0x80..=0x9F).contains(&b0) {
                                sink.execute(b0);
                            } else {
                                sink.print('\u{FFFD}');
                            }
                            i += valid + len;
                            // continue loop — more bytes may follow before ESC.
                            continue;
                        }
                        None => {
                            // Incomplete codepoint at end of chunk.
                            if chunk_has_esc {
                                // ESC interrupts the partial: emit U+FFFD and
                                // consume the ESC.
                                sink.print('\u{FFFD}');
                                return (GroundResult::SawEsc, chunk_end + 1);
                            } else {
                                // Genuine buffer end: stash the tail.
                                let tail = &chunk[valid..];
                                let take = tail.len().min(4);
                                self.partial_buf[..take].copy_from_slice(&tail[..take]);
                                self.partial_len = take as u8;
                                return (GroundResult::Drained, n);
                            }
                        }
                    }
                }
            }

            if chunk_has_esc {
                // ESC at `chunk_end` — consume + signal.
                return (GroundResult::SawEsc, chunk_end + 1);
            }
        }
        (GroundResult::Drained, i)
    }

    /// Try to complete a pending partial codepoint with the head of `bytes`.
    /// Returns how many new bytes were consumed and whether the codepoint
    /// resolved or still needs more input.
    fn advance_partial<D: VtDispatch>(&mut self, sink: &mut D, bytes: &[u8]) -> PartialOutcome {
        let old_len = self.partial_len as usize;
        let need = 4 - old_len;
        let copy = bytes.len().min(need);
        self.partial_buf[old_len..old_len + copy].copy_from_slice(&bytes[..copy]);
        let total = old_len + copy;

        match std::str::from_utf8(&self.partial_buf[..total]) {
            Ok(s) => {
                // The whole buffer is valid; take the first scalar.
                let ch = s.chars().next().expect("non-empty valid utf8");
                let used = ch.len_utf8();
                self.partial_len = 0;
                dispatch_scalar(sink, ch);
                PartialOutcome::Consumed(used - old_len)
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if valid > 0 {
                    let s = std::str::from_utf8(&self.partial_buf[..valid]).expect("valid prefix");
                    let ch = s.chars().next().expect("non-empty");
                    let used = ch.len_utf8();
                    self.partial_len = 0;
                    dispatch_scalar(sink, ch);
                    PartialOutcome::Consumed(used - old_len)
                } else {
                    match err.error_len() {
                        Some(invalid_len) => {
                            // Definitively invalid.
                            let b0 = self.partial_buf[0];
                            self.partial_len = 0;
                            if invalid_len == 1 && (0x80..=0x9F).contains(&b0) {
                                sink.execute(b0);
                            } else {
                                sink.print('\u{FFFD}');
                            }
                            PartialOutcome::Consumed(invalid_len.saturating_sub(old_len))
                        }
                        None => {
                            // Still need more. We've already absorbed `copy`
                            // new bytes; bump partial_len and report consumed.
                            self.partial_len = total as u8;
                            PartialOutcome::NeedMore(copy)
                        }
                    }
                }
            }
        }
    }
}

/// Internal outcome of completing a partial codepoint.
enum PartialOutcome {
    /// Codepoint resolved (or invalid emitted); consumed this many NEW bytes.
    Consumed(usize),
    /// Still incomplete; consumed this many NEW bytes and need more later.
    NeedMore(usize),
}

/// Bulk-dispatch every scalar in a validated UTF-8 run. Equivalent to a loop
/// of [`dispatch_scalar`] but expresses the hot text path in one place where
/// LLVM can specialise the per-codepoint match.
#[inline]
fn dispatch_run<D: VtDispatch>(sink: &mut D, text: &str) {
    for ch in text.chars() {
        dispatch_scalar(sink, ch);
    }
}

/// Dispatch a decoded scalar to the sink.
///
/// Implements the OdyTTY codepoint dispatch policy:
/// - **C0** (`U+0000..=U+001F`, ESC excluded by upstream segmentation) and
///   **C1** (`U+0080..=U+009F`) execute as the corresponding raw control byte.
///   The C1 branch is the accepted uniform-execute divergence: it
///   applies to both whole-buffer and partial-completion arrival paths.
/// - Everything else prints.
#[inline]
fn dispatch_scalar<D: VtDispatch>(sink: &mut D, ch: char) {
    match ch {
        '\u{00}'..='\u{1F}' | '\u{80}'..='\u{9F}' => sink.execute(ch as u8),
        _ => sink.print(ch),
    }
}
