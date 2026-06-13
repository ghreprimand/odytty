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

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
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

/// Whether the font maps `ch` to a real (non-`.notdef`) glyph. `ab_glyph`
/// returns glyph id 0 for codepoints the font lacks, so a missing glyph is
/// detected here rather than relying on the font's own `.notdef` outline.
fn font_has_glyph(font: &FontVec, ch: char) -> bool {
    font.glyph_id(ch).0 != 0
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
    /// Optional symbol / Nerd-font fallback (RV6). When set, a printable
    /// codepoint the **primary** font lacks is rasterized from this font
    /// instead of the hollow-box tofu slot — but only when
    /// [`fallback::is_symbol_codepoint`] classifies it as a PUA icon **and**
    /// this fallback font actually has the glyph. `None` (the default, and the
    /// state on any freshly built atlas) preserves the historical missing-glyph
    /// path byte-for-byte. Held as an `Arc` so the per-glyph lookup can clone a
    /// handle and rasterize without conflicting with the `&mut self` bitmap
    /// borrow. The native layer resolves and sets it after build, mirroring the
    /// synthetic-styles / geometric switches.
    fallback: Option<Arc<FontVec>>,
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
        let mut data = vec![0u8; (width * height * subpixel.bytes_per_pixel()) as usize];

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
            dynamic: HashMap::new(),
            px,
            revision: 0,
            dirty: false,
            subpixel,
            synthetic: 0,
            geometric: false,
            fallback: None,
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
        // Geometric box-drawing (RV2): when enabled, recognized line/block/
        // Powerline codepoints are rasterized from cell-aligned geometry instead
        // of the font glyph — and render even if the font lacks the codepoint.
        let geometric = self.geometric && crate::boxdraw::covers(ch);
        // RV6: when the primary font lacks the glyph, a symbol/Nerd fallback
        // font (if configured) rasterizes PUA prompt icons instead of tofu.
        // `None` here means either no fallback is set, the codepoint is not a
        // symbol, or the fallback also lacks it — all of which fall through to
        // the historical hollow-box slot, keeping the default path identical.
        let mut symbol_font: Option<Arc<FontVec>> = None;
        if !geometric && !font_has_glyph(font, ch) {
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
            // A symbol fallback glyph renders from the fallback face with no
            // synthetic transform (icons are not emboldened/sheared); otherwise
            // the primary font and the style's synthetic mask apply as usual.
            let (raster_font, synth): (&FontVec, SynthTransform) = match symbol_font.as_deref() {
                Some(fb) => (fb, SynthTransform::none()),
                None => (font, self.synth_for(style)),
            };
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
        if self.next_slot + span > MAX_ATLAS_SLOTS {
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
        if self.next_slot + 1 > MAX_ATLAS_SLOTS {
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
                (self.width * self.height * self.subpixel.bytes_per_pixel()) as usize,
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

    /// Install (or clear) the symbol / Nerd-font fallback (RV6).
    ///
    /// When `Some`, a printable codepoint the **primary** font lacks is
    /// rasterized from this font — but only when
    /// [`fallback::is_symbol_codepoint`] classifies it as a PUA prompt icon and
    /// this font actually has the glyph; otherwise the historical hollow-box
    /// slot is used. `None` (the default) restores the pre-feature missing-glyph
    /// path exactly, so a build with no fallback is byte-identical to the
    /// minimal renderer.
    ///
    /// Like [`Self::set_synthetic_styles`] / [`Self::set_geometric_boxdraw`],
    /// this only governs glyphs rasterized after it is set; the native layer
    /// installs it on a freshly built atlas and reinstalls it after a rebuild,
    /// so the dynamic region never mixes resolved and unresolved fallbacks.
    pub fn set_fallback_font(&mut self, font: Option<Arc<FontVec>>) {
        self.fallback = font;
    }

    /// The fallback font to rasterize `ch` from when the primary lacks it, or
    /// `None` to keep the hollow-box behavior. Returns the configured fallback
    /// only when `ch` is a PUA symbol codepoint and the fallback face actually
    /// has a glyph for it.
    fn symbol_fallback(&self, ch: char) -> Option<Arc<FontVec>> {
        let fb = self.fallback.as_ref()?;
        if fallback::is_symbol_codepoint(ch) && font_has_glyph(fb, ch) {
            Some(Arc::clone(fb))
        } else {
            None
        }
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

fn rasterize_glyph(
    font: &FontVec,
    pen: Pen,
    ch: char,
    data: &mut [u8],
    width: u32,
    subpixel: SubpixelMode,
    region: SlotRegion,
    synth: SynthTransform,
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
    let x_lo = ox as i32 + ATLAS_PAD as i32;
    let x_hi = (ox + outer_w) as i32 - ATLAS_PAD as i32;
    let y_lo = oy as i32 + ATLAS_PAD as i32;
    let y_hi = (oy + slot_h(cell)) as i32 - ATLAS_PAD as i32;
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let mut draw_sample = |shift_x: f32, channel: Option<usize>| {
        let glyph = font
            .glyph_id(ch)
            .with_scale_and_position(scale, point(shift_x, pen.baseline));
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
            // drop a glyph's final row/column).
            let ay = inner_y + (bounds.min.y + gy as f32).round() as i32;
            if ay < y_lo || ay >= y_hi {
                return; // clip vertically (rows are unaffected by synthesis)
            }
            // Synthetic italic: shift this sample horizontally in proportion to
            // its height above the baseline. `pen.baseline` is the baseline in
            // the same absolute pixel space as `bounds`, so the unrounded glyph
            // y gives a smooth oblique. Rounding to whole atlas pixels introduces
            // minor stair-stepping along near-horizontal edges — acceptable for a
            // fallback face that exists only when no real italic is installed.
            let shear_dx = if synth.shear != 0.0 {
                let gy_abs = bounds.min.y + gy as f32;
                (synth.shear * (pen.baseline - gy_abs)).round() as i32
            } else {
                0
            };
            let base_ax = inner_x + (bounds.min.x + gx as f32).round() as i32 + shear_dx;
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
    if max_x < min_x {
        return None; // outline produced no inked pixels in the drawable region
    }
    Some(GlyphInk {
        offset_x: min_x - inner_x,
        offset_y: min_y - inner_y,
        width: (max_x - min_x + 1) as u32,
        height: (max_y - min_y + 1) as u32,
    })
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
