// SPDX-License-Identifier: GPL-3.0-only
//! Which scalar of nonzero width extends an open owner, and the width of an
//! owner the general rule formed. Shared by the print path
//! (`screen/cluster.rs`) and its twin for UI text (`text_owners.rs`).
//!
//! The tuned rules decide first, unchanged: Thai and Lao SARA AM, the
//! measured Indic and Southeast Asian groups, and emoji sequences. Every
//! other script follows Unicode 17 extended grapheme clusters (UAX #29)
//! plus the generalized virama link (a same-script Consonant after a Virama
//! or Invisible_Stacker), so a conjunct or a base with its spacing signs is
//! one owner that shaping can draw whole. Zero-width scalars attach as they
//! always have, before these rules are consulted.
//!
//! Profile points where an owner differs from a Unicode cluster: an owner
//! never exceeds two cells, so a cluster whose measured width would (such as
//! two leading Hangul jamo) splits where the width would overflow; emoji
//! sequences keep their own ownership; the tuned groups keep theirs; an
//! owner holds at most seventeen scalars.

use super::char_width::{char_display_width, thai_lao_spacing_extension};
use super::grapheme::{self, GraphemeState};

const ZWJ: char = '\u{200d}';

/// Whether `next`, a scalar of nonzero width, extends the owner `base` plus
/// `extensions`.
pub(crate) fn extends(base: char, extensions: &[char], next: char, ambiguous_wide: bool) -> bool {
    if thai_lao_spacing_extension(base, next) {
        return true;
    }
    let (indic, tuned_base) = super::indic::extends_scoped(base, extensions, next);
    if indic || super::emoji_width::extends(base, extensions, next) {
        return true;
    }
    // A tuned group's letter is outside the general rule's scope.
    !tuned_base && general_extends(base, extensions, next, ambiguous_wide)
}

fn general_extends(base: char, extensions: &[char], next: char, ambiguous_wide: bool) -> bool {
    // Cheap rejections first: almost every scalar of nonzero width starts an
    // owner, and one lookup usually shows it. A scalar no cluster rule
    // continues with joins only after a Prepend, or, as a consonant, after a
    // linker (so never a bare base).
    match grapheme::continuation(next) {
        grapheme::Continuation::Emoji => return false,
        grapheme::Continuation::Possible => {}
        continuation => {
            let last = extensions.last().copied().unwrap_or(base);
            let linked = !extensions.is_empty()
                && (continuation == grapheme::Continuation::Consonant
                    || super::indic::is_linked_consonant(next));
            if !linked && !grapheme::is_prepend(last) {
                return false;
            }
        }
    }
    if !in_general_scope(next) {
        return false;
    }
    let joins = GraphemeState::of_owner(base, extensions).accepts(next)
        || super::indic::general_link(base, extensions, next);
    joins
        && std::iter::once(base)
            .chain(extensions.iter().copied())
            .all(in_general_scope)
        && measured_width(
            std::iter::once(base)
                .chain(extensions.iter().copied())
                .chain(std::iter::once(next)),
            ambiguous_wide,
        ) <= 2
}

/// Scalars the general rule may own: none from a tuned script and none that
/// an emoji sequence owns (pictographs, regional indicators, modifiers).
fn in_general_scope(ch: char) -> bool {
    !matches!(ch, '\u{1f3fb}'..='\u{1f3ff}')
        && !grapheme::is_emoji_sequence_scalar(ch)
        && !super::indic::in_tuned_script(ch)
}

/// Columns of an owner the general rule may have extended, measured the way
/// the frozen width oracle (wcwidth 0.9.1 `wcswidth`) measures the same
/// scalars: a linker adds nothing and the next scalar of nonzero width after
/// it completes a two-cell conjunct; a spacing mark after a measured base
/// makes it two cells; other scalars of nonzero width start a new measured
/// unit. Owners whose extensions all have width zero keep the base width, as
/// they always have.
pub(crate) fn general_width(base: char, extensions: &[char], ambiguous_wide: bool) -> usize {
    if extensions
        .iter()
        .all(|&ch| char_display_width(ch, ambiguous_wide) == 0)
    {
        return char_display_width(base, ambiguous_wide);
    }
    measured_width(
        std::iter::once(base).chain(extensions.iter().copied()),
        ambiguous_wide,
    )
}

/// The oracle measurement of [`general_width`] over an owner's scalars.
fn measured_width(scalars: impl Iterator<Item = char>, ambiguous_wide: bool) -> usize {
    let (mut total, mut unit, mut after_linker) = (0usize, 0usize, false);
    for ch in scalars {
        if ch == ZWJ {
            continue;
        }
        if super::indic::is_linker(ch) {
            after_linker = true;
            continue;
        }
        let width = char_display_width(ch, ambiguous_wide);
        if width == 0 {
            after_linker = false;
        } else if after_linker {
            unit = 2;
            after_linker = false;
        } else if grapheme::is_spacing_mark(ch) {
            unit = if unit > 0 { 2 } else { width };
        } else {
            total += unit;
            unit = width;
        }
    }
    total + unit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_width_follows_the_width_oracle() {
        // Values from wcwidth 0.9.1 `wcswidth` for the same scalars.
        for (text, width) in [
            ("\u{1b13}\u{1b3e}", 2),
            ("\u{1b13}\u{1b3e}\u{1b3f}", 2),
            ("\u{1b13}\u{1b44}\u{1b13}", 2),
            ("\u{11412}\u{11442}\u{11412}", 2),
            ("\u{11412}\u{11442}\u{11412}\u{11442}\u{11412}", 2),
            ("\u{0600}\u{0661}", 2),
            ("\u{1100}\u{1100}", 4),
            ("\u{4e00}\u{1b3e}", 2),
            ("e\u{301}", 1),
        ] {
            let mut chars = text.chars();
            let base = chars.next().unwrap();
            let extensions: Vec<char> = chars.collect();
            assert_eq!(general_width(base, &extensions, false), width, "{text:?}");
        }
    }

    #[test]
    fn tuned_and_emoji_scalars_stay_outside_the_general_rule() {
        // Devanagari visarga after Latin, a skin tone after a letter, and a
        // regional indicator pair are not general joins.
        assert!(!general_extends('a', &[], '\u{903}', false));
        assert!(!general_extends('a', &[], '\u{1f3fb}', false));
        assert!(!general_extends('\u{1f1e6}', &[], '\u{1f1e8}', false));
        assert!(general_extends('a', &[], '\u{1b3e}', false));
    }
}
