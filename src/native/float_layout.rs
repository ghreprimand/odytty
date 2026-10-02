// SPDX-License-Identifier: GPL-3.0-only
//! Stacked and floating pane arrangements: the pure geometry core.
//!
//! A tab is always backed by its split tree ([`crate::native::layout::PaneNode`]),
//! which stays the one source of truth for which panes exist and their stable
//! (tree) order. An [`Arrangement`] only changes how those panes are placed:
//!
//! - [`Arrangement::Tiled`]: the split tree decides the geometry (the default,
//!   and the only mode older layouts know).
//! - [`Arrangement::Stacked`]: every pane fills the content rectangle and the
//!   focused pane is the one shown. The others keep running, fully alive.
//! - [`Arrangement::Floating`]: every pane is a rectangle in whole cells inside
//!   the content grid, painted in z-order. Rectangles may overlap.
//!
//! Floating and stacked are in-window layouts. They are rectangles inside one
//! window's content grid, never native windows, so compositor positioning
//! limits (Wayland) do not apply and the same model runs on every platform.
//!
//! Nothing here touches sessions, the GPU, winit, or settings. The geometry is
//! a pure function of the stored cells, the content rectangle, and the cell
//! size, so a restored layout resolves to the same rectangles at the same
//! window size.

use super::layout::PaneRect;
use super::session::SessionToken;

/// Narrowest a floating pane may be, in cells. A grid narrower than this clamps
/// the minimum down to the grid itself.
pub(super) const MIN_FLOAT_COLS: usize = 8;
/// Shortest a floating pane may be, in cells.
pub(super) const MIN_FLOAT_ROWS: usize = 2;

/// A rectangle in whole cells of the content grid (origin top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CellRect {
    pub(super) col: usize,
    pub(super) row: usize,
    pub(super) cols: usize,
    pub(super) rows: usize,
}

impl CellRect {
    pub(super) fn new(col: usize, row: usize, cols: usize, rows: usize) -> Self {
        Self {
            col,
            row,
            cols,
            rows,
        }
    }

    /// This rectangle forced inside a `grid_cols` x `grid_rows` grid: at least
    /// [`MIN_FLOAT_COLS`] x [`MIN_FLOAT_ROWS`] (or the whole grid when it is
    /// smaller), at most the grid, and moved so no edge leaves it. A pane is
    /// never deleted by clamping, only moved or resized.
    pub(super) fn clamped(self, grid_cols: usize, grid_rows: usize) -> Self {
        let grid_cols = grid_cols.max(1);
        let grid_rows = grid_rows.max(1);
        let min_cols = MIN_FLOAT_COLS.min(grid_cols);
        let min_rows = MIN_FLOAT_ROWS.min(grid_rows);
        let cols = self.cols.clamp(min_cols, grid_cols);
        let rows = self.rows.clamp(min_rows, grid_rows);
        Self {
            col: self.col.min(grid_cols - cols),
            row: self.row.min(grid_rows - rows),
            cols,
            rows,
        }
    }

    /// The pixel rectangle of this cell rectangle over `content`.
    pub(super) fn to_px(self, content: PaneRect, cell_w: u32, cell_h: u32) -> PaneRect {
        let cw = cell_w as f32;
        let ch = cell_h as f32;
        PaneRect::new(
            content.x + self.col as f32 * cw,
            content.y + self.row as f32 * ch,
            self.cols as f32 * cw,
            self.rows as f32 * ch,
        )
    }
}

/// The cell grid a content rectangle holds.
pub(super) fn content_grid(content: PaneRect, cell_w: u32, cell_h: u32) -> (usize, usize) {
    if cell_w == 0 || cell_h == 0 {
        return (0, 0);
    }
    (
        (content.w / cell_w as f32).floor().max(0.0) as usize,
        (content.h / cell_h as f32).floor().max(0.0) as usize,
    )
}

/// The default rectangle for the `index`-th pane that has no stored rectangle:
/// three fifths of the grid, stepped down and right by one slot per pane and
/// wrapping inside the grid. A pure function of its inputs, so it is the same
/// on every launch.
pub(super) fn cascade_rect(index: usize, grid_cols: usize, grid_rows: usize) -> CellRect {
    let cols = (grid_cols * 3 / 5).max(MIN_FLOAT_COLS);
    let rows = (grid_rows * 3 / 5).max(MIN_FLOAT_ROWS);
    let base = CellRect::new(0, 0, cols, rows).clamped(grid_cols, grid_rows);
    let slack_cols = grid_cols.max(1) - base.cols;
    let slack_rows = grid_rows.max(1) - base.rows;
    CellRect {
        col: (index * 2) % (slack_cols + 1),
        row: index % (slack_rows + 1),
        ..base
    }
}

/// One floating pane: its token and its stored rectangle. `None` means the pane
/// was added without geometry (a split while floating, an older save) and takes
/// its [`cascade_rect`] slot until the user moves or resizes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FloatEntry {
    pub(super) token: SessionToken,
    pub(super) rect: Option<CellRect>,
}

/// The floating state of one tab: entries ordered back to front (the last entry
/// is painted on top).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct FloatLayout {
    pub(super) entries: Vec<FloatEntry>,
}

impl FloatLayout {
    /// A layout from explicit entries (back to front).
    pub(super) fn from_entries(entries: Vec<FloatEntry>) -> Self {
        Self { entries }
    }

    /// Convert the current tiled geometry into floating rectangles that cover
    /// the same area, snapped to whole cells and clamped. Non-overlapping when
    /// the source tiles are, and visually the same at the moment of switching.
    pub(super) fn from_tiled(
        tiles: &[(SessionToken, PaneRect)],
        content: PaneRect,
        cell_w: u32,
        cell_h: u32,
    ) -> Self {
        let (grid_cols, grid_rows) = content_grid(content, cell_w, cell_h);
        let cw = cell_w.max(1) as f32;
        let ch = cell_h.max(1) as f32;
        let entries = tiles
            .iter()
            .map(|(token, rect)| {
                let col0 = (((rect.x - content.x) / cw).round().max(0.0)) as usize;
                let col1 = (((rect.x + rect.w - content.x) / cw).round().max(0.0)) as usize;
                let row0 = (((rect.y - content.y) / ch).round().max(0.0)) as usize;
                let row1 = (((rect.y + rect.h - content.y) / ch).round().max(0.0)) as usize;
                FloatEntry {
                    token: *token,
                    rect: Some(
                        CellRect::new(
                            col0,
                            row0,
                            col1.saturating_sub(col0),
                            row1.saturating_sub(row0),
                        )
                        .clamped(grid_cols, grid_rows),
                    ),
                }
            })
            .collect();
        Self { entries }
    }

    /// The entries reconciled against the tab's actual panes (`leaves`, tree
    /// order): entries for panes that no longer exist are dropped, and a pane
    /// with no entry joins at the front with no stored rectangle. A pure view,
    /// so geometry stays correct however the pane set changed since the last
    /// explicit [`Self::normalize`].
    pub(super) fn reconciled(&self, leaves: &[SessionToken]) -> Vec<FloatEntry> {
        let mut out: Vec<FloatEntry> = self
            .entries
            .iter()
            .filter(|entry| leaves.contains(&entry.token))
            .copied()
            .collect();
        for token in leaves {
            if !out.iter().any(|entry| entry.token == *token) {
                out.push(FloatEntry {
                    token: *token,
                    rect: None,
                });
            }
        }
        out
    }

    /// Store the reconciled entries.
    pub(super) fn normalize(&mut self, leaves: &[SessionToken]) {
        self.entries = self.reconciled(leaves);
    }

    /// Paint order, back to front: the reconciled z-order with the focused pane
    /// last, so the pane that receives keys is never covered. Test-only: the
    /// renderer derives its own order from the resolved rectangles.
    #[cfg(test)]
    pub(super) fn paint_order(
        &self,
        leaves: &[SessionToken],
        focused: SessionToken,
    ) -> Vec<FloatEntry> {
        let mut entries = self.reconciled(leaves);
        if let Some(at) = entries.iter().position(|entry| entry.token == focused) {
            let entry = entries.remove(at);
            entries.push(entry);
        }
        entries
    }

    /// Every pane's resolved rectangle in paint order (back to front), clamped
    /// to the grid.
    pub(super) fn resolved(
        &self,
        leaves: &[SessionToken],
        focused: SessionToken,
        grid_cols: usize,
        grid_rows: usize,
    ) -> Vec<(SessionToken, CellRect)> {
        let reconciled = self.reconciled(leaves);
        let mut out: Vec<(SessionToken, CellRect)> = reconciled
            .iter()
            .map(|entry| {
                // The default slot follows the pane's stable tree position, not
                // its z-order, so raising another pane never moves it.
                let slot = leaves
                    .iter()
                    .position(|token| *token == entry.token)
                    .unwrap_or(0);
                let rect = entry
                    .rect
                    .unwrap_or_else(|| cascade_rect(slot, grid_cols, grid_rows))
                    .clamped(grid_cols, grid_rows);
                (entry.token, rect)
            })
            .collect();
        if let Some(at) = out.iter().position(|(token, _)| *token == focused) {
            let entry = out.remove(at);
            out.push(entry);
        }
        out
    }

    /// Raise `token` to the front of the z-order. False when it is not a pane.
    pub(super) fn raise(&mut self, token: SessionToken) -> bool {
        let Some(at) = self.entries.iter().position(|entry| entry.token == token) else {
            return false;
        };
        if at + 1 == self.entries.len() {
            return true;
        }
        let entry = self.entries.remove(at);
        self.entries.push(entry);
        true
    }

    /// Store `rect` for `token`, materializing the entry first when needed.
    fn set_rect(&mut self, token: SessionToken, rect: CellRect) {
        match self.entries.iter_mut().find(|entry| entry.token == token) {
            Some(entry) => entry.rect = Some(rect),
            None => self.entries.push(FloatEntry {
                token,
                rect: Some(rect),
            }),
        }
    }

    /// The rectangle `token` currently resolves to.
    pub(super) fn rect_of(
        &self,
        leaves: &[SessionToken],
        focused: SessionToken,
        token: SessionToken,
        grid_cols: usize,
        grid_rows: usize,
    ) -> Option<CellRect> {
        self.resolved(leaves, focused, grid_cols, grid_rows)
            .into_iter()
            .find(|(candidate, _)| *candidate == token)
            .map(|(_, rect)| rect)
    }

    /// Move `token` by whole cells, clamped inside the grid. Returns whether
    /// the rectangle changed.
    pub(super) fn move_by(
        &mut self,
        leaves: &[SessionToken],
        focused: SessionToken,
        token: SessionToken,
        d_col: isize,
        d_row: isize,
        grid: (usize, usize),
    ) -> bool {
        let (grid_cols, grid_rows) = grid;
        let Some(current) = self.rect_of(leaves, focused, token, grid_cols, grid_rows) else {
            return false;
        };
        let max_col = grid_cols.max(1).saturating_sub(current.cols);
        let max_row = grid_rows.max(1).saturating_sub(current.rows);
        let next = CellRect {
            col: current.col.saturating_add_signed(d_col).min(max_col),
            row: current.row.saturating_add_signed(d_row).min(max_row),
            ..current
        };
        self.normalize(leaves);
        self.set_rect(token, next);
        next != current
    }

    /// Resize `token` by whole cells (the origin stays put unless the grid edge
    /// forces it in), subject to the minimum size and the grid. Returns whether
    /// the rectangle changed.
    pub(super) fn resize_by(
        &mut self,
        leaves: &[SessionToken],
        focused: SessionToken,
        token: SessionToken,
        d_cols: isize,
        d_rows: isize,
        grid: (usize, usize),
    ) -> bool {
        let (grid_cols, grid_rows) = grid;
        let Some(current) = self.rect_of(leaves, focused, token, grid_cols, grid_rows) else {
            return false;
        };
        let next = CellRect {
            cols: current.cols.saturating_add_signed(d_cols),
            rows: current.rows.saturating_add_signed(d_rows),
            ..current
        }
        .clamped(grid_cols, grid_rows);
        self.normalize(leaves);
        self.set_rect(token, next);
        next != current
    }
}

/// How a tab places its panes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum Arrangement {
    /// The split tree decides the geometry.
    #[default]
    Tiled,
    /// The focused pane fills the content rectangle; the rest stay alive behind.
    Stacked,
    /// Rectangles in whole cells, painted in z-order.
    Floating(FloatLayout),
}

impl Arrangement {
    pub(super) fn is_tiled(&self) -> bool {
        matches!(self, Arrangement::Tiled)
    }

    pub(super) fn is_stacked(&self) -> bool {
        matches!(self, Arrangement::Stacked)
    }

    pub(super) fn is_floating(&self) -> bool {
        matches!(self, Arrangement::Floating(_))
    }
}

/// The pane pixel rectangles a floating tab paints, back to front, plus the
/// rectangles of the panes drawn above each one (its occluders).
pub(super) fn floating_pixel_rects(
    layout: &FloatLayout,
    leaves: &[SessionToken],
    focused: SessionToken,
    content: PaneRect,
    cell_w: u32,
    cell_h: u32,
) -> Vec<(SessionToken, PaneRect)> {
    let (grid_cols, grid_rows) = content_grid(content, cell_w, cell_h);
    layout
        .resolved(leaves, focused, grid_cols, grid_rows)
        .into_iter()
        .map(|(token, rect)| (token, rect.to_px(content, cell_w, cell_h)))
        .collect()
}

/// A floating pane's drawable rectangle: the cell rectangle inset by `pad` on
/// every side (never below zero).
pub(super) fn floating_inner_rect(rect: PaneRect, pad: f32) -> PaneRect {
    if pad <= 0.0 {
        return rect;
    }
    PaneRect::new(
        rect.x + pad,
        rect.y + pad,
        (rect.w - 2.0 * pad).max(0.0),
        (rect.h - 2.0 * pad).max(0.0),
    )
}

/// `[x0, y0, x1, y1]` of a pane rectangle.
pub(super) fn rect_edges(rect: PaneRect) -> [f32; 4] {
    [rect.x, rect.y, rect.x + rect.w, rect.y + rect.h]
}

/// The pieces of `rect` (as `[x0, y0, x1, y1]`) that remain after cutting out
/// `hole`: up to four non-overlapping strips, none when `hole` covers `rect`.
pub(super) fn subtract_edges(rect: [f32; 4], hole: [f32; 4]) -> Vec<[f32; 4]> {
    let ix0 = rect[0].max(hole[0]);
    let iy0 = rect[1].max(hole[1]);
    let ix1 = rect[2].min(hole[2]);
    let iy1 = rect[3].min(hole[3]);
    if ix0 >= ix1 || iy0 >= iy1 {
        return vec![rect];
    }
    let mut out = Vec::with_capacity(4);
    if rect[1] < iy0 {
        out.push([rect[0], rect[1], rect[2], iy0]);
    }
    if iy1 < rect[3] {
        out.push([rect[0], iy1, rect[2], rect[3]]);
    }
    if rect[0] < ix0 {
        out.push([rect[0], iy0, ix0, iy1]);
    }
    if ix1 < rect[2] {
        out.push([ix1, iy0, rect[2], iy1]);
    }
    out
}

/// Cut every hole out of `rects`.
pub(super) fn subtract_all(rects: Vec<[f32; 4]>, holes: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let mut current = rects;
    for hole in holes {
        current = current
            .into_iter()
            .flat_map(|piece| subtract_edges(piece, *hole))
            .collect();
    }
    current
}

/// The border of every floating pane as `(pane index, [left, top, right,
/// bottom])` strips, `thickness` px wide along the inside of each outer
/// rectangle. `rects` is in paint order; the parts of a pane's border that a
/// pane painted above it covers are cut away, so a buried pane never draws a
/// line across the pane in front of it.
pub(super) fn border_strips(
    rects: &[(SessionToken, PaneRect)],
    thickness: f32,
) -> Vec<(usize, [f32; 4])> {
    let mut out = Vec::new();
    for (index, (_, rect)) in rects.iter().enumerate() {
        let t = thickness.min(rect.w / 2.0).min(rect.h / 2.0).max(0.0);
        let [x0, y0, x1, y1] = rect_edges(*rect);
        let strips = vec![
            [x0, y0, x1, y0 + t],
            [x0, y1 - t, x1, y1],
            [x0, y0 + t, x0 + t, y1 - t],
            [x1 - t, y0 + t, x1, y1 - t],
        ];
        let holes: Vec<[f32; 4]> = rects[index + 1..]
            .iter()
            .map(|(_, above)| rect_edges(*above))
            .collect();
        for strip in subtract_all(strips, &holes) {
            if strip[2] > strip[0] && strip[3] > strip[1] {
                out.push((index, strip));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(n: u64) -> SessionToken {
        SessionToken(n)
    }

    fn leaves(n: u64) -> Vec<SessionToken> {
        (0..n).map(tok).collect()
    }

    #[test]
    fn default_arrangement_is_tiled() {
        assert!(Arrangement::default().is_tiled());
    }

    #[test]
    fn clamp_enforces_the_minimum_and_the_grid() {
        let grid = (80, 24);
        let tiny = CellRect::new(5, 5, 1, 1).clamped(grid.0, grid.1);
        assert_eq!((tiny.cols, tiny.rows), (MIN_FLOAT_COLS, MIN_FLOAT_ROWS));
        let huge = CellRect::new(70, 20, 500, 500).clamped(grid.0, grid.1);
        assert_eq!((huge.col, huge.row, huge.cols, huge.rows), (0, 0, 80, 24));
        let edge = CellRect::new(78, 23, 20, 6).clamped(grid.0, grid.1);
        assert_eq!(
            (edge.col + edge.cols, edge.row + edge.rows),
            (80, 24),
            "a rectangle past the edge is moved back inside, not cropped"
        );
    }

    #[test]
    fn clamp_in_a_grid_smaller_than_the_minimum_uses_the_grid() {
        let rect = CellRect::new(0, 0, 40, 10).clamped(5, 1);
        assert_eq!((rect.cols, rect.rows), (5, 1));
        let rect = CellRect::new(3, 3, 1, 1).clamped(0, 0);
        assert_eq!((rect.col, rect.row, rect.cols, rect.rows), (0, 0, 1, 1));
    }

    #[test]
    fn cascade_is_deterministic_and_inside_the_grid() {
        for index in 0..40 {
            let a = cascade_rect(index, 100, 30);
            let b = cascade_rect(index, 100, 30);
            assert_eq!(a, b);
            assert_eq!(a, a.clamped(100, 30));
        }
        assert_ne!(cascade_rect(0, 100, 30), cascade_rect(1, 100, 30));
    }

    #[test]
    fn overlap_is_allowed() {
        let mut layout = FloatLayout::default();
        let l = leaves(2);
        layout.normalize(&l);
        let rects = layout.resolved(&l, tok(1), 100, 30);
        let a = rects[0].1;
        let b = rects[1].1;
        let overlap_cols = a.col + a.cols > b.col && b.col + b.cols > a.col;
        let overlap_rows = a.row + a.rows > b.row && b.row + b.rows > a.row;
        assert!(overlap_cols && overlap_rows, "cascaded panes overlap");
    }

    #[test]
    fn the_focused_pane_paints_last_and_raising_reorders() {
        let l = leaves(3);
        let mut layout = FloatLayout::default();
        layout.normalize(&l);
        let order: Vec<_> = layout
            .paint_order(&l, tok(0))
            .iter()
            .map(|e| e.token)
            .collect();
        assert_eq!(order, vec![tok(1), tok(2), tok(0)]);
        assert!(layout.raise(tok(0)));
        let stored: Vec<_> = layout.entries.iter().map(|e| e.token).collect();
        assert_eq!(stored, vec![tok(1), tok(2), tok(0)]);
        assert!(!layout.raise(tok(9)));
    }

    #[test]
    fn an_unplaced_pane_keeps_its_default_slot_when_another_pane_is_raised() {
        let l = leaves(3);
        let mut layout = FloatLayout::default();
        layout.normalize(&l);
        let slot_of = |layout: &FloatLayout, focused: u64, token: u64| {
            layout
                .resolved(&l, tok(focused), 100, 30)
                .into_iter()
                .find(|(t, _)| *t == tok(token))
                .map(|(_, r)| r)
                .expect("pane")
        };
        let before = slot_of(&layout, 2, 1);
        assert!(layout.raise(tok(0)));
        assert!(layout.raise(tok(2)));
        assert_eq!(slot_of(&layout, 0, 1), before, "z-order never moves a pane");
    }

    #[test]
    fn reconcile_drops_closed_panes_and_adds_new_ones_at_the_front() {
        let layout = FloatLayout::from_entries(vec![
            FloatEntry {
                token: tok(0),
                rect: Some(CellRect::new(0, 0, 20, 5)),
            },
            FloatEntry {
                token: tok(1),
                rect: None,
            },
        ]);
        let reconciled = layout.reconciled(&[tok(0), tok(2)]);
        let tokens: Vec<_> = reconciled.iter().map(|e| e.token).collect();
        assert_eq!(tokens, vec![tok(0), tok(2)]);
        assert_eq!(reconciled[1].rect, None);
    }

    #[test]
    fn move_and_resize_respect_the_grid_and_the_minimum() {
        let l = leaves(2);
        let mut layout = FloatLayout::default();
        layout.normalize(&l);
        let grid = (80, 24);
        // Many moves left and up pin at the origin.
        for _ in 0..200 {
            layout.move_by(&l, tok(0), tok(0), -1, -1, grid);
        }
        let at = layout.rect_of(&l, tok(0), tok(0), grid.0, grid.1).unwrap();
        assert_eq!((at.col, at.row), (0, 0));
        // Shrinking stops at the minimum.
        for _ in 0..200 {
            layout.resize_by(&l, tok(0), tok(0), -1, -1, grid);
        }
        let at = layout.rect_of(&l, tok(0), tok(0), grid.0, grid.1).unwrap();
        assert_eq!((at.cols, at.rows), (MIN_FLOAT_COLS, MIN_FLOAT_ROWS));
        // Growing stops at the grid.
        for _ in 0..200 {
            layout.resize_by(&l, tok(0), tok(0), 1, 1, grid);
        }
        let at = layout.rect_of(&l, tok(0), tok(0), grid.0, grid.1).unwrap();
        assert_eq!((at.cols, at.rows), (80, 24));
        // The other pane's rectangle did not change.
        assert!(!layout.move_by(&l, tok(0), tok(9), 1, 0, grid));
    }

    #[test]
    fn a_smaller_window_clamps_without_deleting_a_pane() {
        let l = leaves(2);
        let layout = FloatLayout::from_entries(vec![
            FloatEntry {
                token: tok(0),
                rect: Some(CellRect::new(60, 18, 20, 6)),
            },
            FloatEntry {
                token: tok(1),
                rect: Some(CellRect::new(0, 0, 40, 10)),
            },
        ]);
        let resolved = layout.resolved(&l, tok(1), 30, 8);
        assert_eq!(resolved.len(), 2);
        for (_, rect) in resolved {
            assert!(rect.col + rect.cols <= 30 && rect.row + rect.rows <= 8);
        }
    }

    #[test]
    fn from_tiled_snaps_to_cells_and_keeps_every_pane() {
        let content = PaneRect::new(10.0, 20.0, 801.0, 400.0);
        let tiles = vec![
            (tok(0), PaneRect::new(10.0, 20.0, 400.0, 400.0)),
            (tok(1), PaneRect::new(411.0, 20.0, 400.0, 400.0)),
        ];
        let layout = FloatLayout::from_tiled(&tiles, content, 8, 16);
        assert_eq!(layout.entries.len(), 2);
        let first = layout.entries[0].rect.unwrap();
        let second = layout.entries[1].rect.unwrap();
        assert_eq!(first.col, 0);
        assert!(first.col + first.cols <= second.col + 1, "tiles stay apart");
        assert_eq!(first.rows, 25);
    }

    #[test]
    fn pixel_rects_are_a_pure_function_of_cells_and_content() {
        let content = PaneRect::new(10.0, 20.0, 800.0, 400.0);
        let rect = CellRect::new(2, 3, 10, 4);
        let px = rect.to_px(content, 8, 16);
        assert_eq!((px.x, px.y, px.w, px.h), (26.0, 68.0, 80.0, 64.0));
        assert_eq!(content_grid(content, 8, 16), (100, 25));
        assert_eq!(content_grid(content, 0, 16), (0, 0));
    }

    #[test]
    fn subtraction_leaves_only_the_uncovered_strips() {
        let rect = [0.0, 0.0, 10.0, 10.0];
        assert!(subtract_edges(rect, [-1.0, -1.0, 11.0, 11.0]).is_empty());
        assert_eq!(subtract_edges(rect, [20.0, 20.0, 30.0, 30.0]), vec![rect]);
        let pieces = subtract_edges(rect, [4.0, 4.0, 6.0, 6.0]);
        let area: f32 = pieces.iter().map(|p| (p[2] - p[0]) * (p[3] - p[1])).sum();
        assert_eq!(area, 100.0 - 4.0);
        let left = subtract_all(vec![rect], &[[0.0, 0.0, 5.0, 10.0], [5.0, 0.0, 10.0, 5.0]]);
        assert_eq!(left, vec![[5.0, 5.0, 10.0, 10.0]]);
    }

    #[test]
    fn borders_of_a_buried_pane_are_cut_where_a_pane_covers_them() {
        let rects = vec![
            (tok(0), PaneRect::new(0.0, 0.0, 100.0, 100.0)),
            (tok(1), PaneRect::new(50.0, 0.0, 100.0, 100.0)),
        ];
        let strips = border_strips(&rects, 1.0);
        let front: Vec<_> = strips.iter().filter(|(i, _)| *i == 1).collect();
        assert_eq!(front.len(), 4, "the front pane keeps a full frame");
        for (_, strip) in strips.iter().filter(|(i, _)| *i == 0) {
            assert!(
                strip[2] <= 50.0,
                "no line of the buried pane enters the pane in front: {strip:?}"
            );
        }
        // The buried pane still has its left edge and the visible halves of the
        // top and bottom edges.
        assert!(
            strips
                .iter()
                .any(|(i, s)| *i == 0 && s[0] == 0.0 && s[2] == 1.0)
        );
        assert!(
            strips
                .iter()
                .any(|(i, s)| *i == 0 && s[1] == 0.0 && s[2] == 50.0)
        );
    }

    #[test]
    fn inner_rect_insets_every_side_and_never_goes_negative() {
        let rect = PaneRect::new(0.0, 0.0, 100.0, 40.0);
        assert_eq!(floating_inner_rect(rect, 0.0), rect);
        let inner = floating_inner_rect(rect, 4.0);
        assert_eq!((inner.x, inner.y, inner.w, inner.h), (4.0, 4.0, 92.0, 32.0));
        assert_eq!(floating_inner_rect(rect, 60.0).w, 0.0);
    }
}
