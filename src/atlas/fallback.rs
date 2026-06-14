// SPDX-License-Identifier: GPL-3.0-only
//! Symbol / Nerd-font fallback classification (RV6).
//!
//! Prompt frameworks (starship, powerlevel10k, eza, lsd, …) draw their icons
//! from the Unicode **Private Use Area**: a plain monospace body font has no
//! outline for those codepoints, so without a fallback they render as the
//! hollow-box tofu glyph. This module decides *which* codepoints are worth
//! trying a dedicated symbol / Nerd font for. The atlas consults
//! [`is_symbol_codepoint`] only when the **primary** font lacks the glyph (see
//! [`crate::atlas::GlyphAtlas::ensure_styled`]); a covered codepoint never
//! reaches this path, so body text is untouched.
//!
//! Pure and dependency-free — just range membership — so the whole policy is
//! unit-testable without a font.
//!
//! ## What lives in the PUA
//!
//! Every glyph set a Nerd Font patches in occupies a Private Use Area block.
//! The classifier is therefore defined as PUA membership rather than a brittle
//! enumeration of per-set boundaries (those shift between Nerd Fonts releases).
//! [`NERD_FONT_RANGES`] documents the representative sub-ranges (v3 layout) for
//! reference and is asserted to sit inside the predicate.

/// A named Private-Use-Area sub-range a Nerd Font patches glyphs into.
/// Documentation-only: the live predicate is whole-PUA membership (see
/// [`is_symbol_codepoint`]); these boundaries are representative of the Nerd
/// Fonts v3 layout and may drift between releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolRange {
    /// Human-readable glyph-set name.
    pub name: &'static str,
    /// First codepoint (inclusive).
    pub start: u32,
    /// Last codepoint (inclusive).
    pub end: u32,
}

/// Representative Nerd Fonts code ranges (v3 layout), for documentation and
/// tests. The live classifier does not consult this table; it is the curated
/// map of what the broad PUA predicate is actually catching.
pub const NERD_FONT_RANGES: &[SymbolRange] = &[
    SymbolRange {
        name: "Pomicons",
        start: 0xE000,
        end: 0xE00D,
    },
    SymbolRange {
        name: "Powerline + Powerline Extra",
        start: 0xE0A0,
        end: 0xE0D7,
    },
    SymbolRange {
        name: "Font Awesome Extension",
        start: 0xE200,
        end: 0xE2A9,
    },
    SymbolRange {
        name: "Weather Icons",
        start: 0xE300,
        end: 0xE3EB,
    },
    SymbolRange {
        name: "Seti-UI + Custom",
        start: 0xE5FA,
        end: 0xE6B7,
    },
    SymbolRange {
        name: "Devicons",
        start: 0xE700,
        end: 0xE8EF,
    },
    SymbolRange {
        name: "Codicons",
        start: 0xEA60,
        end: 0xEC1E,
    },
    SymbolRange {
        name: "Font Awesome",
        start: 0xED00,
        end: 0xF2FF,
    },
    SymbolRange {
        name: "Font Logos",
        start: 0xF300,
        end: 0xF381,
    },
    SymbolRange {
        name: "Octicons",
        start: 0xF400,
        end: 0xF533,
    },
    SymbolRange {
        name: "Material Design Icons",
        start: 0xF0001,
        end: 0xF1AF0,
    },
];

/// Whether `ch` is a symbol/icon codepoint worth trying the Nerd-font fallback
/// for: any Private Use Area codepoint.
///
/// - BMP PUA `U+E000..=U+F8FF` holds the classic Nerd Font sets (Powerline,
///   Devicons, Font Awesome, Codicons, Octicons, …).
/// - Supplementary PUA-A `U+F0000..=U+FFFFD` holds Material Design Icons in the
///   current Nerd Fonts layout.
///
/// Plane-16 PUA-B is intentionally excluded: Nerd Fonts do not use it, and the
/// terminal's existing replacement codepoint `U+10FFFD` must stay a non-symbol
/// so its hollow-box fallback behavior is preserved.
pub fn is_symbol_codepoint(ch: char) -> bool {
    let c = ch as u32;
    (0xE000..=0xF8FF).contains(&c) || (0xF0000..=0xFFFFD).contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_pua_is_symbol() {
        // Endpoints and representative interior icons.
        assert!(is_symbol_codepoint('\u{E000}'));
        assert!(is_symbol_codepoint('\u{E0B0}')); // Powerline right arrow
        assert!(is_symbol_codepoint('\u{F031}')); // Font Awesome
        assert!(is_symbol_codepoint('\u{F8FF}'));
    }

    #[test]
    fn supplementary_pua_a_is_symbol() {
        assert!(is_symbol_codepoint('\u{F0001}')); // Material Design Icons start
        assert!(is_symbol_codepoint('\u{F1AF0}'));
        assert!(is_symbol_codepoint('\u{FFFFD}'));
    }

    #[test]
    fn ordinary_text_is_not_symbol() {
        // ASCII, Latin-1, CJK, box-drawing, and the replacement codepoint must
        // never be diverted to the symbol fallback.
        for ch in ['A', 'z', '0', ' ', 'é', '中', '\u{2500}', '\u{2588}'] {
            assert!(!is_symbol_codepoint(ch), "{ch:?} misclassified as symbol");
        }
        // Plane-16 PUA-B is deliberately excluded.
        assert!(!is_symbol_codepoint('\u{10FFFD}'));
        assert!(!is_symbol_codepoint('\u{100000}'));
    }

    #[test]
    fn documented_ranges_are_within_the_predicate() {
        // Every named Nerd Fonts range must sit inside the live PUA predicate,
        // so the documentation table can never claim a range the classifier
        // would reject.
        for r in NERD_FONT_RANGES {
            assert!(r.start <= r.end, "{} range inverted", r.name);
            for c in [r.start, (r.start + r.end) / 2, r.end] {
                let ch = char::from_u32(c).expect("valid codepoint");
                assert!(
                    is_symbol_codepoint(ch),
                    "{} U+{c:04X} not covered by is_symbol_codepoint",
                    r.name
                );
            }
        }
    }
}
