// SPDX-License-Identifier: GPL-3.0-only
//! Vertical tab rail widget (F4-V2 R1) — presentation-only, decoupled from the
//! session model, the vertical sibling of [`super::tab_bar::TabBar`].
//!
//! Where [`TabBar`](super::tab_bar::TabBar) packs variable-width slots along a
//! single top row, the rail **stacks fixed-width slots down a fixed-width column
//! band** on the left (R1) side of the window. It reads layout from the shared
//! [`TabBarSource`] trait (same one `TabSet` already implements), returns the
//! shared [`TabHit`] enum (so pointer/action dispatch is reused verbatim), and
//! paints with the shared [`TabBarColors`] theme roles. It never touches
//! terminal state, PTY, or settings — the integration layer composites the
//! returned region + quads into the frame.
//!
//! ## Visual language (operator-ruled default — Option B "padded slots")
//! Same two orthogonal languages as the horizontal bar:
//! - **Shape** — *every* slot is a bounded, outlined box (`SLOT_ROWS`-tall, a
//!   `SLOT_GAP` band between slots). Ring colors come from **TEXT-side roles**
//!   (active = `foreground` nudged toward the band, inactive = the `inactive`
//!   role), NEVER the frame-side `border` role — which is near-black in every
//!   built-in theme and would be invisible on the dark rail band (the v1.3
//!   horizontal-bar lesson, carried over). The ring is ≥2px thick (the CRT
//!   scanline shader eats a 1px ring).
//! - **State** — the active slot is filled with the `selection` role; a hovered
//!   inactive slot gets a subordinate band→`selection` blend, never as strong as
//!   the active fill. No underlines.
//!
//! The whole `rail_cols × grid_rows` region is painted with an opaque band fill
//! (every cell), so raw wallpaper never leaks through the rail even below the
//! last slot — the horizontal strip's "fill the whole row" invariant, transposed
//! to "fill the whole column band". A 1px `border`-role divider marks the
//! rail↔content seam.
//!
//! ## Connected active tab (F4-V2 follow-up — vertical transposition of v1.4)
//! The active slot reads as the front-of-stack sheet fused to the content area,
//! inactive slots as closed boxes behind it — the horizontal bar's v1.4 metaphor
//! rotated 90°:
//! - The active ring's **content-facing edge is dropped** (the *right* edge for
//!   `Left` placement, the *left* edge for `Right`); the perpendicular top/bottom
//!   edges already span the full rail width to the seam, so the open ring is
//!   three edges. Inactive rings stay fully closed four-edge boxes.
//! - The rail↔content **divider is broken** across the active slot's *row* span:
//!   [`rail_divider`] emits up to two segments — above and below the active slot
//!   — leaving a gap exactly spanning it, so the active `selection` fill flows
//!   into the body where the divider used to run. This is the vertical analog of
//!   the strip's broken `band_separator`; the same edge-flush collapse and
//!   "no visible active slot → one full-height line" fallbacks apply.
//!
//! The whole treatment is gated on [`RAIL_CONNECTED_ACTIVE`] (a one-line revert
//! to closed boxes + a full divider if the operator prefers that on the rail).
//!
//! ## R1 scope
//! `left` placement, fixed width, Option B geometry, connected-active rings.
//! Settings plumbing (`TAB_BAR_PLACEMENT` / `TAB_RAIL_WIDTH`), the `right` arm,
//! and drag-resize are later slices (R2/R3).
//!
//! ## De-duplication note
//! The tiny pure color helpers ([`blend_srgb`], [`srgb_alpha`]) are re-derived
//! here rather than imported from `tab_bar.rs`, so this widget lands fully
//! greenfield without editing `tab_bar.rs` while its v1.x visual iteration is in
//! flight (shared-worktree discipline). Consolidating both widgets' shared pure
//! helpers into a `tab_chrome.rs` module is F4V2-NF2 follow-up work, deferred so
//! it can be done in one clean pass once the horizontal bar's treatment settles.

use super::tab_bar::{TabBarColors, TabBarSource, TabHit};
use super::*;
use crate::core::Attrs;
use crate::theme::Srgb;

// ---------------------------------------------------------------------------
// Geometry constants (Option B — padded slots)
// ---------------------------------------------------------------------------

/// Rows each tab slot occupies (Option B: 2-cell-tall padded slots — label row
/// plus a breathing row).
const SLOT_ROWS: usize = 2;
/// Band-fill gap (in rows) between adjacent slots — reinforces the
/// bounded-object reading (Option B). R1.1 note: the operator asked for a
/// spacing-between-tabs knob; the decision is that `tab_rail_gap` joins
/// `TAB_RAIL_WIDTH` in the R2 settings packet (group "Tabs", Tabs & Panes
/// section). This named const is the value R2 lifts into that setting — do not
/// build the knob here.
const SLOT_GAP: usize = 1;
/// Row stride from one slot's top to the next slot's top.
const SLOT_STRIDE: usize = SLOT_ROWS + SLOT_GAP;
/// Rows the bottom `+` new-tab slot occupies (R1.1: a lightweight 1-cell
/// affordance — no ring at rest, so it never competes with a real tab slot).
const NEW_TAB_ROWS: usize = 1;
/// Top margin (in rows) before the first slot so it doesn't kiss the window edge
/// (R1.1). Matches `SLOT_GAP` so the space above the first slot reads like the
/// gaps between slots.
const RAIL_TOP_MARGIN_ROWS: usize = SLOT_GAP;
/// Horizontal inset (in columns) of each slot's ring/fill from the rail band's
/// left and right edges (R1.1), so slots read as bounded boxes with a margin
/// rather than blocky edge-to-edge bands. The active slot's content-facing edge
/// is exempt — it reaches the divider seam (connected-active). R2 lifts this as
/// part of the `tab_rail_gap` knob work.
const SLOT_INSET_COLS: usize = 1;
/// Left/right in-slot padding (columns) between a slot's ring and the label
/// text, applied INSIDE the inset box.
const RAIL_LABEL_PAD: usize = 1;

/// First content column of a slot's label (inside the inset box + the label
/// padding).
const SLOT_LABEL_START_COL: usize = SLOT_INSET_COLS + RAIL_LABEL_PAD;

/// Fixed rail band width in cells for R1 (the `[8,32]` clamp + the
/// `TAB_RAIL_WIDTH` setting arrive with R2). Wide enough for an Option-B slot's
/// wrapped label plus the `×` cell.
pub(super) const DEFAULT_RAIL_COLS: usize = 16;

// ---------------------------------------------------------------------------
// Visual constants (mirrors of the horizontal bar's, kept in lockstep)
// ---------------------------------------------------------------------------

/// How far the rail-band fill is blended from `background` toward `inactive`
/// (matches the horizontal bar's `BAND_BLEND`).
const BAND_BLEND: f32 = 0.16;
/// How far the ACTIVE-slot ring is blended from `foreground` toward the band —
/// near-full text brightness, but a TEXT-side role (matches the bar).
const ACTIVE_OUTLINE_BLEND: f32 = 0.15;
/// How far a hovered inactive slot's fill is blended from the band toward
/// `selection` — subordinate to the active fill (matches the bar).
const HOVER_FILL_BLEND: f32 = 0.45;
/// Thickness (physical px) of the rail↔content divider line.
const DIVIDER_PX: f32 = 1.0;
/// Whether the active slot reads as *connected* to the content area — the
/// vertical transposition of the horizontal bar's F4 v1.4 treatment: the active
/// slot's content-facing edge is dropped and the rail↔content divider is broken
/// across its row span, so the active `selection` fill flows into the body.
/// `false` reverts to closed-box rings + a full-height divider (the R1 v1
/// look) — a genuine one-line revert if the operator prefers closed boxes on
/// the rail specifically.
const RAIL_CONNECTED_ACTIVE: bool = true;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Which side of the window the rail occupies. R1 ships `Left`; `Right` is
/// wired for the widget (divider on the opposite seam) so R2 only needs the
/// settings/reservation plumbing, not new render code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RailSide {
    Left,
    Right,
}

/// A single text glyph to paint into the rail region, addressed by `(row, col)`
/// within the `rail_cols × grid_rows` band (the vertical analog of the strip's
/// column-addressed [`super::tab_bar::TabBarGlyph`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TabRailGlyph {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) ch: char,
    pub(super) attrs: Attrs,
}

/// Output from [`TabRail::render`]: the fully-painted region glyphs plus the
/// pixel-space chrome quads (per-slot outline rings + the rail↔content divider).
#[derive(Debug, Default)]
pub(super) struct TabRailOutput {
    /// Solid pixel-space quads: the per-slot outline rings and the 1px
    /// rail↔content divider. All opaque, so the chrome reads over background
    /// images (the house image-proof rule).
    pub(super) quads: Vec<SolidQuad>,
    /// One glyph per cell of the `rail_cols × grid_rows` region.
    pub(super) glyphs: Vec<TabRailGlyph>,
}

/// Presentation-only rail state — only hover, mirroring [`TabBar`].
#[derive(Debug, Default, Clone)]
pub(super) struct TabRail {
    pub(super) hover: Option<TabHit>,
}

// ---------------------------------------------------------------------------
// Private layout types
// ---------------------------------------------------------------------------

/// Computed geometry for one rendered tab slot in the rail.
#[derive(Debug, Clone)]
struct RailSlot {
    /// Tab index in the source.
    idx: usize,
    /// First row of the slot (inclusive).
    start_row: usize,
    /// One-past-last row of the slot (exclusive).
    end_row: usize,
    /// Wrapped label lines (≤ `SLOT_ROWS` entries), each already truncated to
    /// the inner column budget.
    label_lines: Vec<String>,
    /// `(row, col)` of the `×` close glyph (top-right cell of the slot), or
    /// `None` when the rail is too narrow.
    close_cell: Option<(usize, usize)>,
}

/// Full rail layout for one rendering pass.
#[derive(Debug, Default)]
struct RailLayout {
    /// Visible tab slots (row coordinates absolute within the rail region).
    slots: Vec<RailSlot>,
    /// `(start_row, end_row)` of the `+` new-tab slot, or `None` when it doesn't
    /// fit (overflow, or a degenerate rail).
    new_tab_rows: Option<(usize, usize)>,
    /// When some tabs are scrolled off the TOP: `Some(hidden_count)`. The `▲`
    /// indicator paints in row 0 and is informational-only in R1.
    overflow_above: Option<usize>,
    /// When some tabs are scrolled off the BOTTOM: `Some(hidden_count)`. The `▼`
    /// indicator paints in the last row and is informational-only in R1.
    overflow_below: Option<usize>,
}

// ---------------------------------------------------------------------------
// TabRail impl
// ---------------------------------------------------------------------------

impl TabRail {
    /// Update the hover state from the latest pointer hit test.
    pub(super) fn set_hover(&mut self, hit: Option<TabHit>) {
        self.hover = hit;
    }

    /// Render the rail for the current frame.
    ///
    /// - `source` — session model accessor (mock or real `TabSet`).
    /// - `rail_cols` — rail band width in cells (the setting; R1 fixed).
    /// - `grid_rows` — window content rows the rail spans.
    /// - `origin_px` — physical-pixel top-left of the rail band (already
    ///   padding-offset by the integration layer).
    /// - `cell` — cell metrics (width / height in physical pixels).
    /// - `placement` — `Left` (divider on the right seam) or `Right`.
    /// - `colors` — the theme-role colors (see [`TabBarColors`]).
    ///
    /// Returns a fully-painted `rail_cols × grid_rows` region (`glyphs`) plus the
    /// chrome `quads`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &self,
        source: &dyn TabBarSource,
        rail_cols: usize,
        grid_rows: usize,
        origin_px: [f32; 2],
        cell: CellSize,
        placement: RailSide,
        colors: TabBarColors,
    ) -> TabRailOutput {
        if rail_cols == 0 || grid_rows == 0 || cell.width == 0 || cell.height == 0 {
            return TabRailOutput::default();
        }
        let layout = compute_rail_layout(source, rail_cols, grid_rows);

        let base_fg = Color::Rgb(
            colors.foreground.0,
            colors.foreground.1,
            colors.foreground.2,
        );
        let dim_fg = Color::Rgb(colors.inactive.0, colors.inactive.1, colors.inactive.2);
        let band_srgb = blend_srgb(colors.background, colors.inactive, BAND_BLEND);
        let band_bg = Color::Rgb(band_srgb.0, band_srgb.1, band_srgb.2);
        let active_bg_color =
            Color::Rgb(colors.active_bg.0, colors.active_bg.1, colors.active_bg.2);
        let hover_srgb = blend_srgb(band_srgb, colors.active_bg, HOVER_FILL_BLEND);
        let hover_bg_color = Color::Rgb(hover_srgb.0, hover_srgb.1, hover_srgb.2);
        let (active_ring_srgb, inactive_ring_srgb) = ring_colors(colors, band_srgb);

        // The whole region starts as opaque band fill so wallpaper never leaks.
        let mut cells = vec![blank_glyph(0, 0, base_fg, band_bg); rail_cols * grid_rows];
        for (i, glyph) in cells.iter_mut().enumerate() {
            glyph.row = i / rail_cols;
            glyph.col = i % rail_cols;
        }

        let mut inactive_rings: Vec<SolidQuad> = Vec::new();
        let mut active_ring: Vec<SolidQuad> = Vec::new();
        // Pixel y-span of the active slot's ring, when one is visible — feeds the
        // broken divider (F4-V2 connected-active). `None` leaves a full divider.
        let mut active_gap: Option<(f32, f32)> = None;
        // Whether the active slot opens toward the content seam this frame.
        let active_open_seam = if RAIL_CONNECTED_ACTIVE {
            Some(placement)
        } else {
            None
        };
        let active_idx = source.active_tab();

        for slot in &layout.slots {
            let is_active = slot.idx == active_idx;
            let is_hovered = is_slot_hovered(self.hover, slot.idx);
            let slot_bg = if is_active {
                active_bg_color
            } else if is_hovered {
                hover_bg_color
            } else {
                band_bg
            };
            let slot_seam = if is_active { active_open_seam } else { None };
            // Fill the slot's cells INSIDE the inset (R1.1), leaving the rail-edge
            // margin columns as band. The active slot bleeds to the divider seam
            // on its content-facing side (connected-active); the closed side and
            // inactive/hover fills inset on both sides.
            let (fill_c0, fill_c1) = slot_fill_cols(rail_cols, slot_seam);
            for row in slot.start_row..slot.end_row.min(grid_rows) {
                for col in fill_c0..fill_c1.min(rail_cols) {
                    cells[row * rail_cols + col].attrs.background = slot_bg;
                }
            }
            // Outline ring — the shape language. Inactive slots are closed boxes
            // inset from both rail edges; the active slot opens toward the content
            // seam (F4-V2 connected-active) so its fill flows through the broken
            // divider, its content-facing edge reaching the seam past the inset.
            let ring = rail_slot_ring(
                slot.start_row,
                slot.end_row,
                rail_cols,
                origin_px,
                cell,
                if is_active {
                    active_ring_srgb
                } else {
                    inactive_ring_srgb
                },
                slot_seam,
                SLOT_INSET_COLS,
            );
            if is_active {
                active_ring = ring;
                if active_open_seam.is_some() {
                    let ch = cell.height as f32;
                    active_gap = Some((
                        origin_px[1] + slot.start_row as f32 * ch,
                        origin_px[1] + slot.end_row as f32 * ch,
                    ));
                }
            } else {
                inactive_rings.extend(ring);
            }
            // Label glyphs (wrapped across the slot's rows).
            let mut la = Attrs::default();
            la.foreground = if is_active { base_fg } else { dim_fg };
            la.background = slot_bg;
            if is_active {
                la.set_bold(true);
            }
            for (line_idx, line) in slot.label_lines.iter().enumerate() {
                let row = slot.start_row + line_idx;
                if row >= slot.end_row {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    let col = SLOT_LABEL_START_COL + i;
                    if row < grid_rows && col < rail_cols {
                        let g = &mut cells[row * rail_cols + col];
                        g.ch = ch;
                        g.attrs = la;
                    }
                }
            }
            // Close `×` glyph — only for the active or hovered slot.
            if let Some((crow, ccol)) = slot.close_cell.filter(|_| is_active || is_hovered)
                && crow < grid_rows
                && ccol < rail_cols
            {
                let g = &mut cells[crow * rail_cols + ccol];
                g.ch = '×';
                g.attrs = la;
            }
        }

        // New-tab `+` slot — a lightweight affordance, not a tab (R1.1): a
        // centered dim `+` on the bare band at rest, gaining a hover fill + ring
        // only when hovered (the strip's hover precedent), so it never competes
        // with a real slot's bounded box.
        if let Some((nt_start, nt_end)) = layout.new_tab_rows {
            let is_hovered = matches!(self.hover, Some(TabHit::NewTab));
            if is_hovered {
                let (fill_c0, fill_c1) = slot_fill_cols(rail_cols, None);
                for row in nt_start..nt_end.min(grid_rows) {
                    for col in fill_c0..fill_c1.min(rail_cols) {
                        cells[row * rail_cols + col].attrs.background = hover_bg_color;
                    }
                }
                inactive_rings.extend(rail_slot_ring(
                    nt_start,
                    nt_end,
                    rail_cols,
                    origin_px,
                    cell,
                    inactive_ring_srgb,
                    None,
                    SLOT_INSET_COLS,
                ));
            }
            let mut a = Attrs::default();
            // Dim `+` at rest (an affordance), brighter under hover.
            a.foreground = if is_hovered { base_fg } else { dim_fg };
            a.background = if is_hovered { hover_bg_color } else { band_bg };
            // Centre the `+` in the (1-cell-tall) slot.
            let prow = nt_start + (nt_end - nt_start) / 2;
            let pcol = rail_cols / 2;
            if prow < grid_rows && pcol < rail_cols {
                let g = &mut cells[prow * rail_cols + pcol];
                g.ch = '+';
                g.attrs = a;
            }
        }

        // Overflow indicators (informational-only in R1): `▲N` in row 0, `▼N` in
        // the last row. Painted as dim-foreground glyphs over the band.
        if let Some(hidden) = layout.overflow_above {
            paint_overflow_indicator(&mut cells, 0, rail_cols, grid_rows, '▲', hidden, dim_fg);
        }
        if let Some(hidden) = layout.overflow_below {
            paint_overflow_indicator(
                &mut cells,
                grid_rows - 1,
                rail_cols,
                grid_rows,
                '▼',
                hidden,
                dim_fg,
            );
        }

        // Chrome quads: rings (inactive first, active last so it stays crisp),
        // then the rail↔content divider on the seam — broken across the active
        // slot's row span so its fill connects to the content (F4-V2).
        let mut out = TabRailOutput {
            glyphs: cells,
            ..Default::default()
        };
        out.quads.extend(inactive_rings);
        out.quads.extend(active_ring);
        out.quads.extend(rail_divider(
            rail_cols,
            grid_rows,
            origin_px,
            cell,
            placement,
            colors.border,
            active_gap,
        ));
        out
    }

    /// Map a physical-pixel pointer position to a [`TabHit`] against the rail.
    ///
    /// Row-major: an X-band gate (`origin_x <= px_x < origin_x + rail_w`), then
    /// `row = (px_y - origin_y) / cell.height`, then the slot whose row range
    /// contains it. Overflow-indicator rows are informational → [`TabHit::None`].
    #[allow(clippy::too_many_arguments)]
    pub(super) fn hit_test(
        &self,
        px_x: f64,
        px_y: f64,
        source: &dyn TabBarSource,
        rail_cols: usize,
        grid_rows: usize,
        origin_px: [f32; 2],
        cell: CellSize,
    ) -> TabHit {
        let cw = cell.width as f64;
        let ch = cell.height as f64;
        if cw <= 0.0 || ch <= 0.0 || rail_cols == 0 || grid_rows == 0 {
            return TabHit::None;
        }
        let ox = origin_px[0] as f64;
        let oy = origin_px[1] as f64;
        // X-band gate.
        let x = px_x - ox;
        if x < 0.0 || x >= rail_cols as f64 * cw {
            return TabHit::None;
        }
        let y = px_y - oy;
        if y < 0.0 {
            return TabHit::None;
        }
        let row = (y / ch) as usize;
        if row >= grid_rows {
            return TabHit::None;
        }
        let col = (x / cw) as usize;

        let layout = compute_rail_layout(source, rail_cols, grid_rows);

        // Overflow-indicator rows are informational-only.
        if layout.overflow_above.is_some() && row == 0 {
            return TabHit::None;
        }
        if layout.overflow_below.is_some() && row == grid_rows - 1 {
            return TabHit::None;
        }

        // New-tab `+` slot.
        if let Some((s, e)) = layout.new_tab_rows
            && row >= s
            && row < e
        {
            return TabHit::NewTab;
        }

        // Tab slots.
        for slot in &layout.slots {
            if row < slot.start_row || row >= slot.end_row {
                continue;
            }
            if slot.close_cell == Some((row, col)) {
                return TabHit::Close(slot.idx);
            }
            return TabHit::Switch(slot.idx);
        }

        TabHit::None
    }
}

// ---------------------------------------------------------------------------
// Layout computation
// ---------------------------------------------------------------------------

/// Build the row-stacked slot layout for the current tab set.
///
/// Pure: no mutable state, so `render` and `hit_test` call it independently.
/// When every tab plus the `+` slot fits, all are laid out top-down with no
/// scroll. Otherwise the view scrolls to keep the active tab visible and emits
/// informational `▲/▼` overflow counts; the `+` slot is dropped when the rail
/// is full (new tabs remain reachable via the menu/keyboard).
fn compute_rail_layout(
    source: &dyn TabBarSource,
    rail_cols: usize,
    grid_rows: usize,
) -> RailLayout {
    let mut layout = RailLayout::default();
    if rail_cols == 0 || grid_rows == 0 {
        return layout;
    }
    let tab_count = source.tab_count();
    if tab_count == 0 {
        if grid_rows >= RAIL_TOP_MARGIN_ROWS + NEW_TAB_ROWS {
            layout.new_tab_rows = Some((RAIL_TOP_MARGIN_ROWS, RAIL_TOP_MARGIN_ROWS + NEW_TAB_ROWS));
        }
        return layout;
    }

    // Rows needed to show the top margin, every tab plus the `+` slot with no
    // scroll. Each tab consumes SLOT_STRIDE (slot + trailing gap); the last
    // trailing gap becomes the gap before the `+` slot, so the total is
    // margin + tabs*stride + NEW_TAB_ROWS (R1.1 adds the leading top margin).
    let total_needed = RAIL_TOP_MARGIN_ROWS + tab_count * SLOT_STRIDE + NEW_TAB_ROWS;

    if total_needed <= grid_rows {
        for i in 0..tab_count {
            let start = RAIL_TOP_MARGIN_ROWS + i * SLOT_STRIDE;
            layout.slots.push(build_slot(source, i, start, rail_cols));
        }
        let nt_start = RAIL_TOP_MARGIN_ROWS + tab_count * SLOT_STRIDE;
        layout.new_tab_rows = Some((nt_start, nt_start + NEW_TAB_ROWS));
        return layout;
    }

    // Overflow: scroll to keep the active tab visible. Compute a conservative
    // visible capacity assuming both indicator rows are present, so the greedy
    // placement below can never overrun the region.
    let active = source.active_tab().min(tab_count - 1);
    let band = grid_rows.saturating_sub(2);
    // n slots need n*SLOT_ROWS + (n-1)*SLOT_GAP = n*STRIDE - SLOT_GAP rows.
    let capacity = ((band + SLOT_GAP) / SLOT_STRIDE).max(1).min(tab_count);
    let max_first = tab_count - capacity;
    let first = active.saturating_sub(capacity / 2).min(max_first);

    // When scrolled below the top, row 0 carries the `▲` indicator; when at the
    // top, apply the same top margin as the no-scroll case (R1.1).
    let top = if first > 0 { 1 } else { RAIL_TOP_MARGIN_ROWS };
    let mut placed = 0usize;
    let mut row = top;
    for j in 0..(tab_count - first) {
        let end = row + SLOT_ROWS;
        // Always keep the last region row free for a potential `▼`.
        if end > grid_rows.saturating_sub(1) {
            break;
        }
        layout
            .slots
            .push(build_slot(source, first + j, row, rail_cols));
        placed += 1;
        row += SLOT_STRIDE;
    }

    layout.overflow_above = (first > 0).then_some(first);
    layout.overflow_below = (first + placed < tab_count).then_some(tab_count - first - placed);
    // No `+` slot in overflow mode — the rail is full.
    layout
}

/// Build one slot at `start_row`, wrapping the tab title across its rows.
fn build_slot(
    source: &dyn TabBarSource,
    idx: usize,
    start_row: usize,
    rail_cols: usize,
) -> RailSlot {
    let end_row = start_row + SLOT_ROWS;
    // The `×` gets its own cell at the slot's top-right, INSIDE the ring inset
    // (R1.1) so it sits within the bounded box, not against the rail edge. It
    // occupies the last inset column, never colliding with the label inner area.
    let close_col = rail_cols.saturating_sub(SLOT_INSET_COLS + 1);
    let close_cell = (close_col > SLOT_LABEL_START_COL).then_some((start_row, close_col));
    // Label inner budget: from `SLOT_LABEL_START_COL` up to (but excluding) the
    // close cell, i.e. the inset box minus the left label pad and the close cell.
    let inner = close_col.saturating_sub(SLOT_LABEL_START_COL);
    let label_lines = if inner == 0 {
        Vec::new()
    } else {
        wrap_label(source.tab_title(idx), inner, SLOT_ROWS)
    };
    RailSlot {
        idx,
        start_row,
        end_row,
        label_lines,
        close_cell,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn blank_glyph(row: usize, col: usize, foreground: Color, background: Color) -> TabRailGlyph {
    let mut attrs = Attrs::default();
    attrs.foreground = foreground;
    attrs.background = background;
    TabRailGlyph {
        row,
        col,
        ch: ' ',
        attrs,
    }
}

fn is_slot_hovered(hover: Option<TabHit>, idx: usize) -> bool {
    matches!(hover, Some(TabHit::Switch(i) | TabHit::Close(i)) if i == idx)
}

/// Wrap `s` across at most `rows` lines of `inner` columns each, truncating the
/// last line with `…` when the title still overflows. Leading/trailing
/// whitespace is stripped. Each Unicode scalar counts as one column (correct for
/// the ASCII-heavy titles typical of terminal tabs).
fn wrap_label(s: &str, inner: usize, rows: usize) -> Vec<String> {
    if inner == 0 || rows == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = s.trim().chars().collect();
    let mut lines = Vec::new();
    let mut i = 0usize;
    for r in 0..rows {
        if i >= chars.len() {
            break;
        }
        let remaining = chars.len() - i;
        if r == rows - 1 && remaining > inner {
            // Last available line and more remains → truncate with `…`.
            let mut line: String = chars[i..i + inner.saturating_sub(1)].iter().collect();
            line.push('…');
            lines.push(line);
            break;
        }
        let take = remaining.min(inner);
        lines.push(chars[i..i + take].iter().collect());
        i += take;
    }
    lines
}

/// Paint an overflow indicator (`▲N` / `▼N`) into `row`, left-aligned after one
/// padding column, as dim-foreground glyphs over the existing band fill.
fn paint_overflow_indicator(
    cells: &mut [TabRailGlyph],
    row: usize,
    rail_cols: usize,
    grid_rows: usize,
    marker: char,
    hidden: usize,
    fg: Color,
) {
    if row >= grid_rows || rail_cols == 0 {
        return;
    }
    let text: String = format!("{marker}{hidden}");
    let mut a = Attrs::default();
    a.foreground = fg;
    // Keep the existing band background under the indicator glyphs.
    for (i, ch) in text.chars().enumerate() {
        let col = RAIL_LABEL_PAD + i;
        if col >= rail_cols {
            break;
        }
        let idx = row * rail_cols + col;
        a.background = cells[idx].attrs.background;
        cells[idx].ch = ch;
        cells[idx].attrs = a;
    }
}

/// The `(active, inactive)` ring colors, from TEXT-side theme roles so each
/// contrasts with the (dark) band by construction — identical derivation to the
/// horizontal bar (F4 v1.3). Active = `foreground` nudged toward the band;
/// inactive = the `inactive` role directly.
fn ring_colors(colors: TabBarColors, band: Srgb) -> (Srgb, Srgb) {
    let active = blend_srgb(colors.foreground, band, ACTIVE_OUTLINE_BLEND);
    (active, colors.inactive)
}

/// A hollow ring of [`SolidQuad`]s framing a rail slot spanning rows
/// `[start_row, end_row)` across the full `rail_cols` width. Thickness is ≥2px
/// (the CRT scanline shader eats a 1px ring). Fully opaque so it reads over
/// background images. Returns empty for a degenerate slot.
///
/// When `open_seam` is `None` (inactive slots) the ring is a closed box of four
/// edges. When it is `Some(side)` (the active slot, F4-V2 connected-active) the
/// **content-facing vertical edge is dropped** — the right edge for `Left`
/// placement, the left edge for `Right` — so the active fill flows through the
/// broken divider into the content area. The perpendicular top/bottom edges
/// already span the full rail width to the seam, so the open ring has three
/// edges (the vertical transposition of the horizontal bar's open-bottom ring).
///
/// `inset_cols` insets the ring from BOTH rail edges (R1.1) so slots read as
/// bounded boxes with a margin rather than blocky edge-to-edge bands. The inset
/// applies to the CLOSED edges only: a content-facing OPEN edge (and the
/// top/bottom edges on the open side) still reach the divider seam, since the
/// connection to content is the point of the connected-active treatment.
#[allow(clippy::too_many_arguments)]
fn rail_slot_ring(
    start_row: usize,
    end_row: usize,
    rail_cols: usize,
    origin_px: [f32; 2],
    cell: CellSize,
    outline: Srgb,
    open_seam: Option<RailSide>,
    inset_cols: usize,
) -> Vec<SolidQuad> {
    if end_row <= start_row || rail_cols == 0 || cell.width == 0 || cell.height == 0 {
        return Vec::new();
    }
    let cw = cell.width as f32;
    let ch = cell.height as f32;
    let thickness = (ch / 8.0).clamp(2.0, 3.0);
    // Clamp the inset so a narrow rail can never invert the ring.
    let inset = (inset_cols.min(rail_cols / 2) as f32) * cw;
    let rail_x0 = origin_px[0];
    let rail_x1 = origin_px[0] + rail_cols as f32 * cw;
    // Closed-edge x positions (inset from the rail edges).
    let left_x = rail_x0 + inset;
    let right_x = rail_x1 - inset;
    let y0 = origin_px[1] + start_row as f32 * ch;
    let y1 = origin_px[1] + end_row as f32 * ch;
    let color = srgb_alpha(outline, 1.0);
    // The top/bottom edges span between the ring's live vertical extents: from
    // the inset on a closed side, or all the way to the rail seam on an open
    // (content-facing) side so the mouth meets the divider.
    let span_x0 = if open_seam == Some(RailSide::Right) {
        rail_x0
    } else {
        left_x
    };
    let span_x1 = if open_seam == Some(RailSide::Left) {
        rail_x1
    } else {
        right_x
    };
    let mut quads = vec![
        SolidQuad {
            rect: [span_x0, y0, span_x1, y0 + thickness],
            color,
        }, // top
        SolidQuad {
            rect: [span_x0, y1 - thickness, span_x1, y1],
            color,
        }, // bottom
    ];
    // Vertical side edges — the content-facing one is dropped when the ring is
    // open toward the content seam; the closed one sits at the inset.
    if open_seam != Some(RailSide::Left) {
        quads.push(SolidQuad {
            rect: [right_x - thickness, y0 + thickness, right_x, y1 - thickness],
            color,
        }); // right (content-facing for Left placement)
    }
    if open_seam != Some(RailSide::Right) {
        quads.push(SolidQuad {
            rect: [left_x, y0 + thickness, left_x + thickness, y1 - thickness],
            color,
        }); // left (content-facing for Right placement)
    }
    quads
}

/// The `[c0, c1)` column range a slot's fill covers (R1.1). Inactive/closed
/// slots inset on both sides; an active slot bleeds to the divider seam on its
/// content-facing side (right for `Left` placement, left for `Right`) so the
/// selection fill connects to the content, and insets on the closed side.
fn slot_fill_cols(rail_cols: usize, open_seam: Option<RailSide>) -> (usize, usize) {
    let inset = SLOT_INSET_COLS.min(rail_cols / 2);
    match open_seam {
        Some(RailSide::Left) => (inset, rail_cols),
        Some(RailSide::Right) => (0, rail_cols.saturating_sub(inset)),
        None => (inset, rail_cols.saturating_sub(inset)),
    }
}

/// The 1px `border`-role divider along the rail↔content seam: the right edge of
/// the rail for `Left` placement, the left edge for `Right`. Opaque so it reads
/// over wallpaper (the vertical analog of the strip's band separator).
///
/// `active_gap` is the pixel y-span `(y0, y1)` of the active slot's outline,
/// when one is visible. The divider is **broken** across that gap (F4-V2
/// connected-active): the line is emitted as up to two segments — above the gap
/// and below it — so the active slot's fill flows uninterrupted into the content
/// area. `None` (no visible active slot, e.g. the active tab scrolled off, or an
/// empty rail) emits one full-height line. A segment that collapses to
/// zero/negative height — which happens when the active slot is flush against
/// the top or bottom of the rail — emits nothing rather than a degenerate quad.
/// Mirrors the horizontal bar's [`super::tab_bar`] broken separator.
fn rail_divider(
    rail_cols: usize,
    grid_rows: usize,
    origin_px: [f32; 2],
    cell: CellSize,
    placement: RailSide,
    border: Srgb,
    active_gap: Option<(f32, f32)>,
) -> Vec<SolidQuad> {
    if grid_rows == 0 || cell.width == 0 || cell.height == 0 {
        return Vec::new();
    }
    let cw = cell.width as f32;
    let ch = cell.height as f32;
    let band_start = origin_px[1];
    let band_end = origin_px[1] + grid_rows as f32 * ch;
    let seam_x = match placement {
        RailSide::Left => origin_px[0] + rail_cols as f32 * cw,
        RailSide::Right => origin_px[0],
    };
    let color = srgb_alpha(border, 1.0);
    let mut quads = Vec::new();
    let mut push_segment = |a: f32, b: f32| {
        if b - a > f32::EPSILON {
            quads.push(SolidQuad {
                rect: [seam_x - DIVIDER_PX, a, seam_x, b],
                color,
            });
        }
    };
    match active_gap {
        Some((gap_y0, gap_y1)) => {
            // Clamp the gap into the band so an off-rail active span can't push a
            // segment past the band extent.
            let gap_y0 = gap_y0.clamp(band_start, band_end);
            let gap_y1 = gap_y1.clamp(band_start, band_end);
            push_segment(band_start, gap_y0);
            push_segment(gap_y1, band_end);
        }
        None => push_segment(band_start, band_end),
    }
    quads
}

/// Blend two sRGB colors: `a*(1-t) + b*t` per channel, `t` clamped to `[0,1]`.
/// Gamma-naive sRGB blend — fine for subtle chrome tints (matches the bar).
fn blend_srgb(a: Srgb, b: Srgb, t: f32) -> Srgb {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        (f32::from(x) * (1.0 - t) + f32::from(y) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Convert an sRGB tuple + alpha to a linear-RGBA `[f32; 4]` for [`SolidQuad`].
fn srgb_alpha(color: Srgb, alpha: f32) -> [f32; 4] {
    let mut linear = text::foreground_linear(Color::Rgb(color.0, color.1, color.2));
    linear[3] = alpha;
    linear
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSource {
        titles: Vec<&'static str>,
        active: usize,
    }
    impl MockSource {
        fn new(titles: &[&'static str], active: usize) -> Self {
            Self {
                titles: titles.to_vec(),
                active,
            }
        }
        fn empty() -> Self {
            Self::new(&[], 0)
        }
    }
    impl TabBarSource for MockSource {
        fn tab_count(&self) -> usize {
            self.titles.len()
        }
        fn tab_title(&self, idx: usize) -> &str {
            self.titles[idx]
        }
        fn active_tab(&self) -> usize {
            self.active
        }
    }

    const CELL: CellSize = CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    };
    const RAIL_COLS: usize = 16;
    const GRID_ROWS: usize = 40;
    const ORIGIN: [f32; 2] = [0.0, 0.0];

    const COLORS: TabBarColors = TabBarColors {
        foreground: (0xCC, 0xCC, 0xCC),
        background: (0x10, 0x10, 0x10),
        inactive: (0x66, 0x66, 0x66),
        active_bg: (0x40, 0x60, 0x90),
        border: (0xE0, 0xE0, 0xE0),
    };

    fn rail() -> TabRail {
        TabRail::default()
    }

    fn render_default(src: &dyn TabBarSource) -> TabRailOutput {
        rail().render(
            src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
        )
    }

    // Physical-pixel centre of cell (row, col).
    fn cell_centre_px(row: usize, col: usize) -> (f64, f64) {
        (
            col as f64 * CELL.width as f64 + CELL.width as f64 / 2.0,
            row as f64 * CELL.height as f64 + CELL.height as f64 / 2.0,
        )
    }

    fn hit_at(row: usize, col: usize, src: &dyn TabBarSource) -> TabHit {
        let (x, y) = cell_centre_px(row, col);
        rail().hit_test(x, y, src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL)
    }

    // -----------------------------------------------------------------------
    // Region shape
    // -----------------------------------------------------------------------

    #[test]
    fn render_emits_one_glyph_per_region_cell() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        assert_eq!(
            out.glyphs.len(),
            RAIL_COLS * GRID_ROWS,
            "one glyph per cell"
        );
    }

    #[test]
    fn empty_region_for_zero_width_or_height() {
        let src = MockSource::new(&["a"], 0);
        let out = rail().render(&src, 0, GRID_ROWS, ORIGIN, CELL, RailSide::Left, COLORS);
        assert!(out.glyphs.is_empty(), "zero rail_cols → empty");
        let out = rail().render(&src, RAIL_COLS, 0, ORIGIN, CELL, RailSide::Left, COLORS);
        assert!(out.glyphs.is_empty(), "zero grid_rows → empty");
    }

    #[test]
    fn every_cell_carries_an_opaque_band_or_slot_background() {
        // No cell may be left transparent — the rail must never leak wallpaper
        // (the fill-the-whole-band invariant).
        let src = MockSource::new(&["a", "b"], 0);
        let out = render_default(&src);
        let raw_bg = Color::Rgb(
            COLORS.background.0,
            COLORS.background.1,
            COLORS.background.2,
        );
        // The band fill specifically must differ from the raw background.
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let band_bg = Color::Rgb(band.0, band.1, band.2);
        assert_ne!(band_bg, raw_bg, "band fill differs from raw background");
        // A gap row between slot 0 and slot 1 (the row after slot 0's rows) is
        // band fill. With the R1.1 top margin, slot 0 spans rows
        // [RAIL_TOP_MARGIN_ROWS, +SLOT_ROWS), so the gap row is below it.
        let gap_row = RAIL_TOP_MARGIN_ROWS + SLOT_ROWS;
        let gap_idx = gap_row * RAIL_COLS; // col=0
        assert_eq!(
            out.glyphs[gap_idx].attrs.background, band_bg,
            "inter-slot gap row is band fill"
        );
    }

    // -----------------------------------------------------------------------
    // Layout: row-stacked slots
    // -----------------------------------------------------------------------

    #[test]
    fn zero_tabs_shows_only_the_new_tab_slot() {
        let src = MockSource::empty();
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        assert!(layout.slots.is_empty(), "no tab slots");
        assert_eq!(
            layout.new_tab_rows,
            Some((RAIL_TOP_MARGIN_ROWS, RAIL_TOP_MARGIN_ROWS + NEW_TAB_ROWS)),
            "the + slot sits below the top margin with zero tabs"
        );
    }

    #[test]
    fn three_tabs_stack_with_stride_and_a_new_tab_slot() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        assert_eq!(layout.slots.len(), 3, "three tab slots");
        // R1.1: the first slot starts a top margin below the rail edge.
        for (i, slot) in layout.slots.iter().enumerate() {
            let start = RAIL_TOP_MARGIN_ROWS + i * SLOT_STRIDE;
            assert_eq!(slot.start_row, start, "slot {i} start row");
            assert_eq!(slot.end_row, start + SLOT_ROWS, "slot {i} end row");
        }
        let nt_start = RAIL_TOP_MARGIN_ROWS + 3 * SLOT_STRIDE;
        assert_eq!(
            layout.new_tab_rows,
            Some((nt_start, nt_start + NEW_TAB_ROWS)),
            "the + slot follows the last tab"
        );
        assert!(layout.overflow_above.is_none() && layout.overflow_below.is_none());
    }

    #[test]
    fn slots_never_overlap() {
        let src = MockSource::new(&["a", "b", "c", "d"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        for w in layout.slots.windows(2) {
            assert!(w[0].end_row <= w[1].start_row, "slots must not overlap");
        }
    }

    // -----------------------------------------------------------------------
    // Labels
    // -----------------------------------------------------------------------

    #[test]
    fn slot_label_glyphs_are_present() {
        let src = MockSource::new(&["zsh"], 0);
        let out = render_default(&src);
        let label: String = out
            .glyphs
            .iter()
            .filter(|g| g.ch != ' ' && g.ch != '+' && g.ch != '×')
            .map(|g| g.ch)
            .collect();
        assert!(label.contains("zsh"), "label 'zsh' present");
    }

    #[test]
    fn active_label_is_bold_foreground() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        let z = out.glyphs.iter().find(|g| g.ch == 'z').expect("'z' glyph");
        assert!(z.attrs.bold(), "active tab label is bold");
        let base_fg = Color::Rgb(
            COLORS.foreground.0,
            COLORS.foreground.1,
            COLORS.foreground.2,
        );
        assert_eq!(z.attrs.foreground, base_fg, "active label is foreground");
    }

    #[test]
    fn inactive_label_is_dim_and_not_bold() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        // 'b' from "bash" (inactive slot 1).
        let b = out.glyphs.iter().find(|g| g.ch == 'b').expect("'b' glyph");
        assert!(!b.attrs.bold(), "inactive label not bold");
        let dim = Color::Rgb(COLORS.inactive.0, COLORS.inactive.1, COLORS.inactive.2);
        assert_eq!(b.attrs.foreground, dim, "inactive label is dimmed");
    }

    #[test]
    fn long_title_wraps_then_truncates_with_ellipsis() {
        // inner = RAIL_COLS - 2 = 14; two rows → up to 28 chars, last line
        // truncated with … beyond that.
        let long = "a-very-long-terminal-title-that-will-not-fit-in-two-rows";
        let lines = wrap_label(long, 14, SLOT_ROWS);
        assert_eq!(lines.len(), 2, "wraps across both slot rows");
        assert!(lines[0].chars().count() <= 14, "first line within inner");
        assert!(lines[1].chars().count() <= 14, "second line within inner");
        assert!(lines[1].ends_with('…'), "overflowing last line ends with …");
    }

    #[test]
    fn short_title_is_single_line_untruncated() {
        let lines = wrap_label("vim", 14, SLOT_ROWS);
        assert_eq!(
            lines,
            vec!["vim".to_string()],
            "short title, one line, no …"
        );
    }

    // -----------------------------------------------------------------------
    // Close × and new-tab +
    // -----------------------------------------------------------------------

    #[test]
    fn active_slot_renders_close_glyph_in_top_right_cell() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        // R1.1: active slot 0 top-right cell is inside the inset box — row
        // RAIL_TOP_MARGIN_ROWS, col RAIL_COLS - SLOT_INSET_COLS - 1.
        let row = RAIL_TOP_MARGIN_ROWS;
        let col = RAIL_COLS - SLOT_INSET_COLS - 1;
        let idx = row * RAIL_COLS + col;
        assert_eq!(
            out.glyphs[idx].ch, '×',
            "active slot shows × at the inset top-right"
        );
    }

    #[test]
    fn inactive_slot_has_no_close_glyph_when_not_hovered() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        // Inactive slot 1 top-right cell.
        let row = SLOT_STRIDE;
        let idx = row * RAIL_COLS + (RAIL_COLS - 1);
        assert_ne!(out.glyphs[idx].ch, '×', "inactive unhovered slot: no ×");
    }

    #[test]
    fn new_tab_slot_renders_plus_glyph() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        assert!(out.glyphs.iter().any(|g| g.ch == '+'), "+ glyph present");
    }

    // Ring quads (wider than the 1px divider) whose vertical extent falls inside
    // `[start_row, end_row)` — used to detect a slot's outline ring vs. the
    // thin rail divider that also runs through those rows.
    fn ring_quads_in_rows(out: &TabRailOutput, start_row: usize, end_row: usize) -> usize {
        let y0 = start_row as f32 * CELL.height as f32;
        let y1 = end_row as f32 * CELL.height as f32;
        out.quads
            .iter()
            .filter(|q| {
                q.rect[1] >= y0 - 0.5
                    && q.rect[3] <= y1 + 0.5
                    && (q.rect[2] - q.rect[0]) > DIVIDER_PX + 0.5
            })
            .count()
    }

    #[test]
    fn new_tab_slot_is_one_row_and_ringless_at_rest() {
        // R1.1: the `+` affordance is a lightweight 1-cell glyph on the bare band
        // — no ring, no fill — so it never competes with a real tab slot.
        let src = MockSource::new(&["a", "b"], 0);
        let (nt_start, nt_end) = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS)
            .new_tab_rows
            .expect("new-tab slot present");
        assert_eq!(nt_end - nt_start, NEW_TAB_ROWS, "the + slot is 1 cell tall");
        let out = render_default(&src);
        assert_eq!(
            ring_quads_in_rows(&out, nt_start, nt_end),
            0,
            "the + slot has no ring at rest"
        );
        // The `+` glyph sits dim on the bare band (no fill).
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let band_bg = Color::Rgb(band.0, band.1, band.2);
        let dim = Color::Rgb(COLORS.inactive.0, COLORS.inactive.1, COLORS.inactive.2);
        let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
        assert_eq!(plus.attrs.background, band_bg, "+ sits on the bare band");
        assert_eq!(plus.attrs.foreground, dim, "+ is dim at rest");
    }

    #[test]
    fn new_tab_slot_gains_a_ring_and_fill_on_hover() {
        // The hover treatment (ring + fill) appears only under hover.
        let src = MockSource::new(&["a", "b"], 0);
        let (nt_start, nt_end) = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS)
            .new_tab_rows
            .expect("new-tab slot present");
        let mut r = rail();
        r.set_hover(Some(TabHit::NewTab));
        let out = r.render(
            &src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
        );
        assert_eq!(
            ring_quads_in_rows(&out, nt_start, nt_end),
            4,
            "the + slot gains a closed ring on hover"
        );
        // The `+` cell now carries the subordinate hover fill.
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let hover = blend_srgb(band, COLORS.active_bg, HOVER_FILL_BLEND);
        let hover_bg = Color::Rgb(hover.0, hover.1, hover.2);
        let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
        assert_eq!(plus.attrs.background, hover_bg, "+ gains the hover fill");
    }

    #[test]
    fn first_slot_has_a_top_margin_off_the_window_edge() {
        // R1.1: the first slot starts a top margin below the rail edge so it does
        // not kiss the window edge.
        let src = MockSource::new(&["a", "b"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        assert_eq!(
            layout.slots[0].start_row, RAIL_TOP_MARGIN_ROWS,
            "first slot begins below the top margin"
        );
        // Row 0 (above the first slot) is bare band — no slot ring there.
        let out = render_default(&src);
        assert_eq!(
            ring_quads_in_rows(&out, 0, RAIL_TOP_MARGIN_ROWS),
            0,
            "no ring in the top-margin band"
        );
    }

    // -----------------------------------------------------------------------
    // Hit-testing (row-major)
    // -----------------------------------------------------------------------

    #[test]
    fn hit_body_of_each_tab_switches() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        for slot in &layout.slots {
            // Body cell: label column, first row of the slot.
            let hit = hit_at(slot.start_row, RAIL_LABEL_PAD, &src);
            assert_eq!(hit, TabHit::Switch(slot.idx), "body → Switch({})", slot.idx);
        }
    }

    #[test]
    fn hit_close_cell_returns_close() {
        let src = MockSource::new(&["a", "b"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        for slot in &layout.slots {
            let (crow, ccol) = slot.close_cell.expect("close cell present");
            let hit = hit_at(crow, ccol, &src);
            assert_eq!(hit, TabHit::Close(slot.idx), "× cell → Close({})", slot.idx);
        }
    }

    #[test]
    fn hit_new_tab_slot_returns_new_tab() {
        let src = MockSource::new(&["a"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        let (s, _e) = layout.new_tab_rows.expect("new-tab slot present");
        let hit = hit_at(s, RAIL_COLS / 2, &src);
        assert_eq!(hit, TabHit::NewTab, "+ slot → NewTab");
    }

    #[test]
    fn hit_left_of_rail_is_none() {
        let src = MockSource::new(&["a"], 0);
        let hit = rail().hit_test(-1.0, 8.0, &src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL);
        assert_eq!(hit, TabHit::None, "left of the rail → None");
    }

    #[test]
    fn hit_right_of_rail_band_is_none() {
        let src = MockSource::new(&["a"], 0);
        // Just past the rail's right edge.
        let x = RAIL_COLS as f64 * CELL.width as f64 + 1.0;
        let hit = rail().hit_test(x, 8.0, &src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL);
        assert_eq!(hit, TabHit::None, "right of the rail band → None");
    }

    #[test]
    fn hit_gap_between_slots_is_none() {
        let src = MockSource::new(&["a", "b"], 0);
        // R1.1: with the top margin, slot 0 spans [RAIL_TOP_MARGIN_ROWS, +ROWS);
        // the gap row that follows it is the inter-slot band.
        let gap_row = RAIL_TOP_MARGIN_ROWS + SLOT_ROWS;
        let hit = hit_at(gap_row, RAIL_LABEL_PAD, &src);
        assert_eq!(hit, TabHit::None, "inter-slot gap → None");
    }

    #[test]
    fn hit_below_all_slots_is_none() {
        let src = MockSource::new(&["a"], 0);
        // A row well below the single tab + new-tab slot, still inside the region.
        let hit = hit_at(GRID_ROWS - 1, RAIL_LABEL_PAD, &src);
        assert_eq!(hit, TabHit::None, "empty band below slots → None");
    }

    // -----------------------------------------------------------------------
    // Overflow (many tabs) — informational indicators
    // -----------------------------------------------------------------------

    #[test]
    fn many_tabs_overflow_scrolls_to_keep_active_visible() {
        // Far more tabs than fit: the active tab must appear in the visible set.
        let titles: Vec<&'static str> = vec!["t"; 100];
        let src = MockSource::new(&titles, 80);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        assert!(!layout.slots.is_empty(), "some slots are visible");
        let visible: Vec<usize> = layout.slots.iter().map(|s| s.idx).collect();
        assert!(visible.contains(&80), "active tab 80 is kept visible");
        assert!(
            layout.overflow_above.is_some(),
            "tabs above are hidden → ▲ indicator"
        );
        assert!(
            layout.new_tab_rows.is_none(),
            "no + slot when the rail is full"
        );
    }

    #[test]
    fn overflow_indicator_rows_are_informational_only() {
        let titles: Vec<&'static str> = vec!["t"; 100];
        let src = MockSource::new(&titles, 80);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        if layout.overflow_above.is_some() {
            let hit = hit_at(0, RAIL_LABEL_PAD, &src);
            assert_eq!(hit, TabHit::None, "▲ indicator row → None (informational)");
        }
        if layout.overflow_below.is_some() {
            let hit = hit_at(GRID_ROWS - 1, RAIL_LABEL_PAD, &src);
            assert_eq!(hit, TabHit::None, "▼ indicator row → None (informational)");
        }
    }

    #[test]
    fn overflow_indicator_glyph_is_painted() {
        let titles: Vec<&'static str> = vec!["t"; 100];
        let src = MockSource::new(&titles, 80);
        let out = render_default(&src);
        assert!(
            out.glyphs.iter().any(|g| g.ch == '▲' || g.ch == '▼'),
            "an overflow indicator glyph is painted"
        );
    }

    // -----------------------------------------------------------------------
    // Chrome: rings + divider
    // -----------------------------------------------------------------------

    #[test]
    fn every_slot_is_framed_by_a_ring_active_open_inactive_closed() {
        let src = MockSource::new(&["a", "b", "c"], 1);
        let out = render_default(&src);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        // Inactive slots are closed 4-quad rings; the active slot is an open
        // 3-quad ring (content-facing edge dropped) under connected-active. The
        // broken divider's segments span across slot boundaries, so a per-slot
        // vertical-extent filter never catches them.
        for slot in &layout.slots {
            let y0 = slot.start_row as f32 * CELL.height as f32;
            let y1 = slot.end_row as f32 * CELL.height as f32;
            let n = out
                .quads
                .iter()
                .filter(|q| q.rect[1] >= y0 - 0.01 && q.rect[3] <= y1 + 0.01)
                .count();
            let expected = if slot.idx == src.active && RAIL_CONNECTED_ACTIVE {
                3
            } else {
                4
            };
            assert_eq!(n, expected, "slot {} ring quad count", slot.idx);
        }
    }

    #[test]
    fn ring_edges_are_at_least_two_pixels_thick() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        // R1.1: active slot 0 top edge sits at the top-margin row, and its left
        // extent starts at the horizontal inset.
        let y_top = RAIL_TOP_MARGIN_ROWS as f32 * CELL.height as f32;
        let inset = SLOT_INSET_COLS as f32 * CELL.width as f32;
        let top = out
            .quads
            .iter()
            .find(|q| (q.rect[1] - y_top).abs() < 0.5 && (q.rect[0] - inset).abs() < 0.5)
            .expect("active ring top edge at the top-margin row");
        let thickness = top.rect[3] - top.rect[1];
        assert!(thickness >= 2.0, "ring edge ≥ 2px (was {thickness})");
    }

    #[test]
    fn active_ring_differs_from_inactive_ring() {
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let (active, inactive) = ring_colors(COLORS, band);
        assert_eq!(
            active,
            blend_srgb(COLORS.foreground, band, ACTIVE_OUTLINE_BLEND),
            "active ring is foreground blended toward the band"
        );
        assert_eq!(
            inactive, COLORS.inactive,
            "inactive ring is the inactive role"
        );
        assert_ne!(
            srgb_alpha(active, 1.0),
            srgb_alpha(inactive, 1.0),
            "active and inactive ring colors differ"
        );
    }

    #[test]
    fn divider_is_on_the_right_seam_for_left_placement() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        let seam = RAIL_COLS as f32 * CELL.width as f32;
        let full_h = GRID_ROWS as f32 * CELL.height as f32;
        // Match the divider specifically (thin in x, spans the full height) so a
        // ring's full-width top edge at the same seam x can't be mistaken for it.
        let divider = out
            .quads
            .iter()
            .find(|q| {
                (q.rect[2] - seam).abs() < 0.01
                    && (q.rect[3] - full_h).abs() < 0.01
                    && (q.rect[2] - q.rect[0] - DIVIDER_PX).abs() < 0.01
            })
            .expect("divider on the right seam");
        // Under connected-active the divider is broken across the active slot's
        // row span; the segment that reaches the band bottom starts at the active
        // slot's bottom edge (top margin + SLOT_ROWS). Otherwise it is one full
        // line starting at the row top.
        let expected_top = if RAIL_CONNECTED_ACTIVE {
            (RAIL_TOP_MARGIN_ROWS + SLOT_ROWS) as f32 * CELL.height as f32
        } else {
            0.0
        };
        assert!(
            (divider.rect[1] - expected_top).abs() < 0.5,
            "divider top at {expected_top} (was {})",
            divider.rect[1]
        );
        assert!(
            (divider.color[3] - 1.0).abs() < f32::EPSILON,
            "opaque divider"
        );
    }

    #[test]
    fn divider_is_on_the_left_seam_for_right_placement() {
        let src = MockSource::new(&["a"], 0);
        let out = rail().render(
            &src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Right,
            COLORS,
        );
        // Right placement: the seam is the rail's left edge (x = origin_x = 0),
        // so the divider's right edge sits at x≈0 and it spans the full height.
        let full_h = GRID_ROWS as f32 * CELL.height as f32;
        let divider = out
            .quads
            .iter()
            .find(|q| q.rect[2].abs() < 0.01 && (q.rect[3] - full_h).abs() < 0.01)
            .expect("divider on the left seam");
        assert!(divider.rect[0] < 0.0, "divider extends left of the seam");
        assert!(
            (divider.color[3] - 1.0).abs() < f32::EPSILON,
            "opaque divider"
        );
    }

    // -----------------------------------------------------------------------
    // Connected active tab (F4-V2 — vertical transposition of v1.4)
    // -----------------------------------------------------------------------

    // Thickness the ring uses for CELL (mirrors `rail_slot_ring`).
    fn ring_thickness() -> f32 {
        (CELL.height as f32 / 8.0).clamp(2.0, 3.0)
    }
    // The horizontal inset (px) the ring uses for CELL (R1.1).
    fn slot_inset_px() -> f32 {
        SLOT_INSET_COLS as f32 * CELL.width as f32
    }
    // A vertical side edge at the given x-left (rect[0]≈x, width == thickness).
    fn is_side_edge_at(q: &SolidQuad, x_left: f32) -> bool {
        let th = ring_thickness();
        (q.rect[0] - x_left).abs() < 0.5 && (q.rect[2] - q.rect[0] - th).abs() < 0.5
    }
    // A full-span top edge (seated at the row top, width > thickness).
    fn is_top_edge(q: &SolidQuad, y_top: f32) -> bool {
        let th = ring_thickness();
        (q.rect[1] - y_top).abs() < 0.5
            && (q.rect[3] - q.rect[1] - th).abs() < 0.5
            && (q.rect[2] - q.rect[0]) > th
    }

    #[test]
    fn closed_ring_insets_both_vertical_side_edges_from_the_rail_edges() {
        let ring = rail_slot_ring(0, 2, RAIL_COLS, ORIGIN, CELL, COLORS.inactive, None, 1);
        let x0 = ORIGIN[0];
        let x1 = ORIGIN[0] + RAIL_COLS as f32 * CELL.width as f32;
        let inset = slot_inset_px();
        assert_eq!(ring.len(), 4, "closed ring is four edges");
        // R1.1: both side edges are inset from the rail band edges.
        assert!(
            ring.iter().any(|q| is_side_edge_at(q, x0 + inset)),
            "closed ring's left edge is inset from the rail left edge"
        );
        assert!(
            ring.iter()
                .any(|q| is_side_edge_at(q, x1 - inset - ring_thickness())),
            "closed ring's right edge is inset from the rail right edge"
        );
        // Neither side edge sits flush at the rail edge any more.
        assert!(
            !ring.iter().any(|q| is_side_edge_at(q, x0)),
            "no flush left edge"
        );
    }

    #[test]
    fn open_left_ring_drops_the_right_edge_and_reaches_the_seam() {
        // Left placement: content is to the RIGHT. The active ring drops its
        // right edge; its left edge stays inset, and its top/bottom edges reach
        // the rail seam (x1) so the mouth meets the divider (R1.1).
        let ring = rail_slot_ring(
            0,
            2,
            RAIL_COLS,
            ORIGIN,
            CELL,
            COLORS.foreground,
            Some(RailSide::Left),
            1,
        );
        let x0 = ORIGIN[0];
        let x1 = ORIGIN[0] + RAIL_COLS as f32 * CELL.width as f32;
        let inset = slot_inset_px();
        assert_eq!(ring.len(), 3, "open ring is three edges");
        assert!(
            !ring
                .iter()
                .any(|q| is_side_edge_at(q, x1 - inset - ring_thickness())),
            "open-left ring must not have a right edge"
        );
        assert!(
            ring.iter().any(|q| is_side_edge_at(q, x0 + inset)),
            "open-left ring keeps its inset left edge"
        );
        // Top edge reaches the seam on the open (right) side despite the inset.
        assert!(
            ring.iter()
                .any(|q| is_top_edge(q, 0.0) && (q.rect[2] - x1).abs() < 0.5),
            "open-left top edge reaches the rail seam"
        );
    }

    #[test]
    fn open_right_ring_drops_the_left_edge_and_reaches_the_seam() {
        // Right placement: content is to the LEFT. The active ring drops its left
        // edge; its right edge stays inset, and its top/bottom edges reach the
        // rail seam (x0) so the mouth meets the divider (R1.1).
        let ring = rail_slot_ring(
            0,
            2,
            RAIL_COLS,
            ORIGIN,
            CELL,
            COLORS.foreground,
            Some(RailSide::Right),
            1,
        );
        let x0 = ORIGIN[0];
        let x1 = ORIGIN[0] + RAIL_COLS as f32 * CELL.width as f32;
        let inset = slot_inset_px();
        assert_eq!(ring.len(), 3, "open ring is three edges");
        assert!(
            !ring.iter().any(|q| is_side_edge_at(q, x0 + inset)),
            "open-right ring must not have a left edge"
        );
        assert!(
            ring.iter()
                .any(|q| is_side_edge_at(q, x1 - inset - ring_thickness())),
            "open-right ring keeps its inset right edge"
        );
        // Top edge reaches the seam on the open (left) side despite the inset.
        assert!(
            ring.iter()
                .any(|q| is_top_edge(q, 0.0) && q.rect[0].abs() < 0.5),
            "open-right top edge reaches the rail seam"
        );
    }

    #[test]
    fn slot_fill_bleeds_to_the_seam_on_the_open_side_and_insets_the_closed_side() {
        // R1.1: the active fill bleeds to the divider seam on the content-facing
        // side and insets on the closed side; a closed slot insets on both.
        assert_eq!(
            slot_fill_cols(RAIL_COLS, Some(RailSide::Left)),
            (1, RAIL_COLS)
        );
        assert_eq!(
            slot_fill_cols(RAIL_COLS, Some(RailSide::Right)),
            (0, RAIL_COLS - 1)
        );
        assert_eq!(slot_fill_cols(RAIL_COLS, None), (1, RAIL_COLS - 1));
    }

    #[test]
    fn divider_breaks_into_two_segments_across_a_mid_rail_active_span() {
        // A gap in the middle of the band yields exactly two divider segments —
        // above and below — with a hole spanning the gap.
        let ch = CELL.height as f32;
        let gap = (5.0 * ch, 7.0 * ch);
        let segs = rail_divider(
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS.border,
            Some(gap),
        );
        assert_eq!(segs.len(), 2, "two divider segments around the gap");
        let band_end = GRID_ROWS as f32 * ch;
        // Upper segment ends at the gap top; lower starts at the gap bottom.
        assert!(
            segs.iter()
                .any(|q| q.rect[1].abs() < 0.01 && (q.rect[3] - gap.0).abs() < 0.01)
        );
        assert!(
            segs.iter()
                .any(|q| (q.rect[1] - gap.1).abs() < 0.01 && (q.rect[3] - band_end).abs() < 0.01)
        );
        // No segment intrudes into the gap.
        assert!(
            !segs
                .iter()
                .any(|q| q.rect[1] > gap.0 + 0.01 && q.rect[3] < gap.1 - 0.01),
            "no divider inside the active-slot gap"
        );
    }

    #[test]
    fn divider_drops_the_collapsed_segment_when_active_is_edge_flush() {
        // Active slot flush against the top: the upper segment collapses to zero
        // height and is dropped, leaving a single segment below the gap.
        let ch = CELL.height as f32;
        let gap = (0.0, 2.0 * ch);
        let segs = rail_divider(
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS.border,
            Some(gap),
        );
        assert_eq!(segs.len(), 1, "collapsed top segment dropped");
        assert!(
            (segs[0].rect[1] - gap.1).abs() < 0.01,
            "surviving segment starts at the gap bottom"
        );
    }

    #[test]
    fn divider_with_no_active_gap_is_one_full_height_line() {
        let segs = rail_divider(
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS.border,
            None,
        );
        assert_eq!(segs.len(), 1, "no gap → one full line");
        let band_end = GRID_ROWS as f32 * CELL.height as f32;
        assert!(segs[0].rect[1].abs() < 0.01 && (segs[0].rect[3] - band_end).abs() < 0.01);
    }

    #[test]
    fn render_opens_the_active_ring_and_breaks_the_divider_across_its_span() {
        // Integration: a mid-list active tab produces an open (3-edge) active
        // ring and a divider broken into two segments straddling that slot.
        let src = MockSource::new(&["a", "b", "c"], 1);
        let out = render_default(&src);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        let active = layout
            .slots
            .iter()
            .find(|s| s.idx == 1)
            .expect("active slot visible");
        let ch = CELL.height as f32;
        let gap0 = active.start_row as f32 * ch;
        let gap1 = active.end_row as f32 * ch;
        let seam = RAIL_COLS as f32 * CELL.width as f32;
        // Two thin divider segments on the right seam, straddling the gap.
        let divider_segs: Vec<_> = out
            .quads
            .iter()
            .filter(|q| {
                (q.rect[2] - seam).abs() < 0.01 && (q.rect[2] - q.rect[0] - DIVIDER_PX).abs() < 0.01
            })
            .collect();
        assert_eq!(divider_segs.len(), 2, "divider broken into two segments");
        assert!(
            divider_segs
                .iter()
                .all(|q| q.rect[3] <= gap0 + 0.01 || q.rect[1] >= gap1 - 0.01),
            "neither divider segment crosses the active-slot span"
        );
        // The active ring is open: no right (content-facing) side edge over the
        // active slot's rows — checked at the inset position an inactive ring's
        // right edge would occupy (R1.1).
        let x1 = RAIL_COLS as f32 * CELL.width as f32;
        let inset = slot_inset_px();
        assert!(
            !out.quads.iter().any(|q| {
                is_side_edge_at(q, x1 - inset - ring_thickness())
                    && q.rect[1] >= gap0 - 0.01
                    && q.rect[3] <= gap1 + 0.01
            }),
            "active ring has no content-facing right edge"
        );
    }

    // -----------------------------------------------------------------------
    // Active fill / hover fill
    // -----------------------------------------------------------------------

    #[test]
    fn active_slot_is_filled_with_selection_role() {
        let src = MockSource::new(&["a", "b"], 0);
        let out = render_default(&src);
        let active_fill = Color::Rgb(COLORS.active_bg.0, COLORS.active_bg.1, COLORS.active_bg.2);
        // Active slot 0, a body cell inside the inset fill (R1.1 top margin +
        // horizontal inset): row RAIL_TOP_MARGIN_ROWS, col SLOT_LABEL_START_COL.
        let idx = RAIL_TOP_MARGIN_ROWS * RAIL_COLS + SLOT_LABEL_START_COL;
        assert_eq!(
            out.glyphs[idx].attrs.background, active_fill,
            "active slot filled with selection role"
        );
        // Inactive slot 1 body cell sits on the band, not the active fill.
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let band_bg = Color::Rgb(band.0, band.1, band.2);
        let iidx = (RAIL_TOP_MARGIN_ROWS + SLOT_STRIDE) * RAIL_COLS + SLOT_LABEL_START_COL;
        assert_eq!(
            out.glyphs[iidx].attrs.background, band_bg,
            "inactive slot sits on the band (no fill)"
        );
    }

    #[test]
    fn hover_fill_is_subordinate_to_active_fill() {
        let src = MockSource::new(&["a", "b"], 0);
        let mut r = rail();
        r.set_hover(Some(TabHit::Switch(1))); // hover inactive slot 1
        let out = r.render(
            &src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
        );
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let hover = blend_srgb(band, COLORS.active_bg, HOVER_FILL_BLEND);
        let hover_bg = Color::Rgb(hover.0, hover.1, hover.2);
        let active_fill = Color::Rgb(COLORS.active_bg.0, COLORS.active_bg.1, COLORS.active_bg.2);
        let iidx = (RAIL_TOP_MARGIN_ROWS + SLOT_STRIDE) * RAIL_COLS + SLOT_LABEL_START_COL;
        let bg = out.glyphs[iidx].attrs.background;
        assert_eq!(
            bg, hover_bg,
            "hovered inactive slot gets the subordinate fill"
        );
        assert_ne!(bg, active_fill, "hover fill differs from active fill");
    }

    // -----------------------------------------------------------------------
    // Luma-contrast regression across every built-in theme (mandate)
    // -----------------------------------------------------------------------

    /// The rail rings must clear this luminance-delta floor against the band in
    /// every built-in theme — the same guard the horizontal bar uses, extended
    /// to the rail's ring derivations (F4-V2 mandate). Pins the v1.3 root cause:
    /// the frame-side `border` role gives ~0 delta (invisible dark-on-dark);
    /// TEXT-side roles clear the floor by construction.
    const MIN_RING_BAND_LUMA_DELTA: f64 = 0.03;

    #[test]
    fn every_builtin_theme_keeps_rail_rings_visible_against_the_band() {
        use crate::theme::relative_luminance;
        for theme in crate::theme::all() {
            let colors = TabBarColors {
                foreground: theme.foreground,
                background: theme.background,
                inactive: theme.inactive,
                active_bg: theme.selection,
                border: theme.border,
            };
            let band = blend_srgb(colors.background, colors.inactive, BAND_BLEND);
            let (active_ring, inactive_ring) = ring_colors(colors, band);
            let band_luma = relative_luminance(band);
            let active_delta = (relative_luminance(active_ring) - band_luma).abs();
            let inactive_delta = (relative_luminance(inactive_ring) - band_luma).abs();
            assert!(
                active_delta >= MIN_RING_BAND_LUMA_DELTA,
                "{}: rail active ring luma delta {active_delta:.4} < {MIN_RING_BAND_LUMA_DELTA} \
                 — ring invisible on the band",
                theme.name
            );
            assert!(
                inactive_delta >= MIN_RING_BAND_LUMA_DELTA,
                "{}: rail inactive ring luma delta {inactive_delta:.4} < {MIN_RING_BAND_LUMA_DELTA} \
                 — ring invisible on the band",
                theme.name
            );
        }
    }

    // -----------------------------------------------------------------------
    // Pure helpers
    // -----------------------------------------------------------------------

    #[test]
    fn blend_srgb_is_endpoint_exact() {
        let a = (0x10, 0x20, 0x30);
        let b = (0xF0, 0xE0, 0xD0);
        assert_eq!(blend_srgb(a, b, 0.0), a);
        assert_eq!(blend_srgb(a, b, 1.0), b);
        assert_eq!(blend_srgb(a, b, -1.0), a, "t clamps low");
        assert_eq!(blend_srgb(a, b, 2.0), b, "t clamps high");
    }

    #[test]
    fn constants_match_option_b() {
        assert_eq!(SLOT_ROWS, 2, "Option B: 2-cell-tall slots");
        assert_eq!(SLOT_GAP, 1, "Option B: 1-cell gap");
        assert_eq!(SLOT_STRIDE, 3);
        // R1.1 polish constants.
        assert_eq!(NEW_TAB_ROWS, 1, "R1.1: lightweight 1-cell + affordance");
        assert_eq!(RAIL_TOP_MARGIN_ROWS, 1, "R1.1: first-slot top margin");
        assert_eq!(SLOT_INSET_COLS, 1, "R1.1: slot ring/fill inset");
        assert_eq!(SLOT_LABEL_START_COL, SLOT_INSET_COLS + RAIL_LABEL_PAD);
    }
}
