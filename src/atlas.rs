//! Monospace glyph atlas: a CPU-rasterized 8-bit coverage texture with a
//! missing-glyph fallback, the printable-ASCII block, and a growable dynamic
//! region for other codepoints.
//!
//! This module is deliberately GPU-agnostic so it can be unit-tested without a
//! window or `wgpu` device. The native renderer (`crate::native`) uploads the
//! atlas bitmap to a texture and uses [`GlyphAtlas::uv_rect`] to build per-cell
//! quads; font loading and color resolution live in [`crate::text`].
//!
//! ## Atlas layout
//!
//! The bitmap is arranged as a fixed grid of equal slots (`ATLAS_COLS` per row).
//! Every terminal cell maps 1:1 onto the **inner** `cell.width × cell.height`
//! region of one slot, so the renderer only needs that rectangle for a
//! character — no per-glyph offset math downstream.
//!
//! Each slot is surrounded by an [`ATLAS_PAD`]-pixel transparent **gutter**, so
//! a slot occupies `(cell.width + 2·PAD) × (cell.height + 2·PAD)` pixels while
//! [`GlyphAtlas::uv_rect`] still hands out only the inner rectangle. The gutter
//! does two jobs: it stops bilinear sampling at non-integer scale factors from
//! bleeding a neighbor's coverage into a glyph edge, and it absorbs the small
//! amount of bearing-driven overflow (box-drawing joins that reach the cell
//! edge, descenders at the bottom) that a hard clip to the cell box would crop.
//! `cell.width`/`cell.height` are unchanged by the gutter, so per-cell layout
//! (advance, line height) downstream is identical.
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

use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont, point};

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

/// Whether the font maps `ch` to a real (non-`.notdef`) glyph. `ab_glyph`
/// returns glyph id 0 for codepoints the font lacks, so a missing glyph is
/// detected here rather than relying on the font's own `.notdef` outline.
fn font_has_glyph(font: &FontVec, ch: char) -> bool {
    font.glyph_id(ch).0 != 0
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

/// A monospace glyph atlas: an 8-bit coverage bitmap with a fallback box, the
/// printable-ASCII block, and a growable dynamic region for other codepoints.
#[derive(Debug, Clone)]
pub struct GlyphAtlas {
    /// Atlas bitmap width in pixels.
    pub width: u32,
    /// Atlas bitmap height in pixels.
    pub height: u32,
    /// Single-channel (R8) coverage data, row-major, length `width * height`.
    pub data: Vec<u8>,
    /// Per-cell pixel metrics shared by every glyph.
    pub cell: CellSize,
    /// Atlas cells per row.
    cols: u32,
    /// Number of cell-rows currently allocated (grows in `ATLAS_GROW_ROWS`
    /// pages). `height == capacity_rows * cell.height`.
    capacity_rows: u32,
    /// Next free slot for dynamic insertion; also the current slot count.
    next_slot: u32,
    /// Resident non-ASCII codepoints → slot index. A codepoint the font lacks
    /// is cached here pointing at [`FALLBACK_SLOT`] so the decision is made once.
    dynamic: HashMap<char, u32>,
    /// Physical pixel size new glyphs are rasterized at (matches the ASCII block).
    px: f32,
    /// Monotonic counter bumped whenever pixels or dimensions change. The native
    /// layer compares this to decide when to re-upload the atlas texture.
    revision: u64,
    /// Set when `data`/dimensions changed since the last [`Self::take_dirty`].
    dirty: bool,
}

impl GlyphAtlas {
    /// Rasterize printable ASCII at `px` pixels into a new atlas.
    ///
    /// `px` is the physical pixel size to rasterize at (caller multiplies the
    /// logical font size by the window scale factor for crisp HiDPI text).
    pub fn build(font: &FontVec, px: f32) -> Self {
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
        let mut data = vec![0u8; (width * height) as usize];

        // Slot 0: synthesized hollow-box fallback, drawn the same for any font.
        let (fox, foy) = slot_offset(FALLBACK_SLOT, cols, cell);
        draw_fallback_box(&mut data, width, fox + ATLAS_PAD, foy + ATLAS_PAD, cell);

        // Slots 1..=95: printable ASCII at the build pixel size.
        for code in FIRST_CHAR..=LAST_CHAR {
            let ch = char::from_u32(code).unwrap_or(' ');
            let slot = code - FIRST_CHAR + 1;
            let origin = slot_offset(slot, cols, cell);
            rasterize_glyph(
                font,
                Pen { px, baseline },
                ch,
                &mut data,
                width,
                origin,
                cell,
            );
        }

        Self {
            width,
            height,
            data,
            cell,
            cols,
            capacity_rows,
            next_slot: base_slots,
            dynamic: HashMap::new(),
            px,
            revision: 0,
            dirty: false,
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
        let code = ch as u32;
        if (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_uv(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&ch) {
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
        let ix = ox + ATLAS_PAD;
        let iy = oy + ATLAS_PAD;
        [
            ix as f32 / self.width as f32,
            iy as f32 / self.height as f32,
            (ix + self.cell.width) as f32 / self.width as f32,
            (iy + self.cell.height) as f32 / self.height as f32,
        ]
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
        let code = ch as u32;
        if (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_uv(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&ch) {
            return Some(self.slot_uv(slot));
        }
        if !wants_glyph(ch) {
            return None;
        }
        if !font_has_glyph(font, ch) {
            // Font lacks the glyph: cache the fallback decision, draw nothing new.
            self.dynamic.insert(ch, FALLBACK_SLOT);
            return Some(self.slot_uv(FALLBACK_SLOT));
        }
        let Some(slot) = self.allocate_slot() else {
            // Atlas is at its hard cap: degrade to the fallback box.
            self.dynamic.insert(ch, FALLBACK_SLOT);
            return Some(self.slot_uv(FALLBACK_SLOT));
        };
        let origin = slot_offset(slot, self.cols, self.cell);
        rasterize_glyph(
            font,
            Pen {
                px: self.px,
                baseline: self.cell.baseline as f32,
            },
            ch,
            &mut self.data,
            self.width,
            origin,
            self.cell,
        );
        self.dynamic.insert(ch, slot);
        self.revision += 1;
        self.dirty = true;
        Some(self.slot_uv(slot))
    }

    /// Reserve the next dynamic slot, appending a page of rows (and zero-filling
    /// the new pixels) when the current capacity is exhausted. Returns `None`
    /// once [`MAX_ATLAS_SLOTS`] is reached. Existing slots never move, so UV
    /// rects handed out before a growth stay valid.
    fn allocate_slot(&mut self) -> Option<u32> {
        if self.next_slot >= MAX_ATLAS_SLOTS {
            return None;
        }
        let slot = self.next_slot;
        let needed_rows = slot / self.cols + 1;
        if needed_rows > self.capacity_rows {
            while needed_rows > self.capacity_rows {
                self.capacity_rows += ATLAS_GROW_ROWS;
            }
            self.height = self.capacity_rows * slot_h(self.cell);
            self.data.resize((self.width * self.height) as usize, 0);
            self.revision += 1;
            self.dirty = true;
        }
        self.next_slot += 1;
        Some(slot)
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
}

/// Full slot width in pixels: the glyph cell plus its gutter on both sides.
fn slot_w(cell: CellSize) -> u32 {
    cell.width + 2 * ATLAS_PAD
}

/// Full slot height in pixels: the glyph cell plus its gutter on both sides.
fn slot_h(cell: CellSize) -> u32 {
    cell.height + 2 * ATLAS_PAD
}

/// Pixel offset `(ox, oy)` of an atlas slot's **outer** top-left within the
/// bitmap (the gutter corner). The glyph's inner origin is `(ox + ATLAS_PAD,
/// oy + ATLAS_PAD)`.
fn slot_offset(slot: u32, cols: u32, cell: CellSize) -> (u32, u32) {
    ((slot % cols) * slot_w(cell), (slot / cols) * slot_h(cell))
}

/// Rasterize one glyph's coverage into the slot whose **outer** top-left is
/// `origin`, positioning it on the shared integer `baseline`.
///
/// Returns `true` if the font produced an outline for `ch` (even if it inked no
/// pixels, e.g. a space), `false` if the font has no outline for it.
///
/// The glyph's pen is placed at the slot's inner origin `(ox + PAD, oy + PAD)`
/// and on `baseline`, then each coverage sample is placed at the **nearest**
/// atlas pixel (rounding, not truncation, for stable sub-pixel placement).
/// Coverage may land anywhere inside the slot **including its gutter**, so a
/// box-drawing stroke or descender that reaches the cell edge is preserved (and
/// picked up by bilinear sampling at the inner edge) rather than hard-cropped;
/// the clip to the slot bounds still keeps a glyph strictly out of its
/// neighbors. The strongest value wins on any overlap.
fn rasterize_glyph(
    font: &FontVec,
    pen: Pen,
    ch: char,
    data: &mut [u8],
    width: u32,
    origin: (u32, u32),
    cell: CellSize,
) -> bool {
    let (ox, oy) = origin;
    let scale = PxScale::from(pen.px);
    let glyph: Glyph = font
        .glyph_id(ch)
        .with_scale_and_position(scale, point(0.0, pen.baseline));
    let Some(outline) = font.outline_glyph(glyph) else {
        return false;
    };
    let bounds = outline.px_bounds();
    // Inner glyph origin, and the inclusive slot bounds (inner cell + gutter)
    // that coverage may occupy.
    let inner_x = (ox + ATLAS_PAD) as i32;
    let inner_y = (oy + ATLAS_PAD) as i32;
    let x_lo = ox as i32;
    let x_hi = (ox + slot_w(cell)) as i32;
    let y_lo = oy as i32;
    let y_hi = (oy + slot_h(cell)) as i32;
    outline.draw(|gx, gy, coverage| {
        // Round to the nearest atlas pixel (truncation drifts edges and can
        // drop a glyph's final row/column).
        let ax = inner_x + (bounds.min.x + gx as f32).round() as i32;
        let ay = inner_y + (bounds.min.y + gy as f32).round() as i32;
        if ax < x_lo || ax >= x_hi || ay < y_lo || ay >= y_hi {
            return; // clip to this glyph's own slot (cell + its gutter)
        }
        let idx = (ay as u32 * width + ax as u32) as usize;
        let value = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
        if value > data[idx] {
            data[idx] = value;
        }
    });
    true
}

/// Draw the synthesized missing-glyph fallback — a hollow rectangle inset from
/// the cell edges — into the atlas cell at `(ox, oy)`. Font-independent so the
/// fallback looks the same regardless of which font is loaded.
fn draw_fallback_box(data: &mut [u8], width: u32, ox: u32, oy: u32, cell: CellSize) {
    let cw = cell.width;
    let ch = cell.height;
    let mut set = |x: u32, y: u32| {
        if x < cw && y < ch {
            data[((oy + y) * width + ox + x) as usize] = 255;
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

    /// The atlas bitmap reserves an `ATLAS_PAD` gutter around every slot, so the
    /// bitmap is wider/taller than a gutterless pack and adjacent inner cells
    /// are separated by `2·PAD` pixels — the guard against UV bleed.
    #[test]
    fn slots_carry_a_padding_gutter() {
        let Some(font) = test_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 24.0);
        // Bitmap dimensions account for the gutter on every slot (ATLAS_PAD >= 1
        // is enforced at compile time).
        assert_eq!(atlas.width, atlas.cols * (atlas.cell.width + 2 * ATLAS_PAD));

        // Two horizontally-adjacent inner rects (slots 1 and 2) are separated by
        // a full 2·PAD-pixel gutter, so bilinear sampling of one cannot reach the
        // other's ink.
        let a = atlas.slot_uv(1);
        let b = atlas.slot_uv(2);
        let a_right = (a[2] * atlas.width as f32).round() as i32;
        let b_left = (b[0] * atlas.width as f32).round() as i32;
        assert_eq!(b_left - a_right, (2 * ATLAS_PAD) as i32);
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
}
