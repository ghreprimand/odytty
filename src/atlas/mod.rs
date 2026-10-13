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
//! fills (no eviction - existing slots never move, so resident UV rects stay
//! valid across growth). A font-size or font-family change is a full rebuild
//! ([`GlyphAtlas::build`]); there is no in-place resize, so glyphs of different
//! sizes can never coexist in one atlas.
//!
//! ## Live vs. dynamic paths
//!
//! Printable ASCII is prebuilt at atlas construction time. Before geometry
//! construction each frame, the native renderer ensures every visible styled
//! and non-ASCII glyph and combining mark is resident, using the cell's
//! effective font style. [`GlyphAtlas::uv_rect`] is the **immutable** lookup:
//! it returns the resident glyph's real rect, or the fallback box for a
//! printable glyph that is unavailable or not yet resident. The **mutable**
//! [`GlyphAtlas::ensure`] rasterizes a glyph on demand, growing the atlas and
//! flagging [`GlyphAtlas::take_dirty`] so the native layer re-uploads the
//! texture.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::text::{GlyphId, PxScale, point};

use crate::text::FontHandle;
use unicode_width::UnicodeWidthChar;

pub mod fallback;

mod allocation;
mod build;
mod cluster;
mod insertion;
mod lookup;
mod marks;
mod ownership;
mod raster;
mod upload;

pub use cluster::{
    CLUSTER_MIN_SCALE, ClusterFace, ClusterFit, ClusterGlyph, cluster_fit, face_maps_all,
};
pub use raster::set_stem_darken;
#[cfg(test)]
pub(crate) use raster::stem_darken_strength_for_test;
use raster::{
    CellFit, SYMBOL_CELL_INSET, SlotRegion, draw_fallback_box, rasterize_geometric,
    rasterize_glyph, rasterize_glyph_id, slot_border, slot_h, slot_offset, slot_w,
};
#[cfg(test)]
use raster::{
    SYMBOL_CELL_FILL, apply_stem_darken, lcd_filter_subpixel_region, overflow_margin, pixel_index,
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
/// Per-bitmap budget; a GPU texture of equal size and upload buffers are additional.
const MAX_ATLAS_BYTES: usize = 192 * 1024 * 1024;
/// Conservative initial texture-axis ceiling, also the headless growth default.
const MAX_ATLAS_DIMENSION: u32 = 8192;

/// First dynamic slot: fallback (0) + 95 printable ASCII (1..=95).
const FIRST_DYNAMIC_SLOT: u32 = LAST_CHAR - FIRST_CHAR + 2;

// The gutter must be at least one pixel for the bleed guard to hold.
const _: () = assert!(ATLAS_PAD >= 1);

/// Checked byte length; all allocation callers admit bounded geometry first.
fn atlas_byte_len(width: u32, height: u32, bytes_per_pixel: u32) -> usize {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|len| len.checked_mul(bytes_per_pixel as usize))
        .expect("admitted atlas byte length fits usize")
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
/// wide / fullwidth codepoints, `1` otherwise.
///
/// This stays on the narrow table on purpose. The atlas is shared by every
/// pane, and two panes may disagree about East Asian Ambiguous width, so a
/// slot cannot be cached at two widths for one codepoint. The grid reserves
/// the second column with `wide_continuation`; the lead cell's glyph is drawn
/// inside that span.
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

/// General_Category Mn or Me (Unicode 17.0.0), less the variation selectors:
/// a mark drawn over its base, from the generated table in `marks.rs`.
fn is_combining_mark(ch: char) -> bool {
    let cp = ch as u32;
    marks::COMBINING_MARKS
        .binary_search_by(|&(start, end)| {
            if end < cp {
                std::cmp::Ordering::Less
            } else if start > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Whether the font maps `ch` to a usable glyph. Fallback-eligible codepoints
/// also require an inked outline so blank placeholder glyphs cannot block the
/// fallback chain.
fn font_has_glyph(font: &FontHandle, ch: char) -> bool {
    let id = font.glyph_id(ch);
    if id.0 == 0 {
        return false;
    }
    if should_attempt_fallback(ch) {
        return glyph_coverage_decision(ch, true, font_has_inked_outline(font, id));
    }
    true
}

fn font_has_inked_outline(font: &FontHandle, id: GlyphId) -> bool {
    font.outline(id).is_some_and(|outline| {
        !outline.curves.is_empty()
            && outline.bounds.min.x != outline.bounds.max.x
            && outline.bounds.min.y != outline.bounds.max.y
    })
}

fn glyph_coverage_decision(ch: char, has_cmap: bool, has_inked_outline: bool) -> bool {
    has_cmap && (!should_attempt_fallback(ch) || has_inked_outline)
}

/// Font style variant for a glyph slot: keys the dynamic region by
/// `(FontStyle, char)` so bold/italic glyphs coexist with regular ones. The
/// native renderer selects a cell's style from its attributes and calls the
/// `_styled` variants with the matching style font (real face when
/// discovered, synthetic fallback otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontStyle {
    /// Upright, regular weight.
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
    /// Font-unit `[x, y]` (y up) pen offset of a combining-mark glyph from
    /// the first glyph of its shaping cluster, as OpenType mark positioning
    /// placed it. `[0, 0]` for every other glyph, which keeps their pen at
    /// `anchor_cell` exactly as before.
    pub mark_offset: [i16; 2],
    /// A whole complex-script owner run rasterized into one span slot by
    /// [`GlyphAtlas::ensure_cluster`]. `face_fingerprint` then hashes the
    /// face, the placed glyph run, and the style source, and `glyph_id`,
    /// `anchor_cell`, and `mark_offset` are zero. Such a key is never
    /// rasterized by [`GlyphAtlas::ensure_shaped`].
    pub cluster: bool,
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

/// Runtime per-codepoint glyph fallback resolver hook: a
/// bare `fn` (not a closure) so [`GlyphAtlas`] stays `Clone`/`Debug`. Given a
/// codepoint the static fallback chain missed, it returns a loaded face that
/// covers it (or `None`). The native layer wires this to a cached `fc-match`
/// query; see [`crate::text::runtime_resolve_symbol_font`].
pub type RuntimeSymbolResolver = fn(char) -> RuntimeSymbol;

/// A runtime glyph-fallback answer. The native resolver runs its fontconfig
/// queries on a worker thread, so a first request is [`Self::Pending`]: the
/// atlas draws the fallback box for that frame without caching anything, and
/// asks again on a later rebuild once the worker has an answer.
#[derive(Debug, Clone)]
pub enum RuntimeSymbol {
    /// A final answer: the covering face, or `None` when no host face covers
    /// the codepoint. Cached per codepoint.
    Ready(Option<Arc<FontHandle>>),
    /// Resolution is still in progress; nothing is cached.
    Pending,
}

/// The fallback-chain answer for one codepoint inside the atlas.
pub(super) enum SymbolFallback {
    /// Final: a face to rasterize from, or none (the fallback box).
    Settled(Option<Arc<FontHandle>>),
    /// The runtime resolver has no answer yet; draw the box uncached.
    Pending,
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
    /// across two cells; shaped and cluster allocations store larger spans, up
    /// to `ATLAS_COLS`. Reserved/filler slots created by a wide allocation keep
    /// span `1` (they are never looked up). Invariant: `slot_span.len() ==
    /// slot_ink.len() == next_slot`.
    slot_span: Vec<u8>,
    /// Atlas cells per row.
    cols: u32,
    /// Number of cell-rows currently allocated (grows in `ATLAS_GROW_ROWS`
    /// pages). `height == capacity_rows * slot_h(cell)` (the cell plus its slot
    /// borders).
    capacity_rows: u32,
    /// Next free slot for dynamic insertion; also the current slot count.
    next_slot: u32,
    /// Slot ceiling imposed by bitmap bytes, texture height and slot count.
    /// Headless growth defaults to the construction axis ceiling.
    max_slots: u32,
    /// Active texture axis limit, rechecked before allocating a growth page.
    max_texture_dimension: u32,
    /// Resident non-ASCII `(style, codepoint)` → slot index. A codepoint the
    /// font lacks is cached pointing at [`FALLBACK_SLOT`] so the decision is made
    /// once. Keyed by style so bold/italic variants get distinct slots; the
    /// native renderer inserts every style it draws, not only
    /// [`FontStyle::Regular`].
    dynamic: HashMap<(FontStyle, char), u32>,
    /// Contextual OpenType glyphs keyed independently from scalar codepoints.
    /// Empty on every freshly built atlas and untouched while ligatures are off.
    shaped: HashMap<ShapedGlyphKey, u32>,
    /// The run each cluster key in `shaped` was drawn from. A cluster key
    /// carries only a 64-bit digest of its run, so a hit is trusted only when
    /// the run at hand equals the one stored here.
    cluster_runs: HashMap<ShapedGlyphKey, cluster::ClusterRun>,
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
    /// instead of from the font outline. Default `false` preserves the
    /// font-glyph path byte-for-byte. Like the synthetic-style mask, it only
    /// governs glyphs rasterized after it is set; the native layer sets it right
    /// after [`Self::build`] and rebuilds the atlas when the setting changes, so
    /// the dynamic region never holds a stale mix of geometric and font glyphs.
    geometric: bool,
    /// Ordered symbol / Nerd-font fallback **chain**. When non-empty, a
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
    fallback_chain: Vec<Arc<FontHandle>>,
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
    symbol_map_fonts: Vec<(u32, u32, Arc<FontHandle>)>,
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
    runtime_symbol_cache: HashMap<char, Option<Arc<FontHandle>>>,
}

#[cfg(test)]
mod tests;
