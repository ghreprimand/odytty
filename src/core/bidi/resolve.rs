// SPDX-License-Identifier: GPL-3.0-only
//! The UAX #9 steps OdyTTY runs itself around the `unicode-bidi` crate.
//!
//! `unicode-bidi` resolves paragraph embedding levels (rules P2 to I2). Its
//! line-level API clones the levels of the whole paragraph for every line,
//! which costs paragraph-length work per physical row, so the line rule L1 and
//! the reordering rule L2 are applied here on character and owner slices
//! instead. L1 follows the crate's own implementation exactly, including its
//! treatment of characters removed by rule X9, so the conformance corpus pins
//! both halves together. Rules L3 and L4 are not applied here: combining marks
//! never leave their owner, and mirroring is reported as a flag.

use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo};

use super::data::Unicode17;

/// Resolved paragraph levels before any line rule, one entry per scalar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParagraphLevels {
    /// Paragraph embedding level.
    pub(super) paragraph: u8,
    /// Original bidi class of each scalar.
    pub(super) classes: Vec<BidiClass>,
    /// Resolved embedding level of each scalar (rules P2 to I2).
    pub(super) levels: Vec<u8>,
}

/// Resolve `text` as one paragraph. `base` forces the paragraph level;
/// `None` applies rules P2 and P3. Never panics for any `text`.
pub(super) fn resolve_paragraph(text: &str, base: Option<u8>) -> ParagraphLevels {
    let base = base.and_then(|level| Level::new(level).ok());
    let info = ParagraphBidiInfo::new_with_data_source(&Unicode17, text, base);
    let scalars = text.chars().count();
    let mut classes = Vec::with_capacity(scalars);
    let mut levels = Vec::with_capacity(scalars);
    for (byte, _) in text.char_indices() {
        classes.push(info.original_classes[byte]);
        levels.push(info.levels[byte].number());
    }
    ParagraphLevels {
        paragraph: info.paragraph_level.number(),
        classes,
        levels,
    }
}

/// Rule L1 on one line: reset separators, and whitespace or isolate controls
/// before a separator or at the line end, to the paragraph level; characters
/// removed by X9 take the level before them. `classes` and `levels` cover the
/// line only and have equal length.
pub(super) fn apply_line_rules(classes: &[BidiClass], levels: &mut [u8], paragraph: u8) {
    use BidiClass::{B, BN, FSI, LRE, LRI, LRO, PDF, PDI, RLE, RLI, RLO, S, WS};
    let mut reset_from: Option<usize> = Some(0);
    let mut reset_to: Option<usize> = None;
    let mut previous = paragraph;
    for (index, class) in classes.iter().enumerate().take(levels.len()) {
        match class {
            B | S => {
                reset_to = Some(index + 1);
                reset_from.get_or_insert(index);
            }
            WS | FSI | LRI | RLI | PDI => {
                reset_from.get_or_insert(index);
            }
            RLE | LRE | RLO | LRO | PDF | BN => {
                reset_from.get_or_insert(index);
                levels[index] = previous;
            }
            _ => reset_from = None,
        }
        if let (Some(from), Some(to)) = (reset_from, reset_to) {
            levels[from..to].fill(paragraph);
            reset_from = None;
            reset_to = None;
        }
        previous = levels[index];
    }
    if let Some(from) = reset_from {
        levels[from..].fill(paragraph);
    }
}

/// Rule L2: the visual order of units with the given line levels, as unit
/// indices from left to right. Work is at most `levels.len()` times the
/// number of distinct odd-or-higher levels, which UAX #9 bounds at 126.
pub(super) fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let (Some(&highest), Some(&lowest)) = (levels.iter().max(), levels.iter().min()) else {
        return order;
    };
    let lowest_odd = lowest | 1;
    if highest < lowest_odd {
        return order;
    }
    let mut arranged = levels.to_vec();
    for level in (lowest_odd..=highest).rev() {
        let mut start = 0;
        while start < arranged.len() {
            if arranged[start] < level {
                start += 1;
                continue;
            }
            let mut end = start;
            while end < arranged.len() && arranged[end] >= level {
                end += 1;
            }
            order[start..end].reverse();
            arranged[start..end].reverse();
            start = end;
        }
    }
    order
}

/// Whether `class` can make any level non-zero in a paragraph forced to level
/// 0. Without one of these every scalar resolves to level 0 (weak types
/// follow the L start of sequence by W7, neutrals by N1 and N2).
pub(super) fn can_raise_level(class: BidiClass) -> bool {
    use BidiClass::{AL, AN, FSI, LRE, LRI, LRO, PDF, PDI, R, RLE, RLI, RLO};
    matches!(
        class,
        R | AL | AN | RLE | LRE | RLO | LRO | PDF | RLI | LRI | FSI | PDI
    )
}
