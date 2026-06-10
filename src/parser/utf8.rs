//! Mid-stream UTF-8 continuation buffer for the OdyTTY VT parser.
//!
//! The parser decodes UTF-8 only in the Ground state. When a multi-byte
//! codepoint straddles the boundary between two `advance()` calls (the PTY
//! delivers reads in arbitrary chunks), the trailing partial bytes are stashed
//! here and completed when the next chunk arrives. This mirrors the canonical
//! parser's behaviour so split codepoints render as one glyph rather than a run
//! of replacement characters.

/// Holds up to a full UTF-8 codepoint's worth of bytes (≤4) carried over from a
/// previous `advance()` call. Empty (`len == 0`) in the common case.
#[derive(Debug, Default, Clone)]
pub(crate) struct PartialUtf8 {
    buf: [u8; 4],
    len: usize,
}

/// Outcome of feeding more bytes to a pending partial codepoint.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PartialResult {
    /// A complete scalar was decoded; `consumed` bytes were taken from the new
    /// input (i.e. excluding the bytes already buffered).
    Char { ch: char, consumed: usize },
    /// The buffered bytes were definitively invalid; emit one replacement
    /// character and skip `consumed` bytes of new input.
    Invalid { consumed: usize },
    /// Still incomplete after consuming all `consumed` available bytes; wait for
    /// the next `advance()` call.
    NeedMore { consumed: usize },
}

impl PartialUtf8 {
    /// Whether bytes from a previous call are awaiting completion.
    pub(crate) fn is_pending(&self) -> bool {
        self.len != 0
    }

    /// Stash the trailing `bytes` of an incomplete codepoint. `bytes.len()` is
    /// always ≤ 3 here (a 4-byte codepoint can be at most 3 bytes short).
    pub(crate) fn stash(&mut self, bytes: &[u8]) {
        let take = bytes.len().min(self.buf.len());
        self.buf[..take].copy_from_slice(&bytes[..take]);
        self.len = take;
    }

    /// Try to complete the pending codepoint using `bytes` from the new chunk.
    ///
    /// Copies as many new bytes as may be needed (up to a full 4-byte codepoint),
    /// then attempts to decode. Returns how many *new* bytes were consumed in
    /// each outcome so the caller can advance its cursor correctly.
    pub(crate) fn advance(&mut self, bytes: &[u8]) -> PartialResult {
        let old = self.len;
        let to_copy = bytes.len().min(self.buf.len() - old);
        self.buf[old..old + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.len += to_copy;

        match std::str::from_utf8(&self.buf[..self.len]) {
            Ok(parsed) => {
                // The whole buffer is valid: take the first scalar.
                let ch = parsed.chars().next().expect("non-empty valid utf8");
                self.len = 0;
                PartialResult::Char {
                    ch,
                    consumed: ch.len_utf8() - old,
                }
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if valid > 0 {
                    // A complete scalar plus the start of another: take the first.
                    let ch = std::str::from_utf8(&self.buf[..valid])
                        .expect("valid prefix")
                        .chars()
                        .next()
                        .expect("non-empty valid prefix");
                    self.len = 0;
                    return PartialResult::Char {
                        ch,
                        consumed: valid - old,
                    };
                }
                match err.error_len() {
                    // Definitively invalid even with the new bytes.
                    Some(invalid_len) => {
                        self.len = 0;
                        PartialResult::Invalid {
                            consumed: invalid_len - old,
                        }
                    }
                    // Still a valid prefix of an incomplete codepoint.
                    None => PartialResult::NeedMore { consumed: to_copy },
                }
            }
        }
    }
}
