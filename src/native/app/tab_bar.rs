// SPDX-License-Identifier: GPL-3.0-only
//! Tab bar widget — presentation-only, decoupled from the session model.
//!
//! Renders a one-row tab strip across the top of the window.  The widget is
//! purely geometrical: it reads layout from a [`TabBarSource`] trait object
//! (GPT will implement this on `SessionSet`) and produces solid quads + glyph
//! outputs that integration code composites into the frame.  It never touches
//! terminal state, PTY, or settings.
//!
//! ## Integration contract
//! 1. Reserve `TAB_BAR_ROWS` extra rows of height at the top of the window
//!    (shift terminal content down by one cell row).
//! 2. Call [`TabBar::render`] each frame to get [`TabBarOutput`]; push the
//!    quads into the overlay quad list and paint each glyph into the reserved
//!    snapshot row: `snapshot.cells[glyph.col] = Cell::new(glyph.ch, glyph.attrs)`.
//! 3. Call [`TabBar::hit_test`] on every pointer move; store the result with
//!    [`TabBar::set_hover`] so the hover highlight refreshes next frame.
//! 4. On pointer press, call `hit_test` again to determine the action.
//!
//! ## Off-path contract
//! Adding `mod tab_bar;` to `app/mod.rs` is the only change to existing files.
//! Until integration code explicitly calls `render` or `hit_test`, the widget is
//! entirely inert and no existing behaviour or test output changes.

// This module is an unintegrated widget scaffold — all public items will be
// wired up by the integration packet. Suppress dead_code lints so the
// warning count stays at or below the project baseline.
#![allow(dead_code)]

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
pub(super) const TAB_BAR_ROWS: u32 = 1;

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
// Private alpha constants
// ---------------------------------------------------------------------------

/// Alpha for the full-width tab bar background quad.
const TAB_BG_ALPHA: f32 = 0.95;
/// Alpha for the active-tab highlight quad.
const TAB_ACTIVE_ALPHA: f32 = 0.85;
/// Alpha for the hover-tab highlight quad (dimmer than active).
const TAB_HOVER_ALPHA: f32 = 0.35;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Read-only interface to the session model.  GPT will `impl TabBarSource for
/// SessionSet`; unit tests use an inline mock.
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
pub(super) enum TabHit {
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

/// Output from [`TabBar::render`]: background quads plus glyph runs.
///
/// Integration code pushes `quads` into the overlay quad list and writes each
/// glyph into the reserved snapshot row:
/// `snapshot.cells[glyph.col] = Cell::new(glyph.ch, glyph.attrs)`.
#[derive(Debug, Default)]
pub(super) struct TabBarOutput {
    /// Solid pixel-space quads (bar background, active/hover highlights).
    pub(super) quads: Vec<SolidQuad>,
    /// Glyph runs to composite into the tab bar row.
    pub(super) glyphs: Vec<TabBarGlyph>,
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
    /// - `source` — session model accessor (mock or real `SessionSet`).
    /// - `grid_cols` — number of terminal columns in the window.
    /// - `y_offset_px` — physical-pixel Y of the top of the tab bar row.
    /// - `cell` — cell metrics (width / height in physical pixels).
    /// - `padding` — window edge padding (shifts the left/right extents).
    /// - `foreground` — sRGB text colour (theme `foreground` role).
    /// - `background` — sRGB bar fill (theme `background` role).
    /// - `active_bg` — sRGB highlight colour for active/hover slots (e.g.
    ///   theme `cursor` or `selection` role).
    ///
    /// Returns [`TabBarOutput`] with quads and glyph runs ready for compositing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &self,
        source: &dyn TabBarSource,
        grid_cols: usize,
        y_offset_px: f32,
        cell: CellSize,
        padding: WindowPadding,
        foreground: Srgb,
        background: Srgb,
        active_bg: Srgb,
    ) -> TabBarOutput {
        if grid_cols == 0 || cell.height == 0 || cell.width == 0 {
            return TabBarOutput::default();
        }
        let layout = compute_layout(source, grid_cols);
        let mut out = TabBarOutput::default();
        let pad = padding.as_f32();
        let cw = cell.width as f32;
        let ch = cell.height as f32;
        let y0 = y_offset_px;
        let y1 = y0 + ch;

        // Full-width tab bar background quad.
        out.quads.push(SolidQuad {
            rect: [pad, y0, pad + grid_cols as f32 * cw, y1],
            color: srgb_alpha(background, TAB_BG_ALPHA),
        });

        for slot in &layout.slots {
            let is_active = slot.idx == source.active_tab();
            let is_hovered = is_slot_hovered(self.hover, slot.idx);

            // Per-slot highlight quad for active / hovered tabs.
            if is_active || is_hovered {
                let alpha = if is_active {
                    TAB_ACTIVE_ALPHA
                } else {
                    TAB_HOVER_ALPHA
                };
                out.quads.push(SolidQuad {
                    rect: [
                        pad + slot.start_col as f32 * cw,
                        y0,
                        pad + slot.end_col as f32 * cw,
                        y1,
                    ],
                    color: srgb_alpha(active_bg, alpha),
                });
            }

            // Label glyphs.
            let fg = Color::Rgb(foreground.0, foreground.1, foreground.2);
            let bg = if is_active {
                Color::Rgb(active_bg.0, active_bg.1, active_bg.2)
            } else {
                Color::Rgb(background.0, background.1, background.2)
            };
            let mut la = Attrs::default();
            la.foreground = fg;
            la.background = bg;
            if is_active {
                la.set_bold(true);
            }
            for (i, ch_char) in slot.label.chars().enumerate() {
                out.glyphs.push(TabBarGlyph {
                    col: slot.label_col + i,
                    ch: ch_char,
                    attrs: la,
                });
            }

            // Close `×` glyph — rendered only for the active or hovered tab.
            if let Some(close_col) = slot.close_col.filter(|_| is_active || is_hovered) {
                let mut ca = Attrs::default();
                ca.foreground = fg;
                ca.background = bg;
                out.glyphs.push(TabBarGlyph {
                    col: close_col,
                    ch: '×',
                    attrs: ca,
                });
            }
        }

        // New-tab `+` affordance.
        if let Some(nt_col) = layout.new_tab_col {
            let is_hovered = matches!(self.hover, Some(TabHit::NewTab));
            if is_hovered {
                out.quads.push(SolidQuad {
                    rect: [
                        pad + nt_col as f32 * cw,
                        y0,
                        pad + (nt_col + NEW_TAB_COLS) as f32 * cw,
                        y1,
                    ],
                    color: srgb_alpha(active_bg, TAB_HOVER_ALPHA),
                });
            }
            let fg = Color::Rgb(foreground.0, foreground.1, foreground.2);
            let bg_role = if is_hovered {
                Color::Rgb(active_bg.0, active_bg.1, active_bg.2)
            } else {
                Color::Rgb(background.0, background.1, background.2)
            };
            let mut a = Attrs::default();
            a.foreground = fg;
            a.background = bg_role;
            // Centre the `+` in the NEW_TAB_COLS block (offset 1 from block start).
            out.glyphs.push(TabBarGlyph {
                col: nt_col + 1,
                ch: '+',
                attrs: a,
            });
        }

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

    fn render_default(src: &dyn TabBarSource) -> TabBarOutput {
        bar().render(
            src,
            GRID_COLS,
            0.0,
            CELL,
            WindowPadding::ZERO,
            (0xCC, 0xCC, 0xCC),
            (0x10, 0x10, 0x10),
            (0x40, 0x60, 0x90),
        )
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
    fn zero_tabs_produces_only_bg_quad() {
        let src = MockSource::empty();
        let out = render_default(&src);
        // Only the full-width background quad; no per-tab glyphs except `+`.
        assert_eq!(out.quads.len(), 1, "one background quad with zero tabs");
        assert!(
            out.glyphs.iter().all(|g| g.ch == '+'),
            "only the new-tab glyph (if any) present with zero tabs"
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
            .filter(|g| g.ch != '+' && g.ch != '×')
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
        let label_start = mid.label_col;
        let label_end = mid.close_col.unwrap_or(mid.end_col);
        for g in &out.glyphs {
            if g.attrs.bold() {
                assert!(
                    g.col >= label_start && g.col < label_end,
                    "bold glyph col {} outside middle slot {}..{}",
                    g.col,
                    label_start,
                    label_end,
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
        let label_start = last.label_col;
        let label_end = last.close_col.unwrap_or(last.end_col);
        for g in &out.glyphs {
            if g.attrs.bold() {
                assert!(
                    g.col >= label_start && g.col < label_end,
                    "bold glyph col {} not in last slot {}..{}",
                    g.col,
                    label_start,
                    label_end,
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
            .filter(|g| g.ch != '+' && g.ch != '×')
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
            .filter(|g| g.ch != '+' && g.ch != '×')
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
        let out = bar().render(
            &src,
            20,
            0.0,
            CELL,
            WindowPadding::ZERO,
            (0xCC, 0xCC, 0xCC),
            (0x10, 0x10, 0x10),
            (0x40, 0x60, 0x90),
        );
        assert!(!out.quads.is_empty(), "at least one quad for narrow grid");
    }

    // -----------------------------------------------------------------------
    // Geometry: background quad dimensions
    // -----------------------------------------------------------------------

    #[test]
    fn background_quad_spans_full_width() {
        let src = MockSource::new(&["a"], 0);
        let out = render_default(&src);
        let bg = &out.quads[0];
        assert_eq!(bg.rect[0], 0.0, "left at x=0 with zero padding");
        assert_eq!(
            bg.rect[2],
            GRID_COLS as f32 * CELL.width as f32,
            "right at grid width"
        );
        assert_eq!(bg.rect[1], 0.0, "top at y=0");
        assert_eq!(bg.rect[3], CELL.height as f32, "bottom at cell height");
    }

    #[test]
    fn background_quad_respects_window_padding() {
        let src = MockSource::new(&["a"], 0);
        let pad = WindowPadding::from_logical(10.0, 1.0);
        let out = bar().render(
            &src,
            GRID_COLS,
            0.0,
            CELL,
            pad,
            (0xCC, 0xCC, 0xCC),
            (0x10, 0x10, 0x10),
            (0x40, 0x60, 0x90),
        );
        let bg = &out.quads[0];
        assert_eq!(bg.rect[0], pad.as_f32(), "left shifted by padding");
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
