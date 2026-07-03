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
//! ## Visual language — "Phosphor Flat" (F4-RESKIN, operator-ruled A+C)
//! Identical treatment to the horizontal [`super::tab_bar`], sharing the same
//! [`super::tab_chrome`] color module (F4V2-NF2 — the "promote shared treatment
//! fns to a shared chrome location" follow-up is now done). The old outlined-box
//! language (per-slot rings, the rail↔content divider) was **deleted, not
//! bypassed** — the operator rejected it as "hacked together / cheap". The rail
//! is now an invisible container; only the active slot is a drawn object, and
//! hierarchy comes from luminance:
//! - **ACTIVE** — a warm `selection` fill (the bloom-off fallback) that bleeds to
//!   the content seam, plus a bright bold `foreground` label brightened above the
//!   bloom threshold so it auto-halos through `bloom.wgsl`.
//! - **INACTIVE** — bare `inactive`-role labels on the wallpaper-through
//!   background (no fill), dimmed along a phosphor-persistence luminance ramp
//!   keyed on distance from the active tab.
//! - **HOVER** — the label warms one tier toward the active label and gains a
//!   whisper of the selection fill.
//!
//! The whole `rail_cols × grid_rows` region paints the F4-P1 **panel tint**
//! (Layer 1 of ODP-1) on every non-active cell — inactive slots and the
//! inter-slot gaps recede into the panel surface. There are **no chrome quads in
//! this widget** ([`TabRailOutput::quads`] is emitted empty); the unified panel
//! wash + seam are separate background-segment quads built by
//! [`super::tab_panel`] and spliced in by the integration layer.
//!
//! ## Scope
//! `left` placement is live; `right` is wired but gated behind
//! [`crate::settings::TabBarPlacement::effective`] until the P-RIGHT packet. The
//! band width, slot height, and inter-slot gap are the live
//! `ODYTTY_TAB_RAIL_WIDTH` / `ODYTTY_TAB_RAIL_SLOT_ROWS` / `ODYTTY_TAB_RAIL_GAP`
//! knobs (resolved by the integration layer into [`RailGeom`] + `rail_cols`);
//! auto-hide and drag-resize are later packets.

use super::tab_bar::{TabBarColors, TabBarSource, TabHit};
use super::tab_chrome;
use super::*;
use crate::core::Attrs;
use crate::theme::Srgb;

// ---------------------------------------------------------------------------
// Geometry constants (Option B — padded slots)
// ---------------------------------------------------------------------------

/// Default rows each tab slot occupies (Option B: 2-cell-tall padded slots —
/// label row plus a breathing row). F4-P1 lifts this to the
/// `ODYTTY_TAB_RAIL_SLOT_ROWS` knob via [`RailGeom::slot_rows`]; this const is
/// the default and the fixed value the unit tests express positions against.
const DEFAULT_SLOT_ROWS: usize = 2;
/// Default band-fill gap (in rows) between adjacent slots. F4-P1 lifts this to
/// the `ODYTTY_TAB_RAIL_GAP` knob via [`RailGeom::slot_gap`]; the top margin
/// before the first slot follows it.
const DEFAULT_SLOT_GAP: usize = 1;
/// Rows the bottom `+` new-tab slot occupies (R1.1: a lightweight 1-cell
/// affordance — no ring at rest, so it never competes with a real tab slot).
const NEW_TAB_ROWS: usize = 1;

/// Runtime rail slot geometry (F4-P1 knobs): how many rows each slot occupies
/// and the inter-slot gap. Threaded through [`TabRail::render`] /
/// [`TabRail::hit_test`] so the operator can tune them live; the widget owns no
/// settings, the integration layer resolves these from `Settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RailGeom {
    /// Rows per slot (`1` = compact list, `2` = padded with a breathing row;
    /// the label is always a single centered line, F4-P4).
    pub(super) slot_rows: usize,
    /// Rows of empty band between adjacent slots (and the top margin).
    pub(super) slot_gap: usize,
}

impl Default for RailGeom {
    fn default() -> Self {
        Self {
            slot_rows: DEFAULT_SLOT_ROWS,
            slot_gap: DEFAULT_SLOT_GAP,
        }
    }
}

impl RailGeom {
    /// Row stride from one slot's top to the next slot's top.
    fn stride(self) -> usize {
        self.slot_rows + self.slot_gap
    }
    /// Top margin (rows) before the first slot — follows the inter-slot gap so
    /// the space above the first slot reads like the gaps between slots.
    fn top_margin(self) -> usize {
        self.slot_gap
    }
}
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

/// Non-label columns a slot reserves around its title (F4-P4): the left label
/// start (`SLOT_LABEL_START_COL`) plus the right inset column and the close-`×`
/// cell. The label's usable inner budget is therefore
/// `rail_cols - RAIL_LABEL_CHROME_COLS`. The auto-width resolver adds this to
/// the longest title so a title fits on one line without truncation; keeping it
/// here (next to the geometry it derives from) is the single source of truth so
/// the integration layer never re-derives the chrome padding.
pub(super) const RAIL_LABEL_CHROME_COLS: usize = SLOT_LABEL_START_COL + SLOT_INSET_COLS + 1;

// The rail band width is now the live `ODYTTY_TAB_RAIL_WIDTH` knob
// (`Settings::rail_width_cols`, default 16, clamp `[8, 32]`) resolved by the
// integration layer and passed in as `rail_cols` — the widget owns no fixed
// width.

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

/// Output from [`TabRail::render`]: the fully-painted region glyphs plus a
/// (currently always empty) chrome-quad list.
#[derive(Debug, Default)]
pub(super) struct TabRailOutput {
    /// Solid pixel-space quads. Phosphor Flat draws no chrome quads (rings and
    /// the divider were deleted), so this is emitted **empty**; the channel is
    /// retained so a future rail↔content divider could be re-added without
    /// changing the integration signature.
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
    /// The row the single-line label (and its `×`) sits on — the slot's
    /// vertically-centered row (F4-P4). For the default 2-row slot the centre
    /// rounds to the slot's first row, leaving the trailing row as breathing
    /// space; taller slots centre exactly.
    label_row: usize,
    /// The single-line label, already truncated with `…` to the inner column
    /// budget (F4-P4 — the rail never wraps to a second line).
    label: String,
    /// `(row, col)` of the `×` close glyph (on the label row, at the slot's
    /// inset top-right), or `None` when the rail is too narrow.
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
    /// - `origin_px` / `cell` — pixel geometry for chrome quads; Phosphor Flat
    ///   emits none, so `origin_px` is unused and `cell` only feeds the degenerate
    ///   guard (retained in the signature so a future divider needs no call-site
    ///   change).
    /// - `placement` — `Left` (active fill bleeds to the right seam) or `Right`.
    /// - `colors` — the theme-role colors (see [`TabBarColors`]).
    ///
    /// Returns a fully-painted `rail_cols × grid_rows` region (`glyphs`) and an
    /// empty quad list — the whole Phosphor Flat treatment is cell backgrounds +
    /// label attributes (shared [`super::tab_chrome`]).
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
        geom: RailGeom,
        panel_strength: f32,
    ) -> TabRailOutput {
        let _ = origin_px;
        if rail_cols == 0 || grid_rows == 0 || cell.width == 0 || cell.height == 0 {
            return TabRailOutput::default();
        }
        let layout = compute_rail_layout(source, rail_cols, grid_rows, geom);
        let active_idx = source.active_tab();

        // Phosphor Flat palette (shared treatment; theme roles only). F4-P1: the
        // resting-region surface is the panel tint (Layer 1 of ODP-1); the wash
        // + seam quads are added separately in the background segment by the
        // integration layer. `panel_strength = 0` collapses the tint to the raw
        // background (the pre-panel bare-labels look).
        let panel_surface = rgb(tab_chrome::panel_tint(colors, panel_strength));
        let active_fill = rgb(tab_chrome::active_fill(colors));
        let active_lbl = rgb(tab_chrome::active_label(colors));
        let hover_fill = rgb(tab_chrome::hover_fill(colors));
        let hover_lbl = rgb(tab_chrome::hover_label(colors));
        let dim_plus = rgb(colors.inactive);

        // The whole region starts as the panel surface; inactive slots and the
        // inter-slot gaps recede into it (no per-slot geometry, no divider).
        let mut cells =
            vec![blank_glyph(0, 0, panel_surface, panel_surface); rail_cols * grid_rows];
        for (i, glyph) in cells.iter_mut().enumerate() {
            glyph.row = i / rail_cols;
            glyph.col = i % rail_cols;
        }

        for slot in &layout.slots {
            let is_active = slot.idx == active_idx;
            let is_hovered = is_slot_hovered(self.hover, slot.idx);
            let (slot_bg, label_fg, bold) = if is_active {
                (active_fill, active_lbl, true)
            } else if is_hovered {
                (hover_fill, hover_lbl, false)
            } else {
                let distance = slot.idx.abs_diff(active_idx);
                (
                    panel_surface,
                    rgb(tab_chrome::inactive_label(colors, distance)),
                    false,
                )
            };
            // Only the active/hover slots carry a fill; inactive slots stay
            // wallpaper-through (the region default). The active fill bleeds to
            // the content seam so it reads as fused to the body; hover insets on
            // both sides (subordinate).
            if is_active || is_hovered {
                let slot_seam = if is_active { Some(placement) } else { None };
                let (fill_c0, fill_c1) = slot_fill_cols(rail_cols, slot_seam);
                for row in slot.start_row..slot.end_row.min(grid_rows) {
                    for col in fill_c0..fill_c1.min(rail_cols) {
                        cells[row * rail_cols + col].attrs.background = slot_bg;
                    }
                }
            }
            // Label glyphs — a single line on the slot's vertically-centered row
            // (F4-P4; no wrapping).
            let mut la = Attrs::default();
            la.foreground = label_fg;
            la.background = slot_bg;
            if bold {
                la.set_bold(true);
            }
            let row = slot.label_row;
            if row < slot.end_row && row < grid_rows {
                for (i, ch) in slot.label.chars().enumerate() {
                    let col = SLOT_LABEL_START_COL + i;
                    if col < rail_cols {
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

        // New-tab `+` slot — a lightweight 1-cell affordance: a centered dim `+`
        // on the bare wallpaper at rest, brightening (and gaining a whisper fill)
        // only when hovered, so it never competes with a real tab slot.
        if let Some((nt_start, nt_end)) = layout.new_tab_rows {
            let is_hovered = matches!(self.hover, Some(TabHit::NewTab));
            let (nt_bg, nt_fg) = if is_hovered {
                (hover_fill, active_lbl)
            } else {
                (panel_surface, dim_plus)
            };
            if is_hovered {
                let (fill_c0, fill_c1) = slot_fill_cols(rail_cols, None);
                for row in nt_start..nt_end.min(grid_rows) {
                    for col in fill_c0..fill_c1.min(rail_cols) {
                        cells[row * rail_cols + col].attrs.background = nt_bg;
                    }
                }
            }
            let mut a = Attrs::default();
            a.foreground = nt_fg;
            a.background = nt_bg;
            // F4-P1 floating-`+` fix: left-align the `+` at the label column (an
            // Arc-style "+ new tab" row) rather than centering it, and anchor it
            // one gap below the last slot's LABEL row (compute_rail_layout), so
            // it reads as the next list item instead of drifting far below.
            let prow = nt_start + (nt_end - nt_start) / 2;
            let pcol = SLOT_LABEL_START_COL.min(rail_cols.saturating_sub(1));
            if prow < grid_rows && pcol < rail_cols {
                let g = &mut cells[prow * rail_cols + pcol];
                g.ch = '+';
                g.attrs = a;
            }
        }

        // Overflow indicators (informational-only in R1): `▲N` in row 0, `▼N` in
        // the last row. Painted as dim-foreground glyphs over the wallpaper.
        if let Some(hidden) = layout.overflow_above {
            paint_overflow_indicator(&mut cells, 0, rail_cols, grid_rows, '▲', hidden, dim_plus);
        }
        if let Some(hidden) = layout.overflow_below {
            paint_overflow_indicator(
                &mut cells,
                grid_rows - 1,
                rail_cols,
                grid_rows,
                '▼',
                hidden,
                dim_plus,
            );
        }

        // Phosphor Flat draws no chrome quads — the treatment is entirely cell
        // backgrounds + label attributes (see the module docs).
        TabRailOutput {
            quads: Vec::new(),
            glyphs: cells,
        }
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
        geom: RailGeom,
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

        let layout = compute_rail_layout(source, rail_cols, grid_rows, geom);

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
    geom: RailGeom,
) -> RailLayout {
    let mut layout = RailLayout::default();
    if rail_cols == 0 || grid_rows == 0 {
        return layout;
    }
    let top_margin = geom.top_margin();
    let stride = geom.stride();
    let tab_count = source.tab_count();
    if tab_count == 0 {
        if grid_rows >= top_margin + NEW_TAB_ROWS {
            layout.new_tab_rows = Some((top_margin, top_margin + NEW_TAB_ROWS));
        }
        return layout;
    }

    // Rows needed to show the top margin, every tab plus the `+` slot with no
    // scroll. Each tab consumes `stride` (slot + trailing gap); the last
    // trailing gap becomes the gap before the `+` slot, so the total is
    // margin + tabs*stride + NEW_TAB_ROWS (R1.1 adds the leading top margin).
    let total_needed = top_margin + tab_count * stride + NEW_TAB_ROWS;

    if total_needed <= grid_rows {
        for i in 0..tab_count {
            let start = top_margin + i * stride;
            layout
                .slots
                .push(build_slot(source, i, start, rail_cols, geom));
        }
        // F4-P1 floating-`+` fix: anchor the `+` `slot_gap` rows below the last
        // slot's LABEL row rather than a full stride below its start, so a
        // single-line label doesn't leave a trailing blank slot row + gap
        // stranding the `+` far below. The label is one row (F4-P4), so this is
        // `label_row + 1 + gap` (arithmetically identical to the old single-line
        // case at slot_rows = 1).
        let nt_start = layout
            .slots
            .last()
            .map(|last| last.label_row + 1 + geom.slot_gap)
            .unwrap_or(top_margin);
        layout.new_tab_rows = Some((nt_start, nt_start + NEW_TAB_ROWS));
        return layout;
    }

    // Overflow: scroll to keep the active tab visible. Compute a conservative
    // visible capacity assuming both indicator rows are present, so the greedy
    // placement below can never overrun the region.
    let active = source.active_tab().min(tab_count - 1);
    let band = grid_rows.saturating_sub(2);
    // n slots need n*slot_rows + (n-1)*slot_gap = n*stride - slot_gap rows.
    let capacity = ((band + geom.slot_gap) / stride).max(1).min(tab_count);
    let max_first = tab_count - capacity;
    let first = active.saturating_sub(capacity / 2).min(max_first);

    // When scrolled below the top, row 0 carries the `▲` indicator; when at the
    // top, apply the same top margin as the no-scroll case (R1.1).
    let top = if first > 0 { 1 } else { top_margin };
    let mut placed = 0usize;
    let mut row = top;
    for j in 0..(tab_count - first) {
        let end = row + geom.slot_rows;
        // Always keep the last region row free for a potential `▼`.
        if end > grid_rows.saturating_sub(1) {
            break;
        }
        layout
            .slots
            .push(build_slot(source, first + j, row, rail_cols, geom));
        placed += 1;
        row += stride;
    }

    layout.overflow_above = (first > 0).then_some(first);
    layout.overflow_below = (first + placed < tab_count).then_some(tab_count - first - placed);
    // No `+` slot in overflow mode — the rail is full.
    layout
}

/// Build one slot at `start_row` with a single-line, ellipsis-truncated label
/// on the slot's vertically-centered row (F4-P4).
fn build_slot(
    source: &dyn TabBarSource,
    idx: usize,
    start_row: usize,
    rail_cols: usize,
    geom: RailGeom,
) -> RailSlot {
    let end_row = start_row + geom.slot_rows;
    // Vertically centre the single label line within the slot. `(rows - 1) / 2`
    // rounds a 2-row slot's centre to its first row (breathing row below) and
    // centres taller slots exactly; a 1-row slot maps to its only row.
    let label_row = start_row + geom.slot_rows.saturating_sub(1) / 2;
    // The `×` shares the label row at the slot's top-right, INSIDE the inset
    // (R1.1) so it sits within the bounded box, not against the rail edge. It
    // occupies the last inset column, never colliding with the label inner area.
    let close_col = rail_cols.saturating_sub(SLOT_INSET_COLS + 1);
    let close_cell = (close_col > SLOT_LABEL_START_COL).then_some((label_row, close_col));
    // Label inner budget: from `SLOT_LABEL_START_COL` up to (but excluding) the
    // close cell, i.e. the inset box minus the left label pad and the close cell.
    let inner = close_col.saturating_sub(SLOT_LABEL_START_COL);
    let label = if inner == 0 {
        String::new()
    } else {
        truncate_label(source.tab_title(idx), inner)
    };
    RailSlot {
        idx,
        start_row,
        end_row,
        label_row,
        label,
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

/// Shorthand: an sRGB tuple (from a [`super::tab_chrome`] treatment fn) as a
/// cell-attribute [`Color`].
fn rgb(c: Srgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

fn is_slot_hovered(hover: Option<TabHit>, idx: usize) -> bool {
    matches!(hover, Some(TabHit::Switch(i) | TabHit::Close(i)) if i == idx)
}

/// Truncate `s` to a single line of at most `inner` columns, ending an
/// overflowing title with `…` (F4-P4 — the rail never wraps to a second line;
/// the auto-width mode grows the rail to fit, and past the cap the title
/// ellipsizes). Leading/trailing whitespace is stripped. Each Unicode scalar
/// counts as one column — correct for the ASCII-heavy titles typical of
/// terminal tabs (the wide-glyph display-width caveat is F4P-NF1, out of scope).
fn truncate_label(s: &str, inner: usize) -> String {
    if inner == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.trim().chars().collect();
    if chars.len() <= inner {
        return chars.into_iter().collect();
    }
    // Overflow: keep `inner - 1` scalars and append the ellipsis.
    let mut line: String = chars[..inner.saturating_sub(1)].iter().collect();
    line.push('…');
    line
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::relative_luminance;

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

    // The default geometry the positional assertions are expressed against
    // (F4-P1 lifted the raw geometry consts into `RailGeom`). Mirrors
    // `RailGeom::default()` = the `DEFAULT_SLOT_*` values.
    const GEOM: RailGeom = RailGeom {
        slot_rows: DEFAULT_SLOT_ROWS,
        slot_gap: DEFAULT_SLOT_GAP,
    };
    const SLOT_ROWS: usize = DEFAULT_SLOT_ROWS;
    const SLOT_GAP: usize = DEFAULT_SLOT_GAP;
    const SLOT_STRIDE: usize = SLOT_ROWS + SLOT_GAP;
    const RAIL_TOP_MARGIN_ROWS: usize = SLOT_GAP;
    // Panel strength for the shared render helpers. `0.0` collapses the panel
    // tint to the theme background so the wallpaper-through assertions stay
    // expressed against `wallpaper_background`; the panel surface at a live
    // strength has its own test.
    const PANEL_STRENGTH: f32 = 0.0;

    const COLORS: TabBarColors = TabBarColors {
        foreground: (0xCC, 0xCC, 0xCC),
        background: (0x10, 0x10, 0x10),
        inactive: (0x66, 0x66, 0x66),
        active_bg: (0x40, 0x60, 0x90),
    };

    fn rail() -> TabRail {
        TabRail::default()
    }

    fn hovered_rail(hit: TabHit) -> TabRail {
        let mut r = TabRail::default();
        r.set_hover(Some(hit));
        r
    }

    fn render_with(r: &TabRail, src: &dyn TabBarSource) -> TabRailOutput {
        r.render(
            src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
            GEOM,
            PANEL_STRENGTH,
        )
    }

    fn render_default(src: &dyn TabBarSource) -> TabRailOutput {
        render_with(&rail(), src)
    }

    /// Relative luminance of a cell-attribute color.
    fn luma(color: Color) -> f64 {
        match color {
            Color::Rgb(r, g, b) => relative_luminance((r, g, b)),
            other => panic!("expected an explicit Rgb color, got {other:?}"),
        }
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
        rail().hit_test(x, y, src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL, GEOM)
    }

    /// Background of a region cell.
    fn bg_at(out: &TabRailOutput, row: usize, col: usize) -> Color {
        out.glyphs[row * RAIL_COLS + col].attrs.background
    }

    // -----------------------------------------------------------------------
    // Region shape + no chrome quads
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
    fn render_emits_no_chrome_quads() {
        // Phosphor Flat draws no rings and no divider (fails-before: the outline
        // era emitted per-slot rings + a rail↔content divider).
        for src in [
            MockSource::empty(),
            MockSource::new(&["a"], 0),
            MockSource::new(&["a", "b", "c"], 1),
        ] {
            let out = render_default(&src);
            assert!(
                out.quads.is_empty(),
                "no chrome quads (got {})",
                out.quads.len()
            );
        }
    }

    #[test]
    fn empty_region_for_zero_width_or_height() {
        let src = MockSource::new(&["a"], 0);
        let out = rail().render(
            &src,
            0,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
            GEOM,
            PANEL_STRENGTH,
        );
        assert!(out.glyphs.is_empty(), "zero rail_cols → empty");
        let out = rail().render(
            &src,
            RAIL_COLS,
            0,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
            GEOM,
            PANEL_STRENGTH,
        );
        assert!(out.glyphs.is_empty(), "zero grid_rows → empty");
    }

    #[test]
    fn inactive_and_gap_cells_are_wallpaper_through() {
        // Inactive slots and the inter-slot gaps recede into the wallpaper-through
        // background (no band fill). Replaces the outline era's opaque-band
        // invariant.
        let src = MockSource::new(&["a", "b"], 0);
        let out = render_default(&src);
        let wallpaper = rgb(tab_chrome::wallpaper_background(COLORS));
        // Gap row after slot 0 (top margin + SLOT_ROWS).
        let gap_row = RAIL_TOP_MARGIN_ROWS + SLOT_ROWS;
        assert_eq!(
            bg_at(&out, gap_row, 0),
            wallpaper,
            "inter-slot gap is wallpaper"
        );
        // Inactive slot 1 label cell is wallpaper (no fill).
        let inactive_start = RAIL_TOP_MARGIN_ROWS + SLOT_STRIDE;
        assert_eq!(
            bg_at(&out, inactive_start, SLOT_LABEL_START_COL),
            wallpaper,
            "inactive slot is wallpaper-through"
        );
    }

    // -----------------------------------------------------------------------
    // Layout: row-stacked slots (unchanged engine)
    // -----------------------------------------------------------------------

    #[test]
    fn zero_tabs_shows_only_the_new_tab_slot() {
        let src = MockSource::empty();
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
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
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        assert_eq!(layout.slots.len(), 3, "three tab slots");
        for (i, slot) in layout.slots.iter().enumerate() {
            let start = RAIL_TOP_MARGIN_ROWS + i * SLOT_STRIDE;
            assert_eq!(slot.start_row, start, "slot {i} start row");
            assert_eq!(slot.end_row, start + SLOT_ROWS, "slot {i} end row");
        }
        // F4-P1 floating-`+` fix: the `+` anchors one gap below the last slot's
        // LABEL row (single-line, F4-P4), NOT a full stride below the slot start
        // (which left a trailing blank slot row stranding it).
        let last = layout.slots.last().unwrap();
        let nt_start = last.label_row + 1 + SLOT_GAP;
        assert_eq!(
            layout.new_tab_rows,
            Some((nt_start, nt_start + NEW_TAB_ROWS)),
            "the + slot follows the last tab's label row"
        );
        // Concretely, with single-line labels the `+` sits ABOVE the old
        // stride-based position (removing the stranding blank row).
        assert!(
            nt_start < RAIL_TOP_MARGIN_ROWS + 3 * SLOT_STRIDE,
            "the + is pulled up to the label row"
        );
        assert!(layout.overflow_above.is_none() && layout.overflow_below.is_none());
    }

    #[test]
    fn floating_plus_is_left_aligned_at_the_label_column() {
        // F4-P1 floating-`+` fix: the `+` glyph left-aligns at the label column
        // (an Arc-style "+ new tab" row), not centered at rail_cols/2.
        let src = MockSource::new(&["a", "b"], 0);
        let out = render_default(&src);
        let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
        assert_eq!(
            plus.col, SLOT_LABEL_START_COL,
            "the + is left-aligned at the label column, not centered"
        );
        // And it sits immediately below the last label row + gap (no stranding).
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        let last = layout.slots.last().unwrap();
        assert_eq!(
            plus.row,
            last.label_row + 1 + SLOT_GAP,
            "the + row anchors one gap below the last label row"
        );
    }

    #[test]
    fn compact_single_row_slots_hold_more_tabs_and_the_plus_follows() {
        // F4-P1 TAB_RAIL_SLOT_ROWS = 1: compact list, no wrap, more tabs fit.
        let geom = RailGeom {
            slot_rows: 1,
            slot_gap: 1,
        };
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, geom);
        assert_eq!(layout.slots.len(), 3);
        for (i, slot) in layout.slots.iter().enumerate() {
            assert_eq!(slot.end_row - slot.start_row, 1, "slot {i} is one row tall");
        }
        // stride = 2 (1 row + 1 gap); `+` one gap below the single label row.
        let last = layout.slots.last().unwrap();
        let (nt_start, _) = layout.new_tab_rows.expect("+ slot present");
        assert_eq!(nt_start, last.start_row + 1 + geom.slot_gap);
    }

    #[test]
    fn zero_gap_removes_the_top_margin_and_inter_slot_space() {
        // F4-P1 TAB_RAIL_GAP = 0: first slot kisses the top, slots are flush.
        let geom = RailGeom {
            slot_rows: 2,
            slot_gap: 0,
        };
        let src = MockSource::new(&["a", "b"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, geom);
        assert_eq!(layout.slots[0].start_row, 0, "no top margin at gap 0");
        assert_eq!(
            layout.slots[1].start_row, layout.slots[0].end_row,
            "slots are flush at gap 0"
        );
    }

    #[test]
    fn resting_region_paints_the_panel_tint_at_strength() {
        // F4-P1: at a live strength the resting rail cells paint the panel tint
        // (Layer 1), distinct from the raw background; strength 0 collapses back.
        let src = MockSource::new(&["a", "b"], 0);
        let out = rail().render(
            &src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
            GEOM,
            0.5,
        );
        // An inter-slot gap cell is the panel surface.
        let gap_row = RAIL_TOP_MARGIN_ROWS + SLOT_ROWS;
        assert_eq!(
            bg_at(&out, gap_row, 0),
            rgb(tab_chrome::panel_tint(COLORS, 0.5)),
            "resting cells paint the panel tint at strength 0.5"
        );
        assert_ne!(
            bg_at(&out, gap_row, 0),
            rgb(tab_chrome::wallpaper_background(COLORS)),
            "the panel tint differs from the raw background"
        );
    }

    #[test]
    fn slots_never_overlap() {
        let src = MockSource::new(&["a", "b", "c", "d"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        for w in layout.slots.windows(2) {
            assert!(w[0].end_row <= w[1].start_row, "slots must not overlap");
        }
    }

    #[test]
    fn first_slot_has_a_top_margin_and_bare_wallpaper_above_it() {
        let src = MockSource::new(&["a", "b"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        assert_eq!(
            layout.slots[0].start_row, RAIL_TOP_MARGIN_ROWS,
            "first slot begins below the top margin"
        );
        // Row 0 (above the first slot) is bare wallpaper.
        let out = render_default(&src);
        let wallpaper = rgb(tab_chrome::wallpaper_background(COLORS));
        for col in 0..RAIL_COLS {
            assert_eq!(
                bg_at(&out, 0, col),
                wallpaper,
                "top-margin row is wallpaper"
            );
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
    fn active_label_is_bright_bold_and_clears_the_bloom_threshold() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        let z = out.glyphs.iter().find(|g| g.ch == 'z').expect("'z' glyph");
        assert!(z.attrs.bold(), "active tab label is bold");
        assert_eq!(
            z.attrs.foreground,
            rgb(tab_chrome::active_label(COLORS)),
            "active label uses the brightened treatment color"
        );
        assert!(
            luma(z.attrs.foreground) >= 0.7,
            "active label luma {} must clear the bloom threshold 0.7",
            luma(z.attrs.foreground)
        );
    }

    #[test]
    fn inactive_labels_dim_along_the_phosphor_ramp_with_distance() {
        // Active = tab 0; tabs 1..3 at increasing distance, monotonically
        // non-increasing luminance.
        let src = MockSource::new(&["a", "b", "c", "d"], 0);
        let out = render_default(&src);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        let mut prev = f64::INFINITY;
        for slot in layout.slots.iter().skip(1) {
            let l = luma(bg_label_fg(&out, slot.start_row));
            assert!(
                l <= prev + 1e-9,
                "slot {} brighter than a nearer tab",
                slot.idx
            );
            prev = l;
        }
        // Nearest inactive label (distance 1) is the full inactive role.
        assert_eq!(
            bg_label_fg(&out, layout.slots[1].start_row),
            rgb(tab_chrome::inactive_label(COLORS, 1)),
        );
    }

    /// Foreground of the first label glyph of the slot whose top row is `row`.
    fn bg_label_fg(out: &TabRailOutput, row: usize) -> Color {
        out.glyphs[row * RAIL_COLS + SLOT_LABEL_START_COL]
            .attrs
            .foreground
    }

    #[test]
    fn inactive_label_is_not_bold() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        let b = out.glyphs.iter().find(|g| g.ch == 'b').expect("'b' glyph");
        assert!(!b.attrs.bold(), "inactive label not bold");
    }

    #[test]
    fn long_title_truncates_on_one_line_with_ellipsis() {
        // F4-P4: no wrapping — an overflowing title stays a single line and ends
        // with `…` (fails-before: the pre-P4 rail wrapped to a 2nd line).
        let long = "a-very-long-terminal-title-that-will-not-fit";
        let line = truncate_label(long, 14);
        assert_eq!(line.chars().count(), 14, "single line within inner budget");
        assert!(line.ends_with('…'), "overflow ends with …");
        assert!(!line.contains('\n'), "one line, never wraps");
    }

    #[test]
    fn short_title_is_single_line_untruncated() {
        assert_eq!(
            truncate_label("vim", 14),
            "vim".to_string(),
            "short title, one line, no …"
        );
        // Exactly-fitting title is not ellipsized.
        assert_eq!(truncate_label("0123456789abcd", 14), "0123456789abcd");
    }

    #[test]
    fn label_is_single_row_and_vertically_centered_in_the_slot() {
        // F4-P4: the 2-row padded slot keeps its breathing room but the label is
        // one centered line (fails-before: labels used to fill both rows).
        let src = MockSource::new(&["vim", "bash"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        for slot in &layout.slots {
            // Centre of a 2-row slot rounds to its first row (breathing row below).
            assert_eq!(
                slot.label_row,
                slot.start_row + (SLOT_ROWS - 1) / 2,
                "label sits on the vertically-centered row"
            );
        }
        // Only one row of a slot carries label glyphs.
        let out = render_default(&src);
        let active = &layout.slots[0];
        let label_row_glyphs = (0..RAIL_COLS)
            .filter(|&c| {
                let g = out.glyphs[active.label_row * RAIL_COLS + c];
                g.ch != ' ' && g.ch != '×'
            })
            .count();
        assert!(label_row_glyphs >= 3, "label row carries the title 'vim'");
        // The other slot row has no title glyphs.
        let other_row = active.start_row + SLOT_ROWS - 1;
        if other_row != active.label_row {
            let other_glyphs = (0..RAIL_COLS)
                .filter(|&c| out.glyphs[other_row * RAIL_COLS + c].ch != ' ')
                .count();
            assert_eq!(other_glyphs, 0, "the breathing row carries no glyphs");
        }
    }

    #[test]
    fn auto_width_chrome_matches_the_label_budget() {
        // F4-P4: the integration layer's auto-width padding must equal exactly
        // the columns a slot reserves around its title, so a title of length L
        // fits without truncation when rail_cols = L + RAIL_LABEL_CHROME_COLS.
        let title_len = 6usize;
        let rail_cols = title_len + RAIL_LABEL_CHROME_COLS;
        let src = MockSource::new(&["abcdef"], 0);
        let layout = compute_rail_layout(&src, rail_cols, GRID_ROWS, GEOM);
        assert_eq!(
            layout.slots[0].label, "abcdef",
            "title of length {title_len} fits without … at rail_cols = L + chrome"
        );
        // One cell narrower forces truncation, proving the padding is not slack.
        let tight = compute_rail_layout(&src, rail_cols - 1, GRID_ROWS, GEOM);
        assert!(
            tight.slots[0].label.ends_with('…'),
            "one column narrower truncates"
        );
    }

    // -----------------------------------------------------------------------
    // Fill / hover treatment
    // -----------------------------------------------------------------------

    #[test]
    fn active_slot_filled_with_selection_bleeding_to_the_content_seam() {
        // Active fill = `selection`, covering the slot from the inset to the
        // content seam (Left placement → to the right edge). Inactive slots have
        // no fill.
        let src = MockSource::new(&["aaa", "bbb"], 0);
        let out = render_default(&src);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        let active_fill = rgb(tab_chrome::active_fill(COLORS));
        let active = &layout.slots[0];
        // Label cell + the content-seam cell (last col) both carry the fill.
        assert_eq!(
            bg_at(&out, active.start_row, SLOT_LABEL_START_COL),
            active_fill,
            "active slot filled with the selection role"
        );
        assert_eq!(
            bg_at(&out, active.start_row, RAIL_COLS - 1),
            active_fill,
            "active fill bleeds to the content seam"
        );
        // The rail-edge inset column stays wallpaper (Left placement insets left).
        assert_eq!(
            bg_at(&out, active.start_row, 0),
            rgb(tab_chrome::wallpaper_background(COLORS)),
            "rail-edge inset column is wallpaper"
        );
    }

    #[test]
    fn hovered_inactive_slot_gets_a_whisper_fill_and_lifted_label() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        let hovered = &layout.slots[2];
        let out = render_with(&hovered_rail(TabHit::Switch(hovered.idx)), &src);
        assert_eq!(
            bg_at(&out, hovered.start_row, SLOT_LABEL_START_COL),
            rgb(tab_chrome::hover_fill(COLORS)),
            "hovered slot gains the whisper fill"
        );
        let hover_luma = luma(bg_label_fg(&out, hovered.start_row));
        let rest_luma = luma(rgb(tab_chrome::inactive_label(COLORS, hovered.idx)));
        let active_luma = luma(rgb(tab_chrome::active_label(COLORS)));
        assert!(hover_luma > rest_luma, "hover lifts the label above rest");
        assert!(
            hover_luma < active_luma,
            "hover stays subordinate to active"
        );
        assert_ne!(
            bg_at(&out, hovered.start_row, SLOT_LABEL_START_COL),
            rgb(tab_chrome::active_fill(COLORS)),
            "hover fill is a whisper, not the active fill"
        );
    }

    // -----------------------------------------------------------------------
    // Close × and new-tab +
    // -----------------------------------------------------------------------

    #[test]
    fn active_slot_renders_close_glyph_in_inset_top_right_cell() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        let row = RAIL_TOP_MARGIN_ROWS;
        let col = RAIL_COLS - SLOT_INSET_COLS - 1;
        assert_eq!(
            out.glyphs[row * RAIL_COLS + col].ch,
            '×',
            "active slot shows × at the inset top-right"
        );
    }

    #[test]
    fn inactive_slot_has_no_close_glyph_when_not_hovered() {
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        let row = RAIL_TOP_MARGIN_ROWS + SLOT_STRIDE;
        let col = RAIL_COLS - SLOT_INSET_COLS - 1;
        assert_ne!(
            out.glyphs[row * RAIL_COLS + col].ch,
            '×',
            "inactive unhovered slot: no ×"
        );
    }

    #[test]
    fn new_tab_slot_renders_plus_glyph() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        assert!(out.glyphs.iter().any(|g| g.ch == '+'), "+ glyph present");
    }

    #[test]
    fn new_tab_plus_is_one_row_dim_at_rest_and_bright_on_hover() {
        let src = MockSource::new(&["a", "b"], 0);
        let (nt_start, nt_end) = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM)
            .new_tab_rows
            .expect("new-tab slot present");
        assert_eq!(nt_end - nt_start, NEW_TAB_ROWS, "the + slot is 1 cell tall");
        // At rest: dim + on bare wallpaper (no fill).
        let out = render_default(&src);
        let wallpaper = rgb(tab_chrome::wallpaper_background(COLORS));
        let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
        assert_eq!(
            plus.attrs.background, wallpaper,
            "+ sits on the bare wallpaper"
        );
        assert_eq!(
            plus.attrs.foreground,
            rgb(COLORS.inactive),
            "+ is dim at rest"
        );
        // On hover: whisper fill + brighter +.
        let out = render_with(&hovered_rail(TabHit::NewTab), &src);
        let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
        assert_eq!(
            plus.attrs.background,
            rgb(tab_chrome::hover_fill(COLORS)),
            "+ gains the whisper fill on hover"
        );
        assert!(
            luma(plus.attrs.foreground) > luma(rgb(COLORS.inactive)),
            "+ brightens on hover"
        );
    }

    // -----------------------------------------------------------------------
    // Bloom-off fallback
    // -----------------------------------------------------------------------

    #[test]
    fn bloom_off_fallback_active_slot_is_identifiable_without_glow() {
        let src = MockSource::new(&["one", "two", "three"], 1);
        let out = render_default(&src);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        let active = &layout.slots[1];
        let inactive = &layout.slots[0];
        assert_ne!(
            bg_at(&out, active.start_row, SLOT_LABEL_START_COL),
            bg_at(&out, inactive.start_row, SLOT_LABEL_START_COL),
            "active fill locatable vs inactive"
        );
        assert!(
            out.glyphs[active.start_row * RAIL_COLS + SLOT_LABEL_START_COL]
                .attrs
                .bold(),
            "active label bold"
        );
    }

    // -----------------------------------------------------------------------
    // Hit-testing (row-major) — unchanged engine
    // -----------------------------------------------------------------------

    #[test]
    fn hit_body_of_each_tab_switches() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        for slot in &layout.slots {
            let hit = hit_at(slot.start_row, RAIL_LABEL_PAD, &src);
            assert_eq!(hit, TabHit::Switch(slot.idx), "body → Switch({})", slot.idx);
        }
    }

    #[test]
    fn hit_close_cell_returns_close() {
        let src = MockSource::new(&["a", "b"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        for slot in &layout.slots {
            let (crow, ccol) = slot.close_cell.expect("close cell present");
            let hit = hit_at(crow, ccol, &src);
            assert_eq!(hit, TabHit::Close(slot.idx), "× cell → Close({})", slot.idx);
        }
    }

    #[test]
    fn hit_new_tab_slot_returns_new_tab() {
        let src = MockSource::new(&["a"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        let (s, _e) = layout.new_tab_rows.expect("new-tab slot present");
        let hit = hit_at(s, RAIL_COLS / 2, &src);
        assert_eq!(hit, TabHit::NewTab, "+ slot → NewTab");
    }

    #[test]
    fn hit_left_of_rail_is_none() {
        let src = MockSource::new(&["a"], 0);
        let hit = rail().hit_test(-1.0, 8.0, &src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL, GEOM);
        assert_eq!(hit, TabHit::None, "left of the rail → None");
    }

    #[test]
    fn hit_right_of_rail_band_is_none() {
        let src = MockSource::new(&["a"], 0);
        let x = RAIL_COLS as f64 * CELL.width as f64 + 1.0;
        let hit = rail().hit_test(x, 8.0, &src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL, GEOM);
        assert_eq!(hit, TabHit::None, "right of the rail band → None");
    }

    #[test]
    fn hit_gap_between_slots_is_none() {
        let src = MockSource::new(&["a", "b"], 0);
        let gap_row = RAIL_TOP_MARGIN_ROWS + SLOT_ROWS;
        let hit = hit_at(gap_row, RAIL_LABEL_PAD, &src);
        assert_eq!(hit, TabHit::None, "inter-slot gap → None");
    }

    #[test]
    fn hit_below_all_slots_is_none() {
        let src = MockSource::new(&["a"], 0);
        let hit = hit_at(GRID_ROWS - 1, RAIL_LABEL_PAD, &src);
        assert_eq!(hit, TabHit::None, "empty band below slots → None");
    }

    // -----------------------------------------------------------------------
    // Overflow (many tabs) — informational indicators (unchanged engine)
    // -----------------------------------------------------------------------

    #[test]
    fn many_tabs_overflow_scrolls_to_keep_active_visible() {
        let titles: Vec<&'static str> = vec!["t"; 100];
        let src = MockSource::new(&titles, 80);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        assert!(!layout.slots.is_empty(), "some slots are visible");
        let visible: Vec<usize> = layout.slots.iter().map(|s| s.idx).collect();
        assert!(visible.contains(&80), "active tab 80 is kept visible");
        assert!(layout.overflow_above.is_some(), "tabs above are hidden → ▲");
        assert!(
            layout.new_tab_rows.is_none(),
            "no + slot when the rail is full"
        );
    }

    #[test]
    fn overflow_indicator_rows_are_informational_only() {
        let titles: Vec<&'static str> = vec!["t"; 100];
        let src = MockSource::new(&titles, 80);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
        if layout.overflow_above.is_some() {
            let hit = hit_at(0, RAIL_LABEL_PAD, &src);
            assert_eq!(hit, TabHit::None, "▲ indicator row → None");
        }
        if layout.overflow_below.is_some() {
            let hit = hit_at(GRID_ROWS - 1, RAIL_LABEL_PAD, &src);
            assert_eq!(hit, TabHit::None, "▼ indicator row → None");
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
    // slot_fill_cols geometry (unchanged engine)
    // -----------------------------------------------------------------------

    #[test]
    fn slot_fill_cols_insets_closed_and_bleeds_open_to_the_seam() {
        // Inactive/hover (None): inset both sides.
        assert_eq!(
            slot_fill_cols(RAIL_COLS, None),
            (SLOT_INSET_COLS, RAIL_COLS - SLOT_INSET_COLS)
        );
        // Active on Left placement: inset left, bleed right to the content seam.
        assert_eq!(
            slot_fill_cols(RAIL_COLS, Some(RailSide::Left)),
            (SLOT_INSET_COLS, RAIL_COLS)
        );
        // Active on Right placement: bleed left to the seam, inset right.
        assert_eq!(
            slot_fill_cols(RAIL_COLS, Some(RailSide::Right)),
            (0, RAIL_COLS - SLOT_INSET_COLS)
        );
    }

    // -----------------------------------------------------------------------
    // F4-P4 seam-drag width geometry (pure)
    // -----------------------------------------------------------------------

    #[test]
    fn rail_width_from_pointer_maps_left_rail_from_the_left_edge() {
        use super::super::rail_width_cols_from_pointer;
        let (pad, cw, surface_w) = (0.0, 8.0, 800.0);
        // Left rail hugs the left edge: width = pointer distance from `pad`,
        // cell-snapped. 128px / 8 = 16 cells.
        assert_eq!(
            rail_width_cols_from_pointer(RailSide::Left, 128.0, pad, cw, surface_w, 8, 32),
            16
        );
        // Sub-cell rounds to the nearest cell (140/8 = 17.5 → 18).
        assert_eq!(
            rail_width_cols_from_pointer(RailSide::Left, 140.0, pad, cw, surface_w, 8, 32),
            18
        );
        // Clamps to [min, max]: far left → min, far right → max.
        assert_eq!(
            rail_width_cols_from_pointer(RailSide::Left, 4.0, pad, cw, surface_w, 8, 32),
            8
        );
        assert_eq!(
            rail_width_cols_from_pointer(RailSide::Left, 100000.0, pad, cw, surface_w, 8, 32),
            32
        );
    }

    #[test]
    fn rail_width_from_pointer_maps_right_rail_from_the_right_edge() {
        use super::super::rail_width_cols_from_pointer;
        let (pad, cw, surface_w) = (0.0, 8.0, 800.0);
        // Right rail hugs the right edge: width = (surface_w - pad - pointer),
        // cell-snapped. Pointer at 800-128 = 672 → 128/8 = 16 cells.
        assert_eq!(
            rail_width_cols_from_pointer(RailSide::Right, 672.0, pad, cw, surface_w, 8, 32),
            16
        );
        // Dragging the seam left (smaller pointer x) widens the right rail.
        assert!(
            rail_width_cols_from_pointer(RailSide::Right, 600.0, pad, cw, surface_w, 8, 32)
                > rail_width_cols_from_pointer(RailSide::Right, 672.0, pad, cw, surface_w, 8, 32),
            "moving the right seam left widens the rail"
        );
        // Padding shifts the pinned edge inward.
        assert_eq!(
            rail_width_cols_from_pointer(RailSide::Right, 664.0, 8.0, cw, surface_w, 8, 32),
            16
        );
    }
}
