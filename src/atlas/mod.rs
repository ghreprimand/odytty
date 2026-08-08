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

mod allocation;
mod build;
mod insertion;
mod lookup;
mod ownership;
mod raster;
mod upload;

pub use raster::set_stem_darken;
#[cfg(test)]
pub(crate) use raster::stem_darken_strength_for_test;
use raster::{
    CellFit, SYMBOL_CELL_INSET, SlotRegion, draw_fallback_box, rasterize_geometric,
    rasterize_glyph, rasterize_glyph_id, slot_border, slot_h, slot_offset, slot_w,
};
#[cfg(test)]
use raster::{
    SYMBOL_CELL_FILL, apply_stem_darken, lcd_filter_subpixel_region, overflow_margin,
    stem_darken_strength, symbol_fit_scale_v2,
};

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
/// live render path resolves `Regular` only today; a future change will
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

/// Atlas identity for one contextual glyph. The face fingerprint prevents a
/// shaped ID from being reused across font faces; span and anchor keep the same
/// outline distinct when it is positioned in a different source-cell window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShapedGlyphKey {
    pub face_fingerprint: u64,
    pub style: FontStyle,
    pub glyph_id: u16,
    pub span_cells: u8,
    pub anchor_cell: u8,
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
    /// Contextual OpenType glyphs keyed independently from scalar codepoints.
    /// Empty on every freshly built atlas and untouched while ligatures are off.
    shaped: HashMap<ShapedGlyphKey, u32>,
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

#[cfg(test)]
mod tests;
