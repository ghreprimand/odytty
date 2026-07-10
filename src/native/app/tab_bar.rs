// SPDX-License-Identifier: GPL-3.0-only
//! Tab bar widget — presentation-only, decoupled from the session model.
//!
//! Renders a one-row tab strip across the top of the window. The widget is
//! purely geometrical: it reads layout from a [`TabBarSource`] trait object
//! (implemented on `WorkspaceSet`) and produces solid quads + glyph outputs that the
//! integration layer composites into the frame. It never touches terminal
//! state, PTY, or settings.
//!
//! ## Integration (live)
//! Both render paths in `app/` drive this widget:
//! `App::decorate_snapshot_with_tab_bar` (single-pane) and
//! `WorkspaceSet`-backed `tab_bar_strip` (multi-pane) call [`TabBar::render`] each
//! frame, push the returned quads into the overlay quad list, and paint each
//! glyph into the reserved tab-bar snapshot row
//! (`snapshot.cells[glyph.col] = Cell::new(glyph.ch, glyph.attrs)`);
//! `App` pointer handling calls [`TabBar::hit_test`] on move (stored via
//! [`TabBar::set_hover`]) and again on press to resolve the action.
//!
//! ## Visual treatment — "Phosphor Flat" (F4-RESKIN, operator-ruled A+C)
//! All color comes from the shared [`super::tab_chrome`] module (theme roles
//! only — no hardcoded colors, so every theme and the CVD modes stay correct);
//! this widget owns layout, `tab_chrome` owns the treatment. The old
//! outlined-box language (per-slot rings, the band↔body separator, the accent
//! underline) was **deleted, not bypassed** — the operator rejected it as
//! "hacked together / cheap". The container is now invisible; only the active
//! tab is a drawn object, and hierarchy comes from luminance:
//!
//! - **ACTIVE** — a warm `selection` fill (the bloom-off fallback) plus a bright,
//!   bold `foreground` label brightened above the bloom threshold so it
//!   auto-halos through `bloom.wgsl` with no new geometry.
//! - **INACTIVE** — bare `inactive`-role labels on the wallpaper-through
//!   background (no fill), dimmed along a phosphor-persistence luminance ramp
//!   keyed on the tab's distance from the active one.
//! - **HOVER** — the label warms one tier toward the active label and gains a
//!   whisper of the selection fill.
//! - `×` shows on the active/hovered tab only; `+` is a dim glyph that brightens
//!   on hover.
//!
//! There are **no chrome quads in this widget** — the label/fill treatment is
//! entirely cell backgrounds + label attributes, so [`TabBarOutput::quads`] is
//! emitted empty. The F4-P1 unified panel + seam are separate background-segment
//! quads built by [`super::tab_panel`] and spliced in by the integration layer;
//! the widget only paints the resting-cell **panel tint** (Layer 1) so the
//! surface reads even at `cell_bg_opacity = 1`. Cells use an explicit
//! `Color::Rgb` so they composite through `cell_bg_opacity` exactly like the
//! terminal body and both render paths agree.

// `use super::*` brings in everything imported at the `app/mod.rs` level:
// `Color`, `SolidQuad`, `CellSize`, `text` (module), `WindowPadding`, etc.
// `Attrs` and `Srgb` are not re-exported from `app` so they are added here.
use super::tab_chrome;
use super::*;
use crate::core::Attrs;
use crate::theme::Srgb;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Physical-pixel row count of the classic single-row tab bar. Retained for
/// the reservation/geometry tests that assert the one-row baseline; the live
/// height is resolved from `tab_bar_height` (`App::tab_bar_rows`).
#[cfg(test)]
pub(in crate::native) const TAB_BAR_ROWS: u32 = 1;

// ---------------------------------------------------------------------------
// Private geometry constants
// ---------------------------------------------------------------------------

/// Maximum column width per tab slot (label + padding + close button).
const MAX_TAB_COLS: usize = 24;
/// Minimum column width per tab slot (must be ≥ TAB_PADDING + CLOSE_COLS + 1).
const MIN_TAB_COLS: usize = 4;
/// Columns reserved at the right of the bar for the ` + ` new-tab affordance.
const NEW_TAB_COLS: usize = 3;
/// Left-margin space inside each tab slot (columns before the label).
const TAB_PADDING: usize = 1;
/// Columns at the right of each slot reserved for the close button (`space + ×`).
const CLOSE_COLS: usize = 2;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Read-only interface to the session model.  GPT will `impl TabBarSource for
/// WorkspaceSet`; unit tests use an inline mock.
pub(in crate::native) trait TabBarSource {
    /// Number of open tabs (0 means the bar renders empty).
    fn tab_count(&self) -> usize;
    /// Display title for tab `idx`.  `idx` is guaranteed to be `< tab_count()`.
    fn tab_title(&self, idx: usize) -> &str;
    /// Zero-based index of the currently focused tab.
    fn active_tab(&self) -> usize;
    /// Whether tab `idx` carries a bound-remote marker (workspace rail only:
    /// a workspace with a default host profile). Default `false` so the tab bar
    /// and the tab-list sources render no badge; only the workspace-rail source
    /// overrides this. `idx` is guaranteed to be `< tab_count()`.
    fn tab_bound(&self, idx: usize) -> bool {
        let _ = idx;
        false
    }
}

/// Result of a pointer hit test against the tab bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum TabHit {
    /// The pointer is over the body of tab `idx` → switch to it on press.
    Switch(usize),
    /// The pointer is over the `×` close affordance of tab `idx`.
    Close(usize),
    /// The pointer is over the `+` new-tab affordance.
    NewTab,
    /// The pointer is outside all interactive tab-bar regions.
    None,
}

/// Presentation-only tab bar state.  Only tracks hover; no terminal or session
/// mutation lives here.
#[derive(Debug, Default, Clone)]
pub(super) struct TabBar {
    /// The interactive region currently under the pointer, if any.
    pub(super) hover: Option<TabHit>,
}

/// A single text glyph to paint into the tab bar row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TabBarGlyph {
    /// Column index within the tab bar row (0-based).
    pub(super) col: usize,
    /// The character to display.
    pub(super) ch: char,
    /// SGR attributes for this glyph (foreground, background, bold, …).
    pub(super) attrs: Attrs,
}

/// Output from [`TabBar::render`]: fully specified row cells plus any optional
/// pixel-space quads the integration layer wants to composite separately.
///
/// Integration code pushes `quads` into the overlay quad list and writes each
/// glyph into the reserved snapshot row:
/// `snapshot.cells[glyph.col] = Cell::new(glyph.ch, glyph.attrs)`.
#[derive(Debug, Default)]
pub(super) struct TabBarOutput {
    /// Solid pixel-space quads the integration layer composites over the row.
    /// Phosphor Flat draws no chrome quads (the whole treatment is cell
    /// backgrounds + label attributes), so this is emitted **empty**; the
    /// channel is retained so a future bar↔body divider could be added back
    /// without changing the integration signature.
    pub(super) quads: Vec<SolidQuad>,
    /// One glyph per column to composite into the reserved tab bar row.
    pub(super) glyphs: Vec<TabBarGlyph>,
}

/// Theme-role colors the tab bar paints with (F4). Grouped into a struct so the
/// render signature stays readable as visual treatments are added, and so every
/// color demonstrably originates from a theme role (nothing hardcoded — CVD
/// modes stay correct).
#[derive(Debug, Clone, Copy)]
pub(super) struct TabBarColors {
    /// Active-tab label text (brightened above the bloom threshold) — theme
    /// `foreground`.
    pub(super) foreground: Srgb,
    /// Terminal body background — theme `background`. Inactive slots and gaps
    /// paint this as their wallpaper-through fill.
    pub(super) background: Srgb,
    /// Dimmed color for inactive tab labels (base of the phosphor ramp) — theme
    /// `inactive`.
    pub(super) inactive: Srgb,
    /// Active slot fill (and the base a hover fill is blended toward) — theme
    /// `selection`.
    pub(super) active_bg: Srgb,
}

// ---------------------------------------------------------------------------
// Private layout types
// ---------------------------------------------------------------------------

/// Computed geometry for one rendered tab slot.
#[derive(Debug)]
struct TabSlot {
    /// Tab index in the source.
    idx: usize,
    /// First column of this slot (inclusive).
    start_col: usize,
    /// One-past-last column of this slot (exclusive).
    end_col: usize,
    /// Column of the `×` close glyph, or `None` when the slot is too narrow.
    close_col: Option<usize>,
    /// The (possibly truncated) label string.
    label: String,
    /// First column of the label text (after left padding).
    label_col: usize,
}

/// Full tab bar column layout for one rendering pass.
struct TabLayout {
    slots: Vec<TabSlot>,
    /// First column of the ` + ` new-tab affordance block, or `None` when the
    /// grid is narrower than `NEW_TAB_COLS`.
    new_tab_col: Option<usize>,
}

// ---------------------------------------------------------------------------
// TabBar impl
// ---------------------------------------------------------------------------

impl TabBar {
    /// Update the hover state from the latest pointer hit test.  Call this
    /// whenever the pointer moves over the tab bar row.
    pub(super) fn set_hover(&mut self, hit: Option<TabHit>) {
        self.hover = hit;
    }

    /// Render the tab bar for the current frame.
    ///
    /// - `source` — session model accessor (mock or real `WorkspaceSet`).
    /// - `grid_cols` — number of terminal columns in the window.
    /// - `y_offset_px` / `cell` / `padding` — pixel geometry for chrome quads;
    ///   Phosphor Flat emits none, so they are currently unused (retained in the
    ///   signature so a future bar↔body divider can be added back without
    ///   touching the two call sites).
    /// - `colors` — the theme-role colors (see [`TabBarColors`]).
    ///
    /// Returns [`TabBarOutput`] with one explicit-background glyph per column and
    /// an **empty** quad list. Every cell's background + label attributes carry
    /// the whole Phosphor Flat treatment (shared [`super::tab_chrome`]): the
    /// active slot gets the `selection` fill + a bright bold auto-blooming label,
    /// inactive slots recede into the wallpaper-through background with a
    /// distance-ramped dim label, and hover warms one tier.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &self,
        source: &dyn TabBarSource,
        grid_cols: usize,
        y_offset_px: f32,
        cell: CellSize,
        padding: WindowPadding,
        colors: TabBarColors,
        panel_strength: f32,
    ) -> TabBarOutput {
        let _ = (y_offset_px, cell, padding);
        if grid_cols == 0 {
            return TabBarOutput::default();
        }
        let layout = compute_layout(source, grid_cols);
        let active_idx = source.active_tab();

        // Phosphor Flat palette (shared treatment; theme roles only). F4-P1: the
        // resting-cell surface is the panel tint (Layer 1 of ODP-1) rather than
        // the raw background, so the bar reads as one quiet surface even at
        // `cell_bg_opacity = 1`; `panel_strength = 0` collapses it to the theme
        // background (the pre-panel bare-labels look).
        let panel_srgb = tab_chrome::panel_tint(colors, panel_strength);
        let panel_surface = rgb(panel_srgb);
        // ACTIVE-FILL: lift the `selection` slab against THIS panel surface so it
        // clears the legibility ratio floor; hover re-bases on the panel toward
        // that guaranteed fill (rest < hover < active).
        let active_fill = rgb(tab_chrome::active_fill(colors, panel_srgb));
        let active_lbl = rgb(tab_chrome::active_label(colors));
        let hover_fill = rgb(tab_chrome::hover_fill(colors, panel_srgb));
        let hover_lbl = rgb(tab_chrome::hover_label(colors));
        // RAIL-PLUS-GAP / F4-PLUS: the resting `+` lifts out of the dim inactive
        // floor so it reads as a deliberate "add" control; hover still goes full
        // active-label bright on top of that.
        let rest_plus = rgb(tab_chrome::new_slot_plus_rest(colors));

        // The whole row starts as the panel surface — inactive tabs and
        // inter-slot gaps recede into it (no per-tab geometry).
        let mut row = vec![blank_glyph(0, panel_surface, panel_surface); grid_cols];
        for (col, glyph) in row.iter_mut().enumerate() {
            glyph.col = col;
        }

        for slot in &layout.slots {
            let is_active = slot.idx == active_idx;
            let is_hovered = is_slot_hovered(self.hover, slot.idx);
            // Active: warm `selection` fill + bright bold label. Hovered inactive:
            // whisper fill + one-tier-warmer label. Otherwise: no fill, a label
            // dimmed along the phosphor ramp by its distance from the active tab.
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

            for col in slot.start_col..slot.end_col.min(row.len()) {
                row[col].attrs.background = slot_bg;
            }

            let mut la = Attrs::default();
            la.foreground = label_fg;
            la.background = slot_bg;
            if bold {
                la.set_bold(true);
            }
            for (i, ch_char) in slot.label.chars().enumerate() {
                let col = slot.label_col + i;
                if let Some(glyph) = row.get_mut(col) {
                    glyph.ch = ch_char;
                    glyph.attrs = la;
                }
            }

            // Close `×` glyph — rendered only for the active or hovered tab.
            if let Some(close_col) = slot.close_col.filter(|_| is_active || is_hovered)
                && let Some(glyph) = row.get_mut(close_col)
            {
                glyph.ch = '×';
                glyph.attrs = la;
            }
        }

        // New-tab `+` affordance: a lifted glyph on the bare wallpaper at rest
        // (brighter than an inactive tab label so it reads as an add control),
        // brightening further (and gaining a whisper fill) on hover.
        if let Some(nt_col) = layout.new_tab_col {
            let is_hovered = matches!(self.hover, Some(TabHit::NewTab));
            let (nt_bg, nt_fg) = if is_hovered {
                (hover_fill, active_lbl)
            } else {
                (panel_surface, rest_plus)
            };
            for col in nt_col..(nt_col + NEW_TAB_COLS).min(row.len()) {
                row[col].attrs.background = nt_bg;
            }
            let mut a = Attrs::default();
            a.foreground = nt_fg;
            a.background = nt_bg;
            // Centre the `+` in the NEW_TAB_COLS block (offset 1 from block start).
            if let Some(glyph) = row.get_mut(nt_col + 1) {
                glyph.ch = '+';
                glyph.attrs = a;
            }
        }

        // Phosphor Flat draws no chrome quads — the treatment is entirely cell
        // backgrounds + label attributes (see the module docs).
        TabBarOutput {
            quads: Vec::new(),
            glyphs: row,
        }
    }

    /// Map a physical-pixel pointer position to a [`TabHit`].
    ///
    /// - `(px_x, px_y)` — pointer in physical pixels (winit `CursorMoved`).
    /// - `y_offset_px` — physical-pixel Y of the top of the tab bar row.
    ///
    /// Returns [`TabHit::None`] when the pointer is outside the tab bar row or
    /// outside all interactive regions.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn hit_test(
        &self,
        px_x: f64,
        px_y: f64,
        source: &dyn TabBarSource,
        grid_cols: usize,
        y_offset_px: f32,
        cell: CellSize,
        padding: WindowPadding,
        rows: usize,
    ) -> TabHit {
        let cw = cell.width as f32;
        let ch = cell.height as f32;
        if cw <= 0.0 || ch <= 0.0 {
            return TabHit::None;
        }
        // Y check — pointer must be inside the tab bar BAND (its full resolved
        // height, not just one row): a click anywhere in a taller band still
        // hits the tab under its column. `rows` is >= 1.
        let y = px_y as f32;
        if y < y_offset_px || y >= y_offset_px + ch * rows.max(1) as f32 {
            return TabHit::None;
        }
        // Map X to a column index.
        let pad = padding.as_f32();
        let x = px_x as f32 - pad;
        if x < 0.0 {
            return TabHit::None;
        }
        let col = (x / cw) as usize;
        if col >= grid_cols {
            return TabHit::None;
        }

        let layout = compute_layout(source, grid_cols);

        // New-tab affordance (rightmost block).
        if layout
            .new_tab_col
            .is_some_and(|nt| col >= nt && col < nt + NEW_TAB_COLS)
        {
            return TabHit::NewTab;
        }

        // Tab slots — iterate in reverse so the rightmost slot at a shared
        // boundary resolves first (slots never overlap in practice).
        for slot in layout.slots.iter().rev() {
            if col < slot.start_col || col >= slot.end_col {
                continue;
            }
            if slot.close_col.is_some_and(|cc| col == cc) {
                return TabHit::Close(slot.idx);
            }
            return TabHit::Switch(slot.idx);
        }

        TabHit::None
    }

    /// Map a physical pointer X to a tab insertion index. Each slot flips at
    /// its horizontal midpoint, matching the rail's vertical drop policy.
    pub(super) fn drop_index(
        &self,
        px_x: f64,
        source: &dyn TabBarSource,
        grid_cols: usize,
        cell: CellSize,
        padding: WindowPadding,
    ) -> Option<usize> {
        if cell.width == 0 || grid_cols == 0 {
            return None;
        }
        let layout = compute_layout(source, grid_cols);
        let last = layout.slots.last()?;
        let local_x = px_x - f64::from(padding.as_f32());
        let mut insert = last.idx + 1;
        for slot in &layout.slots {
            let midpoint = (slot.start_col + slot.end_col) as f64 * f64::from(cell.width) / 2.0;
            if local_x < midpoint {
                insert = slot.idx;
                break;
            }
        }
        Some(insert)
    }

    /// Paint top-strip grab feedback, a live horizontal proxy, and the drop
    /// boundary over an already-rendered bar.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_drag_overlay(
        &self,
        glyphs: &mut [TabBarGlyph],
        origin_idx: usize,
        drop_idx: usize,
        armed: bool,
        pointer_x_px: f64,
        source: &dyn TabBarSource,
        grid_cols: usize,
        cell: CellSize,
        padding: WindowPadding,
        colors: TabBarColors,
        panel_surface: Srgb,
    ) {
        if grid_cols == 0 || glyphs.len() < grid_cols || cell.width == 0 {
            return;
        }
        let layout = compute_layout(source, grid_cols);
        let Some(slot) = layout.slots.iter().find(|slot| slot.idx == origin_idx) else {
            return;
        };
        let span = slot.end_col.saturating_sub(slot.start_col);
        if span == 0 {
            return;
        }
        let lifted_fill = rgb(tab_chrome::active_fill(colors, panel_surface));
        let lifted_label = rgb(tab_chrome::active_label(colors));
        let recessed_label = rgb(tab_chrome::inactive_label(colors, 1));
        let panel = rgb(panel_surface);
        let source_cells = glyphs[slot.start_col..slot.end_col.min(grid_cols)].to_vec();

        for glyph in &mut glyphs[slot.start_col..slot.end_col.min(grid_cols)] {
            glyph.attrs.background = if armed { panel } else { lifted_fill };
            if glyph.ch != ' ' {
                glyph.attrs.foreground = if armed { recessed_label } else { lifted_label };
                glyph.attrs.set_bold(!armed);
            }
        }

        if armed {
            let pointer_col = ((pointer_x_px - f64::from(padding.as_f32())) / f64::from(cell.width))
                .floor() as isize;
            let proxy_start = pointer_col
                .saturating_sub((span / 2) as isize)
                .clamp(0, grid_cols.saturating_sub(span) as isize)
                as usize;
            for (offset, source_glyph) in source_cells.into_iter().enumerate() {
                let mut proxy = source_glyph;
                proxy.col = proxy_start + offset;
                proxy.attrs.background = lifted_fill;
                if proxy.ch != ' ' {
                    proxy.attrs.foreground = lifted_label;
                    proxy.attrs.set_bold(true);
                }
                glyphs[proxy.col] = proxy;
            }
        }

        if armed {
            let boundary = layout
                .slots
                .iter()
                .find(|slot| slot.idx == drop_idx)
                .map_or_else(
                    || layout.slots.last().map_or(0, |slot| slot.end_col),
                    |slot| slot.start_col,
                )
                .min(grid_cols - 1);
            glyphs[boundary].ch = '\u{2503}';
            glyphs[boundary].attrs.foreground = lifted_label;
            glyphs[boundary].attrs.background = lifted_fill;
            glyphs[boundary].attrs.set_bold(true);
        }
    }
}

// ---------------------------------------------------------------------------
// Layout computation
// ---------------------------------------------------------------------------

/// Build the column-slot layout for the current tab set.
///
/// Pure function: takes no mutable state so both `render` and `hit_test` can
/// call it independently without caching concerns.
fn compute_layout(source: &dyn TabBarSource, grid_cols: usize) -> TabLayout {
    let tab_count = source.tab_count();

    // Grid too narrow for the new-tab button, or no tabs at all.
    if grid_cols < NEW_TAB_COLS {
        return TabLayout {
            slots: Vec::new(),
            new_tab_col: None,
        };
    }
    let new_tab_col = Some(grid_cols - NEW_TAB_COLS);
    if tab_count == 0 {
        return TabLayout {
            slots: Vec::new(),
            new_tab_col,
        };
    }

    // Columns available for tab slots (everything left of the new-tab button).
    let available = grid_cols - NEW_TAB_COLS;

    // Equal slot width, clamped to [MIN_TAB_COLS, MAX_TAB_COLS].  If even the
    // minimum doesn't fit all tabs, the loop breaks when space is exhausted.
    let slot_width = (available / tab_count).clamp(MIN_TAB_COLS, MAX_TAB_COLS);
    let col_limit = available; // exclusive upper bound for slot columns

    let mut slots = Vec::with_capacity(tab_count);
    let mut col = 0usize;

    for i in 0..tab_count {
        if col >= col_limit {
            break;
        }
        let start_col = col;
        let end_col = (start_col + slot_width).min(col_limit);
        if end_col <= start_col {
            break;
        }
        let width = end_col - start_col;

        // Label area: [TAB_PADDING][…title…][CLOSE_COLS]
        let label_budget = width.saturating_sub(TAB_PADDING + CLOSE_COLS);
        let (label, close_col) = if label_budget >= 1 {
            let title = source.tab_title(i);
            let truncated = truncate_label(title, label_budget);
            let cc = end_col.saturating_sub(1);
            (truncated, Some(cc))
        } else {
            (String::new(), None)
        };

        slots.push(TabSlot {
            idx: i,
            start_col,
            end_col,
            close_col,
            label,
            label_col: start_col + TAB_PADDING,
        });

        col = end_col;
    }

    TabLayout { slots, new_tab_col }
}

fn blank_glyph(col: usize, foreground: Color, background: Color) -> TabBarGlyph {
    let mut attrs = Attrs::default();
    attrs.foreground = foreground;
    attrs.background = background;
    TabBarGlyph {
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

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Return `true` when the hover state covers the body or close affordance of
/// the slot at `idx`.
fn is_slot_hovered(hover: Option<TabHit>, idx: usize) -> bool {
    match hover {
        Some(TabHit::Switch(i)) | Some(TabHit::Close(i)) => i == idx,
        _ => false,
    }
}

/// Truncate `s` to at most `max_cols` display columns.  Each Unicode scalar
/// value counts as one column, which is correct for the ASCII-heavy titles
/// typical of terminal tab bars.  Leading/trailing whitespace is stripped.
/// When truncation is needed, the last column holds `…`.
fn truncate_label(s: &str, max_cols: usize) -> String {
    let chars: Vec<char> = s.trim().chars().collect();
    if chars.len() <= max_cols {
        return chars.iter().collect();
    }
    let mut out: String = chars[..max_cols.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::relative_luminance;

    // -----------------------------------------------------------------------
    // Mock TabBarSource
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    const CELL: CellSize = CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    };
    const GRID_COLS: usize = 80;

    fn bar() -> TabBar {
        TabBar::default()
    }

    fn hovered_bar(hit: TabHit) -> TabBar {
        let mut b = TabBar::default();
        b.set_hover(Some(hit));
        b
    }

    const COLORS: TabBarColors = TabBarColors {
        foreground: (0xCC, 0xCC, 0xCC),
        background: (0x10, 0x10, 0x10),
        inactive: (0x66, 0x66, 0x66),
        active_bg: (0x40, 0x60, 0x90),
    };

    /// Panel strength used by the shared render helpers. `0.0` collapses the
    /// panel tint to the theme background so the layout/treatment assertions
    /// stay expressed against `wallpaper_background`; the panel surface itself is
    /// covered by `resting_cells_paint_the_panel_tint_at_strength` + the
    /// tab_chrome full-roster theme guard.
    const PANEL_STRENGTH: f32 = 0.0;

    fn render_with(b: &TabBar, src: &dyn TabBarSource) -> TabBarOutput {
        b.render(
            src,
            GRID_COLS,
            0.0,
            CELL,
            WindowPadding::ZERO,
            COLORS,
            PANEL_STRENGTH,
        )
    }

    fn render_default(src: &dyn TabBarSource) -> TabBarOutput {
        render_with(&bar(), src)
    }

    /// Relative luminance of a cell-attribute color (glyphs only ever carry
    /// `Color::Rgb` in this widget).
    fn luma(color: Color) -> f64 {
        match color {
            Color::Rgb(r, g, b) => relative_luminance((r, g, b)),
            other => panic!("expected an explicit Rgb color, got {other:?}"),
        }
    }

    // Compute a pointer X (px) that lands in the centre of `col`.
    fn col_centre_px(col: usize) -> f64 {
        col as f64 * CELL.width as f64 + CELL.width as f64 / 2.0
    }

    fn hit_at_col(col: usize, src: &dyn TabBarSource) -> TabHit {
        bar().hit_test(
            col_centre_px(col),
            8.0, // y inside the 16px-tall row
            src,
            GRID_COLS,
            0.0,
            CELL,
            WindowPadding::ZERO,
            1,
        )
    }

    // -----------------------------------------------------------------------
    // Phosphor Flat — no chrome quads (fails-before: the outline era emitted
    // rings + a band separator)
    // -----------------------------------------------------------------------

    #[test]
    fn render_emits_no_chrome_quads_on_any_tab_count() {
        // The whole treatment is cell backgrounds + label attributes; no rings,
        // no underline, no separator — so the quad list is always empty.
        for src in [
            MockSource::empty(),
            MockSource::new(&["a"], 0),
            MockSource::new(&["a", "b", "c"], 1),
        ] {
            let out = render_default(&src);
            assert!(
                out.quads.is_empty(),
                "Phosphor Flat emits no chrome quads (got {})",
                out.quads.len()
            );
        }
    }

    #[test]
    fn resting_cells_paint_the_panel_tint_at_strength() {
        // F4-P1: at a live strength the resting bar cells paint the panel tint
        // (Layer 1), not the raw background — and that tint differs from the
        // background on the dark palette. At strength 0 they collapse back to
        // the background (the pre-panel look).
        let src = MockSource::new(&["a", "b"], 0);
        let out = bar().render(&src, GRID_COLS, 0.0, CELL, WindowPadding::ZERO, COLORS, 0.5);
        let layout = compute_layout(&src, GRID_COLS);
        let inactive_bg = out.glyphs[layout.slots[1].label_col].attrs.background;
        assert_eq!(
            inactive_bg,
            rgb(tab_chrome::panel_tint(COLORS, 0.5)),
            "resting cells paint the panel tint at strength 0.5"
        );
        assert_ne!(
            inactive_bg,
            rgb(tab_chrome::wallpaper_background(COLORS)),
            "the panel tint is distinct from the raw background"
        );
        let out0 = bar().render(&src, GRID_COLS, 0.0, CELL, WindowPadding::ZERO, COLORS, 0.0);
        assert_eq!(
            out0.glyphs[layout.slots[1].label_col].attrs.background,
            rgb(tab_chrome::wallpaper_background(COLORS)),
            "strength 0 collapses the panel to the background"
        );
    }

    // -----------------------------------------------------------------------
    // Zero tabs
    // -----------------------------------------------------------------------

    #[test]
    fn zero_tabs_produces_explicit_row_cells_and_new_tab_affordance() {
        let src = MockSource::empty();
        let out = render_default(&src);
        assert_eq!(out.glyphs.len(), GRID_COLS, "one glyph per column");
        assert!(
            out.glyphs.iter().filter(|g| g.ch == '+').count() == 1,
            "only one new-tab glyph is present with zero tabs"
        );
    }

    #[test]
    fn zero_tabs_hit_returns_new_tab_in_affordance_zone() {
        let src = MockSource::empty();
        let layout = compute_layout(&src, GRID_COLS);
        let nt_col = layout.new_tab_col.expect("new-tab column present");
        let hit = hit_at_col(nt_col + 1, &src);
        assert_eq!(hit, TabHit::NewTab, "new-tab zone → NewTab with zero tabs");
    }

    // -----------------------------------------------------------------------
    // Single tab
    // -----------------------------------------------------------------------

    #[test]
    fn one_tab_contains_label_glyph() {
        let src = MockSource::new(&["zsh"], 0);
        let out = render_default(&src);
        let label: String = out
            .glyphs
            .iter()
            .filter(|g| g.ch != '+' && g.ch != '×' && g.ch != ' ')
            .map(|g| g.ch)
            .collect();
        assert!(label.contains("zsh"), "label 'zsh' present in glyphs");
    }

    #[test]
    fn one_tab_active_label_is_bold() {
        let src = MockSource::new(&["zsh"], 0);
        let out = render_default(&src);
        let first = out
            .glyphs
            .iter()
            .find(|g| g.ch == 'z')
            .expect("glyph 'z' present");
        assert!(first.attrs.bold(), "active tab label is bold");
    }

    #[test]
    fn one_tab_renders_close_glyph() {
        let src = MockSource::new(&["zsh"], 0);
        let out = render_default(&src);
        assert!(
            out.glyphs.iter().any(|g| g.ch == '×'),
            "close × rendered for active tab"
        );
    }

    #[test]
    fn one_tab_renders_new_tab_glyph() {
        let src = MockSource::new(&["zsh"], 0);
        let out = render_default(&src);
        assert!(
            out.glyphs.iter().any(|g| g.ch == '+'),
            "new-tab + glyph is present"
        );
    }

    // -----------------------------------------------------------------------
    // Three tabs — active = middle
    // -----------------------------------------------------------------------

    #[test]
    fn three_tabs_produce_three_layout_slots() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        assert_eq!(layout.slots.len(), 3, "three tabs → three layout slots");
    }

    #[test]
    fn tab_drop_index_flips_at_each_slot_midpoint() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        for slot in &layout.slots {
            let midpoint_cols = (slot.start_col + slot.end_col) as f64 / 2.0;
            let midpoint_px = midpoint_cols * f64::from(CELL.width);
            assert_eq!(
                bar().drop_index(
                    midpoint_px - 0.1,
                    &src,
                    GRID_COLS,
                    CELL,
                    WindowPadding::ZERO
                ),
                Some(slot.idx)
            );
        }
        assert_eq!(
            bar().drop_index(10_000.0, &src, GRID_COLS, CELL, WindowPadding::ZERO),
            Some(3)
        );
    }

    #[test]
    fn tab_drag_lifts_then_tracks_a_horizontal_proxy() {
        let src = MockSource::new(&["alpha", "beta", "gamma"], 2);
        let panel = tab_chrome::panel_tint(COLORS, PANEL_STRENGTH);
        let layout = compute_layout(&src, GRID_COLS);
        let source = &layout.slots[0];
        let mut pending = render_default(&src).glyphs;
        let before = pending[source.label_col].attrs.background;
        bar().paint_drag_overlay(
            &mut pending,
            0,
            0,
            false,
            8.0,
            &src,
            GRID_COLS,
            CELL,
            WindowPadding::ZERO,
            COLORS,
            panel,
        );
        assert_ne!(pending[source.label_col].attrs.background, before);
        assert!(pending[source.label_col].attrs.bold());

        let mut dragged = render_default(&src).glyphs;
        let pointer_x = 50.0 * f64::from(CELL.width);
        bar().paint_drag_overlay(
            &mut dragged,
            0,
            2,
            true,
            pointer_x,
            &src,
            GRID_COLS,
            CELL,
            WindowPadding::ZERO,
            COLORS,
            panel,
        );
        let proxy_start = 50usize.saturating_sub((source.end_col - source.start_col) / 2);
        assert_eq!(dragged[proxy_start + TAB_PADDING].ch, 'a');
        assert!(!dragged[source.label_col].attrs.bold());
    }

    #[test]
    fn three_tabs_slots_are_contiguous() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        for i in 1..layout.slots.len() {
            assert_eq!(
                layout.slots[i].start_col,
                layout.slots[i - 1].end_col,
                "slots must be contiguous / non-overlapping"
            );
        }
    }

    #[test]
    fn three_tabs_active_middle_only_middle_bold() {
        let src = MockSource::new(&["alpha", "beta", "gamma"], 1);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let mid = &layout.slots[1];
        for g in &out.glyphs {
            if g.attrs.bold() {
                assert!(
                    g.col >= mid.start_col && g.col < mid.end_col,
                    "bold glyph col {} outside middle slot {}..{}",
                    g.col,
                    mid.start_col,
                    mid.end_col,
                );
            }
        }
    }

    #[test]
    fn three_tabs_active_last_slot_bold() {
        let src = MockSource::new(&["a", "b", "c"], 2);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let last = layout.slots.last().expect("three slots");
        for g in &out.glyphs {
            if g.attrs.bold() {
                assert!(
                    g.col >= last.start_col && g.col < last.end_col,
                    "bold glyph col {} not in last slot {}..{}",
                    g.col,
                    last.start_col,
                    last.end_col,
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Title truncation
    // -----------------------------------------------------------------------

    #[test]
    fn long_title_truncated_with_ellipsis() {
        let long = "a-very-long-terminal-tab-title-that-exceeds-the-maximum-column-budget";
        let src = MockSource::new(&[long], 0);
        let out = render_default(&src);
        let label: String = out
            .glyphs
            .iter()
            .filter(|g| g.ch != '+' && g.ch != '×' && g.ch != ' ')
            .map(|g| g.ch)
            .collect();
        assert!(label.contains('…'), "long title ends with '…'");
        let layout = compute_layout(&src, GRID_COLS);
        let budget = layout.slots[0]
            .end_col
            .saturating_sub(TAB_PADDING + CLOSE_COLS);
        assert!(
            label.chars().count() <= budget,
            "truncated label ({} chars) fits in budget ({})",
            label.chars().count(),
            budget,
        );
    }

    #[test]
    fn short_title_not_truncated() {
        let src = MockSource::new(&["vim"], 0);
        let out = render_default(&src);
        let label: String = out
            .glyphs
            .iter()
            .filter(|g| g.ch != '+' && g.ch != '×' && g.ch != ' ')
            .map(|g| g.ch)
            .collect();
        assert!(!label.contains('…'), "short title is not truncated");
        assert!(label.contains("vim"), "full title present");
    }

    // -----------------------------------------------------------------------
    // Hit test — bodies, close, new-tab, out of bounds
    // -----------------------------------------------------------------------

    #[test]
    fn hit_test_body_of_each_tab() {
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        for slot in &layout.slots {
            let hit = hit_at_col(slot.label_col, &src);
            assert_eq!(
                hit,
                TabHit::Switch(slot.idx),
                "body of tab {} → Switch",
                slot.idx
            );
        }
    }

    #[test]
    fn hit_test_close_column_returns_close() {
        let src = MockSource::new(&["bash"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        let close_col = layout.slots[0]
            .close_col
            .expect("slot wide enough for close button");
        let hit = hit_at_col(close_col, &src);
        assert_eq!(hit, TabHit::Close(0), "close column → Close(0)");
    }

    #[test]
    fn hit_test_close_column_of_second_tab() {
        let src = MockSource::new(&["bash", "vim"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        let close_col = layout.slots[1]
            .close_col
            .expect("slot 1 wide enough for close button");
        let hit = hit_at_col(close_col, &src);
        assert_eq!(hit, TabHit::Close(1), "close col of tab 1 → Close(1)");
    }

    #[test]
    fn hit_test_new_tab_centre() {
        let src = MockSource::new(&["a"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        let nt_col = layout.new_tab_col.expect("new-tab column present");
        let hit = hit_at_col(nt_col + 1, &src);
        assert_eq!(hit, TabHit::NewTab, "new-tab centre col → NewTab");
    }

    #[test]
    fn hit_test_new_tab_left_edge() {
        let src = MockSource::new(&["a"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        let nt_col = layout.new_tab_col.expect("new-tab column present");
        let hit = hit_at_col(nt_col, &src);
        assert_eq!(hit, TabHit::NewTab, "new-tab left edge → NewTab");
    }

    #[test]
    fn hit_test_above_bar_row_is_none() {
        let src = MockSource::new(&["a"], 0);
        let hit = bar().hit_test(
            CELL.width as f64,
            -1.0,
            &src,
            GRID_COLS,
            0.0,
            CELL,
            WindowPadding::ZERO,
            1,
        );
        assert_eq!(hit, TabHit::None, "above the bar → None");
    }

    #[test]
    fn hit_test_below_bar_row_is_none() {
        let src = MockSource::new(&["a"], 0);
        let hit = bar().hit_test(
            CELL.width as f64,
            CELL.height as f64 + 1.0,
            &src,
            GRID_COLS,
            0.0,
            CELL,
            WindowPadding::ZERO,
            1,
        );
        assert_eq!(hit, TabHit::None, "below the bar → None");
    }

    #[test]
    fn hit_test_beyond_right_edge_is_none() {
        let src = MockSource::new(&["a"], 0);
        let hit = bar().hit_test(
            (GRID_COLS + 5) as f64 * CELL.width as f64,
            8.0,
            &src,
            GRID_COLS,
            0.0,
            CELL,
            WindowPadding::ZERO,
            1,
        );
        assert_eq!(hit, TabHit::None, "beyond right edge → None");
    }

    #[test]
    fn narrow_grid_renders_without_panic() {
        let src = MockSource::new(&["a", "b", "c", "d", "e"], 0);
        let out = bar().render(
            &src,
            20,
            0.0,
            CELL,
            WindowPadding::ZERO,
            COLORS,
            PANEL_STRENGTH,
        );
        assert_eq!(out.glyphs.len(), 20, "one glyph per column");
    }

    #[test]
    fn render_emits_one_cell_per_column() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        assert_eq!(out.glyphs.len(), GRID_COLS, "all columns are explicit");
    }

    #[test]
    fn active_tab_background_covers_gap_before_close_button() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let slot = &layout.slots[0];
        let gap_col = slot.end_col - CLOSE_COLS;
        assert_eq!(
            out.glyphs[gap_col].attrs.background,
            out.glyphs[slot.label_col].attrs.background
        );
    }

    // -----------------------------------------------------------------------
    // Phosphor Flat — fill / label / ramp / hover treatment
    // -----------------------------------------------------------------------

    #[test]
    fn active_slot_filled_with_selection_inactive_is_wallpaper_through() {
        // Active tab → `selection` fill; inactive tab → the wallpaper-through
        // `background` (no fill, recedes into the wallpaper). Replaces the
        // outline era's ring assertions.
        let src = MockSource::new(&["aaa", "bbb"], 0);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let active_fill = rgb(tab_chrome::active_fill(
            COLORS,
            tab_chrome::panel_tint(COLORS, PANEL_STRENGTH),
        ));
        let wallpaper = rgb(tab_chrome::wallpaper_background(COLORS));
        assert_eq!(
            out.glyphs[layout.slots[0].label_col].attrs.background, active_fill,
            "active slot filled with the selection role"
        );
        assert_eq!(
            out.glyphs[layout.slots[1].label_col].attrs.background, wallpaper,
            "inactive slot is wallpaper-through (no fill)"
        );
        assert_ne!(active_fill, wallpaper, "active fill differs from wallpaper");
    }

    #[test]
    fn active_label_is_bright_bold_and_clears_the_bloom_threshold() {
        // The active label is painted bright above the bloom threshold (0.7) on
        // this dark palette so bloom.wgsl auto-halos it — and it is bold.
        let src = MockSource::new(&["zsh", "bash"], 0);
        let out = render_default(&src);
        let g = out.glyphs.iter().find(|g| g.ch == 'z').expect("'z' glyph");
        assert!(g.attrs.bold(), "active label is bold");
        assert_eq!(
            g.attrs.foreground,
            rgb(tab_chrome::active_label(COLORS)),
            "active label uses the brightened treatment color"
        );
        assert!(
            luma(g.attrs.foreground) >= 0.7,
            "active label luma {} must clear the bloom threshold 0.7",
            luma(g.attrs.foreground)
        );
    }

    #[test]
    fn inactive_labels_dim_along_the_phosphor_ramp_with_distance() {
        // Active = tab 0; tabs 1,2,3 are at increasing distance and must be
        // monotonically non-increasing in luminance (phosphor persistence).
        let src = MockSource::new(&["a", "b", "c", "d"], 0);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let mut prev = f64::INFINITY;
        for slot in layout.slots.iter().skip(1) {
            let l = luma(out.glyphs[slot.label_col].attrs.foreground);
            assert!(
                l <= prev + 1e-9,
                "inactive label at slot {} brighter than a nearer tab",
                slot.idx
            );
            assert!(
                !out.glyphs[slot.label_col].attrs.bold(),
                "inactive not bold"
            );
            prev = l;
        }
        assert_eq!(
            out.glyphs[layout.slots[1].label_col].attrs.foreground,
            rgb(tab_chrome::inactive_label(COLORS, 1)),
        );
    }

    #[test]
    fn hovered_inactive_gets_a_whisper_fill_and_a_lifted_label() {
        // A hovered inactive tab gains a subordinate fill and a label warmed one
        // tier toward the active label — brighter than its resting dim label but
        // never as strong as the active tab.
        let src = MockSource::new(&["a", "b", "c"], 0);
        let layout = compute_layout(&src, GRID_COLS);
        let hovered = &layout.slots[2];
        let out = render_with(&hovered_bar(TabHit::Switch(hovered.idx)), &src);
        assert_eq!(
            out.glyphs[hovered.label_col].attrs.background,
            rgb(tab_chrome::hover_fill(
                COLORS,
                tab_chrome::panel_tint(COLORS, PANEL_STRENGTH)
            )),
            "hovered tab gains the whisper fill"
        );
        let hover_luma = luma(out.glyphs[hovered.label_col].attrs.foreground);
        let rest_luma = luma(rgb(tab_chrome::inactive_label(COLORS, hovered.idx)));
        let active_luma = luma(rgb(tab_chrome::active_label(COLORS)));
        assert!(
            hover_luma > rest_luma,
            "hover lifts the label above its rest tier"
        );
        assert!(
            hover_luma < active_luma,
            "hover stays subordinate to the active label"
        );
        assert_ne!(
            out.glyphs[hovered.label_col].attrs.background,
            rgb(tab_chrome::active_fill(
                COLORS,
                tab_chrome::panel_tint(COLORS, PANEL_STRENGTH)
            )),
            "hover fill is a whisper, not the active fill"
        );
    }

    #[test]
    fn new_tab_plus_is_visible_at_rest_and_brighter_on_hover() {
        // RAIL-PLUS-GAP / F4-PLUS: the resting `+` is lifted above the dim
        // inactive-label floor so it reads as a deliberate add control, yet
        // stays subordinate to the active label; hover brightens it further.
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
        assert_eq!(
            plus.attrs.background,
            rgb(tab_chrome::wallpaper_background(COLORS)),
            "+ sits on the bare wallpaper at rest"
        );
        let rest_luma = luma(plus.attrs.foreground);
        assert!(
            rest_luma > luma(rgb(COLORS.inactive)),
            "resting + is more visible than an inactive tab label"
        );
        assert!(
            rest_luma < luma(rgb(tab_chrome::active_label(COLORS))),
            "resting + stays subordinate to the active label"
        );
        let out = render_with(&hovered_bar(TabHit::NewTab), &src);
        let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
        assert_eq!(
            plus.attrs.background,
            rgb(tab_chrome::hover_fill(
                COLORS,
                tab_chrome::panel_tint(COLORS, PANEL_STRENGTH)
            )),
            "+ gains the whisper fill on hover"
        );
        assert!(
            luma(plus.attrs.foreground) > rest_luma,
            "+ brightens further on hover than at rest"
        );
    }

    #[test]
    fn close_glyph_shows_only_for_active_or_hovered_tabs() {
        let src = MockSource::new(&["aa", "bb"], 0);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let inactive_close = layout.slots[1].close_col.expect("close col");
        assert_ne!(
            out.glyphs[inactive_close].ch, '×',
            "inactive unhovered tab has no ×"
        );
        let out = render_with(&hovered_bar(TabHit::Switch(1)), &src);
        assert_eq!(out.glyphs[inactive_close].ch, '×', "hovered tab shows ×");
    }

    #[test]
    fn bloom_off_fallback_active_tab_is_identifiable_without_glow() {
        // With bloom disabled there is no halo, so the active tab must still be
        // identifiable from the fill + bold bright label alone (F4R-NF3). Both
        // are emitted unconditionally — this asserts the fallback signals exist,
        // independent of any post-process.
        let src = MockSource::new(&["one", "two", "three"], 1);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let active = &layout.slots[1];
        let inactive = &layout.slots[0];
        assert_ne!(
            out.glyphs[active.label_col].attrs.background,
            out.glyphs[inactive.label_col].attrs.background,
            "active fill locatable vs inactive"
        );
        assert!(out.glyphs[active.label_col].attrs.bold());
        assert_ne!(
            out.glyphs[active.label_col].attrs.foreground,
            out.glyphs[inactive.label_col].attrs.foreground,
            "active label distinct from inactive"
        );
    }
}
