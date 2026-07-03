// SPDX-License-Identifier: GPL-3.0-only
//! Tab bar widget — presentation-only, decoupled from the session model.
//!
//! Renders a one-row tab strip across the top of the window. The widget is
//! purely geometrical: it reads layout from a [`TabBarSource`] trait object
//! (implemented on `TabSet`) and produces solid quads + glyph outputs that the
//! integration layer composites into the frame. It never touches terminal
//! state, PTY, or settings.
//!
//! ## Integration (live)
//! Both render paths in `app/` drive this widget:
//! `App::decorate_snapshot_with_tab_bar` (single-pane) and
//! `TabSet`-backed `tab_bar_strip` (multi-pane) call [`TabBar::render`] each
//! frame, push the returned quads into the overlay quad list, and paint each
//! glyph into the reserved tab-bar snapshot row
//! (`snapshot.cells[glyph.col] = Cell::new(glyph.ch, glyph.attrs)`);
//! `App` pointer handling calls [`TabBar::hit_test`] on move (stored via
//! [`TabBar::set_hover`]) and again on press to resolve the action.
//!
//! ## Visual treatment (F4 v1.1)
//! Monochrome, all from theme roles (no hardcoded colors, so every theme and the
//! CVD modes stay correct). Two orthogonal visual languages:
//!
//! - **Shape** — *every* tab (active and inactive alike) is framed by an opaque
//!   outline ring so each slot reads as a bounded object. The active tab uses the
//!   full-strength `border` role; inactive tabs use a subordinate outline blended
//!   from `border` toward the band ([`INACTIVE_OUTLINE_BLEND`]) so they are
//!   visible but clearly secondary.
//! - **State** — the active tab is filled with the `selection` role (as in the
//!   0.7.5 build) and carries a bold `foreground` label. Inactive tabs have no
//!   fill (they sit on the band) with a dimmed `inactive` label. A hovered
//!   inactive tab gets a *subordinate* fill blended from the band toward
//!   `selection` ([`HOVER_FILL_BLEND`]) — never strong enough to be confused with
//!   the active fill.
//!
//! The bar row is a distinct band derived from `background`, and a full-width
//! `border`-role separator divides the band from the terminal body. There is no
//! underline. All chrome quads are fully opaque so the bar reads over background
//! images/effects (the house image-proof rule).

// `use super::*` brings in everything imported at the `app/mod.rs` level:
// `Color`, `SolidQuad`, `CellSize`, `text` (module), `WindowPadding`, etc.
// `Attrs` and `Srgb` are not re-exported from `app` so they are added here.
use super::*;
use crate::core::Attrs;
use crate::theme::Srgb;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Physical-pixel row count the tab bar occupies (one cell-height row).
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

/// How far the bar-band fill is blended from `background` toward `inactive`
/// (F4 T1). Small, so the band reads as a distinct strip without fighting the
/// terminal body or leaving the monochrome + one-accent palette.
const BAND_BLEND: f32 = 0.16;

/// Thickness (physical px) of the band↔body separator line (F4 T1).
const SEPARATOR_PX: f32 = 1.0;

/// How far the inactive-tab outline is blended from `border` toward the band
/// fill (F4 v1.1). Keeps every tab framed as a bounded object while making the
/// inactive rings clearly subordinate to the full-strength active ring.
const INACTIVE_OUTLINE_BLEND: f32 = 0.55;

/// How far a hovered inactive tab's fill is blended from the band toward the
/// `selection` role (F4 v1.1). Deliberately partial so hover feedback never
/// reads as strongly as the active tab's full `selection` fill.
const HOVER_FILL_BLEND: f32 = 0.45;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Read-only interface to the session model.  GPT will `impl TabBarSource for
/// TabSet`; unit tests use an inline mock.
pub(in crate::native) trait TabBarSource {
    /// Number of open tabs (0 means the bar renders empty).
    fn tab_count(&self) -> usize;
    /// Display title for tab `idx`.  `idx` is guaranteed to be `< tab_count()`.
    fn tab_title(&self, idx: usize) -> &str;
    /// Zero-based index of the currently focused tab.
    fn active_tab(&self) -> usize;
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
    /// Solid pixel-space quads. The current integration leaves this empty and
    /// relies on explicit cell backgrounds for correct text contrast.
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
    /// Active-tab label text — theme `foreground`.
    pub(super) foreground: Srgb,
    /// Terminal body background — theme `background`. The bar band is derived
    /// from this (blended toward `inactive` by [`BAND_BLEND`]).
    pub(super) background: Srgb,
    /// Dimmed color for inactive tab labels — theme `inactive`.
    pub(super) inactive: Srgb,
    /// Active slot fill (and the base a hover fill is blended toward) — theme
    /// `selection`.
    pub(super) active_bg: Srgb,
    /// Band↔body separator and the per-tab outline rings — theme `border`.
    /// Inactive rings are blended from this toward the band.
    pub(super) border: Srgb,
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
    /// - `source` — session model accessor (mock or real `TabSet`).
    /// - `grid_cols` — number of terminal columns in the window.
    /// - `y_offset_px` — physical-pixel Y of the top of the tab bar row.
    /// - `cell` — cell metrics (width / height in physical pixels).
    /// - `padding` — window edge padding (shifts the left/right extents).
    /// - `colors` — the theme-role colors (see [`TabBarColors`]).
    ///
    /// Returns [`TabBarOutput`] with one explicit-background glyph per column,
    /// plus pixel-space quads: a full-width band↔body separator, a subordinate
    /// outline ring around every inactive slot, and a full-strength `border`
    /// ring around the active slot (drawn last so it stays crisp over any
    /// shared edge). The active slot is filled with the `selection` role
    /// (F4 v1.1).
    pub(super) fn render(
        &self,
        source: &dyn TabBarSource,
        grid_cols: usize,
        y_offset_px: f32,
        cell: CellSize,
        padding: WindowPadding,
        colors: TabBarColors,
    ) -> TabBarOutput {
        if grid_cols == 0 || cell.height == 0 || cell.width == 0 {
            return TabBarOutput::default();
        }
        let layout = compute_layout(source, grid_cols);
        let mut out = TabBarOutput::default();
        let base_fg = Color::Rgb(
            colors.foreground.0,
            colors.foreground.1,
            colors.foreground.2,
        );
        let dim_fg = Color::Rgb(colors.inactive.0, colors.inactive.1, colors.inactive.2);
        // The bar-band fill: `background` nudged toward `inactive` so the strip
        // reads as its own band, distinct from the terminal body below.
        let band_srgb = blend_srgb(colors.background, colors.inactive, BAND_BLEND);
        let band_bg = Color::Rgb(band_srgb.0, band_srgb.1, band_srgb.2);
        let active_bg_color =
            Color::Rgb(colors.active_bg.0, colors.active_bg.1, colors.active_bg.2);
        // Hover fill: band nudged toward `selection`, kept subordinate to the
        // active tab's full `selection` fill so the two are never confusable
        // (F4 v1.1).
        let hover_srgb = blend_srgb(band_srgb, colors.active_bg, HOVER_FILL_BLEND);
        let hover_bg_color = Color::Rgb(hover_srgb.0, hover_srgb.1, hover_srgb.2);
        // Inactive outline: `border` blended toward the band so every tab is
        // framed as a bounded object while staying subordinate to the active
        // ring (F4 v1.1).
        let inactive_outline_srgb = blend_srgb(colors.border, band_srgb, INACTIVE_OUTLINE_BLEND);
        let mut row = vec![blank_glyph(0, base_fg, band_bg); grid_cols];
        for (col, glyph) in row.iter_mut().enumerate() {
            glyph.col = col;
        }

        // Per-slot outlines: inactive rings accumulate here and are pushed
        // before the active ring so the active (full-strength) ring composites
        // on top of any shared edge and stays crisp (F4 v1.1).
        let mut inactive_outlines: Vec<SolidQuad> = Vec::new();
        let mut active_outline: Vec<SolidQuad> = Vec::new();
        for slot in &layout.slots {
            let is_active = slot.idx == source.active_tab();
            let is_hovered = is_slot_hovered(self.hover, slot.idx);
            // Active slot: full `selection` fill (as in 0.7.5). Hovered inactive
            // slot: subordinate hover fill. Otherwise the tab sits flush on the
            // band with no fill (F4 v1.1).
            let slot_bg = if is_active {
                active_bg_color
            } else if is_hovered {
                hover_bg_color
            } else {
                band_bg
            };

            // Every tab is framed by an outline ring — the shape language. The
            // active ring uses the full `border` role; inactive rings use the
            // subordinate blended color.
            let ring = active_tab_outline(
                slot.start_col,
                slot.end_col,
                y_offset_px,
                cell,
                padding,
                if is_active {
                    colors.border
                } else {
                    inactive_outline_srgb
                },
            );
            if is_active {
                active_outline = ring;
            } else {
                inactive_outlines.extend(ring);
            }

            for col in slot.start_col..slot.end_col.min(row.len()) {
                row[col].attrs.background = slot_bg;
            }

            let mut la = Attrs::default();
            // Active label: full-strength foreground + bold. Inactive label:
            // dimmed `inactive` role (F4 T1). A hovered inactive tab keeps the
            // dim label but gains the selection fill for feedback.
            la.foreground = if is_active { base_fg } else { dim_fg };
            la.background = slot_bg;
            if is_active {
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

        // New-tab `+` affordance.
        if let Some(nt_col) = layout.new_tab_col {
            let is_hovered = matches!(self.hover, Some(TabHit::NewTab));
            let new_tab_bg = if is_hovered { active_bg_color } else { band_bg };
            for col in nt_col..(nt_col + NEW_TAB_COLS).min(row.len()) {
                row[col].attrs.background = new_tab_bg;
            }
            let mut a = Attrs::default();
            a.foreground = base_fg;
            a.background = new_tab_bg;
            // Centre the `+` in the NEW_TAB_COLS block (offset 1 from block start).
            if let Some(glyph) = row.get_mut(nt_col + 1) {
                glyph.ch = '+';
                glyph.attrs = a;
            }
        }

        // Composite order (bottom → top):
        //  1. Band↔body separator: a full-width opaque `border`-role line along
        //     the bottom edge of the bar row, so the strip reads as a distinct
        //     band rather than floating labels (F4 T1).
        //  2. Inactive outline rings — subordinate, framing each inactive slot.
        //  3. The active outline ring — full `border` role, drawn last so it
        //     stays crisp where it shares an edge with a neighbor (F4 v1.1).
        // All opaque, so the chrome reads over background images (the house
        // image-proof rule).
        out.quads.extend(band_separator(
            grid_cols,
            y_offset_px,
            cell,
            padding,
            colors.border,
        ));
        out.quads.extend(inactive_outlines);
        out.quads.extend(active_outline);
        out.glyphs = row;
        out
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
    ) -> TabHit {
        let cw = cell.width as f32;
        let ch = cell.height as f32;
        if cw <= 0.0 || ch <= 0.0 {
            return TabHit::None;
        }
        // Y check — pointer must be inside the tab bar row.
        let y = px_y as f32;
        if y < y_offset_px || y >= y_offset_px + ch {
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

/// Full-width band↔body separator: a single opaque `border`-role [`SolidQuad`]
/// line ~1px tall along the bottom edge of the bar row, dividing the tab band
/// from the terminal body (F4 T1). `grid_cols` is the full column count;
/// `y_offset_px` is the top of the bar row; `cell`/`padding` map to physical
/// pixels. Returns an empty vec for a degenerate row.
fn band_separator(
    grid_cols: usize,
    y_offset_px: f32,
    cell: CellSize,
    padding: WindowPadding,
    border: Srgb,
) -> Vec<SolidQuad> {
    if grid_cols == 0 || cell.width == 0 || cell.height == 0 {
        return Vec::new();
    }
    let pad = padding.as_f32();
    let cw = cell.width as f32;
    let ch = cell.height as f32;
    let x0 = pad;
    let x1 = pad + grid_cols as f32 * cw;
    let y1 = y_offset_px + ch;
    vec![SolidQuad {
        rect: [x0, y1 - SEPARATOR_PX, x1, y1],
        color: srgb_alpha(border, 1.0),
    }]
}

/// Blend two sRGB colors: `a * (1 - t) + b * t` per channel, `t` clamped to
/// `[0, 1]`. Used to derive the bar band from `background` toward `inactive`
/// (F4 T1). A crude gamma-naive blend in sRGB space, which is fine for a subtle
/// chrome tint (not a color-managed image operation).
fn blend_srgb(a: Srgb, b: Srgb, t: f32) -> Srgb {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        (f32::from(x) * (1.0 - t) + f32::from(y) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Pixel-space outline (a hollow ring of four [`SolidQuad`]s) framing a tab
/// slot over the single tab-bar row. Every tab is framed with this ring — the
/// shape language of the bar (F4 v1.1): the active slot passes the full
/// `border` role, inactive slots pass a subordinate blended color.
/// `start_col..end_col` are the slot's columns; `y_offset_px` is the top of the
/// bar row; `cell`/`padding` map columns to physical pixels. The ring `color`
/// is fully opaque so it reads clearly over background images/treatments — a
/// cell-background highlight alone looked "too transparent" (operator). Returns
/// an empty vec for a degenerate (zero-width / zero-height) slot. The four edges
/// tile the ring without overlapping at the corners: top and bottom span the
/// full width; left and right fill the vertical gap between them.
fn active_tab_outline(
    start_col: usize,
    end_col: usize,
    y_offset_px: f32,
    cell: CellSize,
    padding: WindowPadding,
    outline: Srgb,
) -> Vec<SolidQuad> {
    if end_col <= start_col || cell.width == 0 || cell.height == 0 {
        return Vec::new();
    }
    let pad = padding.as_f32();
    let cw = cell.width as f32;
    let ch = cell.height as f32;
    // Outline thickness ~1–2px, scaled gently with cell height, clamped so it
    // never swallows a short row.
    let thickness = (ch / 10.0).clamp(1.0, 2.0);
    let x0 = pad + start_col as f32 * cw;
    let x1 = pad + end_col as f32 * cw;
    let y0 = y_offset_px;
    let y1 = y_offset_px + ch;
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

/// Convert an sRGB colour tuple + alpha to a linear-RGBA `[f32; 4]` array
/// suitable for [`SolidQuad::color`].
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

    const COLORS: TabBarColors = TabBarColors {
        foreground: (0xCC, 0xCC, 0xCC),
        background: (0x10, 0x10, 0x10),
        inactive: (0x66, 0x66, 0x66),
        active_bg: (0x40, 0x60, 0x90),
        border: (0xE0, 0xE0, 0xE0),
    };

    fn render_default(src: &dyn TabBarSource) -> TabBarOutput {
        bar().render(src, GRID_COLS, 0.0, CELL, WindowPadding::ZERO, COLORS)
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
        )
    }

    // -----------------------------------------------------------------------
    // Zero tabs
    // -----------------------------------------------------------------------

    #[test]
    fn zero_tabs_produces_explicit_row_cells_and_new_tab_affordance() {
        let src = MockSource::empty();
        let out = render_default(&src);
        // With no tabs there are no slots to frame, so the only quad is the
        // band↔body separator (F4 T1) — no outline rings.
        assert_eq!(
            out.quads.len(),
            1,
            "zero tabs emits only the band separator"
        );
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
    // Hit test — each tab body
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

    // -----------------------------------------------------------------------
    // Hit test — close × column
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Hit test — new-tab `+` affordance
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Hit test — outside the bar row
    // -----------------------------------------------------------------------

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
        );
        assert_eq!(hit, TabHit::None, "beyond right edge → None");
    }

    // -----------------------------------------------------------------------
    // Narrow grid — does not panic when tabs don't all fit
    // -----------------------------------------------------------------------

    #[test]
    fn narrow_grid_renders_without_panic() {
        let src = MockSource::new(&["a", "b", "c", "d", "e"], 0);
        let out = bar().render(&src, 20, 0.0, CELL, WindowPadding::ZERO, COLORS);
        assert_eq!(out.glyphs.len(), 20, "one glyph per column");
    }

    // -----------------------------------------------------------------------
    // Geometry: explicit cell backgrounds
    // -----------------------------------------------------------------------

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
    // F4 T1 — banded bar: dim inactive labels + separator line
    // -----------------------------------------------------------------------

    #[test]
    fn active_label_is_foreground_inactive_labels_are_dimmed() {
        // Three tabs, middle active: the active label glyphs carry the
        // foreground role + bold; the inactive labels carry the dim `inactive`
        // role and are not bold (F4 T1).
        let src = MockSource::new(&["aaa", "bbb", "ccc"], 1);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let base_fg = Color::Rgb(
            COLORS.foreground.0,
            COLORS.foreground.1,
            COLORS.foreground.2,
        );
        let dim_fg = Color::Rgb(COLORS.inactive.0, COLORS.inactive.1, COLORS.inactive.2);
        for (i, slot) in layout.slots.iter().enumerate() {
            // Inspect the first label glyph of each slot.
            let g = &out.glyphs[slot.label_col];
            if i == 1 {
                assert_eq!(g.attrs.foreground, base_fg, "active label uses foreground");
                assert!(g.attrs.bold(), "active label is bold");
            } else {
                assert_eq!(g.attrs.foreground, dim_fg, "inactive label is dimmed");
                assert!(!g.attrs.bold(), "inactive label is not bold");
            }
        }
    }

    #[test]
    fn bar_band_fill_is_distinct_from_the_terminal_background() {
        // The non-slot band cells are painted on the derived band fill, which
        // must differ from the raw terminal `background` so the strip reads as
        // its own band (F4 T1). Sample a column inside the new-tab affordance
        // gap (band fill, not a slot).
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        let raw_bg = Color::Rgb(
            COLORS.background.0,
            COLORS.background.1,
            COLORS.background.2,
        );
        let expected = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let expected_bg = Color::Rgb(expected.0, expected.1, expected.2);
        // Column 0 of a one-tab render is inside slot 0's left padding, still on
        // the inactive band fill only if the tab is inactive; the sole tab is
        // active, so sample the far-right band gap before the new-tab block.
        let layout = compute_layout(&src, GRID_COLS);
        let nt = layout.new_tab_col.expect("new-tab column");
        let gap_col = nt.saturating_sub(1);
        assert_eq!(out.glyphs[gap_col].attrs.background, expected_bg);
        assert_ne!(
            out.glyphs[gap_col].attrs.background, raw_bg,
            "the band fill must differ from the terminal background"
        );
    }

    #[test]
    fn render_emits_a_full_width_band_separator() {
        // The band↔body separator spans the whole bar width along the bottom
        // edge of the row, in the opaque `border` role (F4 T1).
        let src = MockSource::new(&["a", "b"], 0);
        let out = render_default(&src);
        // Match the full-width bottom-edge quad specifically: per-tab outline
        // rings also place a quad on the bottom edge but never span x=0..full.
        let full_w = GRID_COLS as f32 * CELL.width as f32;
        let sep = out
            .quads
            .iter()
            .find(|q| {
                (q.rect[3] - CELL.height as f32).abs() < f32::EPSILON
                    && q.rect[0].abs() < f32::EPSILON
                    && (q.rect[2] - full_w).abs() < f32::EPSILON
            })
            .expect("a full-width separator quad sits on the row's bottom edge");
        assert!(
            (sep.rect[0] - 0.0).abs() < f32::EPSILON,
            "separator starts at x=0"
        );
        assert!(
            (sep.rect[2] - GRID_COLS as f32 * CELL.width as f32).abs() < f32::EPSILON,
            "separator spans the full grid width"
        );
        assert!(
            (sep.color[3] - 1.0).abs() < f32::EPSILON,
            "opaque separator"
        );
    }

    // -----------------------------------------------------------------------
    // F4 v1.1 — every tab is outlined; active is distinguished by fill
    // -----------------------------------------------------------------------

    /// The four ring quads of an outline over `[start_col, end_col)`, extracted
    /// from a render output by geometry (edges within the slot span, none full
    /// width). Returns every quad whose horizontal extent falls inside the slot.
    fn ring_quads_for_span(
        out: &TabBarOutput,
        start_col: usize,
        end_col: usize,
    ) -> Vec<&SolidQuad> {
        let x0 = start_col as f32 * CELL.width as f32;
        let x1 = end_col as f32 * CELL.width as f32;
        out.quads
            .iter()
            .filter(|q| q.rect[0] >= x0 - 0.01 && q.rect[2] <= x1 + 0.01)
            .collect()
    }

    #[test]
    fn every_tab_including_inactive_is_framed_by_an_outline_ring() {
        // Three tabs, middle active: each of the three slots must carry a
        // four-quad outline ring — the shape language of the bar (F4 v1.1).
        let src = MockSource::new(&["a", "b", "c"], 1);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        for slot in &layout.slots {
            let ring = ring_quads_for_span(&out, slot.start_col, slot.end_col);
            assert_eq!(
                ring.len(),
                4,
                "slot {} must be framed by a 4-quad outline ring",
                slot.idx
            );
        }
    }

    #[test]
    fn active_ring_uses_full_border_role_inactive_rings_are_subordinate() {
        // The active slot's ring uses the full `border` role; inactive rings use
        // the subordinate blended color (border → band). The two must differ so
        // the active tab's frame is clearly stronger (F4 v1.1).
        let src = MockSource::new(&["a", "b"], 1);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let active_color = srgb_alpha(COLORS.border, 1.0);
        let inactive_color =
            srgb_alpha(blend_srgb(COLORS.border, band, INACTIVE_OUTLINE_BLEND), 1.0);
        assert_ne!(
            active_color, inactive_color,
            "active and inactive outline colors must differ"
        );
        // Active slot (idx 1) ring is the full border role.
        let active = &layout.slots[1];
        for q in ring_quads_for_span(&out, active.start_col, active.end_col) {
            assert_eq!(q.color, active_color, "active ring uses full border role");
        }
        // Inactive slot (idx 0) ring is the subordinate blended color.
        let inactive = &layout.slots[0];
        for q in ring_quads_for_span(&out, inactive.start_col, inactive.end_col) {
            assert_eq!(
                q.color, inactive_color,
                "inactive ring uses the subordinate blended color"
            );
        }
    }

    #[test]
    fn active_slot_is_filled_with_the_selection_role() {
        // The active tab is distinguished by a `selection`-role background fill
        // (as in the 0.7.5 build); inactive tabs sit on the band with no fill
        // (F4 v1.1).
        let src = MockSource::new(&["aaa", "bbb"], 0);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let active_fill = Color::Rgb(COLORS.active_bg.0, COLORS.active_bg.1, COLORS.active_bg.2);
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let band_bg = Color::Rgb(band.0, band.1, band.2);
        let active = &layout.slots[0];
        assert_eq!(
            out.glyphs[active.label_col].attrs.background, active_fill,
            "active slot filled with the selection role"
        );
        let inactive = &layout.slots[1];
        assert_eq!(
            out.glyphs[inactive.label_col].attrs.background, band_bg,
            "inactive slot has no fill — it sits on the band"
        );
    }

    #[test]
    fn no_underline_is_emitted() {
        // v1.1 removes the accent underline entirely. No quad may sit in the
        // thin strip just above the separator (where the underline used to be)
        // yet inside a slot span without also being an outline edge — i.e. every
        // bottom-edge quad is either the full-width separator or an outline
        // bottom edge that reaches the row's bottom (y1), never a floating
        // underline seated at `y1 - SEPARATOR_PX`.
        let src = MockSource::new(&["a", "b"], 1);
        let out = render_default(&src);
        let underline_top = CELL.height as f32 - SEPARATOR_PX;
        for q in &out.quads {
            let is_underline_band = (q.rect[3] - underline_top).abs() < 0.01;
            assert!(
                !is_underline_band,
                "no quad may sit at the old underline position (y bottom = {underline_top})"
            );
        }
    }

    #[test]
    fn hover_fill_is_subordinate_to_the_active_fill() {
        // A hovered inactive tab gets a fill blended from the band toward
        // `selection` — visibly present but never as strong as the active tab's
        // full `selection` fill, so the two can't be confused (F4 v1.1).
        let src = MockSource::new(&["a", "b"], 0);
        let mut tb = bar();
        tb.set_hover(Some(TabHit::Switch(1))); // hover the inactive second tab
        let out = tb.render(&src, GRID_COLS, 0.0, CELL, WindowPadding::ZERO, COLORS);
        let layout = compute_layout(&src, GRID_COLS);
        let band = blend_srgb(COLORS.background, COLORS.inactive, BAND_BLEND);
        let hover = blend_srgb(band, COLORS.active_bg, HOVER_FILL_BLEND);
        let hover_bg = Color::Rgb(hover.0, hover.1, hover.2);
        let active_fill = Color::Rgb(COLORS.active_bg.0, COLORS.active_bg.1, COLORS.active_bg.2);
        let band_bg = Color::Rgb(band.0, band.1, band.2);
        let hovered = &layout.slots[1];
        let bg = out.glyphs[hovered.label_col].attrs.background;
        assert_eq!(
            bg, hover_bg,
            "hovered inactive tab gets the subordinate fill"
        );
        assert_ne!(bg, active_fill, "hover fill differs from the active fill");
        assert_ne!(bg, band_bg, "hover fill differs from the plain band");
    }

    #[test]
    fn single_active_tab_emits_separator_and_one_ring() {
        // One active tab: band separator (1) + the active slot's outline ring
        // (4) = 5 quads. The plain single-pane window never shows the tab bar,
        // so this is inert there.
        let src = MockSource::new(&["only"], 0);
        let out = render_default(&src);
        assert_eq!(
            out.quads.len(),
            5,
            "sole tab yields a band separator plus its 4-quad outline ring"
        );
    }

    #[test]
    fn outline_helper_bails_on_a_zero_width_slot() {
        // A degenerate (zero-width) slot yields no ring quads (geometry guard).
        let quads = active_tab_outline(4, 4, 0.0, CELL, WindowPadding::ZERO, (0xE0, 0xE0, 0xE0));
        assert!(quads.is_empty(), "no ring for a zero-width slot");
    }

    #[test]
    fn blend_srgb_is_endpoint_exact_and_monotonic() {
        let a = (0x10, 0x20, 0x30);
        let b = (0xF0, 0xE0, 0xD0);
        assert_eq!(blend_srgb(a, b, 0.0), a, "t=0 returns a");
        assert_eq!(blend_srgb(a, b, 1.0), b, "t=1 returns b");
        // Out-of-range t clamps.
        assert_eq!(blend_srgb(a, b, -1.0), a);
        assert_eq!(blend_srgb(a, b, 2.0), b);
        let mid = blend_srgb(a, b, 0.5);
        assert!(
            mid.0 > a.0 && mid.0 < b.0,
            "midpoint lies between endpoints"
        );
    }

    // -----------------------------------------------------------------------
    // TAB_BAR_ROWS constant
    // -----------------------------------------------------------------------

    #[test]
    fn tab_bar_rows_is_one() {
        assert_eq!(TAB_BAR_ROWS, 1, "tab bar must reserve exactly one row");
    }

    // -----------------------------------------------------------------------
    // TabBarSource trait signature lock (documents the contract for GPT)
    // -----------------------------------------------------------------------

    #[test]
    fn trait_signature_compiles_and_works_with_mock() {
        let src = MockSource::new(&["one", "two"], 0);
        assert_eq!(src.tab_count(), 2);
        assert_eq!(src.tab_title(0), "one");
        assert_eq!(src.tab_title(1), "two");
        assert_eq!(src.active_tab(), 0);
    }
}
