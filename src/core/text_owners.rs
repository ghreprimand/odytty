// SPDX-License-Identifier: GPL-3.0-only
//! Owner segmentation of plain text, the way the screen's print path lays it
//! out: the same width, attachment, script-extension, emoji-sequence and
//! retention-bound rules decide where one terminal owner ends and the next
//! begins and how many columns each takes. UI surfaces that print text
//! outside the terminal grid (overlay rows, previews) measure, cut and paint
//! by these owners, so an emoji sequence or a script cluster is one owner
//! there exactly as it is in the grid. A unit test pins the agreement with
//! the print path.

use super::char_width::{
    char_display_width, is_default_ignorable, owner_display_width, thai_lao_spacing_extension,
};
use super::{Attrs, Cell};

/// The owners of `text` in order: each owner's cell (base scalar plus the
/// scalars it retains, default attributes) and the columns it takes, 1 or 2.
/// A control character ends the open owner and is dropped, as the print path
/// ends a cluster at any control. An unattached default-ignorable scalar of
/// width zero is dropped, as the print path drops it.
pub(crate) fn text_owners(text: &str, ambiguous_wide: bool) -> Vec<(Cell, usize)> {
    let mut owners: Vec<(Cell, usize)> = Vec::new();
    let mut open = false;
    for ch in text.chars() {
        if ch.is_control() {
            open = false;
            continue;
        }
        let width = char_display_width(ch, ambiguous_wide);
        if open
            && let Some((cell, owner_width)) = owners.last_mut()
            && (width == 0
                || thai_lao_spacing_extension(cell.ch, ch)
                || super::indic::extends(cell.ch, cell.combining(), ch)
                || super::emoji_width::extends(cell.ch, cell.combining(), ch))
            && cell.push_combining(ch)
        {
            *owner_width = owner_display_width(cell.ch, cell.combining(), ambiguous_wide).max(1);
            continue;
        }
        if width == 0 && !open && is_default_ignorable(ch) {
            continue;
        }
        owners.push((Cell::new(ch, Attrs::default()), width.max(1)));
        open = true;
    }
    owners
}

#[cfg(test)]
mod tests {
    use super::text_owners;
    use crate::core::Terminal;

    /// Owners and widths agree with the terminal's own print path over
    /// combining marks, emoji sequences (ZWJ, VS16, VS15, modifiers, keycaps,
    /// flags, lone regional indicators), Indic, Thai, Khmer and wide text,
    /// with both ambiguous-width policies.
    #[test]
    fn owners_match_the_print_path() {
        let corpus = [
            "e\u{301}x\u{300}\u{301}",
            "\u{1f469}\u{200d}\u{1f4bb}a\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{2764}\u{fe0f}\u{2764}\u{fe0e}\u{2764}",
            "\u{1f44d}\u{1f3fd}\u{1f44d}",
            "1\u{fe0f}\u{20e3}#\u{20e3}",
            "\u{1f1fa}\u{1f1f8}\u{1f1fa}x",
            "\u{915}\u{94d}\u{937}\u{93f}\u{928}\u{94d}\u{926}\u{940}",
            "\u{b95}\u{bcd}\u{bb7}\u{bc8}",
            "\u{e01}\u{e33}\u{e01}\u{e48}\u{e33}\u{e40}\u{e01}",
            "\u{1780}\u{17d2}\u{179a}\u{17bb}",
            "\u{4e00}a\u{4e8c}\u{3000}",
            "\u{301}lead\u{200d}x",
            "\u{fe0f}\u{200b}after",
            "\u{a1}\u{2026}\u{b0}",
            &format!("a{}", "\u{301}".repeat(20)),
        ];
        for ambiguous_wide in [false, true] {
            for text in corpus {
                let mut terminal = Terminal::new(120, 2);
                terminal.set_ambiguous_wide(ambiguous_wide);
                terminal.advance(text.as_bytes());
                let snapshot = terminal.snapshot();
                let row = &snapshot.cells[..120];
                let mut grid = Vec::new();
                let mut column = 0;
                while column < snapshot.cursor.column {
                    let cell = row[column];
                    let width = if row.get(column + 1).is_some_and(|c| c.wide_continuation) {
                        2
                    } else {
                        1
                    };
                    grid.push((cell.ch, cell.combining().to_vec(), width));
                    column += width;
                }
                let owners: Vec<_> = text_owners(text, ambiguous_wide)
                    .into_iter()
                    .map(|(cell, width)| (cell.ch, cell.combining().to_vec(), width))
                    .collect();
                assert_eq!(owners, grid, "{text:?} ambiguous_wide={ambiguous_wide}");
            }
        }
    }

    /// A control ends the open owner: a mark after it starts its own owner.
    #[test]
    fn a_control_ends_the_open_owner() {
        let owners = text_owners("e\r\u{301}", false);
        assert_eq!(owners.len(), 2);
        assert_eq!(owners[1].0.ch, '\u{301}');
    }
}
