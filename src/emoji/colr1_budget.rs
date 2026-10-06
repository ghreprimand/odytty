// SPDX-License-Identifier: GPL-3.0-only
//! Aggregate work budgets for one COLR v1 glyph, checked before any raster
//! work.
//!
//! Fontations bounds Paint nesting at 64 and refuses cycles on the active
//! path, but a finite depth does not bound total work: layers may share a
//! subgraph (a tiny table can expand exponentially), a `PaintGlyph` traverses
//! its child twice when the fill-glyph shortcut does not apply, and every
//! clip, layer and fill costs a full pass over the raster. [`paint_cost`] walks the graph once per distinct Paint,
//! memoized, and derives an upper bound on the traversal visits, the
//! full-raster passes and the peak number of live intermediate buffers that
//! the bounds and raster passes in `colr1` will incur. [`admits`] compares
//! those against fixed budgets, so an over-budget glyph is refused before its
//! traversal starts and falls back exactly as any other failed color glyph.

use std::collections::HashMap;

use skrifa::GlyphId;
use skrifa::raw::tables::colr::{Colr, Paint};
use skrifa::raw::{FontRef, TableProvider};

/// Most Paint visits one glyph may expand to. Matches the order of the SVG
/// renderer's expanded-node cap.
pub(super) const MAX_PAINT_VISITS: u64 = 1 << 16;
/// Most full-raster passes times raster pixels one glyph may cost.
pub(super) const MAX_PIXEL_WORK: u64 = 1 << 30;
/// Most bytes of root and intermediate buffers live at once.
pub(super) const MAX_LIVE_BYTES: u64 = 256 << 20;
/// Fontations' traversal depth limit.
const MAX_DEPTH: usize = 64;

/// Upper bounds on the work one glyph's traversal incurs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PaintCost {
    /// Paint visits, counting a `PaintGlyph` child twice when the fill-glyph
    /// shortcut does not apply.
    pub visits: u64,
    /// Full passes over the raster (allocation, sampling, clipping, merging).
    pub raster_passes: u64,
    /// Intermediate buffers live at once, beyond the root canvas and clip.
    pub peak_buffers: u64,
    /// Fills in the subgraph.
    fills: u64,
    /// Fills emitted before the subgraph's first clip or layer, which the
    /// fill-glyph shortcut forwards even when it then gives up.
    lead_fills: u64,
    /// Whether the subgraph emits only fills and transforms (no clip or
    /// layer), so an enclosing `PaintGlyph` takes the fill-glyph shortcut.
    fills_only: bool,
}

/// Raster passes of one fill: allocate, sample, draw.
const FILL_PASSES: u64 = 3;
/// Raster passes of one clip: allocate, fill, multiply by the parent.
const CLIP_PASSES: u64 = 3;
/// Raster passes of one composite: two layer allocations and two merges.
const COMPOSITE_PASSES: u64 = 4;

impl PaintCost {
    fn node() -> Self {
        Self {
            visits: 1,
            fills_only: true,
            ..Self::default()
        }
    }

    fn fill() -> Self {
        Self {
            visits: 1,
            raster_passes: FILL_PASSES,
            peak_buffers: 1,
            fills: 1,
            lead_fills: 1,
            fills_only: true,
        }
    }

    /// A node whose first callback is a clip or layer push.
    fn clipping(raster_passes: u64) -> Self {
        Self {
            visits: 1,
            raster_passes,
            fills_only: false,
            ..Self::default()
        }
    }

    /// `self` followed by `next` in traversal order.
    fn then(self, next: Self) -> Self {
        Self {
            visits: self.visits.saturating_add(next.visits),
            raster_passes: self.raster_passes.saturating_add(next.raster_passes),
            peak_buffers: self.peak_buffers.max(next.peak_buffers),
            fills: self.fills.saturating_add(next.fills),
            lead_fills: if self.fills_only {
                self.lead_fills.saturating_add(next.lead_fills)
            } else {
                self.lead_fills
            },
            fills_only: self.fills_only && next.fills_only,
        }
    }

    fn under(self, buffers: u64) -> Self {
        Self {
            peak_buffers: self.peak_buffers.saturating_add(buffers),
            ..self
        }
    }

    /// The pinned Fontations traversal of `PaintGlyph`: the child is first
    /// walked with the fill-glyph shortcut, which turns each fill into a clip
    /// plus fill until a clip or layer appears; when one does, the child is
    /// walked again under the glyph's own clip.
    fn glyph(child: Self) -> Self {
        let shortcut = FILL_PASSES + CLIP_PASSES;
        let body = if child.fills_only {
            Self {
                raster_passes: child.fills.saturating_mul(shortcut),
                ..child
            }
            .under(1)
        } else {
            Self {
                visits: child.visits.saturating_mul(2),
                raster_passes: child
                    .lead_fills
                    .saturating_mul(shortcut)
                    .saturating_add(CLIP_PASSES)
                    .saturating_add(child.raster_passes),
                ..child
            }
            .under(1)
        };
        Self::clipping(0).then(Self {
            fills_only: false,
            lead_fills: 0,
            ..body
        })
    }
}

/// Whether `cost` fits every budget for a `width` x `height` raster. Each
/// buffer is charged as RGBA (four bytes per pixel), and the root canvas and
/// root clip as two more buffers.
pub(super) fn admits(cost: PaintCost, width: u32, height: u32) -> bool {
    let pixels = u64::from(width) * u64::from(height);
    let live = cost
        .peak_buffers
        .saturating_add(2)
        .saturating_mul(pixels)
        .saturating_mul(4);
    cost.visits <= MAX_PAINT_VISITS
        && cost.raster_passes.saturating_mul(pixels) <= MAX_PIXEL_WORK
        && live <= MAX_LIVE_BYTES
}

/// The work bound for `glyph_id`'s COLR v1 Paint graph, or `None` when the
/// graph cannot be read, has a cycle, nests past the traversal depth limit,
/// or expands past [`MAX_PAINT_VISITS`] (the walk stops there).
pub(super) fn paint_cost(font: &FontRef<'_>, glyph_id: GlyphId) -> Option<PaintCost> {
    let colr = font.colr().ok()?;
    let (paint, _) = colr.v1_base_glyph(glyph_id).ok()??;
    let mut walk = Walk {
        colr,
        memo: HashMap::new(),
        stack: Vec::new(),
    };
    walk.colr_glyph(glyph_id, &paint, 0)
}

struct Walk<'a> {
    colr: Colr<'a>,
    memo: HashMap<usize, PaintCost>,
    stack: Vec<usize>,
}

impl<'a> Walk<'a> {
    /// A base glyph's paint, plus the clip box pass when it has one.
    fn colr_glyph(
        &mut self,
        glyph_id: GlyphId,
        paint: &Paint<'a>,
        depth: usize,
    ) -> Option<PaintCost> {
        let body = self.paint(paint, depth)?;
        Some(match self.colr.v1_clip_box(glyph_id) {
            Ok(Some(_)) => PaintCost::clipping(CLIP_PASSES).then(body.under(1)),
            _ => body,
        })
    }

    fn paint(&mut self, paint: &Paint<'a>, depth: usize) -> Option<PaintCost> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let id = paint.offset_data().as_bytes().as_ptr() as usize;
        if let Some(cost) = self.memo.get(&id) {
            return Some(*cost);
        }
        // A cycle on the active path, or more distinct paints than the visit
        // budget can ever admit.
        if self.stack.contains(&id) || self.memo.len() as u64 >= MAX_PAINT_VISITS {
            return None;
        }
        self.stack.push(id);
        let cost = self.expand(paint, depth);
        self.stack.pop();
        let cost = cost.filter(|cost| cost.visits <= MAX_PAINT_VISITS)?;
        self.memo.insert(id, cost);
        Some(cost)
    }

    fn child(
        &mut self,
        child: Result<Paint<'a>, skrifa::raw::ReadError>,
        depth: usize,
    ) -> Option<PaintCost> {
        self.paint(&child.ok()?, depth + 1)
    }

    fn expand(&mut self, paint: &Paint<'a>, depth: usize) -> Option<PaintCost> {
        let node = PaintCost::node();
        Some(match paint {
            Paint::ColrLayers(layers) => {
                let first = layers.first_layer_index() as usize;
                let mut cost = node;
                for index in first..first.saturating_add(usize::from(layers.num_layers())) {
                    let (layer, _) = self.colr.v1_layer(index).ok()?;
                    cost = cost.then(self.paint(&layer, depth + 1)?);
                    if cost.visits > MAX_PAINT_VISITS {
                        return None;
                    }
                }
                cost
            }
            // A fill allocates a temporary buffer, samples into it and draws
            // it.
            Paint::Solid(_)
            | Paint::VarSolid(_)
            | Paint::LinearGradient(_)
            | Paint::VarLinearGradient(_)
            | Paint::RadialGradient(_)
            | Paint::VarRadialGradient(_)
            | Paint::SweepGradient(_)
            | Paint::VarSweepGradient(_) => PaintCost::fill(),
            Paint::Glyph(glyph) => PaintCost::glyph(self.child(glyph.paint(), depth)?),
            Paint::ColrGlyph(colr_glyph) => {
                let glyph_id = GlyphId::from(colr_glyph.glyph_id());
                let (base, _) = self.colr.v1_base_glyph(glyph_id).ok()??;
                node.then(self.colr_glyph(glyph_id, &base, depth + 1)?)
            }
            Paint::Transform(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarTransform(p) => node.then(self.child(p.paint(), depth)?),
            Paint::Translate(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarTranslate(p) => node.then(self.child(p.paint(), depth)?),
            Paint::Scale(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarScale(p) => node.then(self.child(p.paint(), depth)?),
            Paint::ScaleAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarScaleAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            Paint::ScaleUniform(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarScaleUniform(p) => node.then(self.child(p.paint(), depth)?),
            Paint::ScaleUniformAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarScaleUniformAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            Paint::Rotate(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarRotate(p) => node.then(self.child(p.paint(), depth)?),
            Paint::RotateAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarRotateAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            Paint::Skew(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarSkew(p) => node.then(self.child(p.paint(), depth)?),
            Paint::SkewAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            Paint::VarSkewAroundCenter(p) => node.then(self.child(p.paint(), depth)?),
            // Two layers are allocated and merged; the backdrop is drawn
            // into the first, the source into the second.
            Paint::Composite(composite) => {
                let backdrop = self.child(composite.backdrop_paint(), depth)?;
                let source = self.child(composite.source_paint(), depth)?;
                PaintCost::clipping(COMPOSITE_PASSES)
                    .then(backdrop.under(1))
                    .then(source.under(2))
            }
        })
    }
}
