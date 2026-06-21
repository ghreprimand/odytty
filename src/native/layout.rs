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
//! a window. The native layer (Phase 1b) builds `TabSet`/`Tab` around these
//! types and calls these functions; until then this module has no call sites
//! and changes no behaviour.

// Phase-1a scaffold: these items are consumed by the Phase-1b arena/TabSet
// refactor. Suppress dead_code so the warning baseline is unchanged while the
// pure core lands first as an independently testable packet.
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
    let mut out = Vec::new();
    dividers_into(tree, content, divider_px, &mut out);
    out
}

fn dividers_into(node: &PaneNode, rect: PaneRect, divider_px: f32, out: &mut Vec<PaneRect>) {
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
        out.push(divider);
        dividers_into(first, first_rect, divider_px, out);
        dividers_into(second, second_rect, divider_px, out);
    }
}

/// Convert a pane rect to its cell grid dimensions for a given cell size. Pure
/// integer math so it can be unit-tested without `CellSize`/`Dimensions`.
/// Always returns at least `1x1` so a pane never has a zero-dimension grid.
pub(super) fn grid_dims_for_rect(rect: PaneRect, cell_w: u32, cell_h: u32) -> (usize, usize) {
    let cols = (rect.w.max(0.0) as u32) / cell_w.max(1);
    let rows = (rect.h.max(0.0) as u32) / cell_h.max(1);
    (cols.max(1) as usize, rows.max(1) as usize)
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
    fn grid_dims_floor_and_clamp_to_one() {
        assert_eq!(
            grid_dims_for_rect(PaneRect::new(0.0, 0.0, 80.0, 32.0), 8, 16),
            (10, 2)
        );
        // Sub-cell rect still yields at least 1x1.
        assert_eq!(
            grid_dims_for_rect(PaneRect::new(0.0, 0.0, 3.0, 3.0), 8, 16),
            (1, 1)
        );
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
}
