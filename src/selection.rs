// SPDX-License-Identifier: GPL-3.0-only
use crate::core::{Color, Dimensions, Snapshot};
use crate::native::WindowPadding;
use crate::text::CellSize;
use std::time::{Duration, Instant};

/// Maximum delay between clicks for same-cell double/triple click detection.
pub const CLICK_COUNT_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPoint {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: CellPoint,
    pub end: CellPoint,
}

/// Granularity of an in-progress drag-selection (MOUSE-EXTEND). `Char` is the
/// historical single-click drag (focus follows the pointer cell); `Word`/`Line`
/// keep a double/triple-click drag live so it grows by whole words / lines via
/// [`word_range_at`] / [`line_range_at`] instead of finalizing on the click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectGranularity {
    #[default]
    Char,
    Word,
    Line,
}

/// Typed pointer-drag state on the native grid. Replaces the bare
/// `selecting: bool` with one mutually-exclusive home for every grid drag
/// gesture, mirroring the overlay `SliderDrag` win (UX4-P2) so the gestures can
/// never overlap. `Select` carries the active [`SelectGranularity`]; the `block`
/// field is reserved for column/rectangular selection (MOUSE-RECT) and the
/// `Scrollbar` variant for the draggable scroll thumb (MOUSE-SCROLLBAR) — both
/// are wired into the type now and constructed by their own later packets. They
/// are part of this crate's public API (a `pub` enum in a `pub` module), so the
/// not-yet-constructed arm does not trip the dead-code lint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerDrag {
    #[default]
    None,
    Select {
        granularity: SelectGranularity,
        /// Reserved for MOUSE-RECT (Alt+drag column selection). Always `false`
        /// today; threading it through render/extract is a later packet.
        block: bool,
    },
    /// Reserved for MOUSE-SCROLLBAR (grab the right-edge thumb to scrub
    /// scrollback). Not constructed until that packet lands.
    Scrollbar,
}

impl PointerDrag {
    /// Whether a local text selection drag is in progress (any granularity).
    /// This is the typed replacement for the old `selecting` boolean truthiness
    /// at every call site.
    pub fn is_selecting(&self) -> bool {
        matches!(self, PointerDrag::Select { .. })
    }
}

/// Themed selection treatment (ID1). When supplied to [`apply_highlight`], the
/// selected cells are painted with an explicit `fill` background and `fg`
/// foreground (both sRGB bytes) instead of the historical per-cell inverse.
/// The caller precomputes `fg` by flooring the theme foreground over `fill`
/// through the RV1 minimum-contrast machinery, so readability is guaranteed at
/// the active `min_contrast`. Passing `None` to `apply_highlight` preserves the
/// byte-identical inverse path, keeping the default render pixel-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionStyle {
    /// Selection fill background (sRGB bytes), from the theme `selection` role.
    pub fill: [u8; 3],
    /// Foreground over the fill (sRGB bytes), RV1-floored by the caller.
    pub fg: [u8; 3],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectionState {
    anchor: Option<CellPoint>,
    focus: Option<CellPoint>,
}

impl SelectionState {
    pub fn begin(&mut self, point: CellPoint) {
        self.anchor = Some(point);
        self.focus = Some(point);
    }

    pub fn update(&mut self, point: CellPoint) {
        if self.anchor.is_some() {
            self.focus = Some(point);
        }
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
    }

    pub fn range(&self) -> Option<SelectionRange> {
        normalize_range(self.anchor?, self.focus?)
    }

    pub fn is_selecting(&self) -> bool {
        self.anchor.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsoluteCellPoint {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsoluteSelectionRange {
    pub start: AbsoluteCellPoint,
    pub end: AbsoluteCellPoint,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AbsoluteSelectionState {
    anchor: Option<AbsoluteCellPoint>,
    focus: Option<AbsoluteCellPoint>,
}

impl AbsoluteSelectionState {
    pub fn begin(&mut self, point: AbsoluteCellPoint) {
        self.anchor = Some(point);
        self.focus = Some(point);
    }

    pub fn update(&mut self, point: AbsoluteCellPoint) {
        if self.anchor.is_some() {
            self.focus = Some(point);
        }
    }

    pub fn set_range(&mut self, range: AbsoluteSelectionRange) {
        self.anchor = Some(range.start);
        self.focus = Some(range.end);
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
    }

    pub fn range(&self) -> Option<AbsoluteSelectionRange> {
        normalize_absolute_range(self.anchor?, self.focus?)
    }
}

pub fn normalize_absolute_range(
    anchor: AbsoluteCellPoint,
    focus: AbsoluteCellPoint,
) -> Option<AbsoluteSelectionRange> {
    let start_first = (anchor.row, anchor.column) <= (focus.row, focus.column);
    let (start, end) = if start_first {
        (anchor, focus)
    } else {
        (focus, anchor)
    };

    (start != end).then_some(AbsoluteSelectionRange { start, end })
}

/// Union two absolute selection ranges into the smallest range that covers
/// both (MOUSE-EXTEND word/line drag): the selection spans from the earliest
/// start to the latest end of the anchored unit and the unit under the pointer,
/// in row-major order. Unlike [`normalize_absolute_range`] this accepts
/// degenerate single-cell ranges (`start == end`) so a word-drag over
/// whitespace (no word under the pointer) still extends to the pointer cell.
pub fn union_absolute_ranges(
    a: AbsoluteSelectionRange,
    b: AbsoluteSelectionRange,
) -> AbsoluteSelectionRange {
    AbsoluteSelectionRange {
        start: min_absolute_point(a.start, b.start),
        end: max_absolute_point(a.end, b.end),
    }
}

fn min_absolute_point(a: AbsoluteCellPoint, b: AbsoluteCellPoint) -> AbsoluteCellPoint {
    if (a.row, a.column) <= (b.row, b.column) {
        a
    } else {
        b
    }
}

fn max_absolute_point(a: AbsoluteCellPoint, b: AbsoluteCellPoint) -> AbsoluteCellPoint {
    if (a.row, a.column) >= (b.row, b.column) {
        a
    } else {
        b
    }
}

pub fn viewport_top_absolute_row(viewport_offset: usize, scrollback_len: usize) -> usize {
    scrollback_len.saturating_sub(viewport_offset)
}

pub fn visible_to_absolute(
    point: CellPoint,
    viewport_offset: usize,
    scrollback_len: usize,
) -> AbsoluteCellPoint {
    AbsoluteCellPoint {
        row: viewport_top_absolute_row(viewport_offset, scrollback_len).saturating_add(point.row),
        column: point.column,
    }
}

pub fn absolute_range_from_visible(
    range: SelectionRange,
    viewport_offset: usize,
    scrollback_len: usize,
) -> AbsoluteSelectionRange {
    AbsoluteSelectionRange {
        start: visible_to_absolute(range.start, viewport_offset, scrollback_len),
        end: visible_to_absolute(range.end, viewport_offset, scrollback_len),
    }
}

pub fn visible_range_from_absolute(
    range: AbsoluteSelectionRange,
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
) -> Option<SelectionRange> {
    let top = viewport_top_absolute_row(viewport_offset, scrollback_len);
    let bottom = top.saturating_add(dimensions.rows.saturating_sub(1));

    if range.end.row < top || range.start.row > bottom {
        return None;
    }

    let visible_start_row = range.start.row.max(top) - top;
    let visible_end_row = range.end.row.min(bottom) - top;
    let start_column = if range.start.row < top {
        0
    } else {
        range.start.column.min(dimensions.columns - 1)
    };
    let end_column = if range.end.row > bottom {
        dimensions.columns - 1
    } else {
        range.end.column.min(dimensions.columns - 1)
    };

    normalize_range(
        CellPoint {
            row: visible_start_row,
            column: start_column,
        },
        CellPoint {
            row: visible_end_row,
            column: end_column,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ClickRecord {
    point: CellPoint,
    count: u8,
    at: Instant,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ClickTracker {
    last: Option<ClickRecord>,
}

impl ClickTracker {
    pub fn register_click(&mut self, point: CellPoint, at: Instant) -> u8 {
        let count = self
            .last
            .filter(|record| {
                record.point == point
                    && at.saturating_duration_since(record.at) <= CLICK_COUNT_TIMEOUT
                    && record.count < 3
            })
            .map_or(1, |record| record.count + 1);

        self.last = Some(ClickRecord { point, count, at });
        count
    }
}

pub fn cell_at_physical(x_px: f64, y_px: f64, cell: CellSize, dimensions: Dimensions) -> CellPoint {
    cell_at_physical_with_padding(x_px, y_px, cell, dimensions, WindowPadding::ZERO)
}

pub(crate) fn cell_at_physical_with_padding(
    x_px: f64,
    y_px: f64,
    cell: CellSize,
    dimensions: Dimensions,
    padding: WindowPadding,
) -> CellPoint {
    let pad = f64::from(padding.physical_px());
    let column = ((x_px - pad).max(0.0) as u32 / cell.width.max(1)) as usize;
    let row = ((y_px - pad).max(0.0) as u32 / cell.height.max(1)) as usize;
    CellPoint {
        row: row.min(dimensions.rows - 1),
        column: column.min(dimensions.columns - 1),
    }
}

pub fn normalize_range(anchor: CellPoint, focus: CellPoint) -> Option<SelectionRange> {
    let start_first = (anchor.row, anchor.column) <= (focus.row, focus.column);
    let (start, end) = if start_first {
        (anchor, focus)
    } else {
        (focus, anchor)
    };

    (start != end).then_some(SelectionRange { start, end })
}

/// Word selection treats ASCII/Unicode alphanumerics plus `_`, `.`, `/`, `-`,
/// and `~` as one word. The punctuation set is path-friendly so a double-click
/// can select common shell paths like `./src/foo-bar`.
pub fn is_selection_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-' | '~')
}

fn snapshot_cell_char(snapshot: &Snapshot, point: CellPoint) -> Option<char> {
    if point.row >= snapshot.dimensions.rows || point.column >= snapshot.dimensions.columns {
        return None;
    }
    snapshot
        .cells
        .get(point.row * snapshot.dimensions.columns + point.column)
        .map(|cell| cell.ch)
}

pub fn word_range_at(snapshot: &Snapshot, point: CellPoint) -> Option<SelectionRange> {
    if !is_selection_word_char(snapshot_cell_char(snapshot, point)?) {
        return None;
    }

    let mut start = point.column;
    while start > 0
        && snapshot_cell_char(
            snapshot,
            CellPoint {
                row: point.row,
                column: start - 1,
            },
        )
        .is_some_and(is_selection_word_char)
    {
        start -= 1;
    }

    let mut end = point.column;
    while end + 1 < snapshot.dimensions.columns
        && snapshot_cell_char(
            snapshot,
            CellPoint {
                row: point.row,
                column: end + 1,
            },
        )
        .is_some_and(is_selection_word_char)
    {
        end += 1;
    }

    normalize_range(
        CellPoint {
            row: point.row,
            column: start,
        },
        CellPoint {
            row: point.row,
            column: end,
        },
    )
}

pub fn line_range_at(point: CellPoint, dimensions: Dimensions) -> Option<SelectionRange> {
    normalize_range(
        CellPoint {
            row: point.row.min(dimensions.rows - 1),
            column: 0,
        },
        CellPoint {
            row: point.row.min(dimensions.rows - 1),
            column: dimensions.columns - 1,
        },
    )
}

pub fn drag_autoscroll_delta(y_px: f64, cell: CellSize, dimensions: Dimensions) -> isize {
    drag_autoscroll_delta_with_padding(y_px, cell, dimensions, WindowPadding::ZERO)
}

pub(crate) fn drag_autoscroll_delta_with_padding(
    y_px: f64,
    cell: CellSize,
    dimensions: Dimensions,
    padding: WindowPadding,
) -> isize {
    let cell_height = f64::from(cell.height.max(1));
    let pad = f64::from(padding.physical_px());
    let viewport_height = cell_height * dimensions.rows.max(1) as f64;
    let content_y = y_px - pad;

    if content_y < cell_height {
        1
    } else if content_y >= viewport_height - cell_height {
        -1
    } else {
        0
    }
}

pub fn selected_text(snapshot: &Snapshot, range: SelectionRange) -> String {
    let mut lines = Vec::new();
    let start_row = range.start.row.min(snapshot.dimensions.rows - 1);
    let end_row = range.end.row.min(snapshot.dimensions.rows - 1);

    for row in start_row..=end_row {
        let start_column = if row == start_row {
            range.start.column.min(snapshot.dimensions.columns - 1)
        } else {
            0
        };
        let end_column = if row == end_row {
            range.end.column.min(snapshot.dimensions.columns - 1)
        } else {
            snapshot.dimensions.columns - 1
        };
        let offset = row * snapshot.dimensions.columns;
        let line = snapshot.cells[offset + start_column..=offset + end_column]
            .iter()
            .filter(|cell| !cell.wide_continuation)
            .map(|cell| cell.ch)
            .collect::<String>()
            .trim_end()
            .to_owned();
        lines.push(line);
    }

    lines.join("\n")
}

pub fn apply_highlight(
    snapshot: &mut Snapshot,
    range: SelectionRange,
    themed: Option<SelectionStyle>,
) {
    let start_row = range.start.row.min(snapshot.dimensions.rows - 1);
    let end_row = range.end.row.min(snapshot.dimensions.rows - 1);

    for row in start_row..=end_row {
        let start_column = if row == start_row {
            range.start.column.min(snapshot.dimensions.columns - 1)
        } else {
            0
        };
        let end_column = if row == end_row {
            range.end.column.min(snapshot.dimensions.columns - 1)
        } else {
            snapshot.dimensions.columns - 1
        };
        let offset = row * snapshot.dimensions.columns;
        for cell in &mut snapshot.cells[offset + start_column..=offset + end_column] {
            match themed {
                // Themed (opt-in): explicit fill + floored fg. Clear inverse so
                // the role colors are not re-swapped by the renderer.
                Some(style) => {
                    cell.attrs.set_inverse(false);
                    cell.attrs.foreground = Color::Rgb(style.fg[0], style.fg[1], style.fg[2]);
                    cell.attrs.background = Color::Rgb(style.fill[0], style.fill[1], style.fill[2]);
                }
                // Default: historical per-cell inverse, byte-identical.
                None => cell.attrs.set_inverse(true),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Attrs, Cell, Position};

    fn snapshot(lines: &[&str], columns: usize) -> Snapshot {
        let rows = lines.len();
        let mut cells = Vec::new();
        for line in lines {
            let mut chars = line.chars().collect::<Vec<_>>();
            chars.resize(columns, ' ');
            cells.extend(
                chars
                    .into_iter()
                    .take(columns)
                    .map(|ch| Cell::new(ch, Attrs::default())),
            );
        }

        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position::default(),
            cursor_visible: true,
            colors: crate::core::DynamicColors::default(),
            cells,
        }
    }

    #[test]
    fn maps_physical_coordinates_to_clamped_cell() {
        let dims = Dimensions::new(4, 3);
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };

        assert_eq!(
            cell_at_physical(17.0, 33.0, cell, dims),
            CellPoint { row: 2, column: 2 }
        );
        assert_eq!(
            cell_at_physical(-4.0, -9.0, cell, dims),
            CellPoint { row: 0, column: 0 }
        );
        assert_eq!(
            cell_at_physical(999.0, 999.0, cell, dims),
            CellPoint { row: 2, column: 3 }
        );
    }

    #[test]
    fn maps_padded_physical_coordinates_to_grid_cells() {
        let dims = Dimensions::new(4, 3);
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let padding = WindowPadding::from_logical(8.0, 1.0);

        assert_eq!(
            cell_at_physical_with_padding(0.0, 0.0, cell, dims, padding),
            CellPoint { row: 0, column: 0 }
        );
        assert_eq!(
            cell_at_physical_with_padding(8.0, 8.0, cell, dims, padding),
            CellPoint { row: 0, column: 0 }
        );
        assert_eq!(
            cell_at_physical_with_padding(25.0, 41.0, cell, dims, padding),
            CellPoint { row: 2, column: 2 }
        );
    }

    #[test]
    fn normalizes_row_major_range_and_ignores_single_cell() {
        let late = CellPoint { row: 2, column: 1 };
        let early = CellPoint { row: 0, column: 3 };

        assert_eq!(
            normalize_range(late, early),
            Some(SelectionRange {
                start: early,
                end: late,
            })
        );
        assert_eq!(normalize_range(early, early), None);
    }

    #[test]
    fn extracts_row_spanning_text_with_trailing_space_trimmed_per_row() {
        let snapshot = snapshot(&["abcd  ", "  ef  ", "ghij  "], 6);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 2 },
            end: CellPoint { row: 2, column: 1 },
        };

        assert_eq!(selected_text(&snapshot, range), "cd\n  ef\ngh");
    }

    #[test]
    fn extracts_single_row_text_and_preserves_leading_spaces() {
        let snapshot = snapshot(&[" a b  "], 6);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 0 },
            end: CellPoint { row: 0, column: 4 },
        };

        assert_eq!(selected_text(&snapshot, range), " a b");
    }

    #[test]
    fn highlight_inverts_selected_cells_only() {
        let mut snapshot = snapshot(&["abcd", "efgh"], 4);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 2 },
            end: CellPoint { row: 1, column: 1 },
        };

        apply_highlight(&mut snapshot, range, None);

        assert!(!snapshot.cells[1].attrs.inverse());
        assert!(snapshot.cells[2].attrs.inverse());
        assert!(snapshot.cells[3].attrs.inverse());
        assert!(snapshot.cells[4].attrs.inverse());
        assert!(snapshot.cells[5].attrs.inverse());
        assert!(!snapshot.cells[6].attrs.inverse());
    }

    #[test]
    fn themed_highlight_paints_fill_and_floored_fg_without_inverse() {
        let mut snapshot = snapshot(&["abcd", "efgh"], 4);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 2 },
            end: CellPoint { row: 1, column: 1 },
        };
        let style = SelectionStyle {
            fill: [0x24, 0x33, 0x52],
            fg: [0xEA, 0xEE, 0xF4],
        };

        apply_highlight(&mut snapshot, range, Some(style));

        // Unselected cells are untouched (no inverse, default colors).
        assert!(!snapshot.cells[1].attrs.inverse());
        assert_eq!(snapshot.cells[1].attrs.background, Color::Default);
        assert!(!snapshot.cells[6].attrs.inverse());

        // Selected cells carry the themed fill + floored fg, inverse cleared.
        for &i in &[2usize, 3, 4, 5] {
            assert!(!snapshot.cells[i].attrs.inverse());
            assert_eq!(
                snapshot.cells[i].attrs.background,
                Color::Rgb(0x24, 0x33, 0x52)
            );
            assert_eq!(
                snapshot.cells[i].attrs.foreground,
                Color::Rgb(0xEA, 0xEE, 0xF4)
            );
        }
    }

    #[test]
    fn pointer_drag_is_selecting_only_for_select_variants() {
        assert!(!PointerDrag::None.is_selecting());
        assert!(!PointerDrag::Scrollbar.is_selecting());
        assert!(
            PointerDrag::Select {
                granularity: SelectGranularity::Char,
                block: false,
            }
            .is_selecting()
        );
        assert!(
            PointerDrag::Select {
                granularity: SelectGranularity::Word,
                block: false,
            }
            .is_selecting()
        );
        assert_eq!(PointerDrag::default(), PointerDrag::None);
    }

    #[test]
    fn union_absolute_ranges_spans_both_units_in_row_major_order() {
        let p = |row, column| AbsoluteCellPoint { row, column };
        let anchor = AbsoluteSelectionRange {
            start: p(0, 0),
            end: p(0, 4),
        };
        // Focus unit later on the same row: union spans from anchor start to
        // focus end.
        let focus_after = AbsoluteSelectionRange {
            start: p(0, 6),
            end: p(0, 10),
        };
        assert_eq!(
            union_absolute_ranges(anchor, focus_after),
            AbsoluteSelectionRange {
                start: p(0, 0),
                end: p(0, 10),
            }
        );

        // Focus unit earlier (drag back past the anchor): the union still spans
        // the outermost corners, so the order of the arguments does not matter.
        let focus_before = AbsoluteSelectionRange {
            start: p(0, 0),
            end: p(0, 1),
        };
        let later_anchor = AbsoluteSelectionRange {
            start: p(2, 3),
            end: p(2, 7),
        };
        assert_eq!(
            union_absolute_ranges(later_anchor, focus_before),
            AbsoluteSelectionRange {
                start: p(0, 0),
                end: p(2, 7),
            }
        );

        // Degenerate single-cell focus (whitespace, no word) still extends the
        // selection out to that cell.
        let degenerate = AbsoluteSelectionRange {
            start: p(0, 12),
            end: p(0, 12),
        };
        assert_eq!(
            union_absolute_ranges(anchor, degenerate),
            AbsoluteSelectionRange {
                start: p(0, 0),
                end: p(0, 12),
            }
        );
    }

    #[test]
    fn visible_points_map_to_absolute_scrollback_rows() {
        assert_eq!(
            visible_to_absolute(CellPoint { row: 2, column: 5 }, 3, 10),
            AbsoluteCellPoint { row: 9, column: 5 }
        );
    }

    #[test]
    fn absolute_selection_projects_to_current_viewport_intersection() {
        let dims = Dimensions::new(8, 3);
        let range = AbsoluteSelectionRange {
            start: AbsoluteCellPoint { row: 5, column: 3 },
            end: AbsoluteCellPoint { row: 7, column: 4 },
        };

        assert_eq!(
            visible_range_from_absolute(range, 4, 10, dims),
            Some(SelectionRange {
                start: CellPoint { row: 0, column: 0 },
                end: CellPoint { row: 1, column: 4 },
            })
        );

        assert_eq!(visible_range_from_absolute(range, 2, 10, dims), None);
    }

    #[test]
    fn click_tracker_counts_same_cell_with_timeout_and_resets_after_triple() {
        let mut clicks = ClickTracker::default();
        let point = CellPoint { row: 1, column: 2 };
        let later_point = CellPoint { row: 1, column: 3 };
        let t0 = Instant::now();

        assert_eq!(clicks.register_click(point, t0), 1);
        assert_eq!(
            clicks.register_click(point, t0 + Duration::from_millis(100)),
            2
        );
        assert_eq!(
            clicks.register_click(point, t0 + Duration::from_millis(200)),
            3
        );
        assert_eq!(
            clicks.register_click(point, t0 + Duration::from_millis(300)),
            1
        );
        assert_eq!(
            clicks.register_click(later_point, t0 + Duration::from_millis(350)),
            1
        );
        assert_eq!(
            clicks.register_click(later_point, t0 + Duration::from_millis(900)),
            1
        );
    }

    #[test]
    fn word_selection_uses_path_friendly_character_set() {
        let snapshot = snapshot(&["run ./src/foo-bar~ now"], 24);

        assert_eq!(
            word_range_at(&snapshot, CellPoint { row: 0, column: 8 }),
            Some(SelectionRange {
                start: CellPoint { row: 0, column: 4 },
                end: CellPoint { row: 0, column: 17 },
            })
        );
        assert_eq!(
            word_range_at(&snapshot, CellPoint { row: 0, column: 3 }),
            None
        );
    }

    #[test]
    fn drag_autoscroll_uses_top_and_bottom_edge_bands() {
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let dims = Dimensions::new(80, 4);

        assert_eq!(drag_autoscroll_delta(4.0, cell, dims), 1);
        assert_eq!(drag_autoscroll_delta(32.0, cell, dims), 0);
        assert_eq!(drag_autoscroll_delta(60.0, cell, dims), -1);
    }

    #[test]
    fn padded_drag_autoscroll_uses_content_edge_bands() {
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let dims = Dimensions::new(80, 4);
        let padding = WindowPadding::from_logical(8.0, 1.0);

        assert_eq!(
            drag_autoscroll_delta_with_padding(4.0, cell, dims, padding),
            1
        );
        assert_eq!(
            drag_autoscroll_delta_with_padding(40.0, cell, dims, padding),
            0
        );
        assert_eq!(
            drag_autoscroll_delta_with_padding(68.0, cell, dims, padding),
            -1
        );
    }
}
