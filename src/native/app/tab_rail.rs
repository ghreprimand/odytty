// SPDX-License-Identifier: GPL-3.0-only
//! Vertical tab rail widget (F4-V2 R1) — presentation-only, decoupled from the
//! session model, the vertical sibling of [`super::tab_bar::TabBar`].
//!
//! Where [`TabBar`](super::tab_bar::TabBar) packs variable-width slots along a
//! single top row, the rail **stacks fixed-width slots down a fixed-width column
//! band** on the left (R1) side of the window. It reads layout from the shared
//! [`TabBarSource`] trait (same one `WorkspaceSet` already implements), returns the
//! shared [`TabHit`] enum (so pointer/action dispatch is reused verbatim), and
//! paints with the shared [`TabBarColors`] theme roles. It never touches
//! terminal state, PTY, or settings — the integration layer composites the
//! returned region + quads into the frame.
//!
//! ## Visual language: "Phosphor Flat" (F4-RESKIN)
//! Identical treatment to the horizontal [`super::tab_bar`], sharing the same
//! [`super::tab_chrome`] color module (F4V2-NF2 — the "promote shared treatment
//! fns to a shared chrome location" follow-up is now done). The old outlined-box
//! language (per-slot rings, the rail↔content divider) was **deleted, not
//! bypassed** because it read as hacked-together and cheap. The rail
//! is now a continuous surface; the active slot is embedded in the rail, and
//! hierarchy comes from luminance:
//! - **ACTIVE** — a warm full-width `selection` fill (the bloom-off fallback),
//!   plus a bright bold `foreground` label brightened above the
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
//! [`crate::settings::TabBarPlacement::effective`] until right-side placement is wired. The
//! band width, slot height, and inter-slot gap are the live
//! `ODYTTY_TAB_RAIL_WIDTH` / `ODYTTY_TAB_RAIL_SLOT_ROWS` / `ODYTTY_TAB_RAIL_GAP`
//! knobs (resolved by the integration layer into [`RailGeom`] + `rail_cols`);
//! auto-hide and drag-resize are handled in the interaction layer.

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

/// Rows the always-visible auto-hide toggle control occupies at the rail's
/// bottom edge (RAIL-AUTOHIDE-CTL): a single glyph row. It is pinned below the
/// workspace slots and the `+` affordance so the escape hatch for the rail lives
/// on the rail itself.
pub(super) const AUTOHIDE_CONTROL_ROWS: usize = 1;
/// Blank spacer rows between the slot/overflow region and the control, so the
/// control reads as set apart (mirrors the RAIL-PLUS-GAP treatment above the
/// `+`). Non-interactive.
const AUTOHIDE_SEPARATOR_ROWS: usize = 1;
/// Total rows the control reserves at the bottom of the rail region: the control
/// row plus its separator. The slot/overflow placement runs against the region
/// ABOVE this band, so overflow indicators and slots never collide with it.
const AUTOHIDE_RESERVE_ROWS: usize = AUTOHIDE_CONTROL_ROWS + AUTOHIDE_SEPARATOR_ROWS;

/// Solid horizontal-triangle glyphs pointing toward the rail edge — the "tuck
/// the rail away" affordance, mirrored by [`RailSide`]. A full-cell-height
/// geometric glyph (same Geometric Shapes block as the `▲`/`▼` overflow marks,
/// so font coverage is guaranteed) reads as a deliberate collapse control at the
/// rail's own font size, rather than a small punctuation mark floating in the
/// cell.
const AUTOHIDE_CHEVRON_LEFT: char = '\u{25c0}'; // ◀
const AUTOHIDE_CHEVRON_RIGHT: char = '\u{25b6}'; // ▶

/// Marker painted on a bound workspace's rail row (ODP-7B): a compact
/// bidirectional-link glyph in a text-side accent role that reads as "everything
/// opened here is remote". Placed in the rail-edge inset column (the outer,
/// non-content margin) so it never crowds the label and stays width-robust.
const BOUND_BADGE: char = '\u{21c4}';

/// Runtime rail slot geometry (F4-P1 knobs): how many rows each slot occupies
/// and the inter-slot gap. Threaded through [`TabRail::render`] /
/// the shared chrome geometry so the settings can tune them live; the widget owns no
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
pub(super) const SLOT_LABEL_START_COL: usize = SLOT_INSET_COLS + RAIL_LABEL_PAD;

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

/// Output from [`TabRail::render`]: the fully-painted region glyphs plus the
/// chrome-quad list populated by App integration.
#[derive(Debug, Default)]
pub(super) struct TabRailOutput {
    /// Solid pixel-space quads. The widget render starts this empty; App
    /// integration adds active and insertion markers in window-pixel geometry.
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
pub(super) struct RailSlot {
    /// Tab index in the source.
    pub(super) idx: usize,
    /// First row of the slot (inclusive).
    pub(super) start_row: usize,
    /// One-past-last row of the slot (exclusive).
    pub(super) end_row: usize,
    /// The row the single-line label (and its `×`) sits on — the slot's
    /// vertically-centered row (F4-P4). For the default 2-row slot the centre
    /// rounds to the slot's first row, leaving the trailing row as breathing
    /// space; taller slots centre exactly.
    pub(super) label_row: usize,
    /// The single-line label, already truncated with `…` to the inner column
    /// budget (F4-P4 — the rail never wraps to a second line).
    pub(super) label: String,
    /// `(row, col)` of the `×` close glyph (on the label row, at the slot's
    /// inset top-right), or `None` when the rail is too narrow.
    pub(super) close_cell: Option<(usize, usize)>,
}

/// Full rail layout for one rendering pass.
#[derive(Debug, Default)]
pub(super) struct RailLayout {
    /// Visible tab slots (row coordinates absolute within the rail region).
    pub(super) slots: Vec<RailSlot>,
    /// `(start_row, end_row)` of the `+` new-tab slot, or `None` when it doesn't
    /// fit (overflow, or a degenerate rail).
    pub(super) new_tab_rows: Option<(usize, usize)>,
    /// The dead spacer row between the last workspace slot and the `+`
    /// (RAIL-PLUS-GAP), or `None` when there is nothing to separate (empty rail
    /// or overflow). This blank row is non-interactive -- a click on it maps to
    /// [`TabHit::None`] -- so the `+` remains set apart from the workspace list.
    separator_row: Option<usize>,
    /// When some tabs are scrolled off the TOP: `Some(hidden_count)`. The `▲`
    /// indicator paints in row 0 and is informational-only in R1.
    overflow_above: Option<usize>,
    /// When some tabs are scrolled off the BOTTOM: `Some(hidden_count)`. The `▼`
    /// indicator paints in the last row of the SLOT REGION (above the reserved
    /// control band) and is informational-only in R1.
    overflow_below: Option<usize>,
    /// Row of the always-visible auto-hide toggle control at the rail's bottom
    /// edge (RAIL-AUTOHIDE-CTL), or `None` on a degenerate (empty) region. The
    /// slot/overflow region is bounded ABOVE this so nothing collides with it.
    pub(super) autohide_row: Option<usize>,
    /// Height (rows) of the slot/overflow region — the window rows the rail
    /// spans MINUS the reserved control band. Slot placement and the `▼`
    /// overflow indicator are bounded by this, not by the full rail height.
    slot_region_rows: usize,
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
    /// - `source` — session model accessor (mock or real `WorkspaceSet`).
    /// - `rail_cols` — rail band width in cells (the setting; R1 fixed).
    /// - `grid_rows` — window content rows the rail spans.
    /// - `origin_px` / `cell` — pixel geometry for chrome quads; Phosphor Flat
    ///   emits none, so `origin_px` is unused and `cell` only feeds the degenerate
    ///   guard (retained in the signature so a future divider needs no call-site
    ///   change).
    /// - `placement` — `Left` or `Right`; controls mirrored edge affordances.
    /// - `colors` — the theme-role colors (see [`TabBarColors`]).
    ///
    /// Returns a fully-painted `rail_cols × grid_rows` region (`glyphs`) and an
    /// empty quad list — the whole Phosphor Flat treatment is cell backgrounds +
    /// label attributes (shared [`super::tab_chrome`]).
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
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
        bound_accent: Srgb,
        autohide_on: bool,
    ) -> TabRailOutput {
        self.render_with_pressed(
            source,
            None,
            rail_cols,
            grid_rows,
            origin_px,
            cell,
            placement,
            colors,
            geom,
            panel_strength,
            bound_accent,
            autohide_on,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_with_pressed(
        &self,
        source: &dyn TabBarSource,
        pressed_idx: Option<usize>,
        rail_cols: usize,
        grid_rows: usize,
        origin_px: [f32; 2],
        cell: CellSize,
        placement: RailSide,
        colors: TabBarColors,
        geom: RailGeom,
        panel_strength: f32,
        bound_accent: Srgb,
        autohide_on: bool,
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
        let panel_srgb = tab_chrome::panel_tint(colors, panel_strength);
        let panel_surface = rgb(panel_srgb);
        // ACTIVE-FILL: lift the `selection` slab against THIS panel surface (see
        // tab_bar) so the active workspace slot reads on every theme; hover
        // re-bases on the panel toward that guaranteed fill.
        let active_fill = rgb(tab_chrome::active_fill(colors, panel_srgb));
        let active_lbl = rgb(tab_chrome::active_label(colors));
        let hover_fill = rgb(tab_chrome::hover_fill(colors, panel_srgb));
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
            let is_pressed = pressed_idx == Some(slot.idx);
            let (slot_bg, label_fg, bold) = if is_active || is_pressed {
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
            // Only active/hover/pressed slots carry a fill; inactive slots stay
            // on the panel surface. State fills span the rail width so rows read
            // as embedded components rather than detached cards.
            if is_active || is_hovered || is_pressed {
                for row in slot.start_row..slot.end_row.min(grid_rows) {
                    for col in 0..rail_cols {
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
            if let Some((crow, ccol)) = slot
                .close_cell
                .filter(|_| is_active || is_hovered || is_pressed)
                && crow < grid_rows
                && ccol < rail_cols
            {
                let g = &mut cells[crow * rail_cols + ccol];
                g.ch = '×';
                g.attrs = la;
            }
            // Bound-workspace marker (ODP-7B): a compact link glyph in the
            // text-side accent role (never the theme border), painted in the
            // rail-edge inset column (the outer, non-content margin) so it
            // reads as "opens remote" without crowding the label. Unbound
            // rows (the default) paint nothing here — byte-identical to today.
            if source.tab_bound(slot.idx) {
                let badge_col = match placement {
                    RailSide::Left => 0,
                    RailSide::Right => rail_cols - 1,
                };
                let brow = slot.label_row;
                if brow < grid_rows && badge_col < rail_cols {
                    let g = &mut cells[brow * rail_cols + badge_col];
                    g.ch = BOUND_BADGE;
                    g.attrs.foreground = rgb(bound_accent);
                }
            }
        }

        // New-tab `+` slot — a lightweight 1-cell affordance. The preceding
        // RAIL-PLUS-GAP row stays blank and non-interactive, leaving the control
        // set apart from the workspace list without a visible rule.
        // At rest the `+` reads as a deliberate "add" control: lifted out of the
        // dim inactive floor via `new_slot_plus_rest` (RAIL-PLUS-GAP). It still
        // brightens to the full active label (and gains a whisper fill) on hover,
        // so hover stays clearly stronger than the resting state.
        if let Some((nt_start, nt_end)) = layout.new_tab_rows {
            let is_hovered = matches!(self.hover, Some(TabHit::NewTab));
            let (nt_bg, nt_fg) = if is_hovered {
                (hover_fill, active_lbl)
            } else {
                (panel_surface, rgb(tab_chrome::new_slot_plus_rest(colors)))
            };
            if is_hovered {
                for row in nt_start..nt_end.min(grid_rows) {
                    for col in 0..rail_cols {
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
        // the last row of the SLOT REGION (above the reserved control band).
        // Painted as dim-foreground glyphs over the wallpaper.
        if let Some(hidden) = layout.overflow_above {
            paint_overflow_indicator(&mut cells, 0, rail_cols, grid_rows, '▲', hidden, dim_plus);
        }
        if let Some(hidden) = layout.overflow_below {
            let below_row = layout.slot_region_rows.saturating_sub(1);
            paint_overflow_indicator(
                &mut cells, below_row, rail_cols, grid_rows, '▼', hidden, dim_plus,
            );
        }

        // RAIL-AUTOHIDE-CTL: the always-visible auto-hide toggle at the rail's
        // bottom edge. A solid horizontal triangle pointing toward the rail edge;
        // the resting glyph recedes into the inactive floor, hover lifts it to
        // the panel's hover treatment, and an ACTIVE auto-hide state is held in
        // the active-label tint with the triangle flipped to point inward (so the
        // pinned-off and auto-hiding states read distinctly by direction + tint).
        // Glyph-only: no bold weight, no on-hover text label.
        if let Some(ctl_row) = layout.autohide_row.filter(|&r| r < grid_rows) {
            let is_hovered = matches!(self.hover, Some(TabHit::AutohideToggle));
            // Point toward the rail edge when OFF (tuck away), inward when ON.
            let toward_edge = matches!(placement, RailSide::Left);
            let chevron = if autohide_on ^ toward_edge {
                AUTOHIDE_CHEVRON_RIGHT
            } else {
                AUTOHIDE_CHEVRON_LEFT
            };
            let (ctl_bg, ctl_fg) = if is_hovered {
                (hover_fill, hover_lbl)
            } else if autohide_on {
                (panel_surface, active_lbl)
            } else {
                (panel_surface, dim_plus)
            };
            if is_hovered {
                for col in 0..rail_cols {
                    cells[ctl_row * rail_cols + col].attrs.background = ctl_bg;
                }
            }
            let mut a = Attrs::default();
            a.foreground = ctl_fg;
            a.background = ctl_bg;
            // Glyph-only control at the label column, matching the `+`
            // affordance's alignment. The on-state reads through the triangle
            // flip and the active tint alone — no bold weight, no text label.
            let ccol = SLOT_LABEL_START_COL.min(rail_cols.saturating_sub(1));
            let g = &mut cells[ctl_row * rail_cols + ccol];
            g.ch = chevron;
            g.attrs = a;
        }

        // Phosphor Flat draws no chrome quads — the treatment is entirely cell
        // backgrounds + label attributes (see the module docs).
        TabRailOutput {
            quads: Vec::new(),
            glyphs: cells,
        }
    }
}

// ---------------------------------------------------------------------------
// Layout computation
// ---------------------------------------------------------------------------

/// Build the row-stacked slot layout for the current tab set.
///
/// Pure: no mutable state, shared by rendering and window-pixel geometry.
/// When every tab plus the `+` slot fits, all are laid out top-down with no
/// scroll. Otherwise the view scrolls to keep the active tab visible and emits
/// informational `▲/▼` overflow counts; the `+` slot is dropped when the rail
/// is full (new tabs remain reachable via the menu/keyboard).
pub(super) fn compute_rail_layout(
    source: &dyn TabBarSource,
    rail_cols: usize,
    grid_rows: usize,
    geom: RailGeom,
) -> RailLayout {
    let mut layout = RailLayout::default();
    if rail_cols == 0 || grid_rows == 0 {
        return layout;
    }
    // RAIL-AUTOHIDE-CTL: reserve the bottom band for the always-visible auto-hide
    // toggle, then place slots/overflow against the region ABOVE it. The control
    // pins to the true bottom row; the slot region loses the reserved rows so
    // overflow indicators and slots can never collide with the control.
    let reserve = AUTOHIDE_RESERVE_ROWS.min(grid_rows);
    let region_rows = grid_rows - reserve;
    layout.autohide_row = Some(grid_rows - 1);
    layout.slot_region_rows = region_rows;
    let top_margin = geom.top_margin();
    let stride = geom.stride();
    let tab_count = source.tab_count();
    if tab_count == 0 {
        if region_rows >= top_margin + NEW_TAB_ROWS {
            layout.new_tab_rows = Some((top_margin, top_margin + NEW_TAB_ROWS));
        }
        return layout;
    }

    // Rows needed to show the top margin, every tab, a dead separator gap, and
    // the `+` slot with no scroll. Each tab consumes `slot_rows` plus an
    // inter-slot `slot_gap`; below the last slot a guaranteed dead gap
    // (`plus_gap`, at least one row) separates the list from the `+`, so the
    // total is margin + tabs*slot_rows + (tabs-1)*slot_gap + plus_gap +
    // NEW_TAB_ROWS. `plus_gap` == `slot_gap` for the default (>=1) geometry, so
    // the reservation is unchanged there; it only widens if the gap knob is 0.
    let plus_gap = geom.slot_gap.max(1);
    let last_slot_end = top_margin + tab_count.saturating_sub(1) * stride + geom.slot_rows;
    let nt_start = last_slot_end + plus_gap;
    let total_needed = nt_start + NEW_TAB_ROWS;

    if total_needed <= region_rows {
        for i in 0..tab_count {
            let start = top_margin + i * stride;
            layout
                .slots
                .push(build_slot(source, i, start, rail_cols, geom));
        }
        // RAIL-PLUS-GAP: anchor the `+` a guaranteed dead gap below the last
        // slot's END row (not its label row), so a 2-row padded slot no longer
        // leaves the `+` flush against its breathing row. The first row of that
        // gap is a blank spacer; the `+` sits `plus_gap` rows below the slot box.
        // For a 1-row slot with the default gap this is arithmetically identical
        // to the previous `label_row + 1 + slot_gap` placement.
        let nt_start = layout
            .slots
            .last()
            .map(|last| last.end_row + plus_gap)
            .unwrap_or(top_margin);
        // The spacer is the first dead row directly below the last slot box
        // (only meaningful when a slot exists above the `+`).
        layout.separator_row = layout.slots.last().map(|last| last.end_row);
        layout.new_tab_rows = Some((nt_start, nt_start + NEW_TAB_ROWS));
        return layout;
    }

    // Overflow: scroll to keep the active tab visible. Compute a conservative
    // visible capacity assuming both indicator rows are present, so the greedy
    // placement below can never overrun the region.
    let active = source.active_tab().min(tab_count - 1);
    // Measure capacity against the region the greedy placement below actually
    // fills: it starts at `top_margin` (== slot_gap) when the active tab is near
    // the top, or at row 1 once scrolled, and always reserves the final row for
    // the bottom overflow indicator. Using the larger top offset keeps the
    // estimate conservative, so the placement can neither overrun the region nor
    // scroll the active tab out of view. For the default 1-row gap this is
    // arithmetically identical to the previous `grid_rows - 2` band.
    let top_offset = top_margin.max(1);
    let band = region_rows.saturating_sub(1 + top_offset);
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
        // Always keep the last region row free for a potential `▼` (the region
        // excludes the reserved control band at the rail's bottom edge).
        if end > region_rows.saturating_sub(1) {
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

#[cfg(test)]
mod tests;
