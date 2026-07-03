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
//! ## R1 scope
//! `left` placement, fixed width, Option B geometry, closed-box rings (the
//! "connected-active" open-edge variant is held until the operator judges the
//! horizontal bar — design ODP note). Settings plumbing (`TAB_BAR_PLACEMENT` /
//! `TAB_RAIL_WIDTH`), the `right` arm, and drag-resize are later slices (R2/R3).
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
/// bounded-object reading (Option B).
const SLOT_GAP: usize = 1;
/// Row stride from one slot's top to the next slot's top.
const SLOT_STRIDE: usize = SLOT_ROWS + SLOT_GAP;
/// Rows the bottom `+` new-tab slot occupies (matches a tab slot so it reads as
/// the same bounded object).
const NEW_TAB_ROWS: usize = SLOT_ROWS;
/// Left/right in-slot padding (columns) before/after the label text.
const RAIL_LABEL_PAD: usize = 1;

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
            // Fill the slot's cells.
            for row in slot.start_row..slot.end_row.min(grid_rows) {
                for col in 0..rail_cols {
                    cells[row * rail_cols + col].attrs.background = slot_bg;
                }
            }
            // Outline ring (closed box) — the shape language.
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
            );
            if is_active {
                active_ring = ring;
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
                    let col = RAIL_LABEL_PAD + i;
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

        // New-tab `+` slot.
        if let Some((nt_start, nt_end)) = layout.new_tab_rows {
            let is_hovered = matches!(self.hover, Some(TabHit::NewTab));
            let nt_bg = if is_hovered { active_bg_color } else { band_bg };
            for row in nt_start..nt_end.min(grid_rows) {
                for col in 0..rail_cols {
                    cells[row * rail_cols + col].attrs.background = nt_bg;
                }
            }
            inactive_rings.extend(rail_slot_ring(
                nt_start,
                nt_end,
                rail_cols,
                origin_px,
                cell,
                inactive_ring_srgb,
            ));
            let mut a = Attrs::default();
            a.foreground = base_fg;
            a.background = nt_bg;
            // Centre the `+` in the slot.
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
        // then the rail↔content divider on the seam.
        let mut out = TabRailOutput {
            glyphs: cells,
            ..Default::default()
        };
        out.quads.extend(inactive_rings);
        out.quads.extend(active_ring);
        out.quads.push(rail_divider(
            rail_cols,
            grid_rows,
            origin_px,
            cell,
            placement,
            colors.border,
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
        if grid_rows >= NEW_TAB_ROWS {
            layout.new_tab_rows = Some((0, NEW_TAB_ROWS));
        }
        return layout;
    }

    // Rows needed to show every tab plus the `+` slot with no scroll. Each tab
    // consumes SLOT_STRIDE (slot + trailing gap); the last trailing gap becomes
    // the gap before the `+` slot, so total = tabs*stride + NEW_TAB_ROWS.
    let total_needed = tab_count * SLOT_STRIDE + NEW_TAB_ROWS;

    if total_needed <= grid_rows {
        for i in 0..tab_count {
            let start = i * SLOT_STRIDE;
            layout.slots.push(build_slot(source, i, start, rail_cols));
        }
        let nt_start = tab_count * SLOT_STRIDE;
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

    // Reserve row 0 for `▲` only when actually scrolled below the top.
    let top = if first > 0 { 1 } else { 0 };
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
    // The `×` gets its own cell at the slot's top-right (Option B). It sits in
    // the right padding column, so it never collides with the label inner area.
    let close_cell = (rail_cols >= 2).then_some((start_row, rail_cols - 1));
    let inner = rail_cols.saturating_sub(2 * RAIL_LABEL_PAD);
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

/// A hollow ring of four [`SolidQuad`]s framing a rail slot spanning rows
/// `[start_row, end_row)` across the full `rail_cols` width. Thickness is ≥2px
/// (the CRT scanline shader eats a 1px ring). Fully opaque so it reads over
/// background images. Returns empty for a degenerate slot.
fn rail_slot_ring(
    start_row: usize,
    end_row: usize,
    rail_cols: usize,
    origin_px: [f32; 2],
    cell: CellSize,
    outline: Srgb,
) -> Vec<SolidQuad> {
    if end_row <= start_row || rail_cols == 0 || cell.width == 0 || cell.height == 0 {
        return Vec::new();
    }
    let cw = cell.width as f32;
    let ch = cell.height as f32;
    let thickness = (ch / 8.0).clamp(2.0, 3.0);
    let x0 = origin_px[0];
    let x1 = origin_px[0] + rail_cols as f32 * cw;
    let y0 = origin_px[1] + start_row as f32 * ch;
    let y1 = origin_px[1] + end_row as f32 * ch;
    let color = srgb_alpha(outline, 1.0);
    vec![
        SolidQuad {
            rect: [x0, y0, x1, y0 + thickness],
            color,
        }, // top
        SolidQuad {
            rect: [x0, y1 - thickness, x1, y1],
            color,
        }, // bottom
        SolidQuad {
            rect: [x0, y0 + thickness, x0 + thickness, y1 - thickness],
            color,
        }, // left
        SolidQuad {
            rect: [x1 - thickness, y0 + thickness, x1, y1 - thickness],
            color,
        }, // right
    ]
}

/// The 1px `border`-role divider along the rail↔content seam: the right edge of
/// the rail for `Left` placement, the left edge for `Right`. Opaque so it reads
/// over wallpaper (the vertical analog of the strip's band separator).
fn rail_divider(
    rail_cols: usize,
    grid_rows: usize,
    origin_px: [f32; 2],
    cell: CellSize,
    placement: RailSide,
    border: Srgb,
) -> SolidQuad {
    let cw = cell.width as f32;
    let ch = cell.height as f32;
    let y0 = origin_px[1];
    let y1 = origin_px[1] + grid_rows as f32 * ch;
    let seam_x = match placement {
        RailSide::Left => origin_px[0] + rail_cols as f32 * cw,
        RailSide::Right => origin_px[0],
    };
    SolidQuad {
        rect: [seam_x - DIVIDER_PX, y0, seam_x, y1],
        color: srgb_alpha(border, 1.0),
    }
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
        // A gap row between slot 0 and slot 1 (row SLOT_ROWS) is band fill.
        let gap_idx = SLOT_ROWS * RAIL_COLS; // row=SLOT_ROWS, col=0
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
            Some((0, NEW_TAB_ROWS)),
            "the + slot sits at the top with zero tabs"
        );
    }

    #[test]
    fn three_tabs_stack_with_stride_and_a_new_tab_slot() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        assert_eq!(layout.slots.len(), 3, "three tab slots");
        for (i, slot) in layout.slots.iter().enumerate() {
            assert_eq!(slot.start_row, i * SLOT_STRIDE, "slot {i} start row");
            assert_eq!(
                slot.end_row,
                i * SLOT_STRIDE + SLOT_ROWS,
                "slot {i} end row"
            );
        }
        assert_eq!(
            layout.new_tab_rows,
            Some((3 * SLOT_STRIDE, 3 * SLOT_STRIDE + NEW_TAB_ROWS)),
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
        // Active slot 0 top-right cell = (row 0, col RAIL_COLS-1).
        let idx = RAIL_COLS - 1;
        assert_eq!(out.glyphs[idx].ch, '×', "active slot shows × at top-right");
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
        // Gap row between slot 0 (rows 0..2) and slot 1 (rows 3..5) is row 2.
        let hit = hit_at(SLOT_ROWS, RAIL_LABEL_PAD, &src);
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
    fn every_slot_is_framed_by_a_four_quad_ring() {
        let src = MockSource::new(&["a", "b", "c"], 1);
        let out = render_default(&src);
        let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS);
        // Each slot ring is 4 quads; the + slot adds 4; plus 1 divider.
        // Count ring quads whose vertical extent falls within each slot.
        for slot in &layout.slots {
            let y0 = slot.start_row as f32 * CELL.height as f32;
            let y1 = slot.end_row as f32 * CELL.height as f32;
            let n = out
                .quads
                .iter()
                .filter(|q| q.rect[1] >= y0 - 0.01 && q.rect[3] <= y1 + 0.01)
                .count();
            assert_eq!(n, 4, "slot {} framed by a 4-quad ring", slot.idx);
        }
    }

    #[test]
    fn ring_edges_are_at_least_two_pixels_thick() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        // Active slot 0 top edge sits at y=0.
        let top = out
            .quads
            .iter()
            .find(|q| q.rect[1].abs() < f32::EPSILON && q.rect[0].abs() < f32::EPSILON)
            .expect("active ring top edge at y=0");
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
        assert!(
            divider.rect[1].abs() < f32::EPSILON,
            "divider starts at the top"
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
    // Active fill / hover fill
    // -----------------------------------------------------------------------

    #[test]
    fn active_slot_is_filled_with_selection_role() {
        let src = MockSource::new(&["a", "b"], 0);
        let out = render_default(&src);
        let active_fill = Color::Rgb(COLORS.active_bg.0, COLORS.active_bg.1, COLORS.active_bg.2);
        // Active slot 0, a body cell (row 0, col RAIL_LABEL_PAD).
        let idx = RAIL_LABEL_PAD;
        assert_eq!(
            out.glyphs[idx].attrs.background, active_fill,
            "active slot filled with selection role"
        );
        // Inactive slot 1 body cell sits on the band, not the active fill.
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let band_bg = Color::Rgb(band.0, band.1, band.2);
        let iidx = SLOT_STRIDE * RAIL_COLS + RAIL_LABEL_PAD;
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
        let iidx = SLOT_STRIDE * RAIL_COLS + RAIL_LABEL_PAD;
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
    }
}
