// SPDX-License-Identifier: GPL-3.0-only
//! Symbol / Nerd-font fallback classification (RV6).
//!
//! Prompt frameworks (starship, powerlevel10k, eza, lsd, …) draw their icons
//! from two places: the Unicode **Private Use Area** (the patched Nerd Font
//! glyph sets) and a handful of **standard symbol/pictograph blocks** that a
//! plain monospace body font commonly lacks (arrows, power symbols, geometric
//! shapes, and the Dingbats angle brackets the prompt char `U+276F` belongs
//! to). For either source, a body font with no outline for the codepoint
//! renders the hollow-box tofu glyph. This module decides *which* codepoints
//! are worth trying a dedicated symbol / Nerd font for. The atlas consults
//! [`is_symbol_codepoint`] only when the **primary** font lacks the glyph (see
//! [`crate::atlas::GlyphAtlas::ensure_styled`]); a covered codepoint never
//! reaches this path, so body text is untouched.
//!
//! Pure and dependency-free — just range membership — so the whole policy is
//! unit-testable without a font.
//!
//! ## The classifier is a *gate to attempt* the fallback
//!
//! Returning `true` only means "try the configured symbol/Nerd fallback for
//! this codepoint". The atlas then calls `font_has_glyph` on the actual
//! fallback face, so a codepoint **no** symbol font provides still falls
//! through to the tofu box exactly as before. Because of that gate the
//! classifier can safely name whole standard symbol blocks even though the
//! bundled *Symbols Nerd Font Mono* face only covers a sparse subset of them
//! (it is an icon-only face): the bundled face resolves the glyphs it has, and
//! a richer override font (`ODYTTY_SYMBOL_FONT`, SYMMAP, or a fuller system
//! Nerd Font) resolves the rest. Nothing here forces a glyph to exist.
//!
//! ## What the classifier covers
//!
//! - The whole **Private Use Area** — `U+E000..=U+F8FF` (classic Nerd Font
//!   sets: Powerline, Devicons, Font Awesome, Codicons, Octicons, …) and
//!   supplementary PUA-A `U+F0000..=U+FFFFD` (Material Design Icons). Defined
//!   as whole-PUA membership rather than a brittle per-set enumeration (those
//!   boundaries shift between Nerd Fonts releases); [`NERD_FONT_RANGES`]
//!   documents the representative v3 sub-ranges and is asserted to sit inside
//!   the predicate.
//! - The standard symbol/pictograph blocks in [`SYMBOL_BLOCKS`] (arrows,
//!   Miscellaneous Technical, Geometric Shapes, Miscellaneous Symbols,
//!   Dingbats, Miscellaneous Symbols and Arrows).
//!
//! ## What is deliberately *excluded*
//!
//! The geometric path ([`crate::boxdraw`]) owns box drawing `U+2500..=257F`,
//! block elements `U+2580..=259F`, Braille `U+2800..=28FF`, and Symbols for
//! Legacy Computing `U+1FB00..=1FBFF`; it runs *before* the fallback and draws
//! those crisply from cell geometry. None of the symbol blocks above overlap
//! those ranges (Geometric Shapes starts at `U+25A0`, one past the block
//! elements). Ordinary text (Latin, Latin-1, CJK, …) and the replacement
//! codepoint `U+10FFFD` are never classified as symbols, so their primary-font
//! / hollow-box behavior is preserved.

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

/// Standard Unicode symbol/pictograph blocks (outside the PUA) that body fonts
/// commonly lack but symbol / Nerd fonts provide. Unlike [`NERD_FONT_RANGES`],
/// this table **is** consulted by the live classifier: each entry is a range
/// [`is_symbol_codepoint`] will attempt the fallback for. Coverage is then
/// decided per glyph by the installed fallback face (`font_has_glyph`), so a
/// block with only partial coverage in a given face is harmless — present
/// glyphs resolve, absent ones fall through to the tofu box.
///
/// These ranges are chosen to **not** overlap the geometric-owned blocks (box
/// drawing `U+2500..=257F`, block elements `U+2580..=259F`, Braille
/// `U+2800..=28FF`, Legacy Computing `U+1FB00..=1FBFF`), so geometric
/// precedence stays unambiguous. Geometric Shapes therefore starts at `U+25A0`,
/// immediately after the block-elements range.
///
/// Note on the bundled *Symbols Nerd Font Mono* face: it is an icon-only face
/// and covers only a sparse subset of these blocks (the angle brackets
/// `U+276C..=2771` in Dingbats, the IEC power symbols `U+23FB..=23FE` in
/// Miscellaneous Technical, `U+2630`/`U+2665`/`U+26A1` in Miscellaneous
/// Symbols, and `U+2B58` in Miscellaneous Symbols and Arrows). Naming the whole
/// blocks here lets a fuller override / system Nerd Font resolve the remainder
/// without re-touching the classifier.
pub const SYMBOL_BLOCKS: &[SymbolRange] = &[
    SymbolRange {
        name: "Arrows",
        start: 0x2190,
        end: 0x21FF,
    },
    SymbolRange {
        name: "Miscellaneous Technical",
        start: 0x2300,
        end: 0x23FF,
    },
    SymbolRange {
        name: "Geometric Shapes",
        start: 0x25A0,
        end: 0x25FF,
    },
    SymbolRange {
        name: "Miscellaneous Symbols",
        start: 0x2600,
        end: 0x26FF,
    },
    SymbolRange {
        name: "Dingbats",
        start: 0x2700,
        end: 0x27BF,
    },
    SymbolRange {
        name: "Miscellaneous Symbols and Arrows",
        start: 0x2B00,
        end: 0x2BFF,
    },
];

/// Whether `ch` is a symbol/icon codepoint worth trying the symbol / Nerd-font
/// fallback for.
///
/// This is a **gate to attempt** the fallback, not a guarantee a glyph exists:
/// the atlas still calls `font_has_glyph` on the configured fallback face, so a
/// codepoint no symbol font provides falls through to the hollow-box tofu slot
/// exactly as before.
///
/// Returns `true` for:
/// - the whole Private Use Area: BMP PUA `U+E000..=U+F8FF` (classic Nerd Font
///   sets) and supplementary PUA-A `U+F0000..=U+FFFFD` (Material Design Icons);
/// - the standard symbol/pictograph blocks in [`SYMBOL_BLOCKS`] (arrows, power
///   symbols, geometric shapes, Dingbats including the prompt char `U+276F ❯`,
///   …).
///
/// Returns `false` for ordinary text (Latin, Latin-1, CJK, …), the
/// geometric-owned ranges (box drawing, block elements, Braille, Legacy
/// Computing — those are drawn by [`crate::boxdraw`] and never reach here), and
/// the replacement codepoint `U+10FFFD` (kept a non-symbol so its hollow-box
/// fallback is preserved). Plane-16 PUA-B is intentionally excluded: Nerd Fonts
/// do not use it.
pub fn is_symbol_codepoint(ch: char) -> bool {
    let c = ch as u32;
    if (0xE000..=0xF8FF).contains(&c) || (0xF0000..=0xFFFFD).contains(&c) {
        return true;
    }
    SYMBOL_BLOCKS.iter().any(|b| (b.start..=b.end).contains(&c))
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
    fn prompt_chevron_is_symbol() {
        // The must-fix: starship's `❯` (Dingbats) was tofu under the old
        // PUA-only classifier. It must now be classified as a symbol so the
        // fallback is attempted; the bundled Symbols Nerd Font Mono covers it.
        assert!(is_symbol_codepoint('\u{276F}'));
        // Its angle-bracket neighbors the bundled face also covers.
        for ch in ['\u{276C}', '\u{276D}', '\u{276E}', '\u{2770}', '\u{2771}'] {
            assert!(is_symbol_codepoint(ch), "{ch:?} not classified as symbol");
        }
    }

    #[test]
    fn standard_symbol_blocks_are_symbol() {
        // One representative codepoint per newly-added block. These are
        // attempt-the-fallback gates; whether a glyph resolves depends on the
        // installed face, but the classifier must say "try".
        for ch in [
            '\u{2190}', // Arrows: leftwards arrow ←
            '\u{2399}', // Misc Technical: print screen ⎙
            '\u{23FB}', // Misc Technical: IEC power ⏻ (bundled face has it)
            '\u{25A0}', // Geometric Shapes: black square ■ (block-element edge+1)
            '\u{25B6}', // Geometric Shapes: black right triangle ▶
            '\u{2630}', // Misc Symbols: trigram ☰ (bundled face has it)
            '\u{26A1}', // Misc Symbols: high voltage ⚡ (bundled face has it)
            '\u{2705}', // Dingbats: white heavy check ✅
            '\u{2B58}', // Misc Symbols and Arrows: heavy circle ⭘ (bundled has it)
        ] {
            assert!(is_symbol_codepoint(ch), "{ch:?} not classified as symbol");
        }
    }

    #[test]
    fn geometric_owned_ranges_are_not_symbol() {
        // The geometric path (boxdraw) owns these; the classifier must never
        // claim them, so precedence between geometric and fallback stays
        // unambiguous and no symbol block edge overlaps them.
        for ch in [
            '\u{2500}',  // box drawing ─
            '\u{257F}',  // box drawing end
            '\u{2580}',  // block elements ▀
            '\u{259F}',  // block elements end
            '\u{2588}',  // full block █
            '\u{2800}',  // Braille blank
            '\u{28FF}',  // Braille end
            '\u{1FB00}', // Legacy Computing start
            '\u{1FBFF}', // Legacy Computing end
        ] {
            assert!(
                !is_symbol_codepoint(ch),
                "{ch:?} misclassified as symbol (geometric-owned)"
            );
        }
        // The Geometric Shapes block must start exactly one past block elements.
        assert!(!is_symbol_codepoint('\u{259F}'));
        assert!(is_symbol_codepoint('\u{25A0}'));
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
        // Codepoints in the gaps between symbol blocks stay non-symbol.
        assert!(!is_symbol_codepoint('\u{2200}')); // Mathematical Operators
        assert!(!is_symbol_codepoint('\u{2C00}')); // Glagolitic (past Misc Sym & Arrows)
    }

    #[test]
    fn documented_pua_ranges_are_within_the_predicate() {
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

    #[test]
    fn symbol_blocks_are_within_the_predicate_and_disjoint_from_geometric() {
        // Each standard symbol block must be fully classified as symbol, well
        // formed, and never intrude on a geometric-owned range.
        let geometric: &[(u32, u32)] = &[
            (0x2500, 0x257F),
            (0x2580, 0x259F),
            (0x2800, 0x28FF),
            (0x1FB00, 0x1FBFF),
        ];
        for b in SYMBOL_BLOCKS {
            assert!(b.start <= b.end, "{} range inverted", b.name);
            for c in [b.start, (b.start + b.end) / 2, b.end] {
                let ch = char::from_u32(c).expect("valid codepoint");
                assert!(
                    is_symbol_codepoint(ch),
                    "{} U+{c:04X} not covered by is_symbol_codepoint",
                    b.name
                );
            }
            for &(gs, ge) in geometric {
                assert!(
                    b.end < gs || b.start > ge,
                    "{} U+{:04X}..={:04X} overlaps geometric U+{gs:04X}..={ge:04X}",
                    b.name,
                    b.start,
                    b.end
                );
            }
        }
    }
}
