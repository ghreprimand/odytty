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

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use unicode_width::UnicodeWidthChar;

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

/// Rasterization scale and shared baseline for one glyph. The `baseline` is the
/// single per-atlas value (see [`GlyphAtlas::build`]) every glyph is placed on.
#[derive(Clone, Copy)]
struct Pen {
    px: f32,
    baseline: f32,
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
        if !font_has_glyph(font, ch) {
            // Font lacks the glyph: cache the fallback decision, draw nothing new.
            self.dynamic.insert((style, ch), FALLBACK_SLOT);
            return Some(self.slot_uv(FALLBACK_SLOT));
        }
        let cells = glyph_cells(ch);
        let Some(slot) = self.allocate_slots(cells) else {
            // Atlas is at its hard cap: degrade to the fallback box.
            self.dynamic.insert((style, ch), FALLBACK_SLOT);
            return Some(self.slot_uv(FALLBACK_SLOT));
        };
        let origin = slot_offset(slot, self.cols, self.cell);
        let ink = rasterize_glyph(
            font,
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
        )
        .unwrap_or_else(|| GlyphInk::cell(self.cell));
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
            // Round to the nearest atlas pixel (truncation drifts edges and can
            // drop a glyph's final row/column).
            let ax = inner_x + (bounds.min.x + gx as f32).round() as i32;
            let ay = inner_y + (bounds.min.y + gy as f32).round() as i32;
            if ax < x_lo || ax >= x_hi || ay < y_lo || ay >= y_hi {
                return; // clip to this glyph's drawable region (cell + margin)
            }
            write_coverage(data, width, subpixel, ax as u32, ay as u32, channel, value);
            min_x = min_x.min(ax);
            min_y = min_y.min(ay);
            max_x = max_x.max(ax);
            max_y = max_y.max(ay);
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
mod tests {
    use super::*;
    use crate::text::load_font;

    fn test_font() -> Option<FontVec> {
        load_font().ok()
    }

    /// The inner top-left pixel `(x, y)` of the cell a UV rect points at. The
    /// inner origin is an integer pixel, so reconstructing it from the
    /// normalized UV round-trips exactly.
    fn inner_origin(atlas: &GlyphAtlas, uv: [f32; 4]) -> (u32, u32) {
        (
            (uv[0] * atlas.width as f32).round() as u32,
            (uv[1] * atlas.height as f32).round() as u32,
        )
    }

    /// Sum the coverage bytes of the inner atlas cell a UV rect points at, in
    /// the atlas's current pixel space.
    fn cell_ink(atlas: &GlyphAtlas, uv: [f32; 4]) -> u64 {
        let (cx, cy) = inner_origin(atlas, uv);
        let mut sum = 0u64;
        for y in cy..cy + atlas.cell.height {
            for x in cx..cx + atlas.cell.width {
                sum += atlas.data[(y * atlas.width + x) as usize] as u64;
            }
        }
        sum
    }

    fn subpixel_cell_channels(atlas: &GlyphAtlas, uv: [f32; 4]) -> [u64; 4] {
        let (cx, cy) = inner_origin(atlas, uv);
        let mut sum = [0u64; 4];
        for y in cy..cy + atlas.cell.height {
            for x in cx..cx + atlas.cell.width {
                let idx = ((y * atlas.width + x) * 4) as usize;
                for c in 0..4 {
                    sum[c] += atlas.data[idx + c] as u64;
                }
            }
        }
        sum
    }

    /// A non-ASCII codepoint the loaded font actually has an outline for, used
    /// to exercise the dynamic region. `None` if none is found (unusual).
    fn glyph_bearing_non_ascii(font: &FontVec) -> Option<char> {
        (0x00A1u32..=0x05FF)
            .filter_map(char::from_u32)
            .find(|&ch| font_has_glyph(font, ch))
    }

    #[test]
    fn atlas_has_positive_metrics_and_glyph_coverage() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 28.0);
        assert!(atlas.cell.width > 0 && atlas.cell.height > 0);
        assert!(atlas.cell.baseline <= atlas.cell.height);
        assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
        // A glyph with ink (e.g. 'M') must produce non-zero coverage.
        assert!(atlas.data.iter().any(|&v| v > 0));
        // UV rects exist for printable ASCII and not for control chars.
        assert!(atlas.uv_rect('A').is_some());
        assert!(atlas.uv_rect('\n').is_none());
    }

    #[test]
    fn default_atlas_stays_single_channel() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 24.0);

        assert_eq!(atlas.subpixel_mode(), SubpixelMode::Off);
        assert_eq!(atlas.bytes_per_row(), atlas.width);
        assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
    }

    #[test]
    fn subpixel_atlas_stores_rgb_coverage_without_geometry_change() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let gray = GlyphAtlas::build(&font, 24.0);
        let rgb = GlyphAtlas::build_with_subpixel(&font, 24.0, SubpixelMode::Rgb);

        assert_eq!(rgb.subpixel_mode(), SubpixelMode::Rgb);
        assert_eq!(rgb.cell, gray.cell);
        assert_eq!(rgb.width, gray.width);
        assert_eq!(rgb.height, gray.height);
        assert_eq!(rgb.bytes_per_row(), rgb.width * 4);
        assert_eq!(rgb.data.len(), (rgb.width * rgb.height * 4) as usize);

        let channels = subpixel_cell_channels(&rgb, rgb.uv_rect('M').unwrap());
        assert!(
            channels[0] > 0 && channels[1] > 0 && channels[2] > 0,
            "RGB subpixel atlas should populate all color channels: {channels:?}"
        );
        assert!(
            channels[3] > 0,
            "subpixel atlas should mark inked texels with opaque alpha"
        );
    }

    #[test]
    fn fallback_box_is_visible_but_hollow() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 24.0);
        let ink = cell_ink(&atlas, atlas.slot_uv(FALLBACK_SLOT));
        // Has visible ink (the box border) ...
        assert!(ink > 0, "fallback box should have ink");
        // ... but is not a solid block (hollow interior).
        let solid = (atlas.cell.width * atlas.cell.height) as u64 * 255;
        assert!(ink < solid, "fallback box should be hollow, not solid");
    }

    #[test]
    fn uv_rect_falls_back_for_unsupported_printable() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 24.0);
        let fallback = atlas.slot_uv(FALLBACK_SLOT);
        // ASCII resolves to its own cell, never the fallback.
        assert_ne!(atlas.uv_rect('A'), Some(fallback));
        // Unsupported printable codepoints share the one fallback box.
        assert_eq!(atlas.uv_rect('é'), Some(fallback));
        assert_eq!(atlas.uv_rect('★'), Some(fallback));
        assert_eq!(atlas.uv_rect('\u{1F600}'), Some(fallback));
        // Control and whitespace draw nothing.
        assert!(atlas.uv_rect('\t').is_none());
    }

    #[test]
    fn ensure_rasterizes_real_glyph_and_flags_dirty() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(ch) = glyph_bearing_non_ascii(&font) else {
            eprintln!("skipping: font has no non-ASCII glyph");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        let fallback = atlas.slot_uv(FALLBACK_SLOT);
        assert_eq!(atlas.uv_rect(ch), Some(fallback)); // not yet resident

        let uv = atlas.ensure(&font, ch).expect("real glyph uv");
        assert_ne!(uv, fallback, "ensure should pick a real slot, not fallback");
        assert!(atlas.take_dirty(), "insertion must flag dirty");
        // Now resident: the immutable lookup resolves to the same real slot.
        assert_eq!(atlas.uv_rect(ch), Some(uv));
        // The new cell actually got ink.
        assert!(cell_ink(&atlas, uv) > 0);

        // A repeat is a pure cache hit: same uv, no new slot, not dirty.
        let count = atlas.slot_count();
        let uv2 = atlas.ensure(&font, ch).expect("cached uv");
        assert_eq!(uv2, uv);
        assert_eq!(atlas.slot_count(), count);
        assert!(!atlas.take_dirty());
    }

    #[test]
    fn ensure_missing_glyph_uses_fallback_without_a_slot() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        let fallback = atlas.slot_uv(FALLBACK_SLOT);
        let count = atlas.slot_count();
        // A high private-use codepoint no monospace font maps.
        let uv = atlas.ensure(&font, '\u{10FFFD}').expect("fallback uv");
        assert_eq!(uv, fallback);
        assert_eq!(
            atlas.slot_count(),
            count,
            "fallback must not consume a slot"
        );
        assert!(!atlas.take_dirty(), "no pixels changed, so not dirty");
    }

    #[test]
    fn ensure_grows_atlas_and_preserves_existing_glyphs() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(first) = glyph_bearing_non_ascii(&font) else {
            eprintln!("skipping: font has no non-ASCII glyph");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        let base_height = atlas.height;
        let base_count = atlas.slot_count();

        // Insert the first glyph and remember its ink.
        let uv_first = atlas.ensure(&font, first).expect("first uv");
        let ink_first = cell_ink(&atlas, uv_first);
        assert!(
            atlas.height > base_height,
            "first dynamic insert should grow"
        );

        // Insert many more distinct glyph-bearing codepoints to force more
        // growth pages; every returned UV must stay within the bitmap.
        let mut inserted = 1u32;
        for ch in (0x00A1u32..=0x05FF).filter_map(char::from_u32) {
            if ch == first || !font_has_glyph(&font, ch) {
                continue;
            }
            let uv = atlas.ensure(&font, ch).expect("dynamic uv");
            assert!(uv[3] <= 1.0 + 1e-6, "uv must stay in bounds after growth");
            inserted += 1;
            if inserted >= 200 {
                break;
            }
        }
        assert!(
            atlas.slot_count() > base_count + 1,
            "atlas should have grown"
        );

        // The first glyph's pixels survived every intervening growth: its cell
        // offset never moved, so its ink (recomputed against the current size)
        // is unchanged.
        let ink_after = cell_ink(&atlas, atlas.uv_rect(first).unwrap());
        assert_eq!(
            ink_after, ink_first,
            "growth must not corrupt existing glyphs"
        );
        assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
    }

    #[test]
    fn rebuild_is_a_full_invalidation() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(ch) = glyph_bearing_non_ascii(&font) else {
            eprintln!("skipping: font has no non-ASCII glyph");
            return;
        };
        let mut big = GlyphAtlas::build(&font, 24.0);
        big.ensure(&font, ch);
        assert!(big.slot_count() > FIRST_DYNAMIC_SLOT);

        // A size change is a fresh build: different cell metrics, no carried-over
        // dynamic glyphs (no mixed-size glyphs can coexist), revision reset.
        let small = GlyphAtlas::build(&font, 14.0);
        assert_ne!(
            big.cell, small.cell,
            "different px should change cell metrics"
        );
        assert_eq!(small.slot_count(), FIRST_DYNAMIC_SLOT, "no dynamic glyphs");
        assert_eq!(small.revision(), 0);
        assert_eq!(small.uv_rect(ch), Some(small.slot_uv(FALLBACK_SLOT)));
        assert_eq!(
            small.height,
            slot_h(small.cell) * FIRST_DYNAMIC_SLOT.div_ceil(ATLAS_COLS)
        );
    }

    /// The atlas bitmap reserves a border (bleed gutter + overflow margin) around
    /// every slot, so the bitmap is wider/taller than a borderless pack and
    /// adjacent inner cells are separated by `2·slot_border` pixels — the guard
    /// against bleed plus the room for overflow ink.
    #[test]
    fn slots_carry_a_padding_gutter() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 24.0);
        let border = slot_border(atlas.cell);
        // The border is at least the bleed gutter and adds the overflow margin.
        assert!(border >= ATLAS_PAD);
        assert_eq!(border, ATLAS_PAD + overflow_margin(atlas.cell));
        // Bitmap dimensions account for the full border on every slot.
        assert_eq!(atlas.width, atlas.cols * (atlas.cell.width + 2 * border));

        // Two horizontally-adjacent inner cells (slots 1 and 2) are separated by
        // a full 2·border-pixel gap, so sampling one cannot reach the other's ink
        // and each has room to overflow into its own margin.
        let a = atlas.slot_uv(1);
        let b = atlas.slot_uv(2);
        let a_right = (a[2] * atlas.width as f32).round() as i32;
        let b_left = (b[0] * atlas.width as f32).round() as i32;
        assert_eq!(b_left - a_right, (2 * border) as i32);
    }

    /// Box-drawing strokes must reach the cell edges so adjacent cells join
    /// seamlessly. The horizontal line U+2500 should ink the full cell width;
    /// the vertical line U+2502 should ink the full cell height.
    #[test]
    fn box_drawing_strokes_reach_cell_edges() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        if !font_has_glyph(&font, '\u{2500}') || !font_has_glyph(&font, '\u{2502}') {
            eprintln!("skipping: font lacks box-drawing glyphs");
            return;
        }
        let mut atlas = GlyphAtlas::build(&font, 28.0);
        let cw = atlas.cell.width;
        let ch = atlas.cell.height;

        // Horizontal line: across its inked row band, ink reaches the left and
        // right cell edges (within 1px tolerance for font bearing).
        let h = atlas.ensure(&font, '\u{2500}').expect("U+2500 uv");
        let (hx, hy) = inner_origin(&atlas, h);
        let (mut min_col, mut max_col) = (cw, 0u32);
        for y in hy..hy + ch {
            for x in hx..hx + cw {
                if atlas.data[(y * atlas.width + x) as usize] > 0 {
                    min_col = min_col.min(x - hx);
                    max_col = max_col.max(x - hx);
                }
            }
        }
        assert!(
            min_col <= 1,
            "─ should ink the left edge (min_col={min_col})"
        );
        assert!(
            max_col >= cw - 2,
            "─ should ink the right edge (max_col={max_col}, cw={cw})"
        );

        // Vertical line: across its inked column band, ink reaches the top and
        // bottom cell edges.
        let v = atlas.ensure(&font, '\u{2502}').expect("U+2502 uv");
        let (vx, vy) = inner_origin(&atlas, v);
        let (mut min_row, mut max_row) = (ch, 0u32);
        for y in vy..vy + ch {
            for x in vx..vx + cw {
                if atlas.data[(y * atlas.width + x) as usize] > 0 {
                    min_row = min_row.min(y - vy);
                    max_row = max_row.max(y - vy);
                }
            }
        }
        assert!(
            min_row <= 1,
            "│ should ink the top edge (min_row={min_row})"
        );
        assert!(
            max_row >= ch - 2,
            "│ should ink the bottom edge (max_row={max_row}, ch={ch})"
        );
    }

    /// Every glyph is placed on the one shared integer baseline. Two cap-height
    /// letters ('E', 'F') with flat tops therefore start inking on the same row,
    /// proving a single consistent baseline rather than per-glyph drift.
    #[test]
    fn glyphs_share_one_baseline() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 26.0);
        let top_row = |ch: char| -> Option<u32> {
            let uv = atlas.uv_rect(ch)?;
            let (ix, iy) = inner_origin(&atlas, uv);
            for y in iy..iy + atlas.cell.height {
                for x in ix..ix + atlas.cell.width {
                    if atlas.data[(y * atlas.width + x) as usize] > 0 {
                        return Some(y - iy);
                    }
                }
            }
            None
        };
        // 'E' and 'F' share a flat cap top; on a consistent baseline their first
        // inked row matches.
        assert_eq!(top_row('E'), top_row('F'));
        // The recorded baseline sits within the cell box.
        assert!(atlas.cell.baseline > 0 && atlas.cell.baseline <= atlas.cell.height);
    }

    /// A descender ('g') inks the lower part of the cell and is not cropped at
    /// the cell box — its ink extends below the baseline.
    #[test]
    fn descender_is_not_cropped() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 28.0);
        let uv = atlas.uv_rect('g').expect("g uv");
        let (ix, iy) = inner_origin(&atlas, uv);
        let baseline = atlas.cell.baseline;
        // Some ink exists strictly below the baseline row (the descender).
        let mut below = false;
        for y in (iy + baseline + 1)..(iy + atlas.cell.height) {
            for x in ix..ix + atlas.cell.width {
                if atlas.data[(y * atlas.width + x) as usize] > 0 {
                    below = true;
                }
            }
        }
        assert!(below, "'g' descender should ink below the baseline");
    }

    /// The default `FontStyle` is `Regular`, and the regular styled lookups are
    /// byte-for-byte the legacy ones, so existing native call sites are
    /// unaffected by the `(style, char)` keying.
    #[test]
    fn regular_style_matches_legacy_lookup() {
        assert_eq!(FontStyle::default(), FontStyle::Regular);
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 24.0);
        // ASCII, fallback, and control behave identically through both entry points.
        assert_eq!(
            atlas.uv_rect('A'),
            atlas.uv_rect_styled(FontStyle::Regular, 'A')
        );
        assert_eq!(
            atlas.uv_rect('\u{2603}'),
            atlas.uv_rect_styled(FontStyle::Regular, '\u{2603}')
        );
        assert_eq!(
            atlas.uv_rect('\n'),
            atlas.uv_rect_styled(FontStyle::Regular, '\n')
        );
    }

    /// A non-`Regular` style of a glyph-bearing codepoint lands in its own slot,
    /// so styled variants never collide with the regular glyph.
    #[test]
    fn styled_glyph_gets_a_distinct_slot() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(ch) = glyph_bearing_non_ascii(&font) else {
            eprintln!("skipping: font has no non-ASCII glyph");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        let regular = atlas
            .ensure_styled(&font, FontStyle::Regular, ch)
            .expect("regular uv");
        let count_after_regular = atlas.slot_count();
        let bold = atlas
            .ensure_styled(&font, FontStyle::Bold, ch)
            .expect("bold uv");
        // Distinct style => distinct slot => distinct uv, and a new slot consumed.
        assert_ne!(regular, bold, "bold must not reuse the regular slot");
        assert!(
            atlas.slot_count() > count_after_regular,
            "bold should allocate"
        );
        // Re-resolving each style is a stable cache hit.
        assert_eq!(atlas.uv_rect_styled(FontStyle::Regular, ch), Some(regular));
        assert_eq!(atlas.uv_rect_styled(FontStyle::Bold, ch), Some(bold));
    }

    /// For a non-`Regular` style, even printable ASCII flows through the dynamic
    /// region: the immutable lookup returns the fallback until `ensure_styled`
    /// rasterizes it, after which both resolve to the same real slot.
    #[test]
    fn styled_ascii_uses_dynamic_region() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        let fallback = atlas.slot_uv(FALLBACK_SLOT);
        // Bold 'A' is not prebuilt: immutable lookup is the fallback box.
        assert_eq!(atlas.uv_rect_styled(FontStyle::Bold, 'A'), Some(fallback));
        // Regular 'A' still resolves to its prebuilt slot, untouched.
        assert_ne!(atlas.uv_rect('A'), Some(fallback));
        // ensure_styled rasterizes a real bold-keyed slot, distinct from regular.
        let bold_a = atlas
            .ensure_styled(&font, FontStyle::Bold, 'A')
            .expect("bold A uv");
        assert_ne!(bold_a, fallback);
        assert_ne!(Some(bold_a), atlas.uv_rect('A'));
        assert_eq!(atlas.uv_rect_styled(FontStyle::Bold, 'A'), Some(bold_a));
    }

    /// Scan a slot's full drawable region for the tight bounding box of inked
    /// pixels, returning `(min_x, min_y, max_x, max_y)` in absolute atlas pixels.
    fn scan_slot_ink(atlas: &GlyphAtlas, slot: u32) -> Option<(i32, i32, i32, i32)> {
        let (ox, oy) = slot_offset(slot, atlas.cols, atlas.cell);
        let (sw, sh) = (slot_w(atlas.cell), slot_h(atlas.cell));
        let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in oy..oy + sh {
            for x in ox..ox + sw {
                if atlas.data[(y * atlas.width + x) as usize] > 0 {
                    minx = minx.min(x as i32);
                    miny = miny.min(y as i32);
                    maxx = maxx.max(x as i32);
                    maxy = maxy.max(y as i32);
                }
            }
        }
        (maxx >= minx).then_some((minx, miny, maxx, maxy))
    }

    /// The bearing-aware quad for a missing/unsupported glyph is the fallback
    /// box, and its bounds are the full cell with a UV identical to `uv_rect` —
    /// so missing glyphs render exactly as before (no regression).
    #[test]
    fn glyph_quad_fallback_is_full_cell_and_matches_uv_rect() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 24.0);
        let q = atlas.glyph_quad('é').expect("fallback quad");
        assert_eq!((q.offset_x, q.offset_y), (0, 0));
        assert_eq!((q.width, q.height), (atlas.cell.width, atlas.cell.height));
        assert_eq!(q.uv, atlas.uv_rect('é').expect("fallback uv"));
        // Control characters resolve to nothing through either entry point;
        // space resolves to its (blank) ASCII slot exactly like `uv_rect`, and
        // the grid skips it via the `ch != ' '` guard rather than a `None`.
        assert!(atlas.glyph_quad('\n').is_none());
        assert_eq!(atlas.uv_rect('\n'), None);
        assert!(atlas.glyph_quad(' ').is_some());
        assert!(atlas.uv_rect(' ').is_some());
    }

    /// A glyph's quad bounds are tight to its actual inked pixels: the UV rect
    /// reconstructs the exact ink bounding box scanned from the bitmap, and the
    /// reported offset/size match it relative to the cell origin.
    #[test]
    fn glyph_quad_bounds_track_actual_ink() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 28.0);
        let slot = 'g' as u32 - FIRST_CHAR + 1;
        let (minx, miny, maxx, maxy) = scan_slot_ink(&atlas, slot).expect("'g' has ink");
        let q = atlas.glyph_quad('g').expect("g quad");

        // UV reconstructs the exact ink bounding box (inclusive max -> +1).
        let ix = (q.uv[0] * atlas.width as f32).round() as i32;
        let iy = (q.uv[1] * atlas.height as f32).round() as i32;
        let ex = (q.uv[2] * atlas.width as f32).round() as i32;
        let ey = (q.uv[3] * atlas.height as f32).round() as i32;
        assert_eq!((ix, iy), (minx, miny), "uv top-left == ink top-left");
        assert_eq!((ex, ey), (maxx + 1, maxy + 1), "uv extent == ink extent");
        assert_eq!(q.width, (maxx - minx + 1) as u32);
        assert_eq!(q.height, (maxy - miny + 1) as u32);

        // Offset is the ink top-left relative to the cell's inner origin.
        let (ox, oy) = slot_offset(slot, atlas.cols, atlas.cell);
        let border = slot_border(atlas.cell) as i32;
        assert_eq!(q.offset_x, minx - (ox as i32 + border));
        assert_eq!(q.offset_y, miny - (oy as i32 + border));
    }

    /// Box-drawing strokes must join seamlessly across cells: the horizontal line
    /// U+2500's quad spans the full cell width (its ink reaches both edges), so
    /// adjacent cells' strokes meet flush rather than leaving a gutter.
    #[test]
    fn box_drawing_quad_spans_full_cell_width() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        if !font_has_glyph(&font, '\u{2500}') {
            eprintln!("skipping: font lacks box-drawing glyph");
            return;
        }
        let mut atlas = GlyphAtlas::build(&font, 28.0);
        atlas.ensure(&font, '\u{2500}').expect("U+2500 uv");
        let q = atlas.glyph_quad('\u{2500}').expect("U+2500 quad");
        let cw = atlas.cell.width as i32;
        // Ink starts at (or just past) the left edge and reaches the right edge:
        // the quad spans the cell horizontally so neighbors join flush.
        assert!(
            q.offset_x <= 1,
            "horizontal rule should start at the left edge"
        );
        assert!(
            q.offset_x + q.width as i32 >= cw - 1,
            "horizontal rule should reach the right edge"
        );
    }

    /// At least one real glyph inks beyond the cell box, and its quad reports
    /// that overflow (negative offset or size exceeding the cell) instead of
    /// clipping to the cell — the core R3 capability. Best-effort across a broad
    /// codepoint range; skipped only if the loaded font never overflows a cell.
    #[test]
    fn some_glyph_quad_overflows_the_cell() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 28.0);
        let cw = atlas.cell.width as i32;
        let ch_h = atlas.cell.height as i32;
        let exceeds = |q: &GlyphBounds| {
            q.offset_x < 0
                || q.offset_y < 0
                || q.offset_x + q.width as i32 > cw
                || q.offset_y + q.height as i32 > ch_h
        };

        // Printable ASCII first (already resident), then a sweep of common
        // glyph-bearing codepoints (rasterized on demand) likely to overflow.
        let ascii = (FIRST_CHAR..=LAST_CHAR).filter_map(char::from_u32);
        let extras = (0x00A1u32..=0x2600).filter_map(char::from_u32);
        let mut found = None;
        for ch in ascii {
            if let Some(q) = atlas.glyph_quad(ch)
                && exceeds(&q)
            {
                found = Some((ch, q));
                break;
            }
        }
        if found.is_none() {
            for ch in extras {
                if !font_has_glyph(&font, ch) {
                    continue;
                }
                atlas.ensure(&font, ch);
                if let Some(q) = atlas.glyph_quad(ch)
                    && exceeds(&q)
                {
                    found = Some((ch, q));
                    break;
                }
            }
        }

        match found {
            Some((ch, q)) => assert!(
                exceeds(&q),
                "glyph {ch:?} quad {q:?} should exceed the {cw}x{ch_h} cell"
            ),
            None => eprintln!("skipping: loaded font has no cell-overflowing glyph"),
        }
    }

    /// `glyph_quad` resolution mirrors `uv_rect`: a resident styled glyph yields
    /// its own slot's bounds, distinct from the regular glyph.
    #[test]
    fn styled_glyph_quad_resolves_styled_slot() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        atlas
            .ensure_styled(&font, FontStyle::Bold, 'A')
            .expect("bold A");
        let regular = atlas.glyph_quad('A').expect("regular A quad");
        let bold = atlas
            .glyph_quad_styled(FontStyle::Bold, 'A')
            .expect("bold A quad");
        // Distinct slots => distinct UV rects.
        assert_ne!(regular.uv, bold.uv);
    }

    /// Rebuilding the atlas at a larger physical size (the HiDPI rescale path:
    /// `GpuState::set_font_px` constructs a fresh atlas) grows the cell metrics
    /// and starts from a clean dynamic region — no slot from the old density can
    /// survive. This is R1 invalidation by construction.
    #[test]
    fn rebuild_at_larger_size_grows_cell_and_drops_dynamic_slots() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // Build at 1x-equivalent density and populate the dynamic region.
        let mut small = GlyphAtlas::build(&font, 16.0);
        if let Some(ch) = glyph_bearing_non_ascii(&font) {
            small.ensure(&font, ch).expect("resident glyph");
            assert!(
                small.slot_count() > FIRST_DYNAMIC_SLOT,
                "precondition: a dynamic slot was allocated"
            );
        }
        let small_cell = small.cell;

        // The rescale rebuild is a fresh build at 2x physical px.
        let big = GlyphAtlas::build(&font, 32.0);

        // Cell metrics scaled up with the density (no mixed-density reuse).
        assert!(
            big.cell.width > small_cell.width && big.cell.height > small_cell.height,
            "rebuilt cell {:?} should exceed {:?}",
            big.cell,
            small_cell
        );
        // The rebuilt atlas has only its base region — zero stale dynamic slots.
        assert_eq!(
            big.slot_count(),
            FIRST_DYNAMIC_SLOT,
            "a fresh rebuild must carry no slots from the old density"
        );
        // Bitmap is sized to the new (larger) cell, not the old one.
        assert_eq!(big.data.len(), (big.width * big.height) as usize);
        assert!(big.width > small.width || big.height >= small.height);
    }

    /// Cell metrics are deterministic and seam-free across the fractional scales
    /// `physical_font_px` produces from a 16 px logical size: integer (the type
    /// guarantees this), positive, baseline within the cell, and monotonic
    /// non-decreasing as density rises. Building twice at one size is identical.
    #[test]
    fn cell_metrics_deterministic_and_monotonic_across_scales() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // 16 px logical at scales 1.0 / 1.25 / 1.5 / 2.0.
        let sizes = [16.0f32, 20.0, 24.0, 32.0];
        let mut prev: Option<CellSize> = None;
        for &px in &sizes {
            let a = GlyphAtlas::build(&font, px);
            let b = GlyphAtlas::build(&font, px);
            // Determinism: same px => byte-identical metrics and dimensions.
            assert_eq!(a.cell, b.cell, "cell metrics must be deterministic at {px}");
            assert_eq!(a.width, b.width);
            assert_eq!(a.height, b.height);
            // Seam-free: positive extents, baseline within the cell box.
            assert!(a.cell.width > 0 && a.cell.height > 0);
            assert!(a.cell.baseline > 0 && a.cell.baseline <= a.cell.height);
            // Monotonic non-decreasing with density.
            if let Some(p) = prev {
                assert!(
                    a.cell.width >= p.width
                        && a.cell.height >= p.height
                        && a.cell.baseline >= p.baseline,
                    "cell {:?} at {px}px should be >= previous {:?}",
                    a.cell,
                    p
                );
            }
            prev = Some(a.cell);
        }
    }

    // ----- W1: wide-glyph (East Asian width-2) atlas support -----

    /// A width-2 codepoint the loaded font actually has an outline for. `None`
    /// on hosts without a CJK/fullwidth-capable font (the common case here), so
    /// dependent tests skip rather than fail.
    fn wide_glyph_supported(font: &FontVec) -> Option<char> {
        // CJK ideographs, hiragana/katakana, and fullwidth ASCII forms.
        let ranges = [
            0x4E00u32..=0x4F00, // CJK unified
            0x3040..=0x30FF,    // kana
            0xFF01..=0xFF60,    // fullwidth forms
        ];
        ranges
            .into_iter()
            .flatten()
            .filter_map(char::from_u32)
            .find(|&ch| glyph_cells(ch) == 2 && font_has_glyph(font, ch))
    }

    #[test]
    fn glyph_cells_matches_core_width_rule() {
        // Width-2 East Asian forms; width-1 everything else. Mirrors core's
        // `UnicodeWidthChar::width(ch) == Some(2)` cell-layout decision.
        for ch in ['世', '漢', '中', 'あ', '！', 'Ａ', '\u{3000}'] {
            assert_eq!(glyph_cells(ch), 2, "{ch:?} should be a 2-cell glyph");
        }
        for ch in ['A', 'z', '0', 'é', '★', '─', ' '] {
            assert_eq!(glyph_cells(ch), 1, "{ch:?} should be a 1-cell glyph");
        }
    }

    #[test]
    fn allocate_wide_slot_spans_two_cells_in_one_row() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        let lead = atlas.allocate_slots(2).expect("wide slot");
        // Lead carries span 2; the reserved next slot carries span 1.
        assert_eq!(atlas.slot_span[lead as usize], 2);
        assert_eq!(atlas.slot_span[(lead + 1) as usize], 1);
        // The pair is contiguous within one atlas row (no wrap).
        assert_eq!(lead % atlas.cols + 1, (lead + 1) % atlas.cols);
        assert_eq!(lead / atlas.cols, (lead + 1) / atlas.cols);
        // slot_uv reports a 2-cell-wide inner rect for the lead.
        let uv = atlas.slot_uv(lead);
        let (ix, _) = inner_origin(&atlas, uv);
        let right = (uv[2] * atlas.width as f32).round() as u32;
        assert_eq!(right - ix, 2 * atlas.cell.width, "lead uv spans two cells");
        // A normal single slot still reports one cell.
        let narrow = atlas.allocate_slots(1).expect("narrow slot");
        let nuv = atlas.slot_uv(narrow);
        let (nix, _) = inner_origin(&atlas, nuv);
        let nright = (nuv[2] * atlas.width as f32).round() as u32;
        assert_eq!(nright - nix, atlas.cell.width);
    }

    #[test]
    fn wide_allocation_burns_filler_to_avoid_row_wrap() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 20.0);
        // Advance allocation until the next slot is the last column of a row.
        while atlas.next_slot % atlas.cols != atlas.cols - 1 {
            atlas.allocate_slots(1).expect("narrow slot");
        }
        let before = atlas.next_slot;
        let last_col_row = before / atlas.cols;
        let lead = atlas.allocate_slots(2).expect("wide slot at row edge");
        // The last-column slot was burned as a filler (span 1, never used) ...
        assert_eq!(atlas.slot_span[before as usize], 1);
        // ... and the wide pair starts at column 0 of the next row.
        assert_eq!(lead % atlas.cols, 0);
        assert_eq!(lead / atlas.cols, last_col_row + 1);
        assert_eq!(atlas.slot_span[lead as usize], 2);
        // The pair did not wrap.
        assert_eq!(lead / atlas.cols, (lead + 1) / atlas.cols);
    }

    #[test]
    fn rasterize_clip_width_relieves_wide_glyph_clipping() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // Build a cell at one size, then rasterize a heavy glyph at DOUBLE the
        // size so its natural ink exceeds a single cell — the same shape a real
        // width-2 glyph takes relative to a single-cell slot. With a single-cell
        // clip the ink is cropped; with a two-cell clip it is not.
        let atlas = GlyphAtlas::build(&font, 16.0);
        let cell = atlas.cell;
        let big_px = 32.0_f32;
        let stride = 8 * slot_w(cell); // ample horizontal room
        let height = slot_h(cell);
        let pen = Pen {
            px: big_px,
            baseline: cell.baseline as f32,
        };
        let raster = |outer_w: u32| -> Option<GlyphInk> {
            let mut data = vec![0u8; (stride * height) as usize];
            rasterize_glyph(
                &font,
                pen,
                'W',
                &mut data,
                stride,
                SubpixelMode::Off,
                SlotRegion {
                    origin: (0, 0),
                    cell,
                    outer_w,
                },
            )
        };
        let single = raster(slot_w(cell)).expect("single-clip ink");
        let double = raster(2 * slot_w(cell)).expect("double-clip ink");
        // The wider clip must never record less ink than the narrow one, and for
        // an oversized glyph it records strictly more (the cropped right column
        // is now kept).
        assert!(
            double.width >= single.width,
            "wider clip ink {} should be >= narrow clip ink {}",
            double.width,
            single.width
        );
        assert!(
            double.width > single.width,
            "an oversized glyph should be clipped by the single-cell region \
             (single={}, double={})",
            single.width,
            double.width
        );
    }

    #[test]
    fn ensure_wide_codepoint_consumes_two_slots_when_supported() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let Some(ch) = wide_glyph_supported(&font) else {
            eprintln!("skipping: no wide (CJK/fullwidth) glyph in the loaded font");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        let before = atlas.slot_count();
        let uv = atlas.ensure(&font, ch).expect("wide glyph uv");
        // Two slots consumed (lead + reserved continuation); slot is wide.
        assert_eq!(
            atlas.slot_count(),
            before + 2,
            "wide glyph reserves two slots"
        );
        let &slot = atlas
            .dynamic
            .get(&(FontStyle::Regular, ch))
            .expect("resident");
        assert_eq!(atlas.slot_span[slot as usize], 2);
        // The lead UV spans two cells; the glyph quad bounds span ~two cells.
        let (ix, _) = inner_origin(&atlas, uv);
        let right = (uv[2] * atlas.width as f32).round() as u32;
        assert_eq!(right - ix, 2 * atlas.cell.width);
        let bounds = atlas.glyph_quad(ch).expect("wide bounds");
        assert!(
            bounds.width > atlas.cell.width,
            "wide glyph ink {} should exceed one cell {}",
            bounds.width,
            atlas.cell.width
        );
        // Inked across both cells (real coverage past the first-cell boundary).
        let (cx, cy) = inner_origin(&atlas, uv);
        let mut right_cell_ink = 0u64;
        for y in cy..cy + atlas.cell.height {
            for x in cx + atlas.cell.width..cx + 2 * atlas.cell.width {
                right_cell_ink += atlas.data[(y * atlas.width + x) as usize] as u64;
            }
        }
        assert!(
            right_cell_ink > 0,
            "second cell of a wide glyph should hold ink"
        );
    }
}
