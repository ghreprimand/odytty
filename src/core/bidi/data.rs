// SPDX-License-Identifier: GPL-3.0-only
//! Unicode 17.0.0 bidi data for `unicode-bidi`.
//!
//! The crate's bundled tables are Unicode 16.0.0 and its `hardcoded-data`
//! feature is disabled, so every class and bracket lookup in level resolution
//! goes through [`Unicode17`], backed by the generated tables in
//! [`super::classes`] and [`super::brackets`]. Code points absent from the class
//! table are L, matching the general @missing default of
//! DerivedBidiClass.txt; the block-level @missing defaults (R, AL, ET, and BN
//! for unassigned code points) are already expanded into the table.

use std::cmp::Ordering;

use unicode_bidi::BidiClass;
use unicode_bidi::data_source::{BidiDataSource, BidiMatchedOpeningBracket};

use super::brackets::BIDI_BRACKETS;
use super::classes::BIDI_CLASSES;

/// Unicode 17.0.0 Bidi_Class and Bidi_Paired_Bracket lookups.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Unicode17;

impl BidiDataSource for Unicode17 {
    fn bidi_class(&self, c: char) -> BidiClass {
        bidi_class(c)
    }

    fn bidi_matched_opening_bracket(&self, c: char) -> Option<BidiMatchedOpeningBracket> {
        let index = BIDI_BRACKETS
            .binary_search_by(|(bracket, _, _)| bracket.cmp(&c))
            .ok()?;
        let (_, opening, is_open) = BIDI_BRACKETS[index];
        Some(BidiMatchedOpeningBracket { opening, is_open })
    }
}

/// The Unicode 17.0.0 Bidi_Class of `c`.
pub(super) fn bidi_class(c: char) -> BidiClass {
    BIDI_CLASSES
        .binary_search_by(|(start, end, _)| {
            if *end < c {
                Ordering::Less
            } else if *start > c {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .map_or(BidiClass::L, |index| BIDI_CLASSES[index].2)
}
