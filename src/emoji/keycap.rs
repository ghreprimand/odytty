// SPDX-License-Identifier: GPL-3.0-only
//! Feed keycap selectors like selectors on pictographic emoji, retaining source offsets.

use swash::FontRef;
use swash::shape::{ShapeContext, Shaper};
use swash::text::cluster::{CharCluster, Parser, Token};
use swash::text::{Codepoint, Script};

pub(super) fn add_emoji_text(shaper: &mut Shaper<'_>, font: FontRef<'_>, text: &str) {
    if !is_vs16_keycap(text) || keeps_selector(font, text) {
        shaper.add_str(text);
        return;
    }
    add_without_selector(shaper, font, text);
}

/// `text` is exactly a keycap base, VS16 and U+20E3.
fn is_vs16_keycap(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some('#' | '*' | '0'..='9'))
        && chars.next() == Some('\u{fe0f}')
        && chars.next() == Some('\u{20e3}')
        && chars.next().is_none()
}

/// Whether a VS16 keycap is shaped with its selector. Swash drops selectors
/// in its pictographic emoji parser, but keycap bases use its ordinary
/// parser, where VS16 stays in the glyph stream.
///
/// - An unmapped VS16 becomes an intervening glyph zero that blocks the base
///   + enclosing-keycap ligature, so it is omitted.
/// - A mapped VS16 is kept when the font's ligature consumes it (a GSUB rule
///   naming base + VS16 + keycap). A font whose ligature names only base +
///   keycap and treats the selector as default-ignorable, as Apple Color
///   Emoji's AAT `morx` table does under CoreText and HarfBuzz, forms its
///   ligature only once the selector is omitted, so it is omitted when that
///   alone yields one glyph.
///
/// VS16 selects the color route before this seam. Omitting it only from
/// shaping this exact sequence preserves the original byte offsets and the
/// full source range; logical text, owner width, and cache identity continue
/// to include the selector.
fn keeps_selector(font: FontRef<'_>, text: &str) -> bool {
    if font.charmap().map('\u{fe0f}') == 0 {
        return false;
    }
    single_glyph(font, text, false) || !single_glyph(font, text, true)
}

/// Whether shaping `text` (without its selector when `omit`) yields exactly
/// one nonzero glyph.
fn single_glyph(font: FontRef<'_>, text: &str, omit: bool) -> bool {
    let mut context = ShapeContext::new();
    let mut shaper = context.builder(font).script(Script::Common).build();
    if omit {
        add_without_selector(&mut shaper, font, text);
    } else {
        shaper.add_str(text);
    }
    let mut glyphs = Vec::new();
    shaper.shape_with(|cluster| glyphs.extend(cluster.glyphs.iter().map(|glyph| glyph.id)));
    matches!(glyphs.as_slice(), [id] if *id != 0)
}

fn add_without_selector(shaper: &mut Shaper<'_>, font: FontRef<'_>, text: &str) {
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

#[cfg(test)]
pub(super) fn keeps_selector_for_test(font: FontRef<'_>, text: &str) -> bool {
    !is_vs16_keycap(text) || keeps_selector(font, text)
}
