// SPDX-License-Identifier: GPL-3.0-only
//! Aggregate raster budgets for one SVG glyph, checked on the converted tree
//! before any raster work.
//!
//! The XML limits in `svg` bound how many nodes a document expands to, not the
//! raster work those nodes cause. Every isolated group (group opacity, a clip
//! path, a blend mode, or `isolation`) allocates a layer that stays live while
//! its children render, every clip path allocates a buffer the size of its
//! target plus a coverage mask, and every path fill or stroke costs the pixels
//! and rows it covers. [`cost`] walks the tree the way resvg 0.45.1 renders it
//! (`render.rs`, `clip.rs`, `path.rs`) and derives upper bounds on the node
//! visits, the pixel work, and the peak bytes of live raster buffers; a
//! document over [`LIMITS`] is refused before its canvas is allocated and
//! falls back like any other refused document. The model follows the locked
//! resvg release and must be re-checked when that dependency changes.
//!
//! Filters, masks, markers, patterns, and images are outside the model:
//! OpenType does not allow the first four in glyph documents, `svg` refuses
//! them before conversion, and every image resolver returns nothing. A tree
//! that still carries one is refused here as well.

use resvg::tiny_skia::{PathSegment, Point, Rect, Transform};
use resvg::usvg;

use super::colr1_budget::{MAX_LIVE_BYTES, MAX_PIXEL_WORK};

/// Most nodes one glyph's walk may visit. Twice the expanded-node cap leaves
/// room for a clip path shared by several groups, which is walked per use.
const MAX_VISITS: u64 = 2 * super::svg::MAX_EXPANDED_NODES;
/// tiny-skia stops dashing one path past this many dashes.
const MAX_DASHES: f64 = 1_000_000.0;
/// Pixel-work units charged per dash for its stroke outline and edges.
const DASH_WORK: u64 = 16;
/// resvg clamps a layer to five times the canvas per side.
const LAYER_CANVASES: u64 = 5;
/// resvg pads an unfiltered layer by two pixels on each side.
const LAYER_PAD: u64 = 4;
/// Anti-aliasing reaches one pixel past each side of a covered box.
const COVER_PAD: u64 = 2;
/// Edge-building factor for a stroke outline relative to its source path.
const STROKE_EDGE_FACTOR: u64 = 4;

/// Budgets one glyph must fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Limits {
    pub visits: u64,
    pub pixel_work: u64,
    pub live_bytes: u64,
}

/// The budgets every SVG glyph is held to: the COLR v1 pixel-work and
/// live-buffer budgets, and twice the expanded-node cap in visits.
pub(super) const LIMITS: Limits = Limits {
    visits: MAX_VISITS,
    pixel_work: MAX_PIXEL_WORK,
    live_bytes: MAX_LIVE_BYTES,
};

/// Upper bounds on the work rendering one glyph incurs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RasterCost {
    /// Nodes visited, counting a shared clip path once per use.
    pub visits: u64,
    /// Pixels allocated, filled, covered, or composited, plus edge rows.
    pub pixel_work: u64,
    /// Most bytes of the canvas, layers, clip buffers, and masks live at once.
    pub peak_bytes: u64,
}

/// Whether rendering `node` with `transform` onto a `width` x `height` canvas
/// stays within [`LIMITS`].
pub(super) fn admits(node: &usvg::Node, transform: Transform, width: u32, height: u32) -> bool {
    cost(node, transform, width, height, LIMITS).is_some()
}

/// Whether rendering a whole tree's `root` group with `transform` (as
/// `resvg::render` does: each child with that transform, the root itself never
/// isolated) onto a `width` x `height` canvas stays within [`LIMITS`].
pub(super) fn admits_root(
    root: &usvg::Group,
    transform: Transform,
    width: u32,
    height: u32,
) -> bool {
    walk(width, height, LIMITS, |walk, canvas| {
        walk.children(root, transform, canvas)
    })
    .is_some()
}

/// The raster cost of rendering `node` with `transform` onto a `width` x
/// `height` canvas, or `None` when it exceeds `limits` or holds content
/// outside the model. The walk stops at the first exceeded limit.
pub(super) fn cost(
    node: &usvg::Node,
    transform: Transform,
    width: u32,
    height: u32,
    limits: Limits,
) -> Option<RasterCost> {
    // `render_node` translates by the node's layer box; no size below depends
    // on a translation, so the walk uses `transform` as given.
    walk(width, height, limits, |walk, canvas| {
        walk.node(node, transform, canvas)
    })
}

/// Charges the canvas itself (allocated zeroed and kept for the atlas), then
/// runs `body` over it.
fn walk(
    width: u32,
    height: u32,
    limits: Limits,
    body: impl FnOnce(&mut Walk, Size) -> Option<()>,
) -> Option<RasterCost> {
    let canvas = Size {
        width: u64::from(width),
        height: u64::from(height),
    };
    let mut walk = Walk {
        limits,
        max_layer: Size {
            width: canvas.width * LAYER_CANVASES,
            height: canvas.height * LAYER_CANVASES,
        },
        live: 0,
        cost: RasterCost::default(),
    };
    walk.allocate(canvas.bytes())?;
    walk.charge(canvas.pixels())?;
    body(&mut walk, canvas)?;
    Some(walk.cost)
}

#[derive(Clone, Copy, Debug)]
struct Size {
    width: u64,
    height: u64,
}

impl Size {
    fn pixels(self) -> u64 {
        self.width.saturating_mul(self.height)
    }

    fn bytes(self) -> u64 {
        self.pixels().saturating_mul(4)
    }
}

struct Walk {
    limits: Limits,
    max_layer: Size,
    live: u64,
    cost: RasterCost,
}

impl Walk {
    fn visit(&mut self) -> Option<()> {
        self.cost.visits += 1;
        (self.cost.visits <= self.limits.visits).then_some(())
    }

    fn charge(&mut self, work: u64) -> Option<()> {
        self.cost.pixel_work = self.cost.pixel_work.saturating_add(work);
        (self.cost.pixel_work <= self.limits.pixel_work).then_some(())
    }

    fn allocate(&mut self, bytes: u64) -> Option<()> {
        self.live = self.live.saturating_add(bytes);
        self.cost.peak_bytes = self.cost.peak_bytes.max(self.live);
        (self.live <= self.limits.live_bytes).then_some(())
    }

    fn release(&mut self, bytes: u64) {
        self.live = self.live.saturating_sub(bytes);
    }

    /// `render::render_node`.
    fn node(&mut self, node: &usvg::Node, transform: Transform, target: Size) -> Option<()> {
        self.visit()?;
        match node {
            usvg::Node::Group(group) => self.group(group, transform, target),
            usvg::Node::Path(path) => self.path(path, transform, target),
            usvg::Node::Image(_) => None,
            usvg::Node::Text(text) => self.group(text.flattened(), transform, target),
        }
    }

    /// `render::render_group`: an isolated group renders its children into a
    /// layer, applies its clip path, and composites the layer into `target`.
    fn group(&mut self, group: &usvg::Group, transform: Transform, target: Size) -> Option<()> {
        if !group.filters().is_empty() || group.mask().is_some() {
            return None;
        }
        let transform = transform.pre_concat(group.transform());
        if !group.should_isolate() {
            return self.children(group, transform, target);
        }
        // resvg draws nothing for a layer box that does not transform.
        let Some(bbox) = group.layer_bounding_box().transform(transform) else {
            return Some(());
        };
        let layer = Size {
            width: ceil(bbox.width())
                .saturating_add(LAYER_PAD)
                .min(self.max_layer.width),
            height: ceil(bbox.height())
                .saturating_add(LAYER_PAD)
                .min(self.max_layer.height),
        };
        self.allocate(layer.bytes())?;
        self.charge(layer.pixels())?;
        self.children(group, transform, layer)?;
        if let Some(clip) = group.clip_path() {
            self.clip(clip, transform, layer)?;
        }
        self.charge(layer.pixels())?;
        self.release(layer.bytes());
        Some(())
    }

    fn children(&mut self, group: &usvg::Group, transform: Transform, target: Size) -> Option<()> {
        for child in group.children() {
            self.node(child, transform, target)?;
        }
        Some(())
    }

    /// `path::render`: an optional fill and an optional stroke.
    fn path(&mut self, path: &usvg::Path, transform: Transform, target: Size) -> Option<()> {
        if !path.is_visible() {
            return Some(());
        }
        if let Some(fill) = path.fill() {
            if matches!(fill.paint(), usvg::Paint::Pattern(_)) {
                return None;
            }
            self.fill(path, transform, target)?;
        }
        if let Some(stroke) = path.stroke() {
            if matches!(stroke.paint(), usvg::Paint::Pattern(_)) {
                return None;
            }
            let (pixels, rows) = covered(path.stroke_bounding_box(), transform, target);
            let edges = verbs(path).saturating_mul(STROKE_EDGE_FACTOR);
            self.charge(pixels.saturating_add(edges.saturating_mul(rows)))?;
            if let Some(dashes) = stroke.dasharray() {
                self.charge(dash_count(path, dashes).saturating_mul(DASH_WORK))?;
            }
        }
        Some(())
    }

    fn fill(&mut self, path: &usvg::Path, transform: Transform, target: Size) -> Option<()> {
        let (pixels, rows) = covered(path.data().bounds(), transform, target);
        self.charge(pixels.saturating_add(verbs(path).saturating_mul(rows)))
    }

    /// `clip::apply`: a target-sized buffer filled opaque, the clip children
    /// cleared into it, any nested clip applied to the target, then a coverage
    /// mask built, inverted, and applied.
    fn clip(&mut self, clip: &usvg::ClipPath, transform: Transform, target: Size) -> Option<()> {
        self.visit()?;
        self.allocate(target.bytes())?;
        self.charge(target.pixels().saturating_mul(2))?;
        self.clip_children(clip.root(), transform.pre_concat(clip.transform()), target)?;
        if let Some(inner) = clip.clip_path() {
            self.clip(inner, transform, target)?;
        }
        self.allocate(target.pixels())?;
        self.charge(target.pixels().saturating_mul(3))?;
        self.release(target.bytes().saturating_add(target.pixels()));
        Some(())
    }

    /// `clip::draw_children`: paths are filled; a group with its own clip
    /// path draws into a further target-sized buffer that is clipped and then
    /// composited. Images inside a clip path are ignored, as resvg does.
    fn clip_children(
        &mut self,
        group: &usvg::Group,
        transform: Transform,
        target: Size,
    ) -> Option<()> {
        for child in group.children() {
            self.visit()?;
            match child {
                usvg::Node::Path(path) => {
                    if path.is_visible() {
                        self.fill(path, transform, target)?;
                    }
                }
                usvg::Node::Text(text) => {
                    self.clip_children(text.flattened(), transform, target)?;
                }
                usvg::Node::Group(inner) => {
                    let transform = transform.pre_concat(inner.transform());
                    if let Some(clip) = inner.clip_path() {
                        self.allocate(target.bytes())?;
                        self.charge(target.pixels().saturating_mul(2))?;
                        self.clip_children(inner, transform, target)?;
                        self.clip(clip, transform, target)?;
                        self.release(target.bytes());
                    } else {
                        self.clip_children(inner, transform, target)?;
                    }
                }
                usvg::Node::Image(_) => {}
            }
        }
        Some(())
    }
}

/// Pixels and rows `rect` covers in `target` once transformed, padded for
/// anti-aliasing. A box that does not transform is charged the whole target.
fn covered(rect: Rect, transform: Transform, target: Size) -> (u64, u64) {
    match rect.transform(transform) {
        Some(device) => {
            let width = ceil(device.width())
                .saturating_add(COVER_PAD)
                .min(target.width);
            let height = ceil(device.height())
                .saturating_add(COVER_PAD)
                .min(target.height);
            (width.saturating_mul(height), height)
        }
        None => (target.pixels(), target.height),
    }
}

fn verbs(path: &usvg::Path) -> u64 {
    u64::try_from(path.data().len()).unwrap_or(u64::MAX)
}

fn ceil(value: f32) -> u64 {
    // `as` saturates and maps NaN to zero.
    value.ceil() as u64
}

/// Dashes tiny-skia produces for `path`, from the length of its control
/// polygon (never shorter than the curve), capped where tiny-skia gives up.
fn dash_count(path: &usvg::Path, dashes: &[f32]) -> u64 {
    let period: f64 = dashes.iter().map(|dash| f64::from(*dash)).sum();
    let pairs = (dashes.len() / 2) as f64;
    let count = polygon_length(path) * pairs / period;
    if count.is_finite() {
        count.clamp(0.0, MAX_DASHES).ceil() as u64
    } else {
        MAX_DASHES as u64
    }
}

fn polygon_length(path: &usvg::Path) -> f64 {
    let mut length = 0.0f64;
    let mut start = Point::zero();
    let mut current = Point::zero();
    let mut step = |from: Point, to: Point| {
        length += f64::from(from.distance(to));
    };
    for segment in path.data().segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                start = point;
                current = point;
            }
            PathSegment::LineTo(point) => {
                step(current, point);
                current = point;
            }
            PathSegment::QuadTo(control, point) => {
                step(current, control);
                step(control, point);
                current = point;
            }
            PathSegment::CubicTo(first, second, point) => {
                step(current, first);
                step(first, second);
                step(second, point);
                current = point;
            }
            PathSegment::Close => {
                step(current, start);
                current = start;
            }
        }
    }
    length
}

#[cfg(test)]
#[path = "svg_budget_tests.rs"]
mod tests;
