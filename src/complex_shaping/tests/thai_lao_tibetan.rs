// SPDX-License-Identifier: GPL-3.0-only
//! Stage 5: Thai, Lao, and Tibetan classifier checks, and proof that the
//! font's shaping tables change these owners beyond the per-cell path. The
//! group's reference and pixel checks run with every enabled group in the
//! parent module's `GROUPS` table.

use super::*;

/// Structural rows of the group's fixtures.
pub(super) const STRUCTURAL: &[&str] = &[
    "sara-am",
    "tone-stack",
    "below-base-vowel",
    "tone-vowel-stack",
    "tone",
    "subjoined-stack",
    "stacked-vowel",
];

#[test]
fn shaping_substitutes_or_positions_every_structural_thai_lao_and_tibetan_row() {
    // The per-cell path draws each scalar's nominal glyph, marks at the
    // base's pen. Every structural row must substitute a glyph (SARA AM
    // decomposition, tone-mark and descender alternates, precomposed
    // Tibetan stacks) or move one (stacked tone marks, vowel signs).
    let group = GROUPS
        .iter()
        .find(|group| group.name == "thai-lao-tibetan")
        .expect("enabled group");
    let mut seen = Vec::new();
    for row in group_rows(group) {
        if !STRUCTURAL.contains(&row.note.as_str()) {
            continue;
        }
        let face = face(&row.font);
        let nominal: Vec<u16> = row.text.chars().map(|ch| face.glyph_id(ch).0).collect();
        let substituted = row.glyphs != nominal;
        let positioned = row.x_offset.iter().chain(&row.y_offset).any(|&v| v != 0);
        assert!(substituted || positioned, "{row:?}");
        seen.push(row.note.clone());
    }
    for note in STRUCTURAL {
        assert!(seen.iter().any(|seen| seen == note), "{note}");
    }
}

#[test]
fn classifier_enables_thai_lao_and_tibetan() {
    let attrs = crate::core::Attrs::default();
    for ch in [
        '\u{0E01}', '\u{0E33}', '\u{0E81}', '\u{0EB3}', '\u{0F40}', '\u{0F66}',
    ] {
        assert!(owner_is_eligible(&Cell::new(ch, attrs)), "{ch:?}");
    }
    // Later stage groups stay out.
    for ch in ['\u{0D9A}', '\u{11103}'] {
        assert!(!owner_is_eligible(&Cell::new(ch, attrs)), "{ch:?}");
    }
    let mut stack = Cell::new('\u{0F66}', attrs);
    for mark in ['\u{0F90}', '\u{0FB1}', '\u{0F7C}'] {
        assert!(stack.push_combining(mark));
    }
    assert!(owner_is_eligible(&stack));
    let mut tones = Cell::new('\u{0E1B}', attrs);
    assert!(tones.push_combining('\u{0E34}'));
    assert!(tones.push_combining('\u{0E48}'));
    assert!(owner_is_eligible(&tones));
    // A retained scalar from outside every enabled group keeps the per-cell
    // path.
    let mut mixed = Cell::new('\u{0E01}', attrs);
    assert!(mixed.push_combining('\u{0301}'));
    assert!(!owner_is_eligible(&mixed));
}
