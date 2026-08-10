// SPDX-License-Identifier: GPL-3.0-only
// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

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

// A text-side accent color distinct from every COLORS role, so the
// bound-workspace badge assertions can pin the badge to this exact value
// (never a chrome/border-derived color).
const ACCENT: Srgb = (0xF0, 0x50, 0xA0);

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
        ACCENT,
        false,
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
    super::chrome_geometry::ChromeSlotGeom::rail(src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL, GEOM)
        .hit(super::chrome_geometry::PxPoint::new(x, y))
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
fn active_workspace_marker_is_foreground_stable_across_panel_extremes() {
    let src = MockSource::new(&["inactive", "active"], 1);
    let slot = &compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM).slots[1];
    for strength in [0.0, 1.0] {
        let out = rail().render(
            &src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
            GEOM,
            strength,
            ACCENT,
            false,
        );
        let marker = out.glyphs[slot.label_row * RAIL_COLS + SLOT_LABEL_START_COL].attrs;
        assert!(marker.bold());
        assert_eq!(marker.foreground, rgb(tab_chrome::active_label(COLORS)));
        assert!(!marker.underline());
        assert_eq!(marker.underline_color, None);
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
        ACCENT,
        false,
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
        ACCENT,
        false,
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
    // RAIL-PLUS-GAP: the `+` anchors a guaranteed dead gap below the last
    // slot's END row (not its label row), so a padded 2-row slot no longer
    // leaves the `+` flush against its breathing row. The first dead row is
    // blank; the `+` sits `SLOT_GAP` rows below the slot box.
    let last = layout.slots.last().unwrap();
    let nt_start = last.end_row + SLOT_GAP;
    assert_eq!(
        layout.new_tab_rows,
        Some((nt_start, nt_start + NEW_TAB_ROWS)),
        "the + slot follows a dead gap below the last slot box"
    );
    assert_eq!(
        layout.separator_row,
        Some(last.end_row),
        "the spacer is the dead row directly below the last slot box"
    );
    // The spacer row sits strictly between the last slot and the `+`.
    assert!(
        last.end_row < nt_start,
        "a dead row separates the list from the +"
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
    // And it sits a dead gap below the last slot box (RAIL-PLUS-GAP).
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    let last = layout.slots.last().unwrap();
    assert_eq!(
        plus.row,
        last.end_row + SLOT_GAP,
        "the + row anchors a dead gap below the last slot box"
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
        ACCENT,
        false,
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
// Bound-workspace badge (ODP-7B)
// -----------------------------------------------------------------------

/// A source whose rows can carry the bound marker, so the badge assertions
/// exercise the `tab_bound` override the workspace rail supplies. Every other
/// mock keeps the trait default (`false`), so their renders paint no badge.
struct BoundMock {
    titles: Vec<&'static str>,
    active: usize,
    bound: Vec<usize>,
}

struct ActivityBoundMock {
    titles: Vec<&'static str>,
    active: usize,
    bound: Vec<usize>,
    activity: Vec<usize>,
}

impl TabBarSource for ActivityBoundMock {
    fn tab_count(&self) -> usize {
        self.titles.len()
    }
    fn tab_title(&self, idx: usize) -> &str {
        self.titles[idx]
    }
    fn active_tab(&self) -> usize {
        self.active
    }
    fn tab_bound(&self, idx: usize) -> bool {
        self.bound.contains(&idx)
    }
    fn tab_activity(&self, idx: usize) -> bool {
        self.activity.contains(&idx)
    }
}
impl TabBarSource for BoundMock {
    fn tab_count(&self) -> usize {
        self.titles.len()
    }
    fn tab_title(&self, idx: usize) -> &str {
        self.titles[idx]
    }
    fn active_tab(&self) -> usize {
        self.active
    }
    fn tab_bound(&self, idx: usize) -> bool {
        self.bound.contains(&idx)
    }
}

#[test]
fn bound_row_paints_the_accent_badge_in_the_rail_edge_column() {
    // ODP-7B: a workspace bound to a default host gains a compact link glyph
    // in the text-side accent role, in the rail-edge (col 0 for Left) inset
    // column — never crowding the label, never a chrome/border color.
    let src = BoundMock {
        titles: vec!["local", "prod"],
        active: 0,
        bound: vec![1],
    };
    let out = render_default(&src);
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    let bound_slot = &layout.slots[1];
    let g = out.glyphs[bound_slot.label_row * RAIL_COLS];
    assert_eq!(g.ch, BOUND_BADGE, "bound row shows the link badge at col 0");
    assert_eq!(
        g.attrs.foreground,
        rgb(ACCENT),
        "badge uses the text-side accent role passed in"
    );
}

#[test]
fn workspace_activity_badge_is_static_and_coexists_with_bound_marker() {
    let src = ActivityBoundMock {
        titles: vec!["local", "remote"],
        active: 0,
        bound: vec![1],
        activity: vec![1],
    };
    let out = render_default(&src);
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    let row = layout.slots[1].label_row;

    let bound = &out.glyphs[row * RAIL_COLS];
    assert_eq!(bound.ch, BOUND_BADGE);
    assert_eq!(bound.attrs.foreground, rgb(ACCENT));

    let activity = &out.glyphs[row * RAIL_COLS + RAIL_COLS - 1];
    assert_eq!(activity.ch, ACTIVITY_BADGE);
    assert_eq!(
        activity.attrs.foreground,
        rgb(tab_chrome::active_label(COLORS))
    );
    assert!(activity.attrs.bold());
}

#[test]
fn unbound_rows_paint_no_badge() {
    // Zero change for unbound workspaces (the byte-identical default): no row
    // carries the badge glyph. Fails-before: painting unconditionally.
    let src = BoundMock {
        titles: vec!["local", "prod"],
        active: 0,
        bound: vec![],
    };
    let out = render_default(&src);
    assert!(
        out.glyphs.iter().all(|g| g.ch != BOUND_BADGE),
        "no badge glyph anywhere when nothing is bound"
    );
    // And a plain MockSource (trait-default tab_bound = false) is likewise clean.
    let plain = MockSource::new(&["a", "b"], 0);
    let out = render_default(&plain);
    assert!(
        out.glyphs.iter().all(|g| g.ch != BOUND_BADGE),
        "the default source paints no badge"
    );
}

#[test]
fn bound_badge_follows_the_outer_inset_column_for_a_right_rail() {
    // For a Right rail the outer (non-content) margin is the last column, so
    // the badge tracks the rail edge rather than a fixed side.
    let src = BoundMock {
        titles: vec!["prod"],
        active: 0,
        bound: vec![0],
    };
    let out = rail().render(
        &src,
        RAIL_COLS,
        GRID_ROWS,
        ORIGIN,
        CELL,
        RailSide::Right,
        COLORS,
        GEOM,
        PANEL_STRENGTH,
        ACCENT,
        false,
    );
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    let slot = &layout.slots[0];
    let g = out.glyphs[slot.label_row * RAIL_COLS + (RAIL_COLS - 1)];
    assert_eq!(g.ch, BOUND_BADGE, "Right rail badge is in the last column");
    assert_eq!(g.attrs.foreground, rgb(ACCENT));
}

// -----------------------------------------------------------------------
// Fill / hover treatment
// -----------------------------------------------------------------------

#[test]
fn active_slot_fill_spans_the_full_rail_width() {
    // Active fill = `selection`, covering the complete row so the slot reads
    // as embedded in the continuous rail surface.
    let src = MockSource::new(&["aaa", "bbb"], 0);
    let out = render_default(&src);
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    let active_fill = rgb(tab_chrome::active_fill(
        COLORS,
        tab_chrome::panel_tint(COLORS, PANEL_STRENGTH),
    ));
    let active = &layout.slots[0];
    // Both outer edges and the label cell carry the fill.
    assert_eq!(
        bg_at(&out, active.start_row, SLOT_LABEL_START_COL),
        active_fill,
        "active slot filled with the selection role"
    );
    assert_eq!(
        bg_at(&out, active.start_row, RAIL_COLS - 1),
        active_fill,
        "active fill reaches the content seam"
    );
    assert_eq!(
        bg_at(&out, active.start_row, 0),
        active_fill,
        "active fill reaches the outer rail edge"
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
        rgb(tab_chrome::hover_fill(
            COLORS,
            tab_chrome::panel_tint(COLORS, PANEL_STRENGTH)
        )),
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
        rgb(tab_chrome::active_fill(
            COLORS,
            tab_chrome::panel_tint(COLORS, PANEL_STRENGTH)
        )),
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
fn new_tab_plus_is_one_row_visible_at_rest_and_brighter_on_hover() {
    let src = MockSource::new(&["a", "b"], 0);
    let (nt_start, nt_end) = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM)
        .new_tab_rows
        .expect("new-tab slot present");
    assert_eq!(nt_end - nt_start, NEW_TAB_ROWS, "the + slot is 1 cell tall");
    // At rest: a lifted + on bare wallpaper (no fill), brighter than an
    // inactive label but subordinate to the active label (RAIL-PLUS-GAP).
    let out = render_default(&src);
    let wallpaper = rgb(tab_chrome::wallpaper_background(COLORS));
    let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
    assert_eq!(
        plus.attrs.background, wallpaper,
        "+ sits on the bare wallpaper"
    );
    let rest_luma = luma(plus.attrs.foreground);
    assert!(
        rest_luma > luma(rgb(COLORS.inactive)),
        "resting + is more visible than an inactive label"
    );
    assert!(
        rest_luma < luma(rgb(tab_chrome::active_label(COLORS))),
        "resting + stays subordinate to the active label"
    );
    // On hover: whisper fill + brighter +.
    let out = render_with(&hovered_rail(TabHit::NewTab), &src);
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
fn dead_gap_row_above_the_plus_is_not_a_hit() {
    // RAIL-PLUS-GAP: the spacer row between the last slot and the `+` is
    // non-interactive. With the default 2-row slots this is the row that
    // used to BE the `+` (immediately below the last slot box), so a click
    // there no longer opens a new workspace by accident.
    let src = MockSource::new(&["a"], 0);
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    let last = layout.slots.last().expect("one slot");
    let sep = layout.separator_row.expect("spacer present with a slot +");
    assert_eq!(
        sep, last.end_row,
        "spacer is the row below the last slot box"
    );
    assert_eq!(
        hit_at(sep, RAIL_LABEL_PAD, &src),
        TabHit::None,
        "the dead spacer row is not a hit"
    );
    // The old immediately-adjacent position (last slot end row) is exactly
    // that dead spacer now -- no longer NewTab.
    assert_ne!(
        hit_at(last.end_row, RAIL_LABEL_PAD, &src),
        TabHit::NewTab,
        "a click just below the last slot no longer triggers new-workspace"
    );
    // The `+` sits one row further down and still resolves to NewTab.
    let (nt_start, _e) = layout.new_tab_rows.expect("+ present");
    assert_eq!(nt_start, sep + 1, "the + follows the dead spacer row");
    assert_eq!(hit_at(nt_start, RAIL_LABEL_PAD, &src), TabHit::NewTab);
}

#[test]
fn dead_gap_row_stays_blank_without_a_horizontal_rule() {
    // RAIL-PLUS-GAP: the spacer row keeps the `+` set apart from the
    // workspace list without bleeding a horizontal rule into the content.
    let src = MockSource::new(&["a"], 0);
    let out = render_default(&src);
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    let sep = layout.separator_row.expect("spacer present");
    assert!(
        (0..RAIL_COLS).all(|c| out.glyphs[sep * RAIL_COLS + c].ch == ' '),
        "the spacer row contains no rule glyphs"
    );
}

#[test]
fn resting_plus_is_brighter_than_an_inactive_label() {
    // RAIL-PLUS-GAP: the resting `+` is lifted out of the dim inactive floor
    // (new_slot_plus_rest), so it reads as a deliberate add control, and it
    // still brightens further on hover.
    let src = MockSource::new(&["a"], 0);
    let out = render_default(&src);
    let plus = out.glyphs.iter().find(|g| g.ch == '+').expect("+ glyph");
    assert!(
        luma(plus.attrs.foreground) > luma(rgb(COLORS.inactive)),
        "resting + is brighter than the inactive label floor"
    );
    // Hover brightens it further still (toward the active label).
    let hovered = render_with(&hovered_rail(TabHit::NewTab), &src);
    let hplus = hovered
        .glyphs
        .iter()
        .find(|g| g.ch == '+')
        .expect("+ glyph");
    assert!(
        luma(hplus.attrs.foreground) > luma(plus.attrs.foreground),
        "+ brightens further on hover"
    );
}

#[test]
fn hit_left_of_rail_is_none() {
    let src = MockSource::new(&["a"], 0);
    let hit = super::chrome_geometry::ChromeSlotGeom::rail(
        &src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL, GEOM,
    )
    .hit(super::chrome_geometry::PxPoint::new(-1.0, 8.0));
    assert_eq!(hit, TabHit::None, "left of the rail → None");
}

#[test]
fn hit_right_of_rail_band_is_none() {
    let src = MockSource::new(&["a"], 0);
    let x = RAIL_COLS as f64 * CELL.width as f64 + 1.0;
    let hit = super::chrome_geometry::ChromeSlotGeom::rail(
        &src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL, GEOM,
    )
    .hit(super::chrome_geometry::PxPoint::new(x, 8.0));
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
    // A mid-region empty row below the slots and the `+`, but above the
    // reserved auto-hide control band, is non-interactive. (The true bottom
    // row is now the auto-hide toggle; see `bottom_row_is_the_autohide_toggle`.)
    let hit = hit_at(GRID_ROWS - 5, RAIL_LABEL_PAD, &src);
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
        // The `▼` indicator now sits at the bottom of the SLOT REGION, above
        // the reserved control band, not the true last row.
        let below_row = layout.slot_region_rows - 1;
        let hit = hit_at(below_row, RAIL_LABEL_PAD, &src);
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
// RAIL-AUTOHIDE-CTL: bottom-edge auto-hide toggle control
// -----------------------------------------------------------------------

#[test]
fn autohide_control_pins_to_the_bottom_row_and_reserves_a_separator() {
    let src = MockSource::new(&["a", "b"], 0);
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    assert_eq!(
        layout.autohide_row,
        Some(GRID_ROWS - 1),
        "the control pins to the true bottom row"
    );
    assert_eq!(
        layout.slot_region_rows,
        GRID_ROWS - AUTOHIDE_RESERVE_ROWS,
        "the slot region shrinks by the reserved control band"
    );
    // No slot may occupy the reserved band (region bottom .. rail bottom).
    for slot in &layout.slots {
        assert!(
            slot.end_row <= layout.slot_region_rows,
            "slot {} ends at {} inside the reserved control band",
            slot.idx,
            slot.end_row
        );
    }
}

#[test]
fn autohide_control_reserves_rows_without_overflow_collision() {
    // Many tabs force the overflow path; the `▼` indicator and the last
    // visible slot must both stay above the reserved control band.
    let titles: Vec<&'static str> = vec!["t"; 100];
    let src = MockSource::new(&titles, 80);
    let layout = compute_rail_layout(&src, RAIL_COLS, GRID_ROWS, GEOM);
    assert_eq!(layout.autohide_row, Some(GRID_ROWS - 1));
    assert!(layout.overflow_below.is_some(), "bottom tabs hidden → ▼");
    for slot in &layout.slots {
        assert!(
            slot.end_row <= layout.slot_region_rows,
            "overflow slot {} collides with the control band",
            slot.idx
        );
    }
}

#[test]
fn bottom_row_hits_the_autohide_toggle_and_the_separator_is_inert() {
    let src = MockSource::new(&["a", "b"], 0);
    assert_eq!(
        hit_at(GRID_ROWS - 1, RAIL_LABEL_PAD, &src),
        TabHit::AutohideToggle,
        "the bottom row toggles auto-hide"
    );
    assert_eq!(
        hit_at(GRID_ROWS - 2, RAIL_LABEL_PAD, &src),
        TabHit::None,
        "the separator row above the control is inert"
    );
}

#[test]
fn autohide_toggle_is_armed_ahead_of_a_slot_at_the_boundary() {
    // A single compact slot placed to end exactly at the region bottom must
    // not swallow a click on the control row directly below it.
    let src = MockSource::new(&["only"], 0);
    assert_eq!(
        hit_at(GRID_ROWS - 1, RAIL_LABEL_PAD, &src),
        TabHit::AutohideToggle
    );
}

#[test]
fn autohide_control_absent_on_a_degenerate_region() {
    let src = MockSource::new(&["a"], 0);
    // Zero width or zero height => no rail region, hence no control.
    assert_eq!(
        compute_rail_layout(&src, 0, GRID_ROWS, GEOM).autohide_row,
        None
    );
    assert_eq!(
        compute_rail_layout(&src, RAIL_COLS, 0, GEOM).autohide_row,
        None
    );
}

#[test]
fn autohide_control_glyph_reads_state_distinctly() {
    let src = MockSource::new(&["a", "b"], 0);
    let ctl_row = GRID_ROWS - 1;
    let col = SLOT_LABEL_START_COL;
    let render_state = |on: bool| {
        rail().render(
            &src,
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
            GEOM,
            PANEL_STRENGTH,
            ACCENT,
            on,
        )
    };
    let off = render_state(false);
    let on = render_state(true);
    let off_g = off.glyphs[ctl_row * RAIL_COLS + col];
    let on_g = on.glyphs[ctl_row * RAIL_COLS + col];
    // A directional triangle glyph is present in both states.
    assert!(
        off_g.ch == AUTOHIDE_CHEVRON_LEFT || off_g.ch == AUTOHIDE_CHEVRON_RIGHT,
        "control paints a directional triangle when auto-hide is off"
    );
    // The on state is visually distinct through direction + tint (glyph-only,
    // never bold): the triangle flips and the active tint replaces the
    // resting dim, so pinned-off and auto-hiding read differently.
    assert_ne!(off_g.ch, on_g.ch, "the on-state glyph flips direction");
    assert_ne!(
        off_g.attrs.foreground, on_g.attrs.foreground,
        "the on-state control carries the active tint, not the resting dim"
    );
    assert!(
        !off_g.attrs.bold() && !on_g.attrs.bold(),
        "the control is glyph-only, never bold in either state"
    );
}

#[test]
fn autohide_control_hover_lifts_the_row() {
    let src = MockSource::new(&["a", "b"], 0);
    let hovered = hovered_rail(TabHit::AutohideToggle);
    let out = hovered.render(
        &src,
        RAIL_COLS,
        GRID_ROWS,
        ORIGIN,
        CELL,
        RailSide::Left,
        COLORS,
        GEOM,
        PANEL_STRENGTH,
        ACCENT,
        false,
    );
    let ctl_row = GRID_ROWS - 1;
    let hover_bg = bg_at(&out, ctl_row, 0);
    let rest = render_default(&src);
    let rest_bg = bg_at(&rest, ctl_row, 0);
    assert_ne!(
        luma(hover_bg),
        luma(rest_bg),
        "hover lifts the control row background"
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

// -----------------------------------------------------------------------
// RAIL-DRAG: drop-target index math + drop indicator paint
// -----------------------------------------------------------------------

// Default GEOM: slot_rows=2, slot_gap=1, top_margin=1, stride=3. For N
// slots, slot i spans rows [1+3i, 3+3i); its vertical midpoint row is
// 2+3i, i.e. midpoint_y = (2+3i)*CELL.height. CELL.height == 16, so the
// With origin slot 1 excluded, retained neighbors keep their real-space
// midpoints y = 32 and 128 px.
fn drop_index_at(y: f64, src: &dyn TabBarSource) -> Option<usize> {
    super::chrome_geometry::ChromeSlotGeom::rail(src, RAIL_COLS, GRID_ROWS, ORIGIN, CELL, GEOM)
        .drop_index(y, 1)
}

#[test]
fn drop_index_maps_pointer_y_to_insertion_by_slot_midpoint() {
    let src = MockSource::new(&["a", "b", "c"], 0);
    // Above the first slot midpoint (32px) → insert before slot 0.
    assert_eq!(drop_index_at(0.0, &src), Some(0));
    assert_eq!(drop_index_at(31.0, &src), Some(0));
    // Between retained slot 0 and slot 2 (32..128) → insert
    // before original slot 2 (the lifted origin's resting gap).
    assert_eq!(drop_index_at(32.0, &src), Some(2));
    assert_eq!(drop_index_at(127.0, &src), Some(2));
    // Below the last retained midpoint → append.
    assert_eq!(drop_index_at(128.0, &src), Some(3));
}

#[test]
fn drop_index_clamps_to_top_and_bottom_edges() {
    let src = MockSource::new(&["a", "b", "c"], 0);
    // Far above the rail → still the first insertion slot (a no-op move for
    // an already-first item, never a negative / underflowed index).
    assert_eq!(drop_index_at(-100.0, &src), Some(0));
    // Below every midpoint → append after the last visible slot (count).
    assert_eq!(drop_index_at(128.0, &src), Some(3));
    assert_eq!(drop_index_at(10_000.0, &src), Some(3));
}

#[test]
fn drop_index_is_none_without_slots_or_degenerate_geometry() {
    assert_eq!(drop_index_at(50.0, &MockSource::empty()), None);
    let src = MockSource::new(&["a"], 0);
    // Zero-height cell → degenerate, no index.
    let zero = CellSize {
        width: 8,
        height: 0,
        baseline: 0,
    };
    assert_eq!(
        super::chrome_geometry::ChromeSlotGeom::rail(
            &src, RAIL_COLS, GRID_ROWS, ORIGIN, zero, GEOM,
        )
        .drop_index(50.0, 0),
        None
    );
}

#[test]
fn pending_drag_lifts_the_grabbed_slot_during_render() {
    let src = MockSource::new(&["a", "b", "c"], 0);
    let before = render_default(&src).glyphs[4 * RAIL_COLS + 2]
        .attrs
        .background;
    let glyphs = rail()
        .render_with_pressed(
            &src,
            Some(1),
            RAIL_COLS,
            GRID_ROWS,
            ORIGIN,
            CELL,
            RailSide::Left,
            COLORS,
            GEOM,
            PANEL_STRENGTH,
            ACCENT,
            false,
        )
        .glyphs;
    let grabbed = glyphs[4 * RAIL_COLS + 2];
    assert_eq!(grabbed.ch, 'b');
    assert_ne!(grabbed.attrs.background, before, "press lifts the slot");
    assert!(grabbed.attrs.bold(), "grabbed label is emphasized");
}

#[test]
fn two_row_rail_emits_no_underline_attributes() {
    let src = MockSource::new(&["active", "inactive"], 0);
    let output = rail().render(
        &src,
        RAIL_COLS,
        GRID_ROWS,
        ORIGIN,
        CELL,
        RailSide::Left,
        COLORS,
        RailGeom {
            slot_rows: 2,
            slot_gap: 0,
        },
        PANEL_STRENGTH,
        ACCENT,
        false,
    );
    assert!(output.glyphs.iter().all(|glyph| !glyph.attrs.underline()));
}
