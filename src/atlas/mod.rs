// SPDX-License-Identifier: GPL-3.0-only
//! Monospace glyph atlas: a CPU-rasterized 8-bit coverage texture with a
//! missing-glyph fallback, the printable-ASCII block, and a growable dynamic
//! region for other codepoints.
//!
//! This module is deliberately GPU-agnostic so it can be unit-tested without a
//! window or `wgpu` device. The native renderer (`crate::native`) uploads the
//! atlas bitmap to a texture and uses [`GlyphAtlas::glyph_quad`] (bearing-aware
//! geometry) to build per-cell quads; font loading and color resolution live in
//! [`crate::text`].
//!
//! ## Atlas layout
//!
//! The bitmap is arranged as a fixed grid of equal slots (`ATLAS_COLS` per row).
//! Every terminal cell maps onto the **inner** `cell.width × cell.height` region
//! of one slot, and [`GlyphAtlas::uv_rect`] hands out exactly that rectangle.
//!
//! Each slot reserves a border around its cell with two jobs (see
//! [`slot_border`]): an [`ATLAS_PAD`]-pixel transparent **bleed gutter** on the
//! outermost ring, and an inner **overflow margin** ([`overflow_margin`]) sized
//! from the cell. So a slot occupies
//! `(cell.width + 2·border) × (cell.height + 2·border)` pixels. The bleed gutter
//! stops sampling at non-integer scale factors from reaching a neighbor's
//! coverage; the overflow margin is drawable space into which glyph ink that
//! genuinely extends past the cell box (powerline separators, italic side
//! bearing, tall combining stacks, box-drawing joins, descenders) is rasterized
//! instead of being hard-cropped. `cell.width`/`cell.height` are unchanged by
//! the border, so per-cell layout (advance, line height) downstream is identical.
//!
//! ## Bearing-aware glyph geometry
//!
//! Rasterization records each slot's **inked pixel extent** relative to the
//! cell's inner top-left. [`GlyphAtlas::glyph_quad`] returns that extent as a
//! [`GlyphBounds`] (offset that may be negative, size that may exceed the cell,
//! plus a UV rect covering exactly the ink), so the renderer can size a glyph
//! quad to its real ink and draw overflow uncropped. The UV is derived on demand
//! because the atlas height grows as dynamic glyphs are added (which would stale
//! a stored normalized rect). The fallback box keeps full-cell bounds, so a
//! missing glyph renders exactly as before.
//!
//! Slot 0 is a synthesized **missing-glyph fallback** (a hollow box). Slots
//! `1..=95` hold printable ASCII (`0x20..=0x7E`), rasterized at build time.
//! Slots beyond that are a **dynamic region**: non-ASCII glyphs are rasterized
//! on demand by [`GlyphAtlas::ensure`], appending pages of rows when the region
//! fills (no eviction — existing slots never move, so resident UV rects stay
//! valid across growth). A font-size or font-family change is a full rebuild
//! ([`GlyphAtlas::build`]); there is no in-place resize, so glyphs of different
//! sizes can never coexist in one atlas.
//!
//! ## Live vs. dynamic paths
//!
//! [`GlyphAtlas::uv_rect`] is the **immutable** lookup used by the renderer's
//! per-frame geometry builder: printable ASCII resolves to its real glyph and
//! every other printable codepoint resolves to the fallback box. The
//! **mutable** [`GlyphAtlas::ensure`] additionally rasterizes the real glyph for
//! a non-ASCII codepoint, growing the atlas and flagging [`GlyphAtlas::take_dirty`]
//! so the native layer can re-upload the texture. Until that native re-upload
//! seam exists, the live path renders ASCII plus fallback boxes; the dynamic
//! region is exercised by tests and is groundwork for live non-ASCII rendering
//! and the future font-family setting.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};
use unicode_width::UnicodeWidthChar;

pub mod fallback;

/// First and last printable ASCII code points covered by the atlas.
const FIRST_CHAR: u32 = 0x20;
const LAST_CHAR: u32 = 0x7E;
/// Number of atlas cells per row in the bitmap grid.
const ATLAS_COLS: u32 = 16;
/// Transparent gutter, in pixels, around every atlas slot. At least 1 so
/// bilinear sampling at non-integer scale factors cannot reach a neighbor
/// slot's coverage, and so bearing-driven edge overflow (box-drawing joins,
/// descenders) lands in the gutter instead of being hard-cropped.
const ATLAS_PAD: u32 = 1;
/// Slot index of the synthesized missing-glyph fallback (hollow box).
const FALLBACK_SLOT: u32 = 0;
/// Number of cell-rows appended each time the dynamic region fills.
const ATLAS_GROW_ROWS: u32 = 4;
/// Hard cap on total slots so a pathological stream of distinct codepoints
/// cannot grow the atlas without bound. Beyond this, new glyphs use the
/// fallback box instead of consuming a slot.
const MAX_ATLAS_SLOTS: u32 = 8192;

/// First dynamic slot: fallback (0) + 95 printable ASCII (1..=95).
const FIRST_DYNAMIC_SLOT: u32 = LAST_CHAR - FIRST_CHAR + 2;

// The gutter must be at least one pixel for the bleed guard to hold.
const _: () = assert!(ATLAS_PAD >= 1);

/// Byte length of the atlas backing bitmap for `width × height` pixels at
/// `bytes_per_pixel`. Each factor is widened to `usize` BEFORE multiplying:
/// at large HiDPI cells (~288 px physical: the 72 px font cap × 4.0 scale) a
/// full [`MAX_ATLAS_SLOTS`] subpixel atlas exceeds `u32::MAX` bytes, so a
/// `u32` multiply overflows — panicking in debug builds and under-allocating
/// in release (out-of-bounds raster writes into a too-short buffer).
fn atlas_byte_len(width: u32, height: u32, bytes_per_pixel: u32) -> usize {
    width as usize * height as usize * bytes_per_pixel as usize
}

/// Shear ratio for synthetic italic: `tan(12deg)`. A sample `dy` pixels above
/// the baseline is shifted right by `ITALIC_SHEAR * dy` pixels (below-baseline
/// rows shift left), producing a standard ~12-degree oblique from an upright
/// outline when no real italic face exists.
const ITALIC_SHEAR: f32 = 0.2126;

/// Rasterization scale and shared baseline for one glyph. The `baseline` is the
/// single per-atlas value (see [`GlyphAtlas::build`]) every glyph is placed on.
#[derive(Clone, Copy)]
struct Pen {
    px: f32,
    baseline: f32,
}

/// Synthetic-style transform applied while rasterizing an outline, used as a
/// fallback when the font family has no real face for a requested style. Both
/// fields default to "off" ([`SynthTransform::none`]); the real-face path and
/// [`FontStyle::Regular`] always use that, so a present face is never altered.
///
/// - `embolden_px > 0` synthesizes **bold** by double-striking each coverage
///   sample a second time shifted right by that many pixels (max-combined into
///   the atlas), thickening horizontal weight while leaving verticals, the
///   baseline, and metrics untouched.
/// - `shear != 0.0` synthesizes **italic** by shifting each sample horizontally
///   in proportion to its height above the baseline (see [`ITALIC_SHEAR`]).
///
/// Bold-italic composes both (shear, then double-strike). The smear and shear
/// land in the slot's overflow margin and are clamped by the rasterizer's clip,
/// so synthesis never changes the cell advance or leaks into a neighbor slot.
#[derive(Clone, Copy, Default)]
struct SynthTransform {
    embolden_px: u32,
    shear: f32,
}

impl SynthTransform {
    /// No synthesis: a real face (or `Regular`) is rendered as-is.
    fn none() -> Self {
        Self::default()
    }
}

/// Whether a character should resolve to a drawn glyph (real or fallback box).
/// Spaces and control characters render nothing.
fn wants_glyph(ch: char) -> bool {
    ch != ' ' && !ch.is_control()
}

/// Number of terminal cells a glyph occupies horizontally: `2` for East Asian
/// wide / fullwidth codepoints, `1` otherwise. The decision mirrors core's
/// cell-layout rule exactly (`UnicodeWidthChar::width(ch) == Some(2)` in
/// `screen.rs`/`reflow.rs`/`scrollback.rs`) so the render-side slot width never
/// diverges from where core places the `wide_continuation` spacer.
fn glyph_cells(ch: char) -> u32 {
    if UnicodeWidthChar::width(ch) == Some(2) {
        2
    } else {
        1
    }
}

/// Whether a missing primary-font glyph should try the fallback chain/runtime
/// resolver. Printable, spacing codepoints are eligible; categories that do not
/// stand alone as glyphs stay on the hollow-box/no-glyph path.
fn should_attempt_fallback(ch: char) -> bool {
    !ch.is_control()
        && !ch.is_whitespace()
        && !is_format_control(ch)
        && !is_combining_mark(ch)
        && !is_variation_selector(ch)
}

fn is_variation_selector(ch: char) -> bool {
    matches!(ch as u32, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
}

fn is_format_control(ch: char) -> bool {
    matches!(
        ch as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1345F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06EA..=0x06ED
            | 0x0711
            | 0x0730..=0x074A
            | 0x07A6..=0x07B0
            | 0x07EB..=0x07F3
            | 0x07FD
            | 0x0816..=0x0819
            | 0x081B..=0x0823
            | 0x0825..=0x0827
            | 0x0829..=0x082D
            | 0x0859..=0x085B
            | 0x0897..=0x089F
            | 0x08CA..=0x08E1
            | 0x08E3..=0x0903
            | 0x093A
            | 0x093C
            | 0x0941..=0x0948
            | 0x094D
            | 0x0951..=0x0957
            | 0x0962..=0x0963
            | 0x0981
            | 0x09BC
            | 0x09C1..=0x09C4
            | 0x09CD
            | 0x09E2..=0x09E3
            | 0x09FE
            | 0x0A01..=0x0A02
            | 0x0A3C
            | 0x0A41..=0x0A42
            | 0x0A47..=0x0A48
            | 0x0A4B..=0x0A4D
            | 0x0A51
            | 0x0A70..=0x0A71
            | 0x0A75
            | 0x0A81..=0x0A82
            | 0x0ABC
            | 0x0AC1..=0x0AC5
            | 0x0AC7..=0x0AC8
            | 0x0ACD
            | 0x0AE2..=0x0AE3
            | 0x0AFA..=0x0AFF
            | 0x0B01
            | 0x0B3C
            | 0x0B3F
            | 0x0B41..=0x0B44
            | 0x0B4D
            | 0x0B55..=0x0B56
            | 0x0B62..=0x0B63
            | 0x0B82
            | 0x0BC0
            | 0x0BCD
            | 0x0C00
            | 0x0C04
            | 0x0C3C
            | 0x0C3E..=0x0C40
            | 0x0C46..=0x0C48
            | 0x0C4A..=0x0C4D
            | 0x0C55..=0x0C56
            | 0x0C62..=0x0C63
            | 0x0C81
            | 0x0CBC
            | 0x0CBF
            | 0x0CC6
            | 0x0CCC..=0x0CCD
            | 0x0CE2..=0x0CE3
            | 0x0D00..=0x0D01
            | 0x0D3B..=0x0D3C
            | 0x0D41..=0x0D44
            | 0x0D4D
            | 0x0D62..=0x0D63
            | 0x0D81
            | 0x0DCA
            | 0x0DD2..=0x0DD4
            | 0x0DD6
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E47..=0x0E4E
            | 0x0EB1
            | 0x0EB4..=0x0EBC
            | 0x0EC8..=0x0ECE
            | 0x0F18..=0x0F19
            | 0x0F35
            | 0x0F37
            | 0x0F39
            | 0x0F71..=0x0F7E
            | 0x0F80..=0x0F84
            | 0x0F86..=0x0F87
            | 0x0F8D..=0x0F97
            | 0x0F99..=0x0FBC
            | 0x0FC6
            | 0x102D..=0x1030
            | 0x1032..=0x1037
            | 0x1039..=0x103A
            | 0x103D..=0x103E
            | 0x1058..=0x1059
            | 0x105E..=0x1060
            | 0x1071..=0x1074
            | 0x1082
            | 0x1085..=0x1086
            | 0x108D
            | 0x109D
            | 0x135D..=0x135F
            | 0x1712..=0x1714
            | 0x1732..=0x1733
            | 0x1752..=0x1753
            | 0x1772..=0x1773
            | 0x17B4..=0x17B5
            | 0x17B7..=0x17BD
            | 0x17C6
            | 0x17C9..=0x17D3
            | 0x17DD
            | 0x180B..=0x180D
            | 0x180F
            | 0x1885..=0x1886
            | 0x18A9
            | 0x1920..=0x1922
            | 0x1927..=0x1928
            | 0x1932
            | 0x1939..=0x193B
            | 0x1A17..=0x1A18
            | 0x1A1B
            | 0x1A56
            | 0x1A58..=0x1A5E
            | 0x1A60
            | 0x1A62
            | 0x1A65..=0x1A6C
            | 0x1A73..=0x1A7C
            | 0x1A7F
            | 0x1AB0..=0x1AFF
            | 0x1B00..=0x1B03
            | 0x1B34
            | 0x1B36..=0x1B3A
            | 0x1B3C
            | 0x1B42
            | 0x1B6B..=0x1B73
            | 0x1B80..=0x1B81
            | 0x1BA2..=0x1BA5
            | 0x1BA8..=0x1BA9
            | 0x1BAB..=0x1BAD
            | 0x1BE6
            | 0x1BE8..=0x1BE9
            | 0x1BED
            | 0x1BEF..=0x1BF1
            | 0x1C2C..=0x1C33
            | 0x1C36..=0x1C37
            | 0x1CD0..=0x1CD2
            | 0x1CD4..=0x1CE0
            | 0x1CE2..=0x1CE8
            | 0x1CED
            | 0x1CF4
            | 0x1CF8..=0x1CF9
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0x2CEF..=0x2CF1
            | 0x2D7F
            | 0x2DE0..=0x2DFF
            | 0x302A..=0x302F
            | 0x3099..=0x309A
            | 0xA66F
            | 0xA674..=0xA67D
            | 0xA69E..=0xA69F
            | 0xA6F0..=0xA6F1
            | 0xA802
            | 0xA806
            | 0xA80B
            | 0xA825..=0xA826
            | 0xA82C
            | 0xA8C4..=0xA8C5
            | 0xA8E0..=0xA8F1
            | 0xA8FF
            | 0xA926..=0xA92D
            | 0xA947..=0xA951
            | 0xA980..=0xA982
            | 0xA9B3
            | 0xA9B6..=0xA9B9
            | 0xA9BC
            | 0xA9E5
            | 0xAA29..=0xAA2E
            | 0xAA31..=0xAA32
            | 0xAA35..=0xAA36
            | 0xAA43
            | 0xAA4C
            | 0xAA7C
            | 0xAAB0
            | 0xAAB2..=0xAAB4
            | 0xAAB7..=0xAAB8
            | 0xAABE..=0xAABF
            | 0xAAC1
            | 0xAAEC..=0xAAED
            | 0xAAF6
            | 0xABE5
            | 0xABE8
            | 0xABED
            | 0xFB1E
            | 0xFE20..=0xFE2F
            | 0x101FD
            | 0x102E0
            | 0x10376..=0x1037A
            | 0x10A01..=0x10A03
            | 0x10A05..=0x10A06
            | 0x10A0C..=0x10A0F
            | 0x10A38..=0x10A3A
            | 0x10A3F
            | 0x10AE5..=0x10AE6
            | 0x10D24..=0x10D27
            | 0x10EAB..=0x10EAC
            | 0x10EFD..=0x10EFF
            | 0x10F46..=0x10F50
            | 0x10F82..=0x10F85
            | 0x11001
            | 0x11038..=0x11046
            | 0x11070
            | 0x11073..=0x11074
            | 0x1107F..=0x11081
            | 0x110B3..=0x110B6
            | 0x110B9..=0x110BA
            | 0x110C2
            | 0x11100..=0x11102
            | 0x11127..=0x1112B
            | 0x1112D..=0x11134
            | 0x11173
            | 0x11180..=0x11181
            | 0x111B6..=0x111BE
            | 0x111C9..=0x111CC
            | 0x111CF
            | 0x1122F..=0x11231
            | 0x11234
            | 0x11236..=0x11237
            | 0x1123E
            | 0x11241
            | 0x112DF
            | 0x112E3..=0x112EA
            | 0x11300..=0x11301
            | 0x1133B..=0x1133C
            | 0x11340
            | 0x11366..=0x1136C
            | 0x11370..=0x11374
            | 0x11438..=0x1143F
            | 0x11442..=0x11444
            | 0x11446
            | 0x1145E
            | 0x114B3..=0x114B8
            | 0x114BA
            | 0x114BF..=0x114C0
            | 0x114C2..=0x114C3
            | 0x115B2..=0x115B5
            | 0x115BC..=0x115BD
            | 0x115BF..=0x115C0
            | 0x115DC..=0x115DD
            | 0x11633..=0x1163A
            | 0x1163D
            | 0x1163F..=0x11640
            | 0x116AB
            | 0x116AD
            | 0x116B0..=0x116B5
            | 0x116B7
            | 0x1171D..=0x1171F
            | 0x11722..=0x11725
            | 0x11727..=0x1172B
            | 0x1182F..=0x11837
            | 0x11839..=0x1183A
            | 0x1193B..=0x1193C
            | 0x1193E
            | 0x11943
            | 0x119D4..=0x119D7
            | 0x119DA..=0x119DB
            | 0x119E0
            | 0x11A01..=0x11A0A
            | 0x11A33..=0x11A38
            | 0x11A3B..=0x11A3E
            | 0x11A47
            | 0x11A51..=0x11A56
            | 0x11A59..=0x11A5B
            | 0x11A8A..=0x11A96
            | 0x11A98..=0x11A99
            | 0x11C30..=0x11C36
            | 0x11C38..=0x11C3D
            | 0x11C3F
            | 0x11C92..=0x11CA7
            | 0x11CAA..=0x11CB0
            | 0x11CB2..=0x11CB3
            | 0x11CB5..=0x11CB6
            | 0x11D31..=0x11D36
            | 0x11D3A
            | 0x11D3C..=0x11D3D
            | 0x11D3F..=0x11D45
            | 0x11D47
            | 0x11D90..=0x11D91
            | 0x11D95
            | 0x11D97
            | 0x11EF3..=0x11EF4
            | 0x11F00..=0x11F01
            | 0x11F36..=0x11F3A
            | 0x11F40
            | 0x11F42
            | 0x13440
            | 0x13447..=0x13455
            | 0x1611E..=0x16129
            | 0x1612D..=0x1612F
            | 0x16AF0..=0x16AF4
            | 0x16B30..=0x16B36
            | 0x16F4F
            | 0x16F8F..=0x16F92
            | 0x16FE4
            | 0x16FF0..=0x16FF1
            | 0x1BC9D..=0x1BC9E
            | 0x1CF00..=0x1CF2D
            | 0x1CF30..=0x1CF46
            | 0x1D165..=0x1D169
            | 0x1D16D..=0x1D172
            | 0x1D17B..=0x1D182
            | 0x1D185..=0x1D18B
            | 0x1D1AA..=0x1D1AD
            | 0x1D242..=0x1D244
            | 0x1DA00..=0x1DA36
            | 0x1DA3B..=0x1DA6C
            | 0x1DA75
            | 0x1DA84
            | 0x1DA9B..=0x1DA9F
            | 0x1DAA1..=0x1DAAF
            | 0x1E000..=0x1E006
            | 0x1E008..=0x1E018
            | 0x1E01B..=0x1E021
            | 0x1E023..=0x1E024
            | 0x1E026..=0x1E02A
            | 0x1E08F
            | 0x1E130..=0x1E136
            | 0x1E2AE
            | 0x1E2EC..=0x1E2EF
            | 0x1E4EC..=0x1E4EF
            | 0x1E5EE..=0x1E5EF
            | 0x1E8D0..=0x1E8D6
            | 0x1E944..=0x1E94A
    )
}

/// Whether the font maps `ch` to a usable glyph. Fallback-eligible codepoints
/// also require an inked outline so blank placeholder glyphs cannot block the
/// fallback chain.
fn font_has_glyph(font: &FontVec, ch: char) -> bool {
    let id = font.glyph_id(ch);
    if id.0 == 0 {
        return false;
    }
    if should_attempt_fallback(ch) {
        return glyph_coverage_decision(ch, true, font_has_inked_outline(font, id));
    }
    true
}

fn font_has_inked_outline(font: &FontVec, id: GlyphId) -> bool {
    font.outline(id).is_some_and(|outline| {
        !outline.curves.is_empty()
            && outline.bounds.min.x != outline.bounds.max.x
            && outline.bounds.min.y != outline.bounds.max.y
    })
}

fn glyph_coverage_decision(ch: char, has_cmap: bool, has_inked_outline: bool) -> bool {
    has_cmap && (!should_attempt_fallback(ch) || has_inked_outline)
}

/// Font style variant for a glyph slot. Groundwork: keys the dynamic region by
/// `(FontStyle, char)` so bold/italic glyphs can coexist with regular ones. The
/// live render path resolves `Regular` only today; a future grid/gpu packet will
/// call the `_styled` variants with the matching style font, purely additively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontStyle {
    /// Upright, regular weight — the only style rendered live today.
    #[default]
    Regular,
    /// Bold weight.
    Bold,
    /// Italic / oblique.
    Italic,
    /// Bold italic.
    BoldItalic,
}

/// Integer pixel metrics for one monospace cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    /// Cell advance width in pixels.
    pub width: u32,
    /// Cell height (ascent + descent) in pixels.
    pub height: u32,
    /// Baseline offset from the cell top, in pixels.
    pub baseline: u32,
}

/// Atlas coverage storage and subpixel channel order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubpixelMode {
    /// Single-channel grayscale coverage. This is the stable default path.
    #[default]
    Off,
    /// RGB stripe order: red, green, blue from left to right.
    Rgb,
    /// BGR stripe order: blue, green, red from left to right.
    Bgr,
}

impl SubpixelMode {
    pub fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn bytes_per_pixel(self) -> u32 {
        if self.enabled() { 4 } else { 1 }
    }
}

/// Bearing-aware quad geometry for one glyph, in atlas pixels.
///
/// `offset_x`/`offset_y` are the ink's top-left relative to the cell's top-left
/// (the on-screen cell origin). They may be **negative** when ink starts left of
/// or above the cell (left side bearing, tall diacritics); `width`/`height` may
/// **exceed** the cell when ink overflows right or below (powerline separators,
/// italic side bearing, descenders). `uv` is the normalized atlas rectangle
/// `[u0, v0, u1, v1]` covering exactly the inked pixels.
///
/// Because 1 atlas pixel maps to 1 physical screen pixel, the renderer positions
/// a glyph quad at `cell_origin + (offset_x, offset_y)` with size
/// `(width, height)` and samples `uv`, drawing overflow ink uncropped while the
/// cell's background quad stays full-cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphBounds {
    /// Horizontal ink offset from the cell's left edge, in pixels (may be < 0).
    pub offset_x: i32,
    /// Vertical ink offset from the cell's top edge, in pixels (may be < 0).
    pub offset_y: i32,
    /// Ink width in pixels (may exceed `cell.width`).
    pub width: u32,
    /// Ink height in pixels (may exceed `cell.height`).
    pub height: u32,
    /// Normalized UV rect `[u0, v0, u1, v1]` covering exactly the ink.
    pub uv: [f32; 4],
}

/// Inked pixel extent of one glyph slot, relative to that slot's inner cell
/// top-left. Stored per slot in [`GlyphAtlas::slot_ink`]; the public
/// [`GlyphBounds`] (which adds a normalized UV) is derived from this on demand so
/// it stays correct after the atlas grows in height (which changes the V
/// denominator). `offset_*` may be negative and `width`/`height` may exceed the
/// cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlyphInk {
    offset_x: i32,
    offset_y: i32,
    width: u32,
    height: u32,
}

impl GlyphInk {
    /// Default extent for a glyph with no recorded ink (or the fallback box):
    /// exactly the cell, so a quad built from it matches the legacy full-cell
    /// rectangle.
    fn cell(cell: CellSize) -> Self {
        Self {
            offset_x: 0,
            offset_y: 0,
            width: cell.width,
            height: cell.height,
        }
    }
}

/// A monospace glyph atlas: an 8-bit coverage bitmap with a fallback box, the
/// printable-ASCII block, and a growable dynamic region for other codepoints.
/// Runtime per-codepoint glyph fallback resolver hook (RV6 Linux backfill): a
/// bare `fn` (not a closure) so [`GlyphAtlas`] stays `Clone`/`Debug`. Given a
/// codepoint the static fallback chain missed, it returns a loaded face that
/// covers it (or `None`). The native layer wires this to a cached `fc-match`
/// query; see [`crate::text::runtime_resolve_symbol_font`].
type RuntimeSymbolResolver = fn(char) -> Option<Arc<FontVec>>;

#[derive(Debug, Clone)]
pub struct GlyphAtlas {
    /// Atlas bitmap width in pixels.
    pub width: u32,
    /// Atlas bitmap height in pixels.
    pub height: u32,
    /// Coverage data, row-major. Grayscale atlases store R8 coverage
    /// (`width * height` bytes). Subpixel atlases store RGBA8 coverage
    /// (`width * height * 4` bytes), with RGB carrying per-channel coverage and
    /// alpha set opaque.
    pub data: Vec<u8>,
    /// Per-cell pixel metrics shared by every glyph.
    pub cell: CellSize,
    /// Per-slot inked pixel extent (index == slot), for bearing-aware quad
    /// geometry. Invariant: `slot_ink.len() == next_slot`. Grows in lockstep
    /// with dynamic slot allocation.
    slot_ink: Vec<GlyphInk>,
    /// Per-slot horizontal cell span (index == slot): `1` for normal glyphs,
    /// `2` for a wide (East Asian) lead slot whose inner region and ink stretch
    /// across two cells. Reserved/filler slots created by a wide allocation keep
    /// span `1` (they are never looked up). Invariant: `slot_span.len() ==
    /// slot_ink.len() == next_slot`.
    slot_span: Vec<u8>,
    /// Atlas cells per row.
    cols: u32,
    /// Number of cell-rows currently allocated (grows in `ATLAS_GROW_ROWS`
    /// pages). `height == capacity_rows * cell.height`.
    capacity_rows: u32,
    /// Next free slot for dynamic insertion; also the current slot count.
    next_slot: u32,
    /// Effective slot ceiling imposed by the active GPU texture-height limit.
    /// Headless callers retain [`MAX_ATLAS_SLOTS`].
    max_slots: u32,
    /// Resident non-ASCII `(style, codepoint)` → slot index. A codepoint the
    /// font lacks is cached pointing at [`FALLBACK_SLOT`] so the decision is made
    /// once. Keyed by style so bold/italic variants get distinct slots; the live
    /// render path only ever inserts [`FontStyle::Regular`] today.
    dynamic: HashMap<(FontStyle, char), u32>,
    /// Physical pixel size new glyphs are rasterized at (matches the ASCII block).
    px: f32,
    /// Monotonic counter bumped whenever pixels or dimensions change. The native
    /// layer compares this to decide when to re-upload the atlas texture.
    revision: u64,
    /// Set when `data`/dimensions changed since the last [`Self::take_dirty`].
    dirty: bool,
    /// Coverage storage mode. `Off` preserves the original R8 atlas exactly.
    subpixel: SubpixelMode,
    /// Synthetic-style mask: bit 0 = Bold, bit 1 = Italic, bit 2 = BoldItalic.
    /// A set bit means the font family has **no real face** for that style, so
    /// glyphs rasterized for it get the matching [`SynthTransform`] (emboldening
    /// and/or shear) instead of a plain Regular copy. Default `0` (no synthesis)
    /// preserves the original behavior exactly. The native layer sets this right
    /// after [`Self::build`] from `Arc` identity of its loaded faces; it only
    /// affects glyphs rasterized *after* it is set, which on the live path is all
    /// of them (the dynamic region is empty at build time).
    synthetic: u8,
    /// When set, box-drawing / block-element / Powerline codepoints that
    /// [`crate::boxdraw::covers`] recognizes are rasterized **geometrically**
    /// (computed rectangles/rails/arcs/triangles aligned to the cell grid)
    /// instead of from the font outline (RV2). Default `false` preserves the
    /// font-glyph path byte-for-byte. Like the synthetic-style mask, it only
    /// governs glyphs rasterized after it is set; the native layer sets it right
    /// after [`Self::build`] and rebuilds the atlas when the setting changes, so
    /// the dynamic region never holds a stale mix of geometric and font glyphs.
    geometric: bool,
    /// Ordered symbol / Nerd-font fallback **chain** (RV6). When non-empty, a
    /// printable spacing codepoint the **primary** font lacks is rasterized from
    /// the first chain face that has a glyph for it, instead of the hollow-box
    /// tofu slot. The chain composes coverage from multiple faces (e.g. bundled
    /// Nerd Fonts v3 then v2, then a host face), so a glyph missing from the
    /// first face still resolves from a later one; a codepoint no face provides
    /// falls through to the historical missing-glyph path. An empty chain (the
    /// default, and the state on any freshly built atlas) preserves that path
    /// byte-for-byte. Faces are held as `Arc`s so the per-glyph lookup can clone
    /// a handle and rasterize without conflicting with the `&mut self` bitmap
    /// borrow. The native layer resolves and sets it after build, mirroring the
    /// synthetic-styles / geometric switches.
    fallback_chain: Vec<Arc<FontVec>>,
    /// SYMMAP: per-codepoint-range override faces (resolved from the user's
    /// `symbol_map` config). Each entry is an inclusive `(start, end, face)`
    /// range; [`Self::symbol_map_font_for`] returns the first range that
    /// contains a codepoint (first-match-wins, matching `text::SymbolMap`). An
    /// empty `Vec` (the default) means no override: every lookup returns `None`,
    /// the chosen raster font stays the primary `font`, and the glyph path is
    /// byte-identical to the no-SYMMAP renderer. Held as `Arc`s so the per-glyph
    /// lookup clones a handle without conflicting with the `&mut self` bitmap
    /// borrow, exactly like `fallback`. The native layer resolves family names
    /// to faces and installs them after build, rebuilding when the map changes.
    symbol_map_fonts: Vec<(u32, u32, Arc<FontVec>)>,
    /// Runtime per-codepoint glyph fallback resolver (RV6 Linux backfill).
    /// Consulted only when a printable spacing codepoint misses the static
    /// [`Self::fallback_chain`] above -- the static chain (bundled Nerd faces,
    /// host face, macOS system tail) is always tried first and takes the exact
    /// same path as before, so codepoints that already resolve are unaffected.
    /// On Linux the static list cannot promise coverage of arbitrary symbols
    /// across heterogeneous hosts, so the native layer installs a resolver that
    /// queries `fc-match :charset=<cp>` for a host face that has the glyph (see
    /// [`crate::text::runtime_resolve_symbol_font`]). `None` (the default, and
    /// the state on every freshly built atlas) disables the runtime query
    /// entirely, keeping the path byte-identical to the pre-feature renderer.
    /// The resolver is a bare `fn` pointer (not a closure) so the atlas stays
    /// `Clone`/`Debug`; the subprocess cost is bounded by
    /// [`Self::runtime_symbol_cache`].
    runtime_symbol_resolver: Option<RuntimeSymbolResolver>,
    /// Per-codepoint cache of [`Self::runtime_symbol_resolver`] results,
    /// including negative results (`None`), so a given codepoint shells out to
    /// `fc-match` at most once -- the runtime query never runs on the hot
    /// per-glyph render path more than once per distinct missing codepoint. Empty
    /// by default and untouched unless a resolver is installed and the static
    /// chain misses, so the default path costs nothing.
    runtime_symbol_cache: HashMap<char, Option<Arc<FontVec>>>,
}

impl GlyphAtlas {
    /// Rasterize printable ASCII at `px` pixels into a new atlas.
    ///
    /// `px` is the physical pixel size to rasterize at (caller multiplies the
    /// logical font size by the window scale factor for crisp HiDPI text).
    pub fn build(font: &FontVec, px: f32) -> Self {
        Self::build_with_subpixel(font, px, SubpixelMode::Off)
    }

    /// Rasterize printable ASCII into a new atlas with the requested coverage
    /// storage. Subpixel modes keep the same atlas dimensions and slot geometry
    /// as grayscale but store RGB stripe coverage in an RGBA8 bitmap.
    pub fn build_with_subpixel(font: &FontVec, px: f32, subpixel: SubpixelMode) -> Self {
        Self::build_with_options(font, px, subpixel, 1.0)
    }

    /// Rasterize printable ASCII with an explicit `line_height` multiplier
    /// (LINEHEIGHT). `1.0` is the historical cell geometry, byte-identical to
    /// [`Self::build_with_subpixel`]; values above `1.0` add vertical leading,
    /// split symmetrically (half above, half below) so the baseline stays
    /// centered and glyph rasterization is unchanged — only the cell box grows
    /// and the baseline shifts down by the top half. The added rows are
    /// transparent gutter, so default `1.0` produces identical coverage.
    pub fn build_with_options(
        font: &FontVec,
        px: f32,
        subpixel: SubpixelMode,
        line_height: f32,
    ) -> Self {
        let px = px.max(1.0);
        let scale = PxScale::from(px);
        let scaled = font.as_scaled(scale);

        // Monospace: every glyph shares the advance of a representative glyph.
        let advance = scaled.h_advance(font.glyph_id('M'));
        let ascent = scaled.ascent();
        let descent = scaled.descent(); // negative (below baseline)

        // Single documented baseline: the font ascent rounded to the nearest
        // whole pixel. Every glyph — ASCII, accents, box-drawing — is positioned
        // with its baseline on this one integer row, so mixed glyphs sit on a
        // common line and horizontal stems land on pixel boundaries for crisp
        // coverage. The cell height spans this baseline plus the descent so
        // descenders fit within the cell box.
        let baseline = ascent.round().max(0.0);
        let cell_w = advance.ceil().max(1.0) as u32;
        let cell_h = (ascent - descent).ceil().max(1.0) as u32;

        // LINEHEIGHT leading: extra rows added around the natural cell. At the
        // default `1.0` the leading is exactly 0, so `cell_h`/`baseline` are
        // unchanged and the atlas is byte-identical. The leading is split with
        // the larger half on top (`lead_top`) so the baseline moves down by that
        // amount; glyphs still rasterize against the same metrics, just lower in
        // a taller slot, which keeps every glyph's shape pixel-for-pixel.
        let leading = (((line_height.max(1.0) - 1.0) * cell_h as f32).round() as u32).min(cell_h);
        let lead_top = leading.div_ceil(2);
        let cell_h = cell_h + leading;
        // Shift the baseline down by the top leading so every glyph rasterizes
        // lower within the taller slot. Kept as `f32` for the rasterizer Pen;
        // the cell stores the rounded integer row.
        let baseline = baseline + lead_top as f32;
        let cell = CellSize {
            width: cell_w,
            height: cell_h,
            baseline: baseline as u32,
        };

        // Base region: fallback box (slot 0) + printable ASCII (slots 1..=95).
        // Each slot carries a transparent gutter (see `slot_offset`/`slot_uv`).
        let cols = ATLAS_COLS;
        let base_slots = FIRST_DYNAMIC_SLOT;
        let capacity_rows = base_slots.div_ceil(cols);
        let width = cols * slot_w(cell);
        let height = capacity_rows * slot_h(cell);
        let mut data = vec![0u8; atlas_byte_len(width, height, subpixel.bytes_per_pixel())];

        // Per-slot inked extent (index == slot). The fallback box keeps full-cell
        // bounds so a missing glyph renders exactly as before; ASCII slots record
        // their real ink as they are rasterized.
        let mut slot_ink = vec![GlyphInk::cell(cell); base_slots as usize];
        // Base region is all single-cell (fallback box + printable ASCII).
        let slot_span = vec![1u8; base_slots as usize];

        // Slot 0: synthesized hollow-box fallback, drawn the same for any font.
        let border = slot_border(cell);
        let (fox, foy) = slot_offset(FALLBACK_SLOT, cols, cell);
        draw_fallback_box(&mut data, width, subpixel, fox + border, foy + border, cell);

        // Slots 1..=95: printable ASCII at the build pixel size.
        for code in FIRST_CHAR..=LAST_CHAR {
            let ch = char::from_u32(code).unwrap_or(' ');
            let slot = code - FIRST_CHAR + 1;
            let origin = slot_offset(slot, cols, cell);
            if let Some(ink) = rasterize_glyph(
                font,
                Pen { px, baseline },
                ch,
                &mut data,
                width,
                subpixel,
                SlotRegion {
                    origin,
                    cell,
                    outer_w: slot_w(cell),
                },
                SynthTransform::none(),
                None, // ASCII: natural-bearing text, never fitted
            ) {
                slot_ink[slot as usize] = ink;
            }
        }

        Self {
            width,
            height,
            data,
            cell,
            slot_ink,
            slot_span,
            cols,
            capacity_rows,
            next_slot: base_slots,
            max_slots: MAX_ATLAS_SLOTS,
            dynamic: HashMap::new(),
            px,
            revision: 0,
            dirty: false,
            subpixel,
            synthetic: 0,
            geometric: false,
            fallback_chain: Vec::new(),
            symbol_map_fonts: Vec::new(),
            runtime_symbol_resolver: None,
            runtime_symbol_cache: HashMap::new(),
        }
    }

    /// Immutable UV lookup used by the per-frame geometry builder.
    ///
    /// Returns the atlas cell for printable ASCII, the resident cell for a
    /// non-ASCII codepoint already inserted via [`Self::ensure`], and the
    /// **fallback box** for any other printable codepoint (so missing glyphs
    /// render a visible box rather than blank). Spaces and control characters
    /// return `None` (nothing is drawn).
    pub fn uv_rect(&self, ch: char) -> Option<[f32; 4]> {
        self.uv_rect_styled(FontStyle::Regular, ch)
    }

    /// Style-aware immutable UV lookup (groundwork for attribute-driven
    /// rendering). Identical to [`Self::uv_rect`] for [`FontStyle::Regular`].
    ///
    /// The prebuilt printable-ASCII block belongs to `Regular`; for any other
    /// style, ASCII resolves through the dynamic region like every other glyph,
    /// returning the resident `(style, ch)` slot if present and the fallback box
    /// otherwise. Spaces and control characters return `None`.
    pub fn uv_rect_styled(&self, style: FontStyle, ch: char) -> Option<[f32; 4]> {
        let code = ch as u32;
        if style == FontStyle::Regular && (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_uv(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&(style, ch)) {
            return Some(self.slot_uv(slot));
        }
        if wants_glyph(ch) {
            return Some(self.slot_uv(FALLBACK_SLOT));
        }
        None
    }

    /// Normalized UV rectangle `[u0, v0, u1, v1]` for an atlas slot index.
    ///
    /// Returns the slot's **inner** `cell.width × cell.height` rectangle (the
    /// glyph area), inset past the [`ATLAS_PAD`] gutter on every side, so the
    /// per-cell quad samples only this glyph and the surrounding gutter shields
    /// it from neighbor bleed.
    fn slot_uv(&self, slot: u32) -> [f32; 4] {
        let (ox, oy) = slot_offset(slot, self.cols, self.cell);
        let border = slot_border(self.cell);
        let ix = ox + border;
        let iy = oy + border;
        // Wide (East Asian) lead slots span two cells; their inner region is
        // `span * cell.width` wide. `slot_offset` already places consecutive
        // slots contiguously, so the reserved second cell sits immediately right.
        let span = self.slot_span[slot as usize] as u32;
        [
            ix as f32 / self.width as f32,
            iy as f32 / self.height as f32,
            (ix + span * self.cell.width) as f32 / self.width as f32,
            (iy + self.cell.height) as f32 / self.height as f32,
        ]
    }

    /// Bearing-aware quad geometry for a printable codepoint (Regular style).
    ///
    /// The geometry counterpart to [`Self::uv_rect`]: where `uv_rect` returns the
    /// fixed inner-cell rectangle, this returns the glyph's real inked extent
    /// (offset + size relative to the cell, plus the matching UV), so the
    /// renderer can draw ink that overflows the cell box uncropped. Resolution
    /// matches `uv_rect`: ASCII and resident codepoints resolve to their slot,
    /// other printables to the fallback box (full-cell bounds), spaces/controls
    /// to `None`.
    pub fn glyph_quad(&self, ch: char) -> Option<GlyphBounds> {
        self.glyph_quad_styled(FontStyle::Regular, ch)
    }

    /// Style-aware bearing-aware quad geometry. The geometry counterpart to
    /// [`Self::uv_rect_styled`]; see [`Self::glyph_quad`].
    pub fn glyph_quad_styled(&self, style: FontStyle, ch: char) -> Option<GlyphBounds> {
        let code = ch as u32;
        if style == FontStyle::Regular && (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_glyph_bounds(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&(style, ch)) {
            return Some(self.slot_glyph_bounds(slot));
        }
        if wants_glyph(ch) {
            return Some(self.slot_glyph_bounds(FALLBACK_SLOT));
        }
        None
    }

    /// Build the public [`GlyphBounds`] for a slot from its stored ink extent,
    /// normalizing the UV against the current atlas dimensions (the V denominator
    /// changes as the atlas grows, so this is computed on demand, never cached).
    fn slot_glyph_bounds(&self, slot: u32) -> GlyphBounds {
        let ink = self.slot_ink[slot as usize];
        let (ox, oy) = slot_offset(slot, self.cols, self.cell);
        let border = slot_border(self.cell) as i32;
        let cell_x = ox as i32 + border;
        let cell_y = oy as i32 + border;
        let ix = cell_x + ink.offset_x;
        let iy = cell_y + ink.offset_y;
        GlyphBounds {
            offset_x: ink.offset_x,
            offset_y: ink.offset_y,
            width: ink.width,
            height: ink.height,
            uv: [
                ix as f32 / self.width as f32,
                iy as f32 / self.height as f32,
                (ix + ink.width as i32) as f32 / self.width as f32,
                (iy + ink.height as i32) as f32 / self.height as f32,
            ],
        }
    }

    /// Resolve `ch` to a UV rect, rasterizing a real glyph into the dynamic
    /// region on first use (the **mutable** counterpart to [`Self::uv_rect`]).
    ///
    /// - Printable ASCII and already-resident codepoints resolve immediately.
    /// - A non-ASCII codepoint the font provides is rasterized into the next
    ///   free slot, growing the atlas by a page of rows if the region is full;
    ///   [`Self::take_dirty`] then reports the texture needs re-uploading.
    /// - A codepoint the font lacks (or one that would exceed [`MAX_ATLAS_SLOTS`])
    ///   resolves to the fallback box and is cached so the decision is made once.
    /// - Spaces and control characters return `None`.
    pub fn ensure(&mut self, font: &FontVec, ch: char) -> Option<[f32; 4]> {
        self.ensure_styled(font, FontStyle::Regular, ch)
    }

    /// Style-aware mutable insertion (groundwork for attribute-driven
    /// rendering). Identical to [`Self::ensure`] for [`FontStyle::Regular`].
    ///
    /// A non-`Regular` style rasterizes `ch` from the supplied `font` into a
    /// slot keyed by `(style, ch)`, so styled glyphs never collide with regular
    /// ones. Until a future packet supplies a true bold/italic face, callers
    /// pass the regular font, so a styled slot holds the regular outline; the
    /// keying is what matters for groundwork. Growth, fallback, and the hard
    /// slot cap behave exactly as in [`Self::ensure`].
    pub fn ensure_styled(
        &mut self,
        font: &FontVec,
        style: FontStyle,
        ch: char,
    ) -> Option<[f32; 4]> {
        let code = ch as u32;
        if style == FontStyle::Regular && (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_uv(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&(style, ch)) {
            return Some(self.slot_uv(slot));
        }
        if !wants_glyph(ch) {
            return None;
        }
        // SYMMAP (extends RV6): a user-configured codepoint→font-family override
        // takes priority over every other glyph source. `None` (the default,
        // empty-map state) means no override and the scan is skipped, so the
        // path below is byte-identical to the no-SYMMAP renderer.
        let override_arc = self.symbol_map_font_for(ch);
        // Geometric box-drawing (RV2): when enabled, recognized line/block/
        // Powerline codepoints are rasterized from cell-aligned geometry instead
        // of the font glyph — and render even if the font lacks the codepoint.
        // A SYMMAP override suppresses geometric rendering: if the user
        // explicitly remapped a box-drawing range, their font glyph wins.
        let geometric = self.geometric && override_arc.is_none() && crate::boxdraw::covers(ch);
        // The raster face for this glyph, in precedence order:
        //   SYMMAP override > geometric (handled above) > RV6 glyph fallback >
        //   primary font > hollow-box tofu.
        // RV6: when the primary font lacks a printable spacing glyph, a fallback
        // font (if configured) rasterizes it instead of tofu.
        // `None` here means either no fallback is set, the codepoint is not a
        // standalone glyph candidate, or the fallback also lacks it -- all of
        // which fall through to the historical hollow-box slot.
        let mut symbol_font: Option<Arc<FontVec>> = None;
        if let Some(ov) = override_arc {
            // SYMMAP: rasterize from the override face directly (no synthetic
            // transform — icon faces are not emboldened/sheared), bypassing the
            // primary-font glyph-presence check and the fallback chain.
            symbol_font = Some(ov);
        } else if !geometric && !font_has_glyph(font, ch) {
            match self.symbol_fallback(ch) {
                Some(fb) => symbol_font = Some(fb),
                None => {
                    // Font lacks the glyph and no fallback applies: cache the
                    // fallback decision, draw nothing new.
                    self.dynamic.insert((style, ch), FALLBACK_SLOT);
                    return Some(self.slot_uv(FALLBACK_SLOT));
                }
            }
        }
        // Geometric glyphs are always single-cell (box/block/Powerline).
        let cells = if geometric { 1 } else { glyph_cells(ch) };
        let Some(slot) = self.allocate_slots(cells) else {
            // Atlas is at its hard cap: degrade to the fallback box.
            self.dynamic.insert((style, ch), FALLBACK_SLOT);
            return Some(self.slot_uv(FALLBACK_SLOT));
        };
        let origin = slot_offset(slot, self.cols, self.cell);
        let ink = if geometric {
            rasterize_geometric(
                ch,
                &mut self.data,
                self.width,
                self.subpixel,
                SlotRegion {
                    origin,
                    cell: self.cell,
                    outer_w: slot_w(self.cell),
                },
            )
            .unwrap_or_else(|| GlyphInk::cell(self.cell))
        } else {
            // A fallback glyph renders from the fallback face with no synthetic
            // transform (icons are not emboldened/sheared); otherwise
            // the primary font and the style's synthetic mask apply as usual.
            let (raster_font, synth): (&FontVec, SynthTransform) = match symbol_font.as_deref() {
                Some(fb) => (fb, SynthTransform::none()),
                None => (font, self.synth_for(style)),
            };
            // Symbol-fallback and SYMMAP-override faces are icon faces: fit each
            // glyph into its cell box (aspect-preserving) and center it, so they
            // form an even column instead of rendering at the body em-size with
            // natural bearing (overflow/clip + ragged x). Primary-font glyphs
            // (`symbol_font` is `None`) pass `None` → byte-identical placement.
            let fit = symbol_font.as_ref().map(|_| {
                let pad =
                    (SYMBOL_CELL_INSET * self.cell.width.min(self.cell.height) as f32).round();
                CellFit {
                    box_w: (cells * self.cell.width) as f32,
                    box_h: self.cell.height as f32,
                    pad,
                }
            });
            rasterize_glyph(
                raster_font,
                Pen {
                    px: self.px,
                    baseline: self.cell.baseline as f32,
                },
                ch,
                &mut self.data,
                self.width,
                self.subpixel,
                SlotRegion {
                    origin,
                    cell: self.cell,
                    // Wide glyphs draw across `cells` contiguous slots; the clip
                    // extends over all of them so ink is never cropped at the edge.
                    outer_w: cells * slot_w(self.cell),
                },
                synth,
                fit,
            )
            .unwrap_or_else(|| GlyphInk::cell(self.cell))
        };
        // `allocate_slots` already pushed a dense placeholder for the lead (and
        // every reserved/filler slot); overwrite the lead with the real ink.
        self.slot_ink[slot as usize] = ink;
        self.dynamic.insert((style, ch), slot);
        self.revision += 1;
        self.dirty = true;
        Some(self.slot_uv(slot))
    }

    /// Reserve `span` consecutive dynamic slots in a single atlas row and return
    /// the lead slot index, appending pages of rows (zero-filled) as capacity is
    /// exhausted. Returns `None` once [`MAX_ATLAS_SLOTS`] would be exceeded.
    ///
    /// A `span == 2` (wide / East Asian) allocation never straddles a row
    /// boundary: if the lead would land in the last column, one filler slot is
    /// burned so the pair starts at column 0 of the next row, keeping the two
    /// cells' inked region horizontally contiguous. Every consumed slot (filler +
    /// lead + reserved) gets a dense placeholder `slot_ink`/`slot_span` entry —
    /// the caller overwrites the lead's ink — so existing slots never move and UV
    /// rects handed out before a growth stay valid.
    fn allocate_slots(&mut self, span: u32) -> Option<u32> {
        debug_assert!(span >= 1);
        // A wide pair must not wrap across a row: burn a filler slot first.
        if span > 1 && self.next_slot % self.cols + span > self.cols {
            self.push_placeholder_slot(1)?;
        }
        if self.next_slot + span > self.max_slots {
            return None;
        }
        let lead = self.next_slot;
        self.grow_to_fit(lead + span - 1);
        for i in 0..span {
            // Lead carries the real span; reserved cells are never looked up.
            let s = if i == 0 { span as u8 } else { 1 };
            self.slot_ink.push(GlyphInk::cell(self.cell));
            self.slot_span.push(s);
        }
        self.next_slot += span;
        Some(lead)
    }

    /// Append a single dense placeholder slot with the given span (used for the
    /// filler slot that a wide allocation burns to avoid a row wrap). Returns
    /// `None` at the hard cap.
    fn push_placeholder_slot(&mut self, span: u8) -> Option<()> {
        if self.next_slot + 1 > self.max_slots {
            return None;
        }
        self.grow_to_fit(self.next_slot);
        self.slot_ink.push(GlyphInk::cell(self.cell));
        self.slot_span.push(span);
        self.next_slot += 1;
        Some(())
    }

    /// Grow `capacity_rows` (in [`ATLAS_GROW_ROWS`] pages) and the backing bitmap
    /// so `slot` is addressable, zero-filling new pixels. Existing slots never
    /// move. No-op when the slot already fits.
    fn grow_to_fit(&mut self, slot: u32) {
        let needed_rows = slot / self.cols + 1;
        if needed_rows > self.capacity_rows {
            while needed_rows > self.capacity_rows {
                self.capacity_rows += ATLAS_GROW_ROWS;
            }
            self.height = self.capacity_rows * slot_h(self.cell);
            self.data.resize(
                atlas_byte_len(self.width, self.height, self.subpixel.bytes_per_pixel()),
                0,
            );
            self.revision += 1;
            self.dirty = true;
        }
    }

    /// Monotonic revision, bumped on every pixel/dimension change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Return whether the atlas changed since the last call, clearing the flag.
    /// The native layer calls this each frame to decide when to re-upload.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Total slots currently allocated (fallback + ASCII + dynamic). Primarily
    /// for tests asserting growth and fallback-without-allocation behavior.
    pub fn slot_count(&self) -> u32 {
        self.next_slot
    }

    /// Coverage storage mode.
    pub fn subpixel_mode(&self) -> SubpixelMode {
        self.subpixel
    }

    /// Declare which styles have **no real face** and must be synthesized from
    /// the Regular outline. `bold`/`italic`/`bold_italic` are `true` when the
    /// loaded font family lacks that face, so glyphs rasterized for it should be
    /// emboldened and/or sheared rather than rendered as a plain Regular copy.
    ///
    /// The native layer computes these from `Arc` identity of its loaded faces
    /// (a style slot still pointing at the Regular `Arc` means no real face) and
    /// calls this **immediately after** [`Self::build`], before any styled glyph
    /// is inserted. The mask only governs glyphs rasterized after it is set;
    /// because a font change rebuilds the atlas from scratch, swapping in a real
    /// face clears the corresponding bit and the synthetic slots vanish with the
    /// old atlas — invalidation is by construction, exactly like every other
    /// dynamic slot. Calling this is idempotent and never rewrites existing
    /// pixels, so the live path sets it once on a freshly built (empty-dynamic)
    /// atlas.
    pub fn set_synthetic_styles(&mut self, bold: bool, italic: bool, bold_italic: bool) {
        self.synthetic = (bold as u8) | ((italic as u8) << 1) | ((bold_italic as u8) << 2);
    }

    /// Enable or disable geometric box-drawing / block / Powerline rendering
    /// (RV2). When enabled, codepoints [`crate::boxdraw::covers`] recognizes are
    /// rasterized from computed cell-aligned geometry instead of the font glyph,
    /// so TUI borders, progress bars and powerline prompts are pixel-perfect and
    /// seamless at any cell size; everything else still uses the font.
    ///
    /// `false` (the default) is a true no-op: every glyph takes the font path
    /// and the atlas is byte-identical to the pre-feature renderer. Like
    /// [`Self::set_synthetic_styles`], this only affects glyphs rasterized after
    /// it is set, so the native layer calls it on a freshly built atlas and
    /// rebuilds when the setting toggles (resident slots are never rewritten).
    pub fn set_geometric_boxdraw(&mut self, on: bool) {
        self.geometric = on;
    }

    /// Install (or clear) the symbol / Nerd-font fallback **chain** (RV6).
    ///
    /// When non-empty, a printable spacing codepoint the **primary** font lacks
    /// is rasterized from the first chain face that has it; controls, format
    /// characters, whitespace, combining marks, and variation selectors keep the
    /// historical hollow-box/no-glyph path. The chain composes coverage across
    /// faces (bundled v3, then v2, then a host face), so a glyph absent from
    /// earlier faces still resolves from a later one. An empty `Vec` (the
    /// default) restores the pre-feature missing-glyph path exactly, so a build
    /// with no fallback is byte-identical to the minimal renderer.
    ///
    /// Like [`Self::set_synthetic_styles`] / [`Self::set_geometric_boxdraw`],
    /// this only governs glyphs rasterized after it is set; the native layer
    /// installs it on a freshly built atlas and reinstalls it after a rebuild,
    /// so the dynamic region never mixes resolved and unresolved fallbacks.
    pub fn set_fallback_fonts(&mut self, fonts: Vec<Arc<FontVec>>) {
        self.fallback_chain = fonts;
    }

    /// Install the resolved SYMMAP override faces (`(start, end, face)` ranges,
    /// first-match-wins). An empty `Vec` restores the no-override path. The
    /// native layer calls this after build and on every atlas rebuild; like the
    /// fallback/geometric switches it only governs glyphs rasterized after it is
    /// set, and the atlas is rebuilt (clearing the dynamic region) when the map
    /// changes, so cached slots never mix faces.
    pub fn set_symbol_map_fonts(&mut self, fonts: Vec<(u32, u32, Arc<FontVec>)>) {
        self.symbol_map_fonts = fonts;
    }

    /// Bind dynamic growth to the active device's maximum 2D texture height.
    /// Existing base glyphs stay resident; new glyphs use the fallback once
    /// another complete atlas row would cross `max_dimension`.
    pub fn set_texture_dimension_limit(&mut self, max_dimension: u32) {
        let rows = max_dimension / slot_h(self.cell);
        let reachable_rows = self.capacity_rows
            + rows.saturating_sub(self.capacity_rows) / ATLAS_GROW_ROWS * ATLAS_GROW_ROWS;
        self.max_slots = reachable_rows
            .saturating_mul(self.cols)
            .min(MAX_ATLAS_SLOTS)
            .max(self.next_slot);
    }

    /// Install (or clear) the runtime per-codepoint glyph fallback resolver
    /// (RV6 Linux backfill). When `Some`, a printable spacing codepoint that
    /// misses the static [`Self::fallback_chain`] triggers a single, cached call
    /// to the resolver (the native layer wires this to a `fc-match :charset`
    /// query -- see [`crate::text::runtime_resolve_symbol_font`]). `None` (the
    /// default, and the only state on macOS/non-Unix) disables the runtime
    /// query, so the glyph path stays byte-identical to the pre-feature
    /// renderer; the static chain still resolves exactly as before either way.
    /// Switching the resolver clears the per-codepoint cache so a stale negative
    /// result never pins a codepoint to tofu after the host font set changes.
    /// Like the fallback/geometric switches, the native layer reinstalls it
    /// after every atlas rebuild.
    pub fn set_runtime_symbol_resolver(&mut self, resolver: Option<RuntimeSymbolResolver>) {
        self.runtime_symbol_resolver = resolver;
        self.runtime_symbol_cache.clear();
    }

    /// The SYMMAP override face for `ch`, or `None` when no rule matches (the
    /// identity / off path). With no rules the `Vec` is empty and the scan is
    /// skipped entirely, so the default costs nothing. First-match-wins matches
    /// `text::SymbolMap` precedence.
    fn symbol_map_font_for(&self, ch: char) -> Option<Arc<FontVec>> {
        if self.symbol_map_fonts.is_empty() {
            return None;
        }
        let cp = ch as u32;
        self.symbol_map_fonts
            .iter()
            .find(|(start, end, _)| *start <= cp && cp <= *end)
            .map(|(_, _, font)| Arc::clone(font))
    }

    /// The fallback font to rasterize `ch` from when the primary lacks it, or
    /// `None` to keep the hollow-box behavior. Walks the fallback chain and
    /// returns the **first** face that has a glyph for `ch` -- but only when
    /// `ch` is a printable spacing codepoint. A codepoint no chain face provides
    /// (or an empty chain) yields `None`, preserving the hollow-box path.
    fn symbol_fallback(&mut self, ch: char) -> Option<Arc<FontVec>> {
        if !should_attempt_fallback(ch) {
            return None;
        }
        // Static chain first: bundled Nerd faces, host face, and (macOS) the
        // system tail. A codepoint covered here takes the exact same path as
        // before the runtime resolver existed, so already-resolving glyphs are
        // byte-identical. When the resolver is `None` (the default and the only
        // state off-Linux) this is the whole function, identical to the
        // pre-feature behavior including the empty-chain case.
        if let Some(fb) = self.fallback_chain.iter().find(|fb| font_has_glyph(fb, ch)) {
            return Some(Arc::clone(fb));
        }
        // Static chain missed. Consult the runtime resolver (Linux fc-match)
        // exactly once per codepoint, caching the result -- including a negative
        // result -- so the subprocess never runs on the hot path more than once
        // per distinct missing codepoint.
        let resolver = self.runtime_symbol_resolver?;
        if let Some(cached) = self.runtime_symbol_cache.get(&ch) {
            return cached.clone();
        }
        let resolved = resolver(ch);
        self.runtime_symbol_cache.insert(ch, resolved.clone());
        resolved
    }

    /// The [`SynthTransform`] to apply when rasterizing `style`. Returns the
    /// identity transform for [`FontStyle::Regular`] and for any style whose
    /// synthetic bit is clear (a real face is present); otherwise the matching
    /// emboldening (bold) and/or shear (italic), sized from the build pixel size.
    fn synth_for(&self, style: FontStyle) -> SynthTransform {
        let bit = match style {
            FontStyle::Regular => return SynthTransform::none(),
            FontStyle::Bold => 0,
            FontStyle::Italic => 1,
            FontStyle::BoldItalic => 2,
        };
        if self.synthetic & (1 << bit) == 0 {
            return SynthTransform::none();
        }
        let bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
        let italic = matches!(style, FontStyle::Italic | FontStyle::BoldItalic);
        SynthTransform {
            // 1px at typical sizes, 2px on large HiDPI cells; never 0 when bold.
            embolden_px: if bold {
                (self.px / 24.0).round().max(1.0) as u32
            } else {
                0
            },
            shear: if italic { ITALIC_SHEAR } else { 0.0 },
        }
    }

    /// Bytes in one atlas row, for GPU upload.
    pub fn bytes_per_row(&self) -> u32 {
        self.width * self.subpixel.bytes_per_pixel()
    }
}

/// Drawable overflow margin in pixels around the cell, **inside** the bleed
/// gutter. This is the room a glyph's ink may extend past the cell box (powerline
/// separators that fill the advance, italic side bearing, tall accents,
/// descenders) before the rasterizer clips it. Sized from the cell so it scales
/// with the font size; a small floor keeps tiny cells usable.
fn overflow_margin(cell: CellSize) -> u32 {
    (cell.height / 4).max(2)
}

/// Total border each side of a slot's cell: the transparent bleed gutter
/// ([`ATLAS_PAD`]) plus the drawable [`overflow_margin`]. The cell's inner
/// top-left within a slot is `(ox + slot_border, oy + slot_border)`.
fn slot_border(cell: CellSize) -> u32 {
    ATLAS_PAD + overflow_margin(cell)
}

/// Full slot width in pixels: the glyph cell plus its border on both sides.
fn slot_w(cell: CellSize) -> u32 {
    cell.width + 2 * slot_border(cell)
}

/// Full slot height in pixels: the glyph cell plus its border on both sides.
fn slot_h(cell: CellSize) -> u32 {
    cell.height + 2 * slot_border(cell)
}

/// Pixel offset `(ox, oy)` of an atlas slot's **outer** top-left within the
/// bitmap (the border corner). The cell's inner origin is
/// `(ox + slot_border, oy + slot_border)`.
fn slot_offset(slot: u32, cols: u32, cell: CellSize) -> (u32, u32) {
    ((slot % cols) * slot_w(cell), (slot / cols) * slot_h(cell))
}

/// Rasterize one glyph's coverage into the slot whose **outer** top-left is
/// `origin`, positioning it on the shared integer `baseline`, and return its
/// inked pixel extent relative to the cell's inner top-left.
///
/// Returns `None` if the font has no outline for `ch`, or an outline that inks
/// no pixels (e.g. a space). The returned [`GlyphInk`] offsets may be negative
/// (ink left of / above the cell) and its size may exceed the cell (ink right of
/// / below it), which is what lets the renderer draw overflow uncropped.
///
/// The glyph's pen is placed at the cell's inner origin `(ox + slot_border,
/// oy + slot_border)` and on `baseline`, then each coverage sample is placed at
/// the **nearest** atlas pixel (rounding, not truncation, for stable sub-pixel
/// placement). Coverage may land anywhere in the drawable region — the cell plus
/// its overflow margin — so ink genuinely past the cell box (powerline glyphs,
/// box-drawing joins, descenders, italic side bearing) is preserved; only the
/// outermost [`ATLAS_PAD`] bleed ring is kept transparent, and the clip keeps a
/// glyph strictly out of its neighbors. The strongest value wins on any overlap.
///
/// When `synth` is non-identity (no real face for the requested style), the
/// upright Regular outline is transformed at coverage-write time: a per-sample
/// horizontal **shear** for synthetic italic and a rightward **double-strike**
/// for synthetic bold (see [`SynthTransform`]). Both effects are tracked by the
/// same `min/max` ink bounds, so the returned [`GlyphInk`] reports the real
/// (sheared/widened) extent and the renderer draws it uncropped; the existing
/// drawable-region clip keeps the synthesis inside the slot. Synthesis never
/// changes the cell advance.
///
/// Maximum exponent gain for stem-darkening at strength `1.0`. The applied
/// exponent is `1.0 / (1.0 + strength * STEM_DARKEN_GAIN)`; with strength in
/// `0.0..=1.0` this yields an exponent in `1.0 ..= 1/(1+GAIN)`. At `0.6` the
/// strongest setting boosts a 50%-coverage edge sample by roughly +30%, which
/// thickens stems noticeably without flooding glyph counters.
const STEM_DARKEN_GAIN: f32 = 0.6;

/// Active stem-darkening strength, bit-cast `f32` in an atomic so raster reads
/// stay lock-free (mirrors the runtime color-override seams in
/// [`crate::text`]). `0.0` (the default) is a true no-op: coverage is written
/// byte-identically to the pre-feature atlas. Presentation-only — never affects
/// terminal cell contents or metrics.
///
/// **RV5 prototype.** This is a *global* coverage boost applied to every
/// rasterized glyph (it cannot see per-cell fg/bg at raster time). The
/// luminance-conditioned "light-on-dark only" variant is a documented in-shader
/// follow-up; see the audit findings.
static STEM_DARKEN: AtomicU32 = AtomicU32::new(0); // 0.0_f32.to_bits()

/// Set the global stem-darkening strength used when rasterizing glyphs.
///
/// Called once at native startup (and on the atlas-rebuild seam) by the
/// settings layer with the parsed `ODYTTY_STEM_DARKEN` value. The strength is
/// clamped to `0.0..=1.0`; `0.0` disables the boost and is pixel-identical to
/// the pre-feature renderer. Only glyphs rasterized *after* this call observe
/// the new value, which on the live path is the entire atlas (it is rebuilt
/// when the setting changes).
pub fn set_stem_darken(strength: f32) {
    let clamped = if strength.is_finite() {
        strength.clamp(0.0, 1.0)
    } else {
        0.0
    };
    STEM_DARKEN.store(clamped.to_bits(), Ordering::Relaxed);
}

/// The active stem-darkening strength (`0.0` when disabled).
fn stem_darken_strength() -> f32 {
    f32::from_bits(STEM_DARKEN.load(Ordering::Relaxed))
}

/// Apply stem-darkening to one 8-bit coverage sample.
///
/// Compensates for the irradiation illusion (light text on a dark field appears
/// thinner than it is) by raising partial coverage toward full, so stems hold
/// weight at small sizes. The mapping is `c^(1/(1+strength*GAIN))` on the
/// normalized coverage `c = value/255`.
///
/// **Pixel-identity guarantee:** at `strength <= 0.0` this returns `value`
/// unchanged, and the fully-uncovered (`0`) and fully-covered (`255`) endpoints
/// are always returned exactly — only intermediate (anti-aliased edge / thin
/// stem) coverage is boosted. So a disabled or absent setting reproduces the
/// historical atlas byte-for-byte.
fn apply_stem_darken(value: u8, strength: f32) -> u8 {
    if strength <= 0.0 || value == 0 || value == 255 {
        return value;
    }
    let c = value as f32 / 255.0;
    let exponent = 1.0 / (1.0 + strength * STEM_DARKEN_GAIN);
    let boosted = c.powf(exponent);
    (boosted * 255.0).round().clamp(0.0, 255.0) as u8
}

/// **Primary size knob.** Fraction of cell HEIGHT a fitted symbol/icon glyph
/// fills. The glyph is scaled so its ink height is `SYMBOL_CELL_FILL *
/// cell.height` (then width-capped so it can never clip), and centered on the
/// cell. Operator-tuned against ghostty on a dev build: ~0.82 matches; full
/// height (~0.95) reads too big, the old width-fit (~0.6 em cell width) too
/// small. Tune here during the dev-build eyeball.
const SYMBOL_CELL_FILL: f32 = 0.82;

/// Inset padding, as a fraction of the smaller cell dimension, used **only** to
/// narrow the width safety cap for a fitted icon (so a wide glyph scaled to
/// height still leaves a gutter inside the slot's drawable region and never
/// kisses a neighbour). In the height-fraction model the inset no longer drives
/// the size target — [`SYMBOL_CELL_FILL`] does — it just trims `max_draw_w`.
const SYMBOL_CELL_INSET: f32 = 0.10;

/// Maximum upscale applied when fitting a *sub-cell* symbol/icon glyph. The
/// height-fraction model needs more headroom than the old width-fit so small
/// icons reach the target fill; downscaling (oversized glyphs, the common
/// Nerd-Font case) is uncapped, upscaling is capped here so a 1-pixel dot does
/// not balloon to flood the cell. Tunable in the dev build.
const SYMBOL_MAX_UPSCALE: f32 = 2.0;

/// Opt-in directive to **fit-and-center** a glyph inside its cell box instead of
/// placing it at the font's natural left bearing on the text baseline. Passed
/// `Some` only for symbol-fallback / SYMMAP-override (icon) faces, whose em-size
/// ink does not match OdyTTY's cell aspect and would otherwise overflow/clip and
/// render as a ragged column. Every text path (primary font, ASCII build,
/// synthetic styles) passes `None`, leaving placement byte-identical.
#[derive(Clone, Copy)]
struct CellFit {
    /// Drawable cell-box width in pixels (`span * cell.width`).
    box_w: f32,
    /// Drawable cell-box height in pixels (`cell.height`).
    box_h: f32,
    /// Inset padding in pixels per side. In the height-fraction model this only
    /// narrows the width safety cap (`max_draw_w`); it no longer drives size.
    pad: f32,
}

/// Height-fraction fit scale for a symbol/icon glyph (aspect-preserving).
///
/// The glyph is scaled so its natural ink height (`nat_h`) reaches `target_h`
/// (= [`SYMBOL_CELL_FILL`] × cell height), then bounded by a width safety cap so
/// a wide glyph scaled to height still cannot exceed `max_draw_w` (the slot's
/// drawable region minus the inset gutter) and clip. Upscaling is capped at
/// `max_upscale`; downscaling is uncapped. Inputs are clamped strictly
/// positive; a non-finite or non-positive result degrades to `1.0` (no scaling).
fn symbol_fit_scale_v2(
    nat_w: f32,
    nat_h: f32,
    target_h: f32,
    max_draw_w: f32,
    max_upscale: f32,
) -> f32 {
    let s_h = target_h.max(1.0) / nat_h.max(1.0);
    let s_w = max_draw_w.max(1.0) / nat_w.max(1.0);
    let s = s_h.min(s_w).min(max_upscale);
    if s.is_finite() && s > 0.0 { s } else { 1.0 }
}

/// Resolved placement for a fitted symbol glyph: the scaled pen size and the
/// pre-biased atlas-pixel origin. The origin already subtracts the measured ink
/// bbox min, so in the draw closure `origin + bounds.min + (gx, gy)` lands the
/// centered ink top-left exactly (with the per-channel subpixel shift preserved
/// through `bounds.min.x`).
#[derive(Clone, Copy)]
struct FitPlacement {
    scale: PxScale,
    origin_x: f32,
    origin_y: f32,
}

/// Destination slot geometry for [`rasterize_glyph`].
struct SlotRegion {
    /// Outer top-left of the (lead) slot in atlas pixels.
    origin: (u32, u32),
    /// Shared per-cell metrics.
    cell: CellSize,
    /// Total horizontal extent in pixels — `slot_w(cell)` for a normal glyph,
    /// `span * slot_w(cell)` for a wide one — i.e. the right clip edge.
    outer_w: u32,
}

#[allow(clippy::too_many_arguments)]
fn rasterize_glyph(
    font: &FontVec,
    pen: Pen,
    ch: char,
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    region: SlotRegion,
    synth: SynthTransform,
    fit: Option<CellFit>,
) -> Option<GlyphInk> {
    let SlotRegion {
        origin,
        cell,
        outer_w,
    } = region;
    let (ox, oy) = origin;
    let scale = PxScale::from(pen.px);
    if !font_has_glyph(font, ch) {
        return None;
    }
    // Stem-darkening strength is read once per glyph; `0.0` (the default) makes
    // `apply_stem_darken` an identity so coverage is byte-identical to before.
    let stem = stem_darken_strength();
    // Cell inner origin, and the drawable region (cell + overflow margin) that
    // coverage may occupy, leaving the outer ATLAS_PAD bleed ring transparent.
    // `outer_w` is the slot's total horizontal extent in pixels — `slot_w(cell)`
    // for a normal glyph, `span * slot_w(cell)` for a wide (multi-cell) glyph —
    // so the right clip extends across every reserved cell of a wide slot.
    let border = slot_border(cell) as i32;
    let inner_x = ox as i32 + border;
    let inner_y = oy as i32 + border;
    // Symbol/icon fit (RV6+): when `fit` is `Some`, measure the glyph's natural
    // ink box at the body em-size, scale it so its ink height fills
    // `SYMBOL_CELL_FILL` of the cell (width-capped so a wide glyph can't clip),
    // and CENTER it on the cell box — ignoring the font's bearing and text
    // baseline, since icons are not baseline-aligned text. `None` (every text
    // path) leaves placement at the natural bearing on `pen.baseline`,
    // byte-identical to the pre-fit renderer.
    let fit_placement: Option<FitPlacement> = fit.and_then(|f| {
        let glyph_id = font.glyph_id(ch);
        let measure = |sc: PxScale| {
            font.outline_glyph(glyph_id.with_scale_and_position(sc, point(0.0, 0.0)))
                .map(|o| o.px_bounds())
        };
        let nb = measure(PxScale::from(pen.px))?;
        // Height-fraction target: fill SYMBOL_CELL_FILL of the cell height.
        let target_h = (f.box_h * SYMBOL_CELL_FILL).max(1.0);
        // Width safety cap: the slot's drawable region (cell + overflow margin)
        // inside the ATLAS_PAD bleed gutter, minus the inset pad on each side, so
        // a wide glyph scaled to height still cannot clip or kiss a neighbour.
        let max_draw_w = (outer_w as f32 - 2.0 * ATLAS_PAD as f32 - 2.0 * f.pad).max(1.0);
        let s = symbol_fit_scale_v2(
            nb.width(),
            nb.height(),
            target_h,
            max_draw_w,
            SYMBOL_MAX_UPSCALE,
        );
        let scaled = PxScale::from(pen.px * s);
        let sb = measure(scaled)?;
        // Centered ink top-left on the full cell box, both axes.
        let left = inner_x as f32 + (f.box_w - sb.width()) / 2.0;
        let top = inner_y as f32 + (f.box_h - sb.height()) / 2.0;
        Some(FitPlacement {
            scale: scaled,
            // Pre-subtract the measured bbox min so that a glyph re-outlined at
            // position `(shift_x, 0)` in the draw closure lands its ink top-left
            // exactly at `(left, top)`: `origin + bounds.min == (left, top)`
            // plus the per-channel subpixel `shift_x` carried in `bounds.min.x`.
            origin_x: left - sb.min.x,
            origin_y: top - sb.min.y,
        })
    });
    let x_lo = ox as i32 + ATLAS_PAD as i32;
    let x_hi = (ox + outer_w) as i32 - ATLAS_PAD as i32;
    let y_lo = oy as i32 + ATLAS_PAD as i32;
    let y_hi = (oy + slot_h(cell)) as i32 - ATLAS_PAD as i32;
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let mut draw_sample = |shift_x: f32, channel: Option<usize>| {
        // Fitted symbol glyphs re-outline at the scaled pen size on baseline 0
        // (the fit origin supplies absolute placement); text glyphs use the body
        // scale on `pen.baseline` exactly as before.
        let (use_scale, pos_y) = match fit_placement {
            Some(fp) => (fp.scale, 0.0),
            None => (scale, pen.baseline),
        };
        let glyph = font
            .glyph_id(ch)
            .with_scale_and_position(use_scale, point(shift_x, pos_y));
        let Some(outline) = font.outline_glyph(glyph) else {
            return;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let value = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
            if value == 0 {
                return; // uninked sample contributes no ink and no bounds
            }
            // Stem-darkening (RV5): boost partial coverage so light-on-dark
            // stems hold weight. Identity at the default strength of 0.0.
            let value = apply_stem_darken(value, stem);
            // Round to the nearest atlas pixel (truncation drifts edges and can
            // drop a glyph's final row/column). Fitted glyphs center on the cell
            // box via `fit_placement`; text glyphs sit on the cell baseline.
            let ay = match fit_placement {
                Some(fp) => (fp.origin_y + bounds.min.y + gy as f32).round() as i32,
                None => inner_y + (bounds.min.y + gy as f32).round() as i32,
            };
            if ay < y_lo || ay >= y_hi {
                return; // clip vertically (rows are unaffected by synthesis)
            }
            // Synthetic italic: shift this sample horizontally in proportion to
            // its height above the baseline. `pen.baseline` is the baseline in
            // the same absolute pixel space as `bounds`, so the unrounded glyph
            // y gives a smooth oblique. Rounding to whole atlas pixels introduces
            // minor stair-stepping along near-horizontal edges — acceptable for a
            // fallback face that exists only when no real italic is installed.
            // Synthesis is never combined with fit (icon faces use `synth`
            // identity), so the fitted branch needs no shear.
            let base_ax = match fit_placement {
                Some(fp) => (fp.origin_x + bounds.min.x + gx as f32).round() as i32,
                None => {
                    let shear_dx = if synth.shear != 0.0 {
                        let gy_abs = bounds.min.y + gy as f32;
                        (synth.shear * (pen.baseline - gy_abs)).round() as i32
                    } else {
                        0
                    };
                    inner_x + (bounds.min.x + gx as f32).round() as i32 + shear_dx
                }
            };
            // Synthetic bold: strike a second time shifted right by `embolden_px`
            // (max-combined by `write_coverage`), thickening horizontal weight
            // without touching verticals, the baseline, or the cell advance.
            let strikes = if synth.embolden_px > 0 { 2 } else { 1 };
            for s in 0..strikes {
                let ax = base_ax + s * synth.embolden_px as i32;
                if ax < x_lo || ax >= x_hi {
                    continue; // clip horizontally to the drawable region
                }
                write_coverage(data, width, subpixel, ax as u32, ay as u32, channel, value);
                min_x = min_x.min(ax);
                min_y = min_y.min(ay);
                max_x = max_x.max(ax);
                max_y = max_y.max(ay);
            }
        });
    };
    match subpixel {
        SubpixelMode::Off => draw_sample(0.0, None),
        SubpixelMode::Rgb => {
            draw_sample(-1.0 / 3.0, Some(0));
            draw_sample(0.0, Some(1));
            draw_sample(1.0 / 3.0, Some(2));
        }
        SubpixelMode::Bgr => {
            draw_sample(-1.0 / 3.0, Some(2));
            draw_sample(0.0, Some(1));
            draw_sample(1.0 / 3.0, Some(0));
        }
    }

    if subpixel.enabled() {
        lcd_filter_subpixel_region(
            data,
            width,
            subpixel,
            x_lo as u32,
            y_lo as u32,
            (x_hi - x_lo) as u32,
            (y_hi - y_lo) as u32,
        );
    }

    let Some((filtered_min_x, filtered_min_y, filtered_max_x, filtered_max_y)) =
        scan_coverage_bounds(data, width, subpixel, x_lo, y_lo, x_hi, y_hi)
    else {
        return None; // outline produced no inked pixels in the drawable region
    };
    min_x = filtered_min_x;
    min_y = filtered_min_y;
    max_x = filtered_max_x;
    max_y = filtered_max_y;

    Some(GlyphInk {
        offset_x: min_x - inner_x,
        offset_y: min_y - inner_y,
        width: (max_x - min_x + 1) as u32,
        height: (max_y - min_y + 1) as u32,
    })
}

fn scan_coverage_bounds(
    data: &[u8],
    width: u32,
    subpixel: SubpixelMode,
    x_lo: i32,
    y_lo: i32,
    x_hi: i32,
    y_hi: i32,
) -> Option<(i32, i32, i32, i32)> {
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for ay in y_lo..y_hi {
        for ax in x_lo..x_hi {
            let base =
                (ay as u32 * width + ax as u32) as usize * subpixel.bytes_per_pixel() as usize;
            let inked = match subpixel {
                SubpixelMode::Off => data[base] > 0,
                SubpixelMode::Rgb | SubpixelMode::Bgr => {
                    data[base..base + 3].iter().any(|&v| v > 0)
                }
            };
            if inked {
                min_x = min_x.min(ax);
                min_y = min_y.min(ay);
                max_x = max_x.max(ax);
                max_y = max_y.max(ay);
            }
        }
    }
    (max_x >= min_x).then_some((min_x, min_y, max_x, max_y))
}

const LCD_FILTER_TAPS: [u16; 5] = [1, 2, 3, 2, 1];
const LCD_FILTER_SUM: u16 = 9;

fn lcd_filter_subpixel_region(
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    x0: u32,
    y0: u32,
    region_w: u32,
    region_h: u32,
) {
    if !subpixel.enabled() || region_w == 0 || region_h == 0 {
        return;
    }

    let samples_per_row = region_w as usize * 3;
    let mut src = vec![0u8; samples_per_row];
    let mut dst = vec![0u8; samples_per_row];
    for row in 0..region_h {
        let y = y0 + row;
        for dx in 0..region_w {
            let x = x0 + dx;
            let base = ((y * width + x) * 4) as usize;
            for physical in 0..3 {
                let channel = physical_channel(subpixel, physical);
                src[dx as usize * 3 + physical] = data[base + channel];
            }
        }

        for (i, dst_sample) in dst.iter_mut().enumerate().take(samples_per_row) {
            let mut weighted = 0u16;
            for (tap, weight) in LCD_FILTER_TAPS.iter().enumerate() {
                let source_i = i as isize + tap as isize - 2;
                if (0..samples_per_row as isize).contains(&source_i) {
                    weighted += src[source_i as usize] as u16 * weight;
                }
            }
            *dst_sample = ((weighted + LCD_FILTER_SUM / 2) / LCD_FILTER_SUM) as u8;
        }

        for dx in 0..region_w {
            let x = x0 + dx;
            let base = ((y * width + x) * 4) as usize;
            let mut any = false;
            for physical in 0..3 {
                let channel = physical_channel(subpixel, physical);
                let value = dst[dx as usize * 3 + physical];
                data[base + channel] = value;
                any |= value > 0;
            }
            data[base + 3] = if any { 255 } else { 0 };
        }
    }
}

fn physical_channel(subpixel: SubpixelMode, physical: usize) -> usize {
    match subpixel {
        SubpixelMode::Off => 0,
        SubpixelMode::Rgb => physical,
        SubpixelMode::Bgr => 2 - physical,
    }
}

/// Rasterize a geometric box-drawing / block / Powerline glyph into the slot
/// whose outer top-left is `region.origin` (RV2).
///
/// The coverage bitmap comes from [`crate::boxdraw::coverage`], computed at the
/// exact cell pixel size, and is written into the slot's inner cell region (no
/// overflow, no synthesis, no stem-darkening — the geometry is already crisp).
/// Achromatic coverage is replicated across all channels for subpixel atlases.
/// Returns the full-cell ink extent, or `None` if the codepoint is uncovered or
/// produced an empty bitmap (the caller then falls back to the cell box).
fn rasterize_geometric(
    ch: char,
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    region: SlotRegion,
) -> Option<GlyphInk> {
    let SlotRegion { origin, cell, .. } = region;
    let (ox, oy) = origin;
    let border = slot_border(cell);
    let inner_x = ox + border;
    let inner_y = oy + border;
    let cov = crate::boxdraw::coverage(ch, cell.width, cell.height)?;
    let mut any = false;
    for cy in 0..cell.height {
        for cx in 0..cell.width {
            let value = cov[(cy * cell.width + cx) as usize];
            if value == 0 {
                continue;
            }
            any = true;
            let ax = inner_x + cx;
            let ay = inner_y + cy;
            match subpixel {
                SubpixelMode::Off => write_coverage(data, width, subpixel, ax, ay, None, value),
                SubpixelMode::Rgb | SubpixelMode::Bgr => {
                    for channel in 0..3 {
                        write_coverage(data, width, subpixel, ax, ay, Some(channel), value);
                    }
                }
            }
        }
    }
    if !any {
        return None;
    }
    Some(GlyphInk {
        offset_x: 0,
        offset_y: 0,
        width: cell.width,
        height: cell.height,
    })
}

fn write_coverage(
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    ax: u32,
    ay: u32,
    channel: Option<usize>,
    value: u8,
) {
    let base = (ay * width + ax) as usize * subpixel.bytes_per_pixel() as usize;
    match subpixel {
        SubpixelMode::Off => {
            if value > data[base] {
                data[base] = value;
            }
        }
        SubpixelMode::Rgb | SubpixelMode::Bgr => {
            let channel = channel.unwrap_or(1).min(2);
            if value > data[base + channel] {
                data[base + channel] = value;
            }
            data[base + 3] = 255;
        }
    }
}

/// Draw the synthesized missing-glyph fallback — a hollow rectangle inset from
/// the cell edges — into the atlas cell at `(ox, oy)`. Font-independent so the
/// fallback looks the same regardless of which font is loaded.
fn draw_fallback_box(
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    ox: u32,
    oy: u32,
    cell: CellSize,
) {
    let cw = cell.width;
    let ch = cell.height;
    let mut set = |x: u32, y: u32| {
        if x < cw && y < ch {
            match subpixel {
                SubpixelMode::Off => {
                    data[((oy + y) * width + ox + x) as usize] = 255;
                }
                SubpixelMode::Rgb | SubpixelMode::Bgr => {
                    let idx = ((oy + y) * width + ox + x) as usize * 4;
                    data[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    };
    if cw < 3 || ch < 3 {
        // Degenerate cell: a single inked pixel is the best "visible" marker.
        set(cw / 2, ch / 2);
        return;
    }
    let inset_x = (cw / 6).max(1);
    let inset_y = (ch / 6).max(1);
    let thick = (cw / 12).max(1);
    let x0 = inset_x;
    let x1 = cw - inset_x - 1;
    let y0 = inset_y;
    let y1 = ch - inset_y - 1;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let on_v = x < x0 + thick || x + thick > x1;
            let on_h = y < y0 + thick || y + thick > y1;
            if on_v || on_h {
                set(x, y);
            }
        }
    }
}

#[cfg(test)]
mod tests;
