// SPDX-License-Identifier: GPL-3.0-only
//! Bounded measured script width units, using Unicode 17 properties.
//! Gurmukhi, Tamil, Sinhala, Chakma, and Grantha units can cross a UAX #29 break.
//! This module does not shape glyphs or change other script groups.
mod data;

fn script(ranges: &[(u32, u32, u8)], ch: char) -> u8 {
    let cp = ch as u32;
    let index = ranges.partition_point(|&(_, end, _)| end < cp);
    ranges
        .get(index)
        .filter(|&&(start, _, _)| start <= cp)
        .map_or(0, |&(_, _, sid)| sid)
}
fn is_extend(ch: char) -> bool {
    let cp = ch as u32;
    let index = data::EXTEND.partition_point(|&(_, end, _)| end < cp);
    data::EXTEND
        .get(index)
        .is_some_and(|&(start, _, _)| start <= cp)
}

/// A spacing sign or measured linked consonant extends its same-script owner.
/// Chakma U+11134 is a bounded Pure_Killer exception for direct consonant links.
/// Khmer/Myanmar also use InCB Consonant, including linked independent vowels.
/// Zero-width extensions already take the common streaming path.
pub(super) fn extends(base: char, extensions: &[char], next: char) -> bool {
    let sid = script(data::BASES, base);
    if sid == 0 {
        return false;
    }
    if script(data::SPACING, next) == sid {
        return true;
    }
    if script(data::CONSONANTS, next) != sid {
        return false;
    }
    let mut linker = false;
    for ch in extensions
        .iter()
        .rev()
        .copied()
        .chain(std::iter::once(base))
    {
        if ch == '\u{200c}' {
            return false;
        }
        let linking_script = script(data::LINKERS, ch);
        if linking_script != 0 {
            if linking_script != sid {
                return false;
            }
            linker = true;
        } else if ch == '\u{200d}' {
            // U+11134 is Chakma Pure_Killer, not a virama. Its frozen direct
            // consonant sequence is bounded here; ZWJ keeps the prior split.
            if script(data::LINKERS, '\u{11134}') == sid
                && extensions
                    .iter()
                    .rev()
                    .find(|&&scalar| script(data::LINKERS, scalar) != 0)
                    == Some(&'\u{11134}')
            {
                return false;
            }
            continue;
        } else if is_extend(ch) {
            continue;
        } else {
            return linker && script(data::CONSONANTS, ch) == sid;
        }
    }
    false
}

/// A spacing mark or retained conjunct promotes a measured Indic base to two
/// cells, regardless of further linked consonants. Nonspacing marks add zero.
pub(super) fn has_two_cell_footprint(base: char, extensions: &[char]) -> bool {
    let sid = script(data::BASES, base);
    sid != 0
        && extensions
            .iter()
            .any(|&ch| script(data::SPACING, ch) == sid || script(data::CONSONANTS, ch) == sid)
}
