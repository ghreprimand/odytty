// SPDX-License-Identifier: GPL-3.0-only
//! Bounded measured script width units, using Unicode 17 properties.
//! Gurmukhi, Tamil, Sinhala, Chakma, and Grantha units can cross a UAX #29 break.
//! Every other script with a Virama or Invisible_Stacker takes one general
//! rule, [`general_link`]. This module does not shape glyphs.
mod data;
mod linked;

fn script(ranges: &[(u32, u32, u8)], ch: char) -> u8 {
    // The pinned script tables contain no ASCII property members.
    if ch.is_ascii() {
        return 0;
    }
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
/// Returns whether `next` extends, and whether `base` is a letter of a tuned
/// group (so no other owner rule applies to it), from one table lookup.
pub(super) fn extends_scoped(base: char, extensions: &[char], next: char) -> (bool, bool) {
    let sid = script(data::BASES, base);
    if sid == 0 {
        return (false, false);
    }
    (extends_tuned(sid, base, extensions, next), true)
}

fn extends_tuned(sid: u8, base: char, extensions: &[char], next: char) -> bool {
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
        } else if linker && retains_sign_before_stacker(sid, ch) {
            // Tai Tham spellings place a medial or spacing vowel sign before
            // U+1A60 SAKOT; the stack still belongs to the earlier consonant.
            continue;
        } else {
            return linker && script(data::CONSONANTS, ch) == sid;
        }
    }
    false
}

/// Whether `ch` belongs to a script whose ownership stays with its tuned
/// rules: the measured groups here, and Thai and Lao.
pub(super) fn in_tuned_script(ch: char) -> bool {
    script(linked::TUNED_SCRIPTS, ch) != 0
}

/// A Virama or Invisible_Stacker of any script, tuned or general.
pub(super) fn is_linker(ch: char) -> bool {
    script(linked::LINKED_LINKERS, ch) != 0 || script(data::LINKERS, ch) != 0
}

/// A Consonant of a script the generalized virama link covers.
pub(super) fn is_linked_consonant(ch: char) -> bool {
    script(linked::LINKED_CONSONANTS, ch) != 0
}

/// The generalized virama link for scripts outside the tuned groups: a
/// Consonant of the owner's script joins when the owner's last non-extend
/// scalar is a linker of that script. Nonspacing extends and ZWJ between
/// them keep the link; ZWNJ breaks it.
pub(super) fn general_link(base: char, extensions: &[char], next: char) -> bool {
    let sid = script(linked::LINKED_CONSONANTS, next);
    if sid == 0 || script(linked::SCRIPTS, base) != sid {
        return false;
    }
    for &ch in extensions.iter().rev() {
        if ch == '\u{200c}' {
            return false;
        }
        let linker = script(linked::LINKED_LINKERS, ch);
        if linker != 0 {
            return linker == sid;
        }
        if ch != '\u{200d}' && !is_extend(ch) {
            return false;
        }
    }
    false
}

/// A retained Tai Tham spacing sign followed, after zero-width marks, by SAKOT
/// stays inside the owner during the reverse linker scan. Other scripts keep a
/// retained spacing sign as the scan boundary: the frozen conformance corpus
/// shows no failure of this shape for them, so they are not widened here.
fn retains_sign_before_stacker(sid: u8, ch: char) -> bool {
    script(data::LINKERS, '\u{1a60}') == sid && script(data::SPACING, ch) == sid
}

/// A spacing mark or retained conjunct promotes a measured Indic base to two
/// cells, regardless of further linked consonants. Nonspacing marks add zero.
/// Returns whether the owner takes two cells by this rule, and whether
/// `base` is a tuned group's letter, from one table lookup.
pub(super) fn two_cell_footprint_scoped(base: char, extensions: &[char]) -> (bool, bool) {
    let sid = script(data::BASES, base);
    let two = sid != 0
        && extensions
            .iter()
            .any(|&ch| script(data::SPACING, ch) == sid || script(data::CONSONANTS, ch) == sid);
    (two, sid != 0)
}

#[cfg(test)]
mod tests {
    use super::data;

    #[test]
    fn script_property_ranges_exclude_ascii() {
        for (name, ranges) in [
            ("BASES", data::BASES),
            ("SPACING", data::SPACING),
            ("CONSONANTS", data::CONSONANTS),
            ("LINKERS", data::LINKERS),
        ] {
            for &(start, _, _) in ranges {
                assert!(start >= 0x80, "{name} contains an ASCII property member");
            }
        }
    }
}
