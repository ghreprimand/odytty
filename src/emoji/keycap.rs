// SPDX-License-Identifier: GPL-3.0-only
//! Feed keycap selectors like selectors on pictographic emoji, retaining source offsets.

use swash::FontRef;
use swash::shape::Shaper;
use swash::text::cluster::{CharCluster, Parser, Token};
use swash::text::{Codepoint, Script};

pub(super) fn add_emoji_text(shaper: &mut Shaper<'_>, font: FontRef<'_>, text: &str) {
    let mut chars = text.chars();
    let is_vs16_keycap = matches!(chars.next(), Some('#' | '*' | '0'..='9'))
        && chars.next() == Some('\u{fe0f}')
        && chars.next() == Some('\u{20e3}')
        && chars.next().is_none();
    if !is_vs16_keycap || font.charmap().map('\u{fe0f}') != 0 {
        shaper.add_str(text);
        return;
    }

    // Swash drops selectors in its pictographic emoji parser, but keycap
    // bases use its ordinary parser. An unmapped VS16 there becomes an
    // intervening glyph zero and blocks the base + enclosing-keycap GSUB
    // ligature. VS16 selects the color route before this seam. Omit it only
    // from shaping this exact sequence, preserving original byte offsets and
    // the full source range. Logical text, owner width, and cache identity
    // continue to include the selector.
    let mut parser = Parser::new(
        Script::Common,
        text.char_indices()
            .filter(|(_, ch)| *ch != '\u{fe0f}')
            .map(|(offset, ch)| Token {
                ch,
                offset: offset as u32,
                len: ch.len_utf8() as u8,
                info: ch.properties().into(),
                data: 0,
            }),
    );
    let mut cluster = CharCluster::new();
    let charmap = font.charmap();
    while parser.next(&mut cluster) {
        cluster.map(|ch| charmap.map(ch));
        shaper.add_cluster(&cluster);
    }
}
