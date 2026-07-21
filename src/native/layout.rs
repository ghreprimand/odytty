// SPDX-License-Identifier: GPL-3.0-only
//! Pure pane-layout geometry — the headless core of the splits/panes feature.
//!
//! This module is **presentation-agnostic and GPU-free**. It owns the binary
//! split-tree model ([`PaneNode`]) and the pure functions that turn a tree plus
//! a content rectangle into per-pane pixel rectangles ([`layout_rects`]),
//! produce the 1px divider rectangles ([`divider_rects`]), and resolve
//! directional focus movement ([`focus_move`]). It also provides the pure tree
//! transforms a tab performs in response to split/close/equalize actions.
//!
//! Per `docs/panes-and-sessions-design.md` §3.1/§4.3/§8: nothing here touches
//! `Session`, the GPU, winit, or settings, so it is fully unit-testable without
//! a window. The native layer (Phase 1b) builds `WorkspaceSet`/`Tab` around these
//! types and calls these functions; until then this module has no call sites
//! and changes no behaviour.

// Phase-1a scaffold: these items are consumed by the Phase-1b arena/WorkspaceSet
// refactor. Suppress dead_code so the warning baseline is unchanged while the
// pure core lands first as an independently testable unit.
#![allow(dead_code)]

use super::session::SessionToken;

/// Minimum fraction of a split's primary extent that may be given to either
/// child, so a pane can never collapse to zero cells. A ratio is the fraction
/// allotted to the `first` child; it is clamped to `[MIN_RATIO, MAX_RATIO]`.
pub(super) const MIN_RATIO: f32 = 0.05;
/// Maximum fraction of a split's primary extent for the `first` child.
pub(super) const MAX_RATIO: f32 = 0.95;
/// Default ratio for a fresh split / the equalized value.
pub(super) const EVEN_RATIO: f32 = 0.5;

/// Tolerance (physical px) for the side-of test in [`focus_move`]. Pane rects
/// tile exactly with `divider_px` gaps, so a neighbor's near edge is at least
/// `divider_px` beyond the focused rect; a sub-pixel epsilon absorbs float
/// rounding without admitting same-side panes.
const SIDE_EPS: f32 = 0.5;

/// Axis a [`PaneNode::Split`] divides along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SplitAxis {
    /// Children sit side-by-side, separated by a **vertical** divider line.
    /// (tmux `split-window -h` / `Ctrl-b %`.)
    Columns,
    /// Children stack top/bottom, separated by a **horizontal** divider line.
    /// (tmux `split-window -v` / `Ctrl-b "`.)
    Rows,
}

/// Direction for [`focus_move`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}

/// A pane's pixel rectangle in physical-pixel, origin-top-left space — the same
/// basis as `grid.rs` vertices and `WindowPadding`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PaneRect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
}

impl PaneRect {
    pub(super) fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn right(&self) -> f32 {
        self.x + self.w
    }

    fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// A tab's pane layout: a binary tree whose leaves are sessions and whose
/// internal nodes are splits with a ratio. A single-pane tab is a lone
/// [`PaneNode::Leaf`], which the render/resize paths treat byte-identically to
/// today's single-session window (design doc §2.3).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PaneNode {
    /// One pane, identified by its session.
    Leaf(SessionToken),
    /// A split of two subtrees. `ratio` is the fraction of the parent's primary
    /// extent (width for [`SplitAxis::Columns`], height for [`SplitAxis::Rows`])
    /// given to `first`; it is always within `[MIN_RATIO, MAX_RATIO]`.
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    /// A fresh single-pane tab.
    pub(super) fn leaf(token: SessionToken) -> Self {
        PaneNode::Leaf(token)
    }

    /// True when this tab holds exactly one pane (the byte-identical path).
    pub(super) fn is_single_pane(&self) -> bool {
        matches!(self, PaneNode::Leaf(_))
    }

    /// The sole pane's token iff this is a single-pane tab.
    pub(super) fn sole_pane(&self) -> Option<SessionToken> {
        match self {
            PaneNode::Leaf(token) => Some(*token),
            PaneNode::Split { .. } => None,
        }
    }

    /// Append every leaf token in left-to-right / top-to-bottom tree order
    /// (the order `Ctrl-b o` "next pane" cycles through).
    pub(super) fn collect_leaves(&self, out: &mut Vec<SessionToken>) {
        match self {
            PaneNode::Leaf(token) => out.push(*token),
            PaneNode::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    /// All leaf tokens in tree order.
    pub(super) fn leaves(&self) -> Vec<SessionToken> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    /// Number of panes (leaves) in the tab.
    pub(super) fn pane_count(&self) -> usize {
        match self {
            PaneNode::Leaf(_) => 1,
            PaneNode::Split { first, second, .. } => first.pane_count() + second.pane_count(),
        }
    }

    /// True when `token` is a leaf somewhere in this tree.
    pub(super) fn contains(&self, token: SessionToken) -> bool {
        match self {
            PaneNode::Leaf(t) => *t == token,
            PaneNode::Split { first, second, .. } => {
                first.contains(token) || second.contains(token)
            }
        }
    }

    /// The token that follows `focused` in tree order, wrapping to the first
    /// leaf after the last (tmux `Ctrl-b o`). Returns `None` only when the tree
    /// is empty of leaves; an unknown `focused` falls back to the first leaf.
    pub(super) fn next_leaf_after(&self, focused: SessionToken) -> Option<SessionToken> {
        let leaves = self.leaves();
        if leaves.is_empty() {
            return None;
        }
        match leaves.iter().position(|t| *t == focused) {
            Some(i) => Some(leaves[(i + 1) % leaves.len()]),
            // Focused token not in this tree: fall back to the first leaf.
            None => Some(leaves[0]),
        }
    }

    /// Replace the leaf `target` with a split of `[target | new_token]` along
    /// `axis` at `ratio` (clamped). The pre-existing pane becomes `first`; the
    /// freshly spawned pane becomes `second` (the side tmux gives focus to).
    /// Returns the rewritten tree; a no-op (returns `self`) if `target` is
    /// absent.
    pub(super) fn split_leaf(
        self,
        target: SessionToken,
        axis: SplitAxis,
        ratio: f32,
        new_token: SessionToken,
    ) -> PaneNode {
        match self {
            PaneNode::Leaf(t) if t == target => PaneNode::Split {
                axis,
                ratio: clamp_ratio(ratio),
                first: Box::new(PaneNode::Leaf(t)),
                second: Box::new(PaneNode::Leaf(new_token)),
            },
            PaneNode::Leaf(t) => PaneNode::Leaf(t),
            PaneNode::Split {
                axis: a,
                ratio: r,
                first,
                second,
            } => PaneNode::Split {
                axis: a,
                ratio: r,
                first: Box::new(first.split_leaf(target, axis, ratio, new_token)),
                second: Box::new(second.split_leaf(target, axis, ratio, new_token)),
            },
        }
    }

    /// Remove the leaf `target`, collapsing its parent split into the surviving
    /// sibling. Returns `None` when `target` was the whole tree (the tab should
    /// then close). A no-op tree (returns `Some(self)`) if `target` is absent.
    pub(super) fn close_leaf(self, target: SessionToken) -> Option<PaneNode> {
        match self {
            PaneNode::Leaf(t) if t == target => None,
            leaf @ PaneNode::Leaf(_) => Some(leaf),
            PaneNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                if first.contains(target) {
                    match first.close_leaf(target) {
                        // First subtree collapsed away → promote the sibling.
                        None => Some(*second),
                        Some(new_first) => Some(PaneNode::Split {
                            axis,
                            ratio,
                            first: Box::new(new_first),
                            second,
                        }),
                    }
                } else {
                    match second.close_leaf(target) {
                        None => Some(*first),
                        Some(new_second) => Some(PaneNode::Split {
                            axis,
                            ratio,
                            first,
                            second: Box::new(new_second),
                        }),
                    }
                }
            }
        }
    }

    /// Reset every split ratio to [`EVEN_RATIO`] (tmux `Ctrl-b Space`/`=`).
    pub(super) fn equalized(self) -> PaneNode {
        match self {
            leaf @ PaneNode::Leaf(_) => leaf,
            PaneNode::Split {
                axis,
                first,
                second,
                ..
            } => PaneNode::Split {
                axis,
                ratio: EVEN_RATIO,
                first: Box::new(first.equalized()),
                second: Box::new(second.equalized()),
            },
        }
    }
}

/// Clamp a split ratio to the legal `[MIN_RATIO, MAX_RATIO]` band.
pub(super) fn clamp_ratio(ratio: f32) -> f32 {
    ratio.clamp(MIN_RATIO, MAX_RATIO)
}

/// Tile `content` across the tree's leaves, reserving `divider_px` between
/// split children. Returns one `(token, rect)` per leaf in tree order. The
/// rects partition `content` exactly: for any split, `first + divider + second`
/// covers the parent extent with no gap or overlap (design doc §3.1).
///
/// Each split floors the `first` child's extent to an integer pixel boundary so
/// dividers stay crisp; the `second` child takes the exact remainder.
pub(super) fn layout_rects(
    tree: &PaneNode,
    content: PaneRect,
    divider_px: f32,
) -> Vec<(SessionToken, PaneRect)> {
    let mut out = Vec::with_capacity(tree.pane_count());
    layout_into(tree, content, divider_px, &mut out);
    out
}

fn layout_into(
    node: &PaneNode,
    rect: PaneRect,
    divider_px: f32,
    out: &mut Vec<(SessionToken, PaneRect)>,
) {
    match node {
        PaneNode::Leaf(token) => out.push((*token, rect)),
        PaneNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (first_rect, second_rect) = split_rect(rect, *axis, *ratio, divider_px);
            layout_into(first, first_rect, divider_px, out);
            layout_into(second, second_rect, divider_px, out);
        }
    }
}

/// Split `rect` into the two child rects for a split node, leaving a
/// `divider_px` gap between them along the split axis.
fn split_rect(
    rect: PaneRect,
    axis: SplitAxis,
    ratio: f32,
    divider_px: f32,
) -> (PaneRect, PaneRect) {
    let ratio = clamp_ratio(ratio);
    match axis {
        SplitAxis::Columns => {
            let usable = (rect.w - divider_px).max(0.0);
            let first_w = (usable * ratio).floor();
            let second_w = usable - first_w;
            let first = PaneRect::new(rect.x, rect.y, first_w, rect.h);
            let second = PaneRect::new(rect.x + first_w + divider_px, rect.y, second_w, rect.h);
            (first, second)
        }
        SplitAxis::Rows => {
            let usable = (rect.h - divider_px).max(0.0);
            let first_h = (usable * ratio).floor();
            let second_h = usable - first_h;
            let first = PaneRect::new(rect.x, rect.y, rect.w, first_h);
            let second = PaneRect::new(rect.x, rect.y + first_h + divider_px, rect.w, second_h);
            (first, second)
        }
    }
}

/// The 1px (or `divider_px`-wide) divider rectangles for every split in the
/// tree, for the render layer to paint as themed `SolidQuad`s. Order is tree
/// order; one rect per internal split node.
pub(super) fn divider_rects(tree: &PaneNode, content: PaneRect, divider_px: f32) -> Vec<PaneRect> {
    divider_rects_with_axis(tree, content, divider_px)
        .into_iter()
        .map(|(rect, _)| rect)
        .collect()
}

/// Like [`divider_rects`] but tags each divider with the [`SplitAxis`] it
/// divides along, in the same pre-order numbering. A column split yields a
/// vertical divider (panes side-by-side → a horizontal `↔` resize affordance);
/// a row split yields a horizontal divider (panes stacked → a vertical `↕`
/// one). Used by the hover cursor-shape path to pick `ColResize` vs `RowResize`.
pub(super) fn divider_rects_with_axis(
    tree: &PaneNode,
    content: PaneRect,
    divider_px: f32,
) -> Vec<(PaneRect, SplitAxis)> {
    let mut out = Vec::new();
    dividers_into(tree, content, divider_px, &mut out);
    out
}

fn dividers_into(
    node: &PaneNode,
    rect: PaneRect,
    divider_px: f32,
    out: &mut Vec<(PaneRect, SplitAxis)>,
) {
    if let PaneNode::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    {
        let (first_rect, second_rect) = split_rect(rect, *axis, *ratio, divider_px);
        // The divider occupies the gap between the two child rects.
        let divider = match axis {
            SplitAxis::Columns => PaneRect::new(first_rect.right(), rect.y, divider_px, rect.h),
            SplitAxis::Rows => PaneRect::new(rect.x, first_rect.bottom(), rect.w, divider_px),
        };
        out.push((divider, *axis));
        dividers_into(first, first_rect, divider_px, out);
        dividers_into(second, second_rect, divider_px, out);
    }
}

/// The [`SplitAxis`] of the divider under a pixel point (widened by `grab_px`),
/// or `None` when the point is over no divider grab band. Mirrors
/// [`divider_at_point`]'s hit-test exactly so the hover cursor shape and the
/// press-to-drag grab agree on what counts as "on a divider". Drives the
/// `ColResize`/`RowResize` hover affordance.
pub(super) fn divider_axis_at_point(
    tree: &PaneNode,
    content: PaneRect,
    divider_px: f32,
    x: f32,
    y: f32,
    grab_px: f32,
) -> Option<SplitAxis> {
    divider_rects_with_axis(tree, content, divider_px)
        .into_iter()
        .find(|(d, _)| {
            x >= d.x - grab_px
                && x <= d.x + d.w + grab_px
                && y >= d.y - grab_px
                && y <= d.y + d.h + grab_px
        })
        .map(|(_, axis)| axis)
}

/// Convert a pane rect to its drawable cell-grid dimensions for a given cell
/// size. Pure integer math so it can be unit-tested without
/// `CellSize`/`Dimensions`.
///
/// A rect narrower or shorter than one cell returns zero on that axis. Terminal
/// models and PTYs still require non-zero dimensions, so the resize path clamps
/// these drawable dimensions to `1x1`; render and pointer paths use the zero to
/// skip content that cannot fit inside the actual padded pane.
pub(super) fn grid_dims_for_rect(rect: PaneRect, cell_w: u32, cell_h: u32) -> (usize, usize) {
    let cols = (rect.w.max(0.0) as u32) / cell_w.max(1);
    let rows = (rect.h.max(0.0) as u32) / cell_h.max(1);
    (cols as usize, rows as usize)
}

/// Tolerance (physical px) for the "is this edge flush with the content
/// boundary?" test in [`pane_grid_origin`]. Pane rects tile `content` exactly,
/// so an edge is either flush with the content boundary (a window-margin edge)
/// or inset by at least `divider_px` (a divider-facing edge); a sub-pixel
/// epsilon absorbs float rounding without misclassifying a divider edge.
const EDGE_EPS: f32 = 0.5;

/// The physical-pixel origin at which a pane's (floored) cell grid should be
/// drawn inside its `rect`, given the surrounding `content` rect.
///
/// [`grid_dims_for_rect`] floors a pane to whole cells, leaving a sub-cell
/// remainder (`rect.w - cols·cell_w`, `rect.h - rows·cell_h`). Drawn naïvely at
/// `rect`'s top-left, that remainder strands as background **between the grid
/// edge and the adjacent divider** — the visible inter-pane gap the operator
/// reported (and, since cells are ~2:1 tall, a *row* split strands ~2× a
/// *column* split's pixels, so gaps look non-uniform across axes).
///
/// This absorbs the remainder onto each pane's **outer** (window-margin) side so
/// the grid edge that abuts a divider sits flush to it: the visible separation
/// between any two adjacent panes is then exactly the 1px themed divider,
/// uniform across both axes and stable as the divider drags smoothly (the
/// divider position is unchanged — only the grid content shifts within the
/// pane). Leftover pixels pool at the window margin, where a sub-cell strip is
/// invisible/acceptable.
///
/// Edge classification is geometric: an edge flush with the `content` boundary
/// is an outer (margin) edge; an inset edge abuts a divider. A pane is pushed
/// toward a divider only when its *opposite* edge is a margin (so the remainder
/// has somewhere to pool). Consequences:
/// - **Single-pane / zoomed** (`rect == content`): both edges are margins on
///   each axis → zero offset → byte-identical to today's top-left placement.
/// - **Two-pane split**: the first child (margin on its outer side, divider on
///   its inner) is pushed flush to the divider; the second child stays flush at
///   the divider with its remainder at the far margin → exact 1px gap.
/// - **Sandwiched pane** (a 3+-way split along one axis, divider on *both*
///   sides): can't be flush on both with a floored grid, so it stays flush on
///   its low (left/top) side and a sub-cell residual remains on the far side.
///   2×2 grids and binary trees where every leaf touches a margin are exact.
pub(super) fn pane_grid_origin(
    rect: PaneRect,
    content: PaneRect,
    cell_w: u32,
    cell_h: u32,
) -> [f32; 2] {
    let (cols, rows) = grid_dims_for_rect(rect, cell_w, cell_h);
    let rem_w = (rect.w - cols as f32 * cell_w.max(1) as f32).max(0.0);
    let rem_h = (rect.h - rows as f32 * cell_h.max(1) as f32).max(0.0);

    let left_outer = rect.x <= content.x + EDGE_EPS;
    let right_inner = rect.right() < content.right() - EDGE_EPS;
    let top_outer = rect.y <= content.y + EDGE_EPS;
    let bottom_inner = rect.bottom() < content.bottom() - EDGE_EPS;

    // Push toward an inner (divider) edge only when the opposite edge is an
    // outer (margin) edge, so the absorbed remainder pools at the margin.
    let off_x = if left_outer && right_inner {
        rem_w
    } else {
        0.0
    };
    let off_y = if top_outer && bottom_inner {
        rem_h
    } else {
        0.0
    };
    [rect.x + off_x, rect.y + off_y]
}

/// PANE-PADDING: inset a pane's tiled `rect` by the configured window padding
/// `pad` on every **divider-facing** edge, returning the pane's drawable grid
/// rect. An edge flush with the `content` boundary is an outer window-margin
/// edge — its padding was already applied when `content` was inset from the
/// window, so it is left untouched. An edge that abuts a divider gets `pad`
/// physical pixels of breathing room, so a pane's glyphs no longer sit flush
/// against the divider after a split (the reported regression). Horizontal and
/// nested splits are handled uniformly because the classification is purely
/// geometric per edge.
///
/// Every downstream per-pane geometry — grid cell dimensions
/// ([`grid_dims_for_rect`]), the drawn origin ([`pane_grid_origin`]), the image
/// scissor ([`pane_image_scissor`]), and pointer→cell mapping — derives from
/// this inner rect, so the PTY size, the rendered grid, and hit-testing all stay
/// consistent. The tiled `rect`, the divider positions, and the divider grab
/// bands are unchanged (they keep using the full tiled rects), so divider drag
/// and focus movement are byte-identical.
///
/// Identity paths:
/// - **`pad == 0`**: no edge moves → inner rect == `rect` → byte-identical.
/// - **Single-pane / zoomed** (`rect == content`): every edge is a window
///   margin → nothing is inset → inner rect == `rect` → byte-identical.
pub(super) fn pane_inner_rect(rect: PaneRect, content: PaneRect, pad: f32) -> PaneRect {
    if pad <= 0.0 {
        return rect;
    }
    // A divider-facing edge is one NOT flush with the content boundary; those
    // are the only edges that gain per-pane padding (outer margins already have
    // it, folded into `content`).
    let left_div = rect.x > content.x + EDGE_EPS;
    let right_div = rect.right() < content.right() - EDGE_EPS;
    let top_div = rect.y > content.y + EDGE_EPS;
    let bottom_div = rect.bottom() < content.bottom() - EDGE_EPS;

    let x = rect.x + if left_div { pad } else { 0.0 };
    let y = rect.y + if top_div { pad } else { 0.0 };
    let w =
        (rect.w - if left_div { pad } else { 0.0 } - if right_div { pad } else { 0.0 }).max(0.0);
    let h =
        (rect.h - if top_div { pad } else { 0.0 } - if bottom_div { pad } else { 0.0 }).max(0.0);
    PaneRect::new(x, y, w, h)
}

/// PANE-SUBCELL-CLIP: resolve a gliding pane's RENDER origin and its vertical
/// clip band from its at-rest `base_origin` (from [`pane_grid_origin`]) and the
/// current sub-cell remainder `frac_px` (in `[0, cell_h)`, from the glide
/// follower). A downward glide shifts the content origin down by `frac_px`
/// (matching the single-pane `content_origin` shift) and clamps the pane's
/// vertices to its at-rest grid rect `[base_y, base_y + rows·cell_h]`, so the
/// partial bottom row the shift pushes out is cropped at the pane's own content
/// bottom instead of smearing across the divider into the neighbour.
///
/// At rest (`frac_px == 0.0`) this returns the base origin and an inert
/// [`VClip::NONE`], so the split-at-rest frame is byte-identical. Pure, so the
/// per-pane frac accounting is unit-testable without a GPU.
pub(super) fn pane_glide_origin_and_clip(
    base_origin: [f32; 2],
    frac_px: f32,
    rows: usize,
    cell_h: f32,
) -> ([f32; 2], crate::grid::VClip) {
    if frac_px == 0.0 {
        return (base_origin, crate::grid::VClip::NONE);
    }
    let top_y = base_origin[1];
    let bottom_y = top_y + rows as f32 * cell_h;
    (
        [base_origin[0], top_y + frac_px],
        crate::grid::VClip { top_y, bottom_y },
    )
}

/// Cut 1 (inline graphics in splits): the scissor rect (physical px
/// `[x, y, w, h]`) an inline image is clipped to inside a split pane — the
/// pane's at-rest grid rect (`base_origin` from [`pane_grid_origin`] plus the
/// pane's grid extent), clamped to the pane rect and the surface. Bounds BOTH
/// axes, so an image
/// rasterized wider or taller than its pane cannot bleed across a vertical OR
/// horizontal divider into a neighbour (a vertical-only clip could not stop the
/// horizontal case). Pure, so the no-bleed geometry is unit-testable without a
/// GPU. A zero-area pane (no cols/rows, or fully off-surface) yields a zero
/// width/height, which the draw path skips.
#[allow(clippy::too_many_arguments)]
pub(super) fn pane_image_scissor(
    base_origin: [f32; 2],
    cols: usize,
    rows: usize,
    cell_w: f32,
    cell_h: f32,
    pane_rect: PaneRect,
    surface_w: f32,
    surface_h: f32,
) -> [u32; 4] {
    let grid_w = cols as f32 * cell_w;
    let grid_h = rows as f32 * cell_h;
    // Bound the scissor by the pane's OWN rect as well as the surface: an image
    // rasterized larger than its grid, or a glide origin nudged a hair past the
    // pane edge, must still be cropped at the pane boundary so it cannot bleed
    // across a divider into a neighbour before the surface edge would stop it.
    let sx = base_origin[0].max(pane_rect.x).max(0.0);
    let sy = base_origin[1].max(pane_rect.y).max(0.0);
    let sx1 = (base_origin[0] + grid_w)
        .min(pane_rect.right())
        .min(surface_w);
    let sy1 = (base_origin[1] + grid_h)
        .min(pane_rect.bottom())
        .min(surface_h);
    [
        sx as u32,
        sy as u32,
        (sx1 - sx).max(0.0) as u32,
        (sy1 - sy).max(0.0) as u32,
    ]
}

/// Resolve directional focus movement: from the pane currently `focused`, pick
/// the spatial neighbor in direction `dir`, or `None` when there is none.
///
/// Pure function over the rect list produced by [`layout_rects`] (design doc
/// §4.3). A candidate must lie wholly on the `dir` side of the focused rect and
/// share some perpendicular overlap with it; among those, the nearest along the
/// movement axis wins, breaking ties by the largest perpendicular overlap.
pub(super) fn focus_move(
    rects: &[(SessionToken, PaneRect)],
    focused: SessionToken,
    dir: FocusDir,
) -> Option<SessionToken> {
    let focused_rect = rects.iter().find(|(t, _)| *t == focused).map(|(_, r)| *r)?;

    let mut best: Option<(SessionToken, f32, f32)> = None; // (token, distance, overlap)
    for (token, rect) in rects {
        if *token == focused {
            continue;
        }
        let Some((distance, overlap)) = neighbor_metric(&focused_rect, rect, dir) else {
            continue;
        };
        let better = match best {
            None => true,
            Some((_, best_dist, best_overlap)) => {
                distance < best_dist - SIDE_EPS
                    || ((distance - best_dist).abs() <= SIDE_EPS && overlap > best_overlap)
            }
        };
        if better {
            best = Some((*token, distance, overlap));
        }
    }
    best.map(|(token, _, _)| token)
}

/// If `candidate` is a valid neighbor of `focused` in direction `dir`, return
/// `(distance_along_axis, perpendicular_overlap)`; else `None`.
fn neighbor_metric(focused: &PaneRect, candidate: &PaneRect, dir: FocusDir) -> Option<(f32, f32)> {
    match dir {
        FocusDir::Right => {
            if candidate.x + SIDE_EPS < focused.right() {
                return None;
            }
            let overlap =
                span_overlap(focused.y, focused.bottom(), candidate.y, candidate.bottom());
            (overlap > 0.0).then_some((candidate.x - focused.right(), overlap))
        }
        FocusDir::Left => {
            if candidate.right() - SIDE_EPS > focused.x {
                return None;
            }
            let overlap =
                span_overlap(focused.y, focused.bottom(), candidate.y, candidate.bottom());
            (overlap > 0.0).then_some((focused.x - candidate.right(), overlap))
        }
        FocusDir::Down => {
            if candidate.y + SIDE_EPS < focused.bottom() {
                return None;
            }
            let overlap = span_overlap(focused.x, focused.right(), candidate.x, candidate.right());
            (overlap > 0.0).then_some((candidate.y - focused.bottom(), overlap))
        }
        FocusDir::Up => {
            if candidate.bottom() - SIDE_EPS > focused.y {
                return None;
            }
            let overlap = span_overlap(focused.x, focused.right(), candidate.x, candidate.right());
            (overlap > 0.0).then_some((focused.y - candidate.bottom(), overlap))
        }
    }
}

/// Length of the overlap of `[a0, a1]` and `[b0, b1]` (0 when disjoint).
fn span_overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// Hit-test a pixel point against the tree's pane dividers, returning the
/// tree-order index (matching [`divider_rects`]) of the divider under the
/// point, widened by `grab_px` on every side so a 1px hairline is actually
/// grabbable. `None` when no divider is hit. Used to start a divider drag
/// (design doc §4.2). Pure over the rect list so it is unit-testable headless.
pub(super) fn divider_at_point(
    tree: &PaneNode,
    content: PaneRect,
    divider_px: f32,
    x: f32,
    y: f32,
    grab_px: f32,
) -> Option<usize> {
    divider_rects(tree, content, divider_px)
        .into_iter()
        .position(|d| {
            x >= d.x - grab_px
                && x <= d.x + d.w + grab_px
                && y >= d.y - grab_px
                && y <= d.y + d.h + grab_px
        })
}

/// Drag the divider at tree-order `target` to the pixel point `(x, y)`,
/// re-deriving that split's ratio from the point's position within the split's
/// own rect and clamping it to `[MIN_RATIO, MAX_RATIO]`. Returns the new ratio
/// when the target split exists, else `None` (and leaves the tree unchanged).
/// The walk reproduces [`divider_rects`] pre-order numbering so the index a
/// drag started from maps to the same split throughout the gesture.
pub(super) fn drag_divider_to(
    tree: &mut PaneNode,
    content: PaneRect,
    divider_px: f32,
    target: usize,
    x: f32,
    y: f32,
) -> Option<f32> {
    let mut counter = 0usize;
    drag_into(tree, content, divider_px, target, &mut counter, x, y)
}

fn drag_into(
    node: &mut PaneNode,
    rect: PaneRect,
    divider_px: f32,
    target: usize,
    counter: &mut usize,
    x: f32,
    y: f32,
) -> Option<f32> {
    let PaneNode::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return None;
    };
    let me = *counter;
    *counter += 1;
    if me == target {
        let new_ratio = match axis {
            SplitAxis::Columns => (x - rect.x) / (rect.w - divider_px).max(1.0),
            SplitAxis::Rows => (y - rect.y) / (rect.h - divider_px).max(1.0),
        };
        let clamped = clamp_ratio(new_ratio);
        *ratio = clamped;
        return Some(clamped);
    }
    let (first_rect, second_rect) = split_rect(rect, *axis, *ratio, divider_px);
    if let Some(found) = drag_into(first, first_rect, divider_px, target, counter, x, y) {
        return Some(found);
    }
    drag_into(second, second_rect, divider_px, target, counter, x, y)
}

/// Snap the active dragged split (`target`, in the same pre-order numbering as
/// [`drag_divider_to`]) so its divider lands on a whole-cell boundary, returning
/// the snapped ratio when the split exists. Called once on drag *release* so the
/// outer window margins are identical at every rest position (the smooth
/// per-pixel drag is untouched — only the release re-derives a snapped ratio).
///
/// Why this makes the outer margin constant: snapping the first child to exactly
/// `k·cell` pixels leaves it flush on both its sides (zero remainder), so the
/// only stranded remainder is the far pane's `usable mod cell`, which is
/// **independent of `k`** → the same at every snap position. Without the snap,
/// the first child's sub-cell remainder varies continuously with the drag and
/// (via [`pane_grid_origin`]) breathes at its outer margin.
///
/// The first child is clamped to `[cell, usable − cell]` so neither child ever
/// snaps below one cell (then re-clamped to the legal ratio band), mirroring the
/// min-size guarantee the drag path already enforces.
pub(super) fn snap_divider_to_cells(
    tree: &mut PaneNode,
    content: PaneRect,
    divider_px: f32,
    target: usize,
    cell_w: u32,
    cell_h: u32,
) -> Option<f32> {
    let mut counter = 0usize;
    snap_into(
        tree,
        content,
        divider_px,
        target,
        &mut counter,
        cell_w,
        cell_h,
    )
}

fn snap_into(
    node: &mut PaneNode,
    rect: PaneRect,
    divider_px: f32,
    target: usize,
    counter: &mut usize,
    cell_w: u32,
    cell_h: u32,
) -> Option<f32> {
    let PaneNode::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return None;
    };
    let me = *counter;
    *counter += 1;
    if me == target {
        // `usable` and `cell` along the split's primary axis; matches the
        // floored first-child extent `split_rect` computes for this split.
        let (usable, cell) = match axis {
            SplitAxis::Columns => ((rect.w - divider_px).max(0.0), cell_w.max(1) as f32),
            SplitAxis::Rows => ((rect.h - divider_px).max(0.0), cell_h.max(1) as f32),
        };
        let first_px = (usable * clamp_ratio(*ratio)).floor();
        // Snap to the nearest whole-cell boundary, then keep both children ≥ 1
        // cell when the split is large enough to hold two.
        let mut snapped = (first_px / cell).round() * cell;
        if usable >= 2.0 * cell {
            snapped = snapped.clamp(cell, usable - cell);
        }
        let new_ratio = clamp_ratio(snapped / usable.max(1.0));
        *ratio = new_ratio;
        return Some(new_ratio);
    }
    let (first_rect, second_rect) = split_rect(rect, *axis, *ratio, divider_px);
    if let Some(found) = snap_into(
        first, first_rect, divider_px, target, counter, cell_w, cell_h,
    ) {
        return Some(found);
    }
    snap_into(
        second,
        second_rect,
        divider_px,
        target,
        counter,
        cell_w,
        cell_h,
    )
}

/// The pane token whose rect contains the pixel point, or `None` when the point
/// falls in a divider gap or outside the content. Focus-follows-click resolves
/// the clicked pane through this (design doc §4.3, audit row #6).
pub(super) fn pane_at_point(
    rects: &[(SessionToken, PaneRect)],
    x: f32,
    y: f32,
) -> Option<SessionToken> {
    rects
        .iter()
        .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
        .map(|(token, _)| *token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(n: u64) -> SessionToken {
        SessionToken(n)
    }

    fn content() -> PaneRect {
        PaneRect::new(0.0, 0.0, 800.0, 600.0)
    }

    // ----- Cut 1: inline-graphics per-pane scissor (no bleed across divider) -----

    #[test]
    fn pane_image_scissor_bounds_the_pane_grid_rect() {
        // A pane whose grid sits at (100, 50) spanning 40 cols x 20 rows of an
        // 8x16 cell => 320 x 320 px. The scissor is exactly that rect.
        let pane = PaneRect::new(100.0, 50.0, 320.0, 320.0);
        let scissor = pane_image_scissor([100.0, 50.0], 40, 20, 8.0, 16.0, pane, 2000.0, 2000.0);
        assert_eq!(scissor, [100, 50, 320, 320]);
    }

    #[test]
    fn pane_image_scissor_clamps_to_the_surface() {
        // A pane grid extending past the surface right/bottom is clamped so the
        // scissor never exceeds the render target (wgpu would reject it).
        let pane = PaneRect::new(700.0, 500.0, 320.0, 320.0);
        let scissor = pane_image_scissor([700.0, 500.0], 40, 20, 8.0, 16.0, pane, 900.0, 700.0);
        // right: min(700+320, 900) = 900 -> w = 200; bottom: min(500+320,700)=700 -> h=200
        assert_eq!(scissor, [700, 500, 200, 200]);
    }

    #[test]
    fn pane_image_scissor_clamps_to_a_pane_smaller_than_its_grid() {
        // C21: a grid extent (320x320) larger than the pane rect it belongs to
        // (200x160) is cropped at the pane edge, well inside the surface, so an
        // over-large image cannot bleed past the pane boundary.
        let pane = PaneRect::new(100.0, 50.0, 200.0, 160.0);
        let scissor = pane_image_scissor([100.0, 50.0], 40, 20, 8.0, 16.0, pane, 2000.0, 2000.0);
        // right: min(100+320, 300, 2000) = 300 -> w = 200; bottom: min(50+320, 210, 2000) = 210 -> h = 160
        assert_eq!(scissor, [100, 50, 200, 160]);
    }

    #[test]
    fn column_split_pane_scissors_do_not_overlap() {
        // A 50/50 column split of an 800px-wide content with a 1px divider.
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let rects = layout_rects(&tree, content(), 1.0);
        let (cell_w, cell_h) = (8.0, 16.0);
        // Derive each pane's grid dims + base origin, then its scissor.
        let mut boxes = Vec::new();
        for (_, rect) in &rects {
            let (cols, rows) = grid_dims_for_rect(*rect, cell_w as u32, cell_h as u32);
            let base = pane_grid_origin(*rect, content(), cell_w as u32, cell_h as u32);
            let sc = pane_image_scissor(base, cols, rows, cell_w, cell_h, *rect, 800.0, 600.0);
            boxes.push(sc);
        }
        let left = boxes[0];
        let right = boxes[1];
        // Left scissor's right edge must not reach the right scissor's left edge:
        // a wide image in the left pane is clipped before the divider/neighbour.
        let left_right_edge = left[0] + left[2];
        let right_left_edge = right[0];
        assert!(
            left_right_edge <= right_left_edge,
            "left scissor {left:?} bleeds into right scissor {right:?}"
        );
    }

    #[test]
    fn row_split_pane_scissors_do_not_overlap() {
        // A 50/50 row split; the top pane's scissor bottom must not reach the
        // bottom pane's scissor top (no vertical bleed across the divider).
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Rows, 0.5, tok(1));
        let rects = layout_rects(&tree, content(), 1.0);
        let (cell_w, cell_h) = (8.0, 16.0);
        let mut boxes = Vec::new();
        for (_, rect) in &rects {
            let (cols, rows) = grid_dims_for_rect(*rect, cell_w as u32, cell_h as u32);
            let base = pane_grid_origin(*rect, content(), cell_w as u32, cell_h as u32);
            let sc = pane_image_scissor(base, cols, rows, cell_w, cell_h, *rect, 800.0, 600.0);
            boxes.push(sc);
        }
        let top = boxes[0];
        let bottom = boxes[1];
        let top_bottom_edge = top[1] + top[3];
        let bottom_top_edge = bottom[1];
        assert!(
            top_bottom_edge <= bottom_top_edge,
            "top scissor {top:?} bleeds into bottom scissor {bottom:?}"
        );
    }

    // ----- tree shape / leaves -----

    #[test]
    fn single_leaf_is_single_pane() {
        let tree = PaneNode::leaf(tok(0));
        assert!(tree.is_single_pane());
        assert_eq!(tree.sole_pane(), Some(tok(0)));
        assert_eq!(tree.pane_count(), 1);
        assert_eq!(tree.leaves(), vec![tok(0)]);
    }

    #[test]
    fn split_is_not_single_pane() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        assert!(!tree.is_single_pane());
        assert_eq!(tree.sole_pane(), None);
        assert_eq!(tree.pane_count(), 2);
        assert_eq!(tree.leaves(), vec![tok(0), tok(1)]);
    }

    // ----- layout_rects: single pane is byte-identical content -----

    #[test]
    fn single_pane_fills_content_exactly() {
        let tree = PaneNode::leaf(tok(7));
        let rects = layout_rects(&tree, content(), 1.0);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].0, tok(7));
        assert_eq!(rects[0].1, content());
    }

    // ----- layout_rects: columns tile exactly -----

    #[test]
    fn columns_split_tiles_width_exactly() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let rects = layout_rects(&tree, content(), 1.0);
        assert_eq!(rects.len(), 2);
        let (first, second) = (rects[0].1, rects[1].1);
        // first + divider(1) + second == total width, no overlap/gap.
        assert!((first.w + 1.0 + second.w - 800.0).abs() < f32::EPSILON);
        assert!((second.x - first.right() - 1.0).abs() < f32::EPSILON);
        // Heights span the full content height.
        assert_eq!(first.h, 600.0);
        assert_eq!(second.h, 600.0);
        // 50% of usable 799 → floor(399.5) = 399.
        assert_eq!(first.w, 399.0);
        assert_eq!(second.w, 400.0);
    }

    #[test]
    fn rows_split_tiles_height_exactly() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Rows, 0.5, tok(1));
        let rects = layout_rects(&tree, content(), 1.0);
        let (first, second) = (rects[0].1, rects[1].1);
        assert!((first.h + 1.0 + second.h - 600.0).abs() < f32::EPSILON);
        assert!((second.y - first.bottom() - 1.0).abs() < f32::EPSILON);
        assert_eq!(first.w, 800.0);
        assert_eq!(second.w, 800.0);
    }

    #[test]
    fn nested_split_tiles_without_gaps() {
        // Left column is one pane; right column splits into two rows.
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(tok(1))),
                second: Box::new(PaneNode::Leaf(tok(2))),
            }),
        };
        let rects = layout_rects(&tree, content(), 1.0);
        assert_eq!(rects.len(), 3);
        let map: std::collections::HashMap<_, _> = rects.iter().copied().collect();
        let left = map[&tok(0)];
        let top_right = map[&tok(1)];
        let bot_right = map[&tok(2)];
        assert_eq!(left.h, 600.0);
        assert!((top_right.x - bot_right.x).abs() < f32::EPSILON);
        assert!((bot_right.y - top_right.bottom() - 1.0).abs() < f32::EPSILON);
        assert!(left.right() < top_right.x); // divider gap between columns
    }

    #[test]
    fn ratio_is_clamped_into_legal_band() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.0, tok(1));
        if let PaneNode::Split { ratio, .. } = tree {
            assert!((ratio - MIN_RATIO).abs() < f32::EPSILON);
        } else {
            panic!("expected split");
        }
        assert_eq!(clamp_ratio(2.0), MAX_RATIO);
        assert_eq!(clamp_ratio(-1.0), MIN_RATIO);
    }

    // ----- divider_rects -----

    #[test]
    fn divider_count_matches_split_count() {
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(tok(1))),
                second: Box::new(PaneNode::Leaf(tok(2))),
            }),
        };
        // 3 leaves → 2 internal splits → 2 dividers.
        assert_eq!(divider_rects(&tree, content(), 1.0).len(), 2);
    }

    #[test]
    fn columns_divider_is_vertical_strip_in_the_gap() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let dividers = divider_rects(&tree, content(), 1.0);
        assert_eq!(dividers.len(), 1);
        let d = dividers[0];
        assert_eq!(d.w, 1.0);
        assert_eq!(d.h, 600.0);
        assert_eq!(d.x, 399.0); // immediately right of the first pane
    }

    #[test]
    fn single_pane_has_no_dividers() {
        let tree = PaneNode::leaf(tok(0));
        assert!(divider_rects(&tree, content(), 1.0).is_empty());
    }

    /// §8 pixel-smoke: the exact geometry contract `GpuState::update_from_panes`
    /// relies on for divider + multi-grid composition. For every split, the
    /// divider is a crisp 1px strip that exactly fills the gap between the two
    /// child panes — `first.right == divider near edge`, `divider far edge ==
    /// second near edge`, and `first + divider + second == parent` with no gap
    /// or overlap, on both axes and when nested. This is the headless analogue
    /// of the divider/multi-grid pixel render (the GPU draws these rects as
    /// themed solid quads in panes that tile exactly).
    #[test]
    fn pixel_smoke_divider_fills_gap_with_no_overlap() {
        let divider_px = 1.0;
        // A columns split nested with a rows split → two dividers, both axes.
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.4,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.6,
                first: Box::new(PaneNode::Leaf(tok(1))),
                second: Box::new(PaneNode::Leaf(tok(2))),
            }),
        };
        let c = content();
        let rects: std::collections::HashMap<_, _> =
            layout_rects(&tree, c, divider_px).into_iter().collect();
        let dividers = divider_rects(&tree, c, divider_px);
        assert_eq!(dividers.len(), 2);

        // Outer columns divider: crisp 1px vertical strip between left pane and
        // the right column, full content height, exactly filling the gap.
        let left = rects[&tok(0)];
        let top_right = rects[&tok(1)];
        let col_div = dividers
            .iter()
            .find(|d| d.w == divider_px && d.h == c.h)
            .expect("vertical divider spanning full height");
        assert_eq!(col_div.w, 1.0, "divider is crisp 1px");
        assert_eq!(
            col_div.x,
            left.right(),
            "divider starts at first pane's edge"
        );
        assert_eq!(
            col_div.right(),
            top_right.x,
            "second column starts at divider's far edge"
        );
        // No gap / no overlap across the whole content width.
        assert!((left.w + divider_px + top_right.w - c.w).abs() < f32::EPSILON);

        // Inner rows divider: crisp 1px horizontal strip between the two
        // right-column panes, full column width, exactly filling the gap.
        let bot_right = rects[&tok(2)];
        let row_div = dividers
            .iter()
            .find(|d| d.h == divider_px && (d.w - top_right.w).abs() < f32::EPSILON)
            .expect("horizontal divider spanning the right column width");
        assert_eq!(row_div.h, 1.0, "divider is crisp 1px");
        assert_eq!(row_div.y, top_right.bottom(), "divider at top pane's edge");
        assert_eq!(
            row_div.bottom(),
            bot_right.y,
            "bottom pane starts at divider's far edge"
        );
        assert!((top_right.h + divider_px + bot_right.h - c.h).abs() < f32::EPSILON);

        // Integer-pixel boundaries keep dividers crisp (no sub-pixel seam).
        for d in &dividers {
            assert_eq!(d.x, d.x.floor());
            assert_eq!(d.y, d.y.floor());
        }
    }

    // ----- grid_dims_for_rect -----

    #[test]
    fn grid_dims_floor_and_allow_collapsed_axes() {
        assert_eq!(
            grid_dims_for_rect(PaneRect::new(0.0, 0.0, 80.0, 32.0), 8, 16),
            (10, 2)
        );
        // A sub-cell rect has no drawable cells. The PTY resize path separately
        // clamps this to its required 1x1 minimum.
        assert_eq!(
            grid_dims_for_rect(PaneRect::new(0.0, 0.0, 3.0, 3.0), 8, 16),
            (0, 0)
        );
        assert_eq!(
            grid_dims_for_rect(PaneRect::new(0.0, 0.0, 8.0, 3.0), 8, 16),
            (1, 0)
        );
    }

    // ----- pane_grid_origin: remainder absorption (uniform 1px gap) -----

    /// A single-pane tab (`rect == content`) must keep its top-left placement
    /// exactly — the byte-identical path. Both edges are window margins, so no
    /// remainder is absorbed regardless of how the content fails to tile evenly.
    #[test]
    fn single_pane_grid_origin_is_byte_identical_top_left() {
        // 101x99 content, 8x16 cells: neither axis divides evenly.
        let content = PaneRect::new(7.0, 13.0, 101.0, 99.0);
        assert_eq!(pane_grid_origin(content, content, 8, 16), [7.0, 13.0]);
    }

    /// Regression guard for the reported non-uniform gap: at a
    /// non-cell-aligned ratio, the gap between the two panes' grid edges must be
    /// exactly `divider_px` on a COLUMN split (vertical divider).
    #[test]
    fn column_split_grid_gap_is_exactly_the_divider() {
        let (cell_w, cell_h, divider_px) = (8u32, 16u32, 1.0f32);
        // usable = 101 - 1 = 100; first_w = floor(100*0.5) = 50 (not a multiple
        // of 8, so the left pane floors to 6 cols = 48px with a 2px remainder).
        let content = PaneRect::new(0.0, 0.0, 101.0, 64.0);
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Leaf(tok(1))),
        };
        let rects = layout_rects(&tree, content, divider_px);
        let (left, right) = (rects[0].1, rects[1].1);
        let (cols_l, _) = grid_dims_for_rect(left, cell_w, cell_h);
        let ol = pane_grid_origin(left, content, cell_w, cell_h);
        let or = pane_grid_origin(right, content, cell_w, cell_h);
        let left_grid_right = ol[0] + cols_l as f32 * cell_w as f32;
        let right_grid_left = or[0];
        assert_eq!(
            right_grid_left - left_grid_right,
            divider_px,
            "column-split inter-pane gap must equal the divider width exactly"
        );
    }

    /// Same regression guard on a ROW split (horizontal divider) — the axis that
    /// looked visibly worse because cells are ~2:1 tall.
    #[test]
    fn row_split_grid_gap_is_exactly_the_divider() {
        let (cell_w, cell_h, divider_px) = (8u32, 16u32, 1.0f32);
        // usable = 99 - 1 = 98; first_h = floor(98*0.5) = 49 → 3 rows = 48px,
        // 1px remainder on the top pane.
        let content = PaneRect::new(0.0, 0.0, 80.0, 99.0);
        let tree = PaneNode::Split {
            axis: SplitAxis::Rows,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Leaf(tok(1))),
        };
        let rects = layout_rects(&tree, content, divider_px);
        let (top, bottom) = (rects[0].1, rects[1].1);
        let (_, rows_t) = grid_dims_for_rect(top, cell_w, cell_h);
        let ot = pane_grid_origin(top, content, cell_w, cell_h);
        let ob = pane_grid_origin(bottom, content, cell_w, cell_h);
        let top_grid_bottom = ot[1] + rows_t as f32 * cell_h as f32;
        let bottom_grid_top = ob[1];
        assert_eq!(
            bottom_grid_top - top_grid_bottom,
            divider_px,
            "row-split inter-pane gap must equal the divider width exactly"
        );
    }

    /// The absorbed origin must keep pointer→cell mapping consistent: a pixel at
    /// the center of cell (c, r) inside a pane maps back to (c, r) when the
    /// pane's grid origin is folded in. Uses the same flooring a consumer does.
    #[test]
    fn pointer_to_cell_round_trips_through_absorbed_origin() {
        let (cell_w, cell_h, divider_px) = (8u32, 16u32, 1.0f32);
        let content = PaneRect::new(0.0, 0.0, 101.0, 64.0);
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Leaf(tok(1))),
        };
        let rects = layout_rects(&tree, content, divider_px);
        // The left pane carries the absorbed x-offset (its grid is pushed right
        // to sit flush against the divider), so it is the interesting case.
        let left = rects[0].1;
        let (cols, rows) = grid_dims_for_rect(left, cell_w, cell_h);
        let [ox, oy] = pane_grid_origin(left, content, cell_w, cell_h);
        for (c, r) in [(0usize, 0usize), (2, 1), (cols - 1, rows - 1)] {
            let px = ox + (c as f32 + 0.5) * cell_w as f32;
            let py = oy + (r as f32 + 0.5) * cell_h as f32;
            let col = ((px - ox).max(0.0) as u32 / cell_w) as usize;
            let row = ((py - oy).max(0.0) as u32 / cell_h) as usize;
            assert_eq!((col, row), (c, r), "round-trip failed at cell ({c},{r})");
        }
    }

    // ----- pane_inner_rect: per-divider padding (breathing room) -----

    /// Zero padding is a strict identity: the inner rect equals the tiled rect
    /// on every pane of a split, so the padding-0 frame is byte-identical.
    #[test]
    fn pane_inner_rect_zero_padding_is_identity() {
        let content = PaneRect::new(4.0, 4.0, 800.0, 600.0);
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        for (_, rect) in layout_rects(&tree, content, 1.0) {
            assert_eq!(pane_inner_rect(rect, content, 0.0), rect);
        }
    }

    /// A single-pane / zoomed pane (`rect == content`) never has a divider-facing
    /// edge, so even at nonzero padding the inner rect equals the tiled rect —
    /// the single-pane path keeps its full outer-padded content unchanged.
    #[test]
    fn pane_inner_rect_single_pane_is_identity() {
        let content = PaneRect::new(4.0, 4.0, 800.0, 600.0);
        assert_eq!(pane_inner_rect(content, content, 8.0), content);
    }

    /// A column split insets the left pane's RIGHT edge and the right pane's
    /// LEFT edge by `pad`, and nothing else: outer margins (left of left pane,
    /// right of right pane) and both full-height edges keep the tiled geometry.
    /// The gap between the two panes' drawable rects becomes `2*pad + divider`.
    #[test]
    fn pane_inner_rect_column_split_pads_the_divider_facing_edges() {
        let content = PaneRect::new(4.0, 4.0, 801.0, 600.0);
        let pad = 8.0;
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let rects = layout_rects(&tree, content, 1.0);
        let (left, right) = (rects[0].1, rects[1].1);
        let li = pane_inner_rect(left, content, pad);
        let ri = pane_inner_rect(right, content, pad);
        // Left pane: outer-left edge untouched, right edge pulled in by pad.
        assert_eq!(li.x, left.x, "outer-left edge keeps the window margin");
        assert_eq!(li.w, left.w - pad, "left pane's divider edge gains pad");
        assert_eq!(li.y, left.y);
        assert_eq!(li.h, left.h, "full-height edges are outer margins");
        // Right pane: left edge pushed in by pad, outer-right edge untouched.
        assert_eq!(ri.x, right.x + pad, "right pane's divider edge gains pad");
        assert_eq!(ri.w, right.w - pad, "outer-right edge keeps the margin");
        // The visible gap between drawable rects is pad + divider + pad.
        let gap = ri.x - (li.x + li.w);
        assert_eq!(gap, 2.0 * pad + 1.0, "gap = pad + 1px divider + pad");
        // Both drawable rects stay clear of the 1px divider strip on both sides.
        let divider_x = left.right(); // == content.x-relative divider left edge
        assert!(li.x + li.w <= divider_x - pad + 0.001);
        assert!(ri.x >= divider_x + 1.0 + pad - 0.001);
    }

    /// A row split is the exact vertical analogue: the top pane's BOTTOM edge and
    /// the bottom pane's TOP edge each gain `pad`; the full-width edges (outer
    /// margins) are untouched.
    #[test]
    fn pane_inner_rect_row_split_pads_the_divider_facing_edges() {
        let content = PaneRect::new(4.0, 4.0, 800.0, 601.0);
        let pad = 6.0;
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Rows, 0.5, tok(1));
        let rects = layout_rects(&tree, content, 1.0);
        let (top, bottom) = (rects[0].1, rects[1].1);
        let ti = pane_inner_rect(top, content, pad);
        let bi = pane_inner_rect(bottom, content, pad);
        assert_eq!(ti.y, top.y, "outer-top edge keeps the margin");
        assert_eq!(ti.h, top.h - pad, "top pane's divider edge gains pad");
        assert_eq!(ti.x, top.x);
        assert_eq!(ti.w, top.w, "full-width edges are outer margins");
        assert_eq!(bi.y, bottom.y + pad, "bottom pane's divider edge gains pad");
        assert_eq!(bi.h, bottom.h - pad, "outer-bottom edge keeps the margin");
    }

    /// A 2x2 grid's inner (sandwiched) corners: every pane touches the outer
    /// window margin on exactly two sides and a divider on the other two, so each
    /// pane is inset by `pad` on precisely its two divider-facing edges — uniform
    /// across the whole grid regardless of nesting order.
    #[test]
    fn pane_inner_rect_2x2_pads_every_interior_edge() {
        let content = PaneRect::new(0.0, 0.0, 800.0, 600.0);
        let pad = 5.0;
        let rects = layout_rects(&grid_2x2(), content, 1.0);
        for (_, rect) in &rects {
            let inner = pane_inner_rect(*rect, content, pad);
            // Each corner pane is a window margin on two sides and a divider on
            // the other two: exactly one of {x margin, x divider} per axis.
            let left_margin = rect.x <= content.x + EDGE_EPS;
            let top_margin = rect.y <= content.y + EDGE_EPS;
            assert_eq!(inner.x, rect.x + if left_margin { 0.0 } else { pad });
            assert_eq!(inner.y, rect.y + if top_margin { 0.0 } else { pad });
            // Total width/height each lose exactly one pad (one divider edge per
            // axis in a 2x2), so the drawable area shrinks by pad on both axes.
            assert_eq!(inner.w, rect.w - pad, "one divider edge on the x axis");
            assert_eq!(inner.h, rect.h - pad, "one divider edge on the y axis");
        }
    }

    /// Padding larger than a small pane clamps the drawable extent at zero rather
    /// than going negative (the drawable grid is empty on that axis).
    #[test]
    fn pane_inner_rect_oversized_padding_clamps_at_zero() {
        let content = PaneRect::new(0.0, 0.0, 40.0, 40.0);
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let rects = layout_rects(&tree, content, 1.0);
        // pad far exceeds each pane's ~19px width.
        for (_, rect) in &rects {
            let inner = pane_inner_rect(*rect, content, 100.0);
            assert!(inner.w >= 0.0 && inner.h >= 0.0);
            assert_eq!(grid_dims_for_rect(inner, 8, 16).0, 0);
        }
    }

    // ----- snap_divider_to_cells: release-snap → constant outer margin -----

    /// Helper: far-pane (second-child) outer-margin remainder for a 2-pane split
    /// after a drag-release-snap to `drag_px`. Returns the far margin in pixels;
    /// the regression guard asserts it is constant across release positions and
    /// that the first child lands on an exact whole-cell boundary.
    fn far_margin_after_snap(
        axis: SplitAxis,
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
        divider_px: f32,
        drag_px: f32,
    ) -> (f32, f32) {
        // Build a fresh 2-pane split, drag it to `drag_px`, then snap on release.
        let mut tree = PaneNode::Split {
            axis,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Leaf(tok(1))),
        };
        let (dx, dy) = match axis {
            SplitAxis::Columns => (content.x + drag_px, content.y),
            SplitAxis::Rows => (content.x, content.y + drag_px),
        };
        drag_divider_to(&mut tree, content, divider_px, 0, dx, dy);
        snap_divider_to_cells(&mut tree, content, divider_px, 0, cell_w, cell_h);
        let rects = layout_rects(&tree, content, divider_px);
        let (first, second) = (rects[0].1, rects[1].1);
        // First child's grid extent along the axis (must be a whole-cell
        // multiple → zero remainder → flush both sides).
        let (first_extent, first_remainder, far_margin) = match axis {
            SplitAxis::Columns => {
                let (cols, _) = grid_dims_for_rect(first, cell_w, cell_h);
                let (cols2, _) = grid_dims_for_rect(second, cell_w, cell_h);
                (
                    first.w,
                    first.w - cols as f32 * cell_w as f32,
                    second.w - cols2 as f32 * cell_w as f32,
                )
            }
            SplitAxis::Rows => {
                let (_, rows) = grid_dims_for_rect(first, cell_w, cell_h);
                let (_, rows2) = grid_dims_for_rect(second, cell_w, cell_h);
                (
                    first.h,
                    first.h - rows as f32 * cell_h as f32,
                    second.h - rows2 as f32 * cell_h as f32,
                )
            }
        };
        let _ = first_extent;
        // The first child must be flush (no sub-cell remainder) post-snap.
        assert_eq!(
            first_remainder, 0.0,
            "release-snap must land the divider on a whole-cell boundary"
        );
        (first_remainder, far_margin)
    }

    /// THE release-snap regression guard (column split): dragging the divider to
    /// two different non-cell-aligned positions and releasing must leave the
    /// SAME far-margin remainder — the outer margin is constant at every rest
    /// position. Revert the snap → margins differ across positions → this fails.
    #[test]
    fn column_release_snap_gives_constant_outer_margin() {
        let content = PaneRect::new(0.0, 0.0, 101.0, 64.0);
        let (_, m_a) = far_margin_after_snap(SplitAxis::Columns, content, 8, 16, 1.0, 37.0);
        let (_, m_b) = far_margin_after_snap(SplitAxis::Columns, content, 8, 16, 1.0, 61.0);
        assert_eq!(
            m_a, m_b,
            "outer margin must be identical across two release positions"
        );
        // usable = 100; 100 mod 8 = 4 → the constant far margin.
        assert_eq!(m_a, 4.0);
    }

    /// Same guard on a row split (the axis that looked worse: ~2:1 tall cells).
    #[test]
    fn row_release_snap_gives_constant_outer_margin() {
        let content = PaneRect::new(0.0, 0.0, 80.0, 101.0);
        let (_, m_a) = far_margin_after_snap(SplitAxis::Rows, content, 8, 16, 1.0, 33.0);
        let (_, m_b) = far_margin_after_snap(SplitAxis::Rows, content, 8, 16, 1.0, 71.0);
        assert_eq!(
            m_a, m_b,
            "outer margin must be identical across two release positions"
        );
        // usable = 100; 100 mod 16 = 4 → the constant far margin.
        assert_eq!(m_a, 4.0);
    }

    /// Snap never collapses a pane below one cell even when dragged to the edge.
    #[test]
    fn release_snap_keeps_both_panes_at_least_one_cell() {
        let content = PaneRect::new(0.0, 0.0, 101.0, 64.0);
        let mut tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Leaf(tok(1))),
        };
        // Drag hard to the left edge, then snap.
        drag_divider_to(&mut tree, content, 1.0, 0, content.x, content.y);
        snap_divider_to_cells(&mut tree, content, 1.0, 0, 8, 16);
        let rects = layout_rects(&tree, content, 1.0);
        let (c0, _) = grid_dims_for_rect(rects[0].1, 8, 16);
        let (c1, _) = grid_dims_for_rect(rects[1].1, 8, 16);
        assert!(c0 >= 1 && c1 >= 1, "both panes must keep at least one cell");
    }

    // ----- focus_move: 2x2 grid -----

    /// Build a 2x2 pane grid: columns split, each column split into rows.
    /// Tokens: 0=top-left, 1=bottom-left, 2=top-right, 3=bottom-right.
    fn grid_2x2() -> PaneNode {
        PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(tok(0))),
                second: Box::new(PaneNode::Leaf(tok(1))),
            }),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(tok(2))),
                second: Box::new(PaneNode::Leaf(tok(3))),
            }),
        }
    }

    #[test]
    fn focus_move_directions_in_2x2() {
        let rects = layout_rects(&grid_2x2(), content(), 1.0);
        // From top-left (0): right→top-right(2), down→bottom-left(1).
        assert_eq!(focus_move(&rects, tok(0), FocusDir::Right), Some(tok(2)));
        assert_eq!(focus_move(&rects, tok(0), FocusDir::Down), Some(tok(1)));
        // From bottom-right (3): left→bottom-left(1), up→top-right(2).
        assert_eq!(focus_move(&rects, tok(3), FocusDir::Left), Some(tok(1)));
        assert_eq!(focus_move(&rects, tok(3), FocusDir::Up), Some(tok(2)));
    }

    #[test]
    fn focus_move_returns_none_at_edges() {
        let rects = layout_rects(&grid_2x2(), content(), 1.0);
        // Top-left has no neighbor to the left or up.
        assert_eq!(focus_move(&rects, tok(0), FocusDir::Left), None);
        assert_eq!(focus_move(&rects, tok(0), FocusDir::Up), None);
        // Bottom-right has no neighbor to the right or down.
        assert_eq!(focus_move(&rects, tok(3), FocusDir::Right), None);
        assert_eq!(focus_move(&rects, tok(3), FocusDir::Down), None);
    }

    #[test]
    fn focus_move_single_pane_has_no_neighbors() {
        let rects = layout_rects(&PaneNode::leaf(tok(0)), content(), 1.0);
        for dir in [
            FocusDir::Left,
            FocusDir::Right,
            FocusDir::Up,
            FocusDir::Down,
        ] {
            assert_eq!(focus_move(&rects, tok(0), dir), None);
        }
    }

    #[test]
    fn focus_move_unknown_focused_is_none() {
        let rects = layout_rects(&grid_2x2(), content(), 1.0);
        assert_eq!(focus_move(&rects, tok(99), FocusDir::Right), None);
    }

    #[test]
    fn focus_move_picks_a_right_pane_across_a_split_column() {
        // Left pane (0) full height; right column split into two rows (1 top, 2
        // bottom). Moving Right from 0 must land on one of the right panes, not
        // None (both are the same distance; either is an acceptable neighbor).
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(tok(1))),
                second: Box::new(PaneNode::Leaf(tok(2))),
            }),
        };
        let rects = layout_rects(&tree, content(), 1.0);
        let got = focus_move(&rects, tok(0), FocusDir::Right);
        assert!(got == Some(tok(1)) || got == Some(tok(2)));
    }

    // ----- tree transforms: split / close / equalize -----

    #[test]
    fn split_leaf_replaces_target_only() {
        let tree = PaneNode::leaf(tok(0))
            .split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1))
            .split_leaf(tok(1), SplitAxis::Rows, 0.5, tok(2));
        assert_eq!(tree.leaves(), vec![tok(0), tok(1), tok(2)]);
        assert_eq!(tree.pane_count(), 3);
    }

    #[test]
    fn split_leaf_absent_target_is_noop() {
        let tree = PaneNode::leaf(tok(0));
        let same = tree
            .clone()
            .split_leaf(tok(42), SplitAxis::Columns, 0.5, tok(1));
        assert_eq!(same, tree);
    }

    #[test]
    fn close_leaf_collapses_parent_into_sibling() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let after = tree.close_leaf(tok(1)).expect("tab still has a pane");
        assert_eq!(after, PaneNode::Leaf(tok(0)));
        assert!(after.is_single_pane());
    }

    #[test]
    fn close_leaf_promotes_nested_sibling() {
        // 3-pane: 0 | (1 / 2). Closing 0 promotes the right column to the root.
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(tok(1))),
                second: Box::new(PaneNode::Leaf(tok(2))),
            }),
        };
        let after = tree.close_leaf(tok(0)).expect("panes remain");
        assert_eq!(after.leaves(), vec![tok(1), tok(2)]);
        assert_eq!(after.pane_count(), 2);
    }

    #[test]
    fn close_last_pane_returns_none() {
        let tree = PaneNode::leaf(tok(0));
        assert_eq!(tree.close_leaf(tok(0)), None);
    }

    #[test]
    fn close_absent_leaf_is_noop() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let after = tree.clone().close_leaf(tok(42));
        assert_eq!(after, Some(tree));
    }

    #[test]
    fn next_leaf_after_wraps() {
        let tree = PaneNode::leaf(tok(0))
            .split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1))
            .split_leaf(tok(1), SplitAxis::Rows, 0.5, tok(2));
        assert_eq!(tree.next_leaf_after(tok(0)), Some(tok(1)));
        assert_eq!(tree.next_leaf_after(tok(2)), Some(tok(0))); // wrap
    }

    #[test]
    fn equalized_resets_all_ratios() {
        let tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.2,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.9,
                first: Box::new(PaneNode::Leaf(tok(1))),
                second: Box::new(PaneNode::Leaf(tok(2))),
            }),
        };
        let eq = tree.equalized();
        fn all_even(node: &PaneNode) -> bool {
            match node {
                PaneNode::Leaf(_) => true,
                PaneNode::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    (*ratio - EVEN_RATIO).abs() < f32::EPSILON
                        && all_even(first)
                        && all_even(second)
                }
            }
        }
        assert!(all_even(&eq));
    }

    // ----- divider drag / pane hit-test (1c-3b) -----

    #[test]
    fn divider_at_point_hits_the_grab_band_only() {
        // Columns split at 0.5 → divider strip at x=399, width 1px.
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let c = content();
        // Dead center of the strip hits.
        assert_eq!(divider_at_point(&tree, c, 1.0, 399.5, 300.0, 4.0), Some(0));
        // Within the grab band (4px) hits.
        assert_eq!(divider_at_point(&tree, c, 1.0, 396.0, 10.0, 4.0), Some(0));
        // Far from the divider misses.
        assert_eq!(divider_at_point(&tree, c, 1.0, 100.0, 300.0, 4.0), None);
        // A single pane has no dividers to hit.
        let single = PaneNode::leaf(tok(0));
        assert_eq!(divider_at_point(&single, c, 1.0, 399.5, 300.0, 4.0), None);
    }

    #[test]
    fn drag_divider_to_sets_the_clamped_ratio() {
        let mut tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let c = content(); // 800 wide
        // Drag the column divider to x=200 → ratio ≈ 200/799.
        let new = drag_divider_to(&mut tree, c, 1.0, 0, 200.0, 300.0).expect("split exists");
        assert!((new - 200.0 / 799.0).abs() < 1e-4);
        if let PaneNode::Split { ratio, .. } = &tree {
            assert!((ratio - new).abs() < f32::EPSILON);
        } else {
            panic!("still a split");
        }
        // Dragging beyond the legal band clamps (x near 0 → MIN_RATIO).
        let clamped = drag_divider_to(&mut tree, c, 1.0, 0, 1.0, 300.0).expect("split exists");
        assert_eq!(clamped, MIN_RATIO);
        // Unknown target index leaves the tree unchanged and returns None.
        assert_eq!(drag_divider_to(&mut tree, c, 1.0, 9, 400.0, 300.0), None);
    }

    #[test]
    fn drag_divider_targets_the_right_nested_split() {
        // Columns(0) over Rows(1,2): divider 0 = column, divider 1 = inner row.
        let mut tree = PaneNode::Split {
            axis: SplitAxis::Columns,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(tok(0))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Rows,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(tok(1))),
                second: Box::new(PaneNode::Leaf(tok(2))),
            }),
        };
        let c = content(); // 800x600
        // Drag the inner row divider (index 1) up to y=150 → ratio ≈ 150/599.
        let new = drag_divider_to(&mut tree, c, 1.0, 1, 600.0, 150.0).expect("inner split");
        assert!((new - 150.0 / 599.0).abs() < 1e-4);
        // The outer column ratio is untouched.
        if let PaneNode::Split { ratio, .. } = &tree {
            assert!((ratio - 0.5).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn pane_at_point_resolves_the_containing_pane() {
        let tree = PaneNode::leaf(tok(0)).split_leaf(tok(0), SplitAxis::Columns, 0.5, tok(1));
        let rects = layout_rects(&tree, content(), 1.0);
        // Left half → pane 0; right half → pane 1.
        assert_eq!(pane_at_point(&rects, 100.0, 300.0), Some(tok(0)));
        assert_eq!(pane_at_point(&rects, 600.0, 300.0), Some(tok(1)));
        // A point in the 1px divider gap (x=399) belongs to no pane.
        assert_eq!(pane_at_point(&rects, 399.0, 300.0), None);
        // Outside the content entirely.
        assert_eq!(pane_at_point(&rects, 5000.0, 300.0), None);
    }

    // ----- PANE-SUBCELL-CLIP: pane_glide_origin_and_clip (frac accounting) -----

    #[test]
    fn glide_at_rest_is_inert() {
        // frac 0 ⇒ base origin unchanged and an inert clip, so a split at rest
        // renders byte-identically.
        let (origin, clip) = pane_glide_origin_and_clip([12.0, 40.0], 0.0, 24, 16.0);
        assert_eq!(origin, [12.0, 40.0]);
        assert_eq!(clip, crate::grid::VClip::NONE);
        assert!(!clip.active());
    }

    #[test]
    fn glide_shifts_origin_down_and_bands_the_grid_rect() {
        // frac 5px into a 24-row pane of 16px cells at base y=40: origin shifts
        // down by 5, and the clip band is exactly the at-rest grid rect
        // [40, 40 + 24*16] = [40, 424].
        let (origin, clip) = pane_glide_origin_and_clip([12.0, 40.0], 5.0, 24, 16.0);
        assert_eq!(origin, [12.0, 45.0], "x unchanged, y shifted down by frac");
        assert!(clip.active());
        assert!(
            (clip.top_y - 40.0).abs() < 1e-4,
            "band top at the at-rest grid top"
        );
        assert!(
            (clip.bottom_y - (40.0 + 24.0 * 16.0)).abs() < 1e-4,
            "band bottom at the at-rest grid bottom, not the shifted bottom"
        );
    }

    #[test]
    fn glide_band_bottom_is_below_the_shifted_content_bottom_by_exactly_frac() {
        // The shifted content bottom overhangs the band bottom by exactly frac —
        // that overhang is the partial row `clip_quads_vertical` crops so it can
        // never reach the divider.
        let rows = 10usize;
        let cell_h = 20.0_f32;
        let frac = 7.0_f32;
        let base_y = 100.0_f32;
        let (origin, clip) = pane_glide_origin_and_clip([0.0, base_y], frac, rows, cell_h);
        let shifted_content_bottom = origin[1] + rows as f32 * cell_h;
        assert!(
            (shifted_content_bottom - clip.bottom_y - frac).abs() < 1e-4,
            "content overhangs the band by exactly the sub-cell remainder"
        );
    }
}
