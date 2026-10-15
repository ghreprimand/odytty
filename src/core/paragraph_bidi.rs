// SPDX-License-Identifier: GPL-3.0-only
//! Per-paragraph bidi properties from the terminal-wg BiDi recommendation.
//!
//! A paragraph is one logical line (rows joined by soft wraps). Each one
//! carries an implicit or explicit mode (BDSM, `CSI 8 h` and `CSI 8 l`) and a
//! direction (SCP, `CSI Ps SP k`: 0 the terminal default, 1 left to right,
//! 2 right to left). The terminal also tracks a *current* value, the latest
//! received, which is stamped onto paragraphs as the recommendation
//! describes: at once when the sequence arrives with the cursor at a
//! paragraph's first position, on a line feed into a new paragraph, and on
//! rows that erase or scroll operations create. A wrap that joins two
//! paragraphs keeps the first one's value.
//!
//! The value is model state only. Display reads it in a later stage; until
//! then it changes nothing on screen.

/// Paragraph direction as stored: `Default` follows the live setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ParagraphDirection {
    #[default]
    Default,
    LeftToRight,
    RightToLeft,
}

/// One paragraph's bidi properties, packed into a byte: bits 0-1 hold the
/// direction (0 default, 1 left to right, 2 right to left) and bit 2 is set
/// for explicit mode. The all-zero value, implicit with the default
/// direction, is the power-on state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ParagraphBidi(u8);

const DIRECTION_MASK: u8 = 0b011;
const EXPLICIT: u8 = 0b100;

impl ParagraphBidi {
    /// Implicit mode with the terminal's default direction.
    pub const DEFAULT: Self = Self(0);

    /// Whether the paragraph is in explicit mode (BDSM reset, `CSI 8 l`):
    /// the application supplies visual order and the terminal must not
    /// reorder or contextually shape it.
    pub fn explicit(self) -> bool {
        self.0 & EXPLICIT != 0
    }

    pub fn direction(self) -> ParagraphDirection {
        match self.0 & DIRECTION_MASK {
            1 => ParagraphDirection::LeftToRight,
            2 => ParagraphDirection::RightToLeft,
            _ => ParagraphDirection::Default,
        }
    }

    pub fn with_explicit(self, explicit: bool) -> Self {
        Self((self.0 & !EXPLICIT) | if explicit { EXPLICIT } else { 0 })
    }

    pub fn with_direction(self, direction: ParagraphDirection) -> Self {
        let bits = match direction {
            ParagraphDirection::Default => 0,
            ParagraphDirection::LeftToRight => 1,
            ParagraphDirection::RightToLeft => 2,
        };
        Self((self.0 & !DIRECTION_MASK) | bits)
    }

    /// The wire byte.
    pub fn to_byte(self) -> u8 {
        self.0
    }

    /// Decode a wire byte; `None` for unused bits or direction value 3.
    pub fn from_byte(byte: u8) -> Option<Self> {
        (byte & !(DIRECTION_MASK | EXPLICIT) == 0 && byte & DIRECTION_MASK != DIRECTION_MASK)
            .then_some(Self(byte))
    }
}

/// One bidi property a received sequence changes. Only that property is
/// applied to a paragraph stamped at its first position; the paragraph's
/// other property stays as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core) enum ParagraphBidiChange {
    Explicit(bool),
    Direction(ParagraphDirection),
}

impl ParagraphBidiChange {
    pub(in crate::core) fn apply(self, value: ParagraphBidi) -> ParagraphBidi {
        match self {
            Self::Explicit(explicit) => value.with_explicit(explicit),
            Self::Direction(direction) => value.with_direction(direction),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_valid_byte_round_trips_and_invalid_bytes_are_refused() {
        let mut valid = 0;
        for byte in 0..=u8::MAX {
            match ParagraphBidi::from_byte(byte) {
                Some(value) => {
                    valid += 1;
                    assert_eq!(value.to_byte(), byte);
                }
                None => assert!(byte > 7 || byte & 3 == 3, "{byte:#x}"),
            }
        }
        assert_eq!(valid, 6, "two modes times three directions");
    }

    #[test]
    fn changes_touch_only_their_own_property() {
        let rtl = ParagraphBidi::DEFAULT.with_direction(ParagraphDirection::RightToLeft);
        let explicit_rtl = ParagraphBidiChange::Explicit(true).apply(rtl);
        assert!(explicit_rtl.explicit());
        assert_eq!(explicit_rtl.direction(), ParagraphDirection::RightToLeft);
        let back = ParagraphBidiChange::Direction(ParagraphDirection::Default).apply(explicit_rtl);
        assert!(back.explicit());
        assert_eq!(back.direction(), ParagraphDirection::Default);
        assert_eq!(ParagraphBidi::default(), ParagraphBidi::DEFAULT);
    }
}
