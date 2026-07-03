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
//! ## Visual treatment (F4 v1)
//! Monochrome + one accent, all from theme roles (no hardcoded colors, so every
//! theme and the CVD modes stay correct): the bar row is a distinct band derived
//! from `background`, inactive tab labels are dimmed (`inactive` role) while the
//! active label stays `foreground` + bold, a full-width `border`-role separator
//! divides the band from the terminal body, and the active tab carries an opaque
//! `cursor`-role accent underline. The older four-quad `border` ring
//! ([`active_tab_outline`]) is retained as a fallback style but is not on the
//! default path.

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
    /// Active / hover slot fill — theme `selection`.
    pub(super) active_bg: Srgb,
    /// Active-tab accent underline — theme `cursor` (the natural accent role).
    pub(super) accent: Srgb,
    /// Band↔body separator line and the retained fallback ring — theme `border`.
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
    /// plus pixel-space quads: a full-width band↔body separator and, when a tab
    /// is active, an accent underline on the active slot (F4 T1+T2).
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
        let mut row = vec![blank_glyph(0, base_fg, band_bg); grid_cols];
        for (col, glyph) in row.iter_mut().enumerate() {
            glyph.col = col;
        }

        // Column span of the active slot, captured during the loop so the
        // accent underline can be drawn in pixel space afterward.
        let mut active_span: Option<(usize, usize)> = None;
        for slot in &layout.slots {
            let is_active = slot.idx == source.active_tab();
            let is_hovered = is_slot_hovered(self.hover, slot.idx);
            if is_active {
                active_span = Some((slot.start_col, slot.end_col));
            }
            // Active/hover slots get the selection fill; inactive slots sit
            // flush on the band so the bar reads as one strip (F4 T1).
            let slot_bg = if is_active || is_hovered {
                active_bg_color
            } else {
                band_bg
            };

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

        // Band↔body separator: a full-width opaque `border`-role line along the
        // bottom edge of the bar row, so the strip reads as a distinct band
        // rather than floating labels (F4 T1). Emitted whenever the bar renders.
        out.quads.extend(band_separator(
            grid_cols,
            y_offset_px,
            cell,
            padding,
            colors.border,
        ));

        // Active-tab accent underline: a 1–2px opaque `cursor`-role bar along the
        // bottom of the active slot, sitting just above the separator so both
        // stay visible regardless of composite order (F4 T2). Opaque, so it
        // reads over background images/treatments (the house image-proof rule).
        // Single-pane windows never render the tab bar, so this is inert on the
        // plain/fast path (no active slot ⇒ no accent quad). The older four-quad
        // ring ([`active_tab_outline`]) is retained as a fallback style.
        if let Some((start_col, end_col)) = active_span {
            out.quads.extend(active_tab_underline(
                start_col,
                end_col,
                y_offset_px,
                cell,
                padding,
                colors.accent,
            ));
        }
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

/// Active-tab accent underline: a single opaque `cursor`-role [`SolidQuad`]
/// 1–2px tall along the bottom of the active slot, sitting just above the
/// [`band_separator`] so both remain visible independent of composite order
/// (F4 T2). `start_col..end_col` are the active slot's columns. Opaque so it
/// reads over background images (the house image-proof rule). Returns an empty
/// vec for a degenerate slot or when the row is too short to fit both the
/// separator and the accent.
fn active_tab_underline(
    start_col: usize,
    end_col: usize,
    y_offset_px: f32,
    cell: CellSize,
    padding: WindowPadding,
    accent: Srgb,
) -> Vec<SolidQuad> {
    if end_col <= start_col || cell.width == 0 || cell.height == 0 {
        return Vec::new();
    }
    let pad = padding.as_f32();
    let cw = cell.width as f32;
    let ch = cell.height as f32;
    let thickness = (ch / 10.0).clamp(1.0, 2.0);
    let x0 = pad + start_col as f32 * cw;
    let x1 = pad + end_col as f32 * cw;
    let y1 = y_offset_px + ch;
    let accent_bottom = y1 - SEPARATOR_PX;
    let accent_top = accent_bottom - thickness;
    // Bail if the row is too short to seat the accent above the separator
    // without colliding with the bar's top edge.
    if accent_top <= y_offset_px {
        return Vec::new();
    }
    vec![SolidQuad {
        rect: [x0, accent_top, x1, accent_bottom],
        color: srgb_alpha(accent, 1.0),
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

/// Pixel-space outline (a hollow ring of four [`SolidQuad`]s) framing the active
/// tab slot over the single tab-bar row — the **retained fallback style** (F4
/// keeps the underline on the default path; this ring is available if a future
/// style option wants the framed look). `start_col..end_col` are the active
/// slot's columns; `y_offset_px` is the top of the bar row; `cell`/`padding` map
/// columns to physical pixels. The ring uses the themed `border` role and is
/// fully opaque so it reads clearly over background images/treatments — the
/// cell-background highlight alone looked "too transparent" (operator). Returns
/// an empty vec for a degenerate (zero-width / zero-height) slot. The four edges
/// tile the ring without overlapping at the corners: top and bottom span the
/// full width; left and right fill the vertical gap between them.
#[allow(dead_code)] // retained fallback style (F4 T2); default path uses the accent underline
fn active_tab_outline(
    start_col: usize,
    end_col: usize,
    y_offset_px: f32,
    cell: CellSize,
    padding: WindowPadding,
    border: Srgb,
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
    let color = srgb_alpha(border, 1.0);
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
        accent: (0x80, 0xC0, 0xFF),
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
        // With no tabs there is no active slot, so the only quad is the
        // band↔body separator (F4 T1) — no accent underline.
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
        let sep = out
            .quads
            .iter()
            .find(|q| (q.rect[3] - CELL.height as f32).abs() < f32::EPSILON)
            .expect("a separator quad sits on the row's bottom edge");
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
    // F4 T2 — active-tab accent underline
    // -----------------------------------------------------------------------

    #[test]
    fn active_tab_emits_an_accent_underline_over_its_span() {
        // Two tabs, second active: besides the full-width separator there is a
        // single accent underline confined to the active slot's column span,
        // seated just above the separator (F4 T2).
        let src = MockSource::new(&["a", "b"], 1);
        let out = render_default(&src);
        let layout = compute_layout(&src, GRID_COLS);
        let active = &layout.slots[1];
        let x0 = active.start_col as f32 * CELL.width as f32;
        let x1 = active.end_col as f32 * CELL.width as f32;
        let accent = Color::Rgb(COLORS.accent.0, COLORS.accent.1, COLORS.accent.2);
        let accent_linear = {
            let mut l = text::foreground_linear(accent);
            l[3] = 1.0;
            l
        };
        // The accent quad: within the active span, above the bottom separator.
        let underline = out
            .quads
            .iter()
            .find(|q| {
                (q.rect[3] - (CELL.height as f32 - SEPARATOR_PX)).abs() < 0.01
                    && q.rect[0] >= x0 - 0.01
                    && q.rect[2] <= x1 + 0.01
            })
            .expect("an accent underline over the active slot");
        assert!(
            (underline.rect[0] - x0).abs() < 0.01 && (underline.rect[2] - x1).abs() < 0.01,
            "accent spans exactly the active slot"
        );
        assert_eq!(
            underline.color, accent_linear,
            "accent uses the cursor role"
        );
        assert!(
            underline.rect[1] >= 0.0,
            "accent stays within the bar row (top ≥ row top)"
        );
    }

    #[test]
    fn single_active_tab_emits_separator_and_accent() {
        // Even one tab is active — separator + accent both render, though the
        // plain single-pane window never shows the tab bar (inert there).
        let src = MockSource::new(&["only"], 0);
        let out = render_default(&src);
        assert_eq!(
            out.quads.len(),
            2,
            "the sole tab yields a band separator plus its accent underline"
        );
    }

    #[test]
    fn accent_underline_helper_bails_on_a_too_short_row() {
        // A row too short to seat the accent above the separator without hitting
        // the top edge yields no accent quad (defensive geometry guard).
        let tiny = CellSize {
            width: 8,
            height: 2,
            baseline: 1,
        };
        let quads = active_tab_underline(0, 4, 0.0, tiny, WindowPadding::ZERO, (0x80, 0xC0, 0xFF));
        assert!(quads.is_empty(), "no accent when the row cannot seat it");
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
