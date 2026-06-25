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
/// field is reserved for column/rectangular selection (MOUSE-RECT) and is wired
/// into the type now, constructed by its own later packet — it is part of this
/// crate's public API (a `pub` enum in a `pub` module), so the not-yet-
/// constructed arm does not trip the dead-code lint. `Scrollbar` carries the
/// grab offset for the draggable scroll thumb (MOUSE-SCROLLBAR).
///
/// `Eq` is intentionally not derived: `Scrollbar` carries an `f32` grab offset,
/// which is only `PartialEq`. No call site needs total equality.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PointerDrag {
    #[default]
    None,
    Select {
        granularity: SelectGranularity,
        /// Reserved for MOUSE-RECT (Alt+drag column selection). Always `false`
        /// today; threading it through render/extract is a later packet.
        block: bool,
    },
    /// Dragging the right-edge scroll thumb to scrub scrollback
    /// (MOUSE-SCROLLBAR). `grab_dy` is where on the thumb the press landed
    /// (px below the thumb top), so the drag keeps the cursor anchored to that
    /// point rather than snapping the thumb to the cursor.
    Scrollbar { grab_dy: f32 },
}

impl PointerDrag {
    /// Whether a local text selection drag is in progress (any granularity).
    /// This is the typed replacement for the old `selecting` boolean truthiness
    /// at every call site.
    pub fn is_selecting(&self) -> bool {
        matches!(self, PointerDrag::Select { .. })
    }

    /// The thumb grab offset when a scroll-thumb drag (MOUSE-SCROLLBAR) is in
    /// progress, else `None`. Lets the drag-update path read the anchor without
    /// re-matching the variant.
    pub fn scrollbar_grab(&self) -> Option<f32> {
        match self {
            PointerDrag::Scrollbar { grab_dy } => Some(*grab_dy),
            _ => None,
        }
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
        range.start.column.min(dimensions.columns.saturating_sub(1))
    };
    let end_column = if range.end.row > bottom {
        dimensions.columns.saturating_sub(1)
    } else {
        range.end.column.min(dimensions.columns.saturating_sub(1))
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

/// The inclusive column band `[lo, hi]` a block (rectangular/column) selection
/// covers (MOUSE-RECT). A range's two corner columns are the dragged endpoints,
/// which may be in either order, so this min/max's them. Shared by block text
/// extraction and block highlight (and reused by the keyboard block-selection
/// mode later), so the column geometry has one source of truth.
pub fn block_column_bounds(range: SelectionRange) -> (usize, usize) {
    (
        range.start.column.min(range.end.column),
        range.start.column.max(range.end.column),
    )
}

/// Map an absolute block selection into the visible grid (MOUSE-RECT). Like
/// [`visible_range_from_absolute`], the row span is clipped to the viewport, but
/// — crucially for a block — the column band is taken from the absolute range's
/// two corners and preserved on every visible row. The wrapped helper zeroes a
/// top-clipped corner's column and maximizes a bottom-clipped corner's column
/// (correct for wrapped text, where a clipped row spans full width); a block
/// must keep the SAME column band regardless of vertical clipping. Returns a
/// normalized range with `start = (top_row, lo)` and `end = (bottom_row, hi)`,
/// or `None` when the block is entirely outside the viewport.
pub fn visible_block_range_from_absolute(
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
    let last_column = dimensions.columns.saturating_sub(1);
    let lo = range.start.column.min(range.end.column).min(last_column);
    let hi = range.start.column.max(range.end.column).min(last_column);

    Some(SelectionRange {
        start: CellPoint {
            row: visible_start_row,
            column: lo,
        },
        end: CellPoint {
            row: visible_end_row,
            column: hi,
        },
    })
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
        row: row.min(dimensions.rows.saturating_sub(1)),
        column: column.min(dimensions.columns.saturating_sub(1)),
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
            row: point.row.min(dimensions.rows.saturating_sub(1)),
            column: 0,
        },
        CellPoint {
            row: point.row.min(dimensions.rows.saturating_sub(1)),
            column: dimensions.columns.saturating_sub(1),
        },
    )
}

pub fn drag_autoscroll_delta(y_px: f64, cell: CellSize, dimensions: Dimensions) -> isize {
    drag_autoscroll_delta_with_padding(y_px, cell, dimensions, WindowPadding::ZERO)
}

/// Fixed-rate (±1/0) drag-edge autoscroll delta: the historical one-row-per-tick
/// behavior. A thin wrapper over [`drag_autoscroll_step_with_padding`] capped at
/// a single row, so the band geometry has one source of truth.
pub(crate) fn drag_autoscroll_delta_with_padding(
    y_px: f64,
    cell: CellSize,
    dimensions: Dimensions,
    padding: WindowPadding,
) -> isize {
    drag_autoscroll_step_with_padding(y_px, cell, dimensions, padding, 1)
}

/// Velocity-proportional drag-edge autoscroll step (MOUSE-AUTOSCROLL-VEL).
///
/// Returns a signed row count: positive scrolls up into history (pointer above
/// the top edge band), negative scrolls toward the live bottom, `0` while the
/// pointer is inside the content area between the bands. The magnitude grows by
/// one row for every additional cell-height the pointer is dragged past the band
/// edge, clamped to `max_rows` so it can never run away. `max_rows == 1`
/// collapses the ramp to exactly ±1/0 — byte-identical to the historical fixed
/// one-row-per-tick autoscroll, which is the `scroll_drag_speed = legacy`
/// opt-out path. The 80 ms tick cadence lives in the caller and is unchanged.
pub(crate) fn drag_autoscroll_step_with_padding(
    y_px: f64,
    cell: CellSize,
    dimensions: Dimensions,
    padding: WindowPadding,
    max_rows: usize,
) -> isize {
    let cell_height = f64::from(cell.height.max(1));
    let pad = f64::from(padding.physical_px());
    let viewport_height = cell_height * dimensions.rows.max(1) as f64;
    let content_y = y_px - pad;
    let max_rows = max_rows.max(1) as isize;

    if content_y < cell_height {
        autoscroll_rows(cell_height - content_y, cell_height, max_rows)
    } else if content_y >= viewport_height - cell_height {
        -autoscroll_rows(
            content_y - (viewport_height - cell_height),
            cell_height,
            max_rows,
        )
    } else {
        0
    }
}

/// Map how far (in pixels) the pointer overshot the band edge to a row count:
/// one base row plus one extra per full cell-height of overshoot, clamped to
/// `[1, max_rows]`. With `max_rows == 1` this is always `1`.
fn autoscroll_rows(overshoot_px: f64, cell_height: f64, max_rows: isize) -> isize {
    let extra = (overshoot_px / cell_height).floor().max(0.0) as isize;
    (1 + extra).clamp(1, max_rows)
}

pub fn selected_text(snapshot: &Snapshot, range: SelectionRange) -> String {
    let mut lines = Vec::new();
    let start_row = range
        .start
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));
    let end_row = range
        .end
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));

    for row in start_row..=end_row {
        let start_column = if row == start_row {
            range
                .start
                .column
                .min(snapshot.dimensions.columns.saturating_sub(1))
        } else {
            0
        };
        let end_column = if row == end_row {
            range
                .end
                .column
                .min(snapshot.dimensions.columns.saturating_sub(1))
        } else {
            snapshot.dimensions.columns.saturating_sub(1)
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

/// Extract a block (rectangular/column) selection as text (MOUSE-RECT). Unlike
/// the wrapped [`selected_text`] — where only the first and last rows are
/// partial and the interior rows span the full width — every row contributes
/// the SAME inclusive column band [`block_column_bounds`].
///
/// Ragged-line rule: the terminal grid is a fixed cell matrix where short lines
/// are space-padded to full width, so every row always has a cell at every
/// column. A row therefore contributes exactly its slice of the band: leading
/// spaces inside the band are PRESERVED (the band's left edge is a hard column
/// boundary), trailing spaces are trimmed per row (matching the wrapped path),
/// and a row that is entirely blank within the band trims to an empty string.
/// Wide-continuation spacers are dropped. Rows are joined with `'\n'`. `range`
/// is a visible-space range whose two corner columns define the band.
pub fn selected_text_block(snapshot: &Snapshot, range: SelectionRange) -> String {
    let (lo, hi) = block_column_bounds(range);
    let start_row = range
        .start
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));
    let end_row = range
        .end
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));
    let lo = lo.min(snapshot.dimensions.columns.saturating_sub(1));
    let hi = hi.min(snapshot.dimensions.columns.saturating_sub(1));

    let mut lines = Vec::new();
    for row in start_row..=end_row {
        let offset = row * snapshot.dimensions.columns;
        let line = snapshot.cells[offset + lo..=offset + hi]
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

/// Apply the selection treatment to one cell: the historical per-cell inverse
/// when `themed` is `None` (byte-identical default), or an explicit
/// RV1-floored fill + foreground when a [`SelectionStyle`] is supplied (ID1).
/// Shared by the wrapped [`apply_highlight`] and the block
/// [`apply_highlight_block`] so the two paths can never diverge in how a
/// selected cell is painted.
fn highlight_cell(cell: &mut crate::core::Cell, themed: Option<SelectionStyle>) {
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

pub fn apply_highlight(
    snapshot: &mut Snapshot,
    range: SelectionRange,
    themed: Option<SelectionStyle>,
) {
    let start_row = range
        .start
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));
    let end_row = range
        .end
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));

    for row in start_row..=end_row {
        let start_column = if row == start_row {
            range
                .start
                .column
                .min(snapshot.dimensions.columns.saturating_sub(1))
        } else {
            0
        };
        let end_column = if row == end_row {
            range
                .end
                .column
                .min(snapshot.dimensions.columns.saturating_sub(1))
        } else {
            snapshot.dimensions.columns.saturating_sub(1)
        };
        let offset = row * snapshot.dimensions.columns;
        for cell in &mut snapshot.cells[offset + start_column..=offset + end_column] {
            highlight_cell(cell, themed);
        }
    }
}

/// Resolve an absolute selection into its visible range and paint the
/// highlight, dispatching between the wrapped and block (MOUSE-RECT) projections
/// so the render path stays a single call. A no-op when the selection is
/// entirely outside the viewport. The block branch preserves the column band on
/// every visible row via [`visible_block_range_from_absolute`]; the wrapped
/// branch is byte-identical to the historical
/// [`visible_range_from_absolute`] + [`apply_highlight`] pair.
pub fn apply_selection_highlight(
    snapshot: &mut Snapshot,
    range: AbsoluteSelectionRange,
    block: bool,
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    themed: Option<SelectionStyle>,
) {
    if block {
        if let Some(visible) =
            visible_block_range_from_absolute(range, viewport_offset, scrollback_len, dimensions)
        {
            apply_highlight_block(snapshot, visible, themed);
        }
    } else if let Some(visible) =
        visible_range_from_absolute(range, viewport_offset, scrollback_len, dimensions)
    {
        apply_highlight(snapshot, visible, themed);
    }
}

/// Block (rectangular/column) variant of [`apply_highlight`] (MOUSE-RECT). The
/// same column band [`block_column_bounds`] is painted on EVERY row in the
/// range, instead of the wrapped path's partial first/last rows. `range` is a
/// visible-space range whose two corner columns define the band; the caller
/// derives it from the absolute selection via [`visible_block_range_from_absolute`]
/// so the band is preserved even when the block extends past the viewport.
pub fn apply_highlight_block(
    snapshot: &mut Snapshot,
    range: SelectionRange,
    themed: Option<SelectionStyle>,
) {
    let (lo, hi) = block_column_bounds(range);
    let start_row = range
        .start
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));
    let end_row = range
        .end
        .row
        .min(snapshot.dimensions.rows.saturating_sub(1));
    let lo = lo.min(snapshot.dimensions.columns.saturating_sub(1));
    let hi = hi.min(snapshot.dimensions.columns.saturating_sub(1));

    for row in start_row..=end_row {
        let offset = row * snapshot.dimensions.columns;
        for cell in &mut snapshot.cells[offset + lo..=offset + hi] {
            highlight_cell(cell, themed);
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
    fn cell_at_physical_zero_area_surface_does_not_underflow() {
        // P0-3 headline: a pointer-move while the surface is 0-area (minimized,
        // mid-resize-to-zero, or a degenerate pre-layout grid) must NOT underflow
        // `rows - 1` / `columns - 1` — that panics in debug and wraps to
        // usize::MAX in release, letting an out-of-range cell escape downstream.
        // saturating_sub(1) pins a zero-dim grid to the (0,0) edge cell, which
        // downstream `.get()` bounds checks absorb safely.
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let pad = WindowPadding::ZERO;
        // Fully zero-area.
        assert_eq!(
            cell_at_physical_with_padding(40.0, 40.0, cell, Dimensions::new(0, 0), pad),
            CellPoint { row: 0, column: 0 },
        );
        // Degenerate single-axis permutations (one dim zero).
        assert_eq!(
            cell_at_physical_with_padding(40.0, 40.0, cell, Dimensions::new(1, 0), pad),
            CellPoint { row: 0, column: 0 },
        );
        assert_eq!(
            cell_at_physical_with_padding(40.0, 40.0, cell, Dimensions::new(0, 1), pad),
            CellPoint { row: 0, column: 0 },
        );
        // A negative (off-window-left/above) coordinate at zero-dim is still (0,0)
        // — lower clamp and saturating upper clamp compose.
        assert_eq!(
            cell_at_physical_with_padding(-9999.0, -9999.0, cell, Dimensions::new(0, 0), pad),
            CellPoint { row: 0, column: 0 },
        );
    }

    #[test]
    fn cell_at_physical_far_multidisplay_coordinate_clamps_without_wrap() {
        // An off-window-right / far secondary-display coordinate must saturate to
        // the last cell, never wrap. Rust's saturating float→int cast pins the
        // huge pixel value to u32::MAX, then `.min(dim-1)` clamps.
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let dims = Dimensions::new(4, 3); // cols=4, rows=3
        assert_eq!(
            cell_at_physical(5_000_000.0, 5_000_000.0, cell, dims),
            CellPoint { row: 2, column: 3 },
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
        assert!(!PointerDrag::Scrollbar { grab_dy: 0.0 }.is_selecting());
        assert_eq!(
            PointerDrag::Scrollbar { grab_dy: 4.5 }.scrollbar_grab(),
            Some(4.5)
        );
        assert_eq!(PointerDrag::None.scrollbar_grab(), None);
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

    #[test]
    fn autoscroll_step_legacy_cap_is_byte_identical_to_fixed_rate() {
        // MOUSE-AUTOSCROLL-VEL OFF-path parity: `max_rows == 1` must reproduce
        // the historical ±1/0 fixed-rate delta exactly, at every y the legacy
        // helper is sampled at — including well past the band where the ramp
        // would otherwise accelerate.
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let dims = Dimensions::new(80, 4);
        let padding = WindowPadding::ZERO;

        for y in [-200.0, -16.0, 0.0, 4.0, 15.9, 32.0, 48.0, 60.0, 64.0, 200.0] {
            assert_eq!(
                drag_autoscroll_step_with_padding(y, cell, dims, padding, 1),
                drag_autoscroll_delta_with_padding(y, cell, dims, padding),
                "legacy cap must match the fixed-rate delta at y={y}"
            );
        }
    }

    #[test]
    fn autoscroll_step_ramps_with_overshoot_and_caps() {
        // The step grows one row per cell-height of overshoot past the band edge
        // and is clamped to `max_rows`, preserving sign.
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let dims = Dimensions::new(80, 4);
        let padding = WindowPadding::ZERO;
        let max = 8;

        // Top band edge (content_y == cell_height boundary): one row.
        assert_eq!(
            drag_autoscroll_step_with_padding(15.0, cell, dims, padding, max),
            1
        );
        // At the very top of the viewport: one cell-height of overshoot -> 2.
        assert_eq!(
            drag_autoscroll_step_with_padding(0.0, cell, dims, padding, max),
            2
        );
        // One cell-height above the window top: two overshoot -> 3.
        assert_eq!(
            drag_autoscroll_step_with_padding(-16.0, cell, dims, padding, max),
            3
        );
        // Far above: clamped to +max.
        assert_eq!(
            drag_autoscroll_step_with_padding(-1000.0, cell, dims, padding, max),
            max as isize
        );
        // Inside the content band: no scroll regardless of the cap.
        assert_eq!(
            drag_autoscroll_step_with_padding(32.0, cell, dims, padding, max),
            0
        );
        // Far below the bottom band: clamped to -max (sign preserved).
        assert_eq!(
            drag_autoscroll_step_with_padding(1000.0, cell, dims, padding, max),
            -(max as isize)
        );
    }

    #[test]
    fn autoscroll_step_zero_cap_is_floored_to_one_row() {
        // Defensive: a `max_rows` of 0 is floored to 1 so the helper can never
        // return a zero step while the pointer is past the band (which would
        // silently disable autoscroll).
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let dims = Dimensions::new(80, 4);
        let padding = WindowPadding::ZERO;

        assert_eq!(
            drag_autoscroll_step_with_padding(-1000.0, cell, dims, padding, 0),
            1
        );
    }

    #[test]
    fn block_column_bounds_orders_either_corner() {
        // Corners in either column order yield the same inclusive [lo, hi] band.
        let forward = SelectionRange {
            start: CellPoint { row: 0, column: 2 },
            end: CellPoint { row: 3, column: 6 },
        };
        let reversed = SelectionRange {
            start: CellPoint { row: 0, column: 6 },
            end: CellPoint { row: 3, column: 2 },
        };
        assert_eq!(block_column_bounds(forward), (2, 6));
        assert_eq!(block_column_bounds(reversed), (2, 6));
    }

    #[test]
    fn block_text_extracts_the_same_column_band_on_every_row() {
        // Unlike wrapped extraction, interior rows do NOT span the full width;
        // every row contributes only columns [2, 4], trimmed per row.
        let snapshot = snapshot(&["ab12ef", "gh34ij", "kl56mn"], 6);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 2 },
            end: CellPoint { row: 2, column: 4 },
        };

        assert_eq!(selected_text_block(&snapshot, range), "12e\n34i\n56m");
    }

    #[test]
    fn block_text_trims_trailing_spaces_per_row() {
        // A band that runs into trailing whitespace trims each row independently
        // (a row that is all spaces in the band becomes empty).
        let snapshot = snapshot(&["aXY   ", "b     ", "cZ    "], 6);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 1 },
            end: CellPoint { row: 2, column: 3 },
        };

        assert_eq!(selected_text_block(&snapshot, range), "XY\n\nZ");
    }

    #[test]
    fn block_text_preserves_leading_spaces_inside_the_band() {
        // The band's left edge is a hard column boundary: a space at the band's
        // first column is part of the rectangle and is preserved, while trailing
        // spaces are trimmed. Band = columns [1, 4].
        let snapshot = snapshot(&[" a x  ", "  bb  ", " cc   "], 6);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 1 },
            end: CellPoint { row: 2, column: 4 },
        };

        // Row 0 band "a x" (col1='a', col2=' ', col3='x', col4=' ' trimmed).
        // Row 1 band " bb" (col1=' ', col2='b', col3='b', col4=' ' trimmed).
        // Row 2 band "cc" (col1='c', col2='c', col3=' ', col4=' ' trimmed).
        assert_eq!(selected_text_block(&snapshot, range), "a x\n bb\ncc");
    }

    #[test]
    fn block_text_diverges_from_wrapped_on_the_same_corners() {
        // The same two corners produce a column band under block extraction but
        // wrapped (first/last partial, interior full-width) under the wrapped
        // path — proving the two are genuinely different selections.
        let snapshot = snapshot(&["abcdef", "ghijkl", "mnopqr"], 6);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 1 },
            end: CellPoint { row: 2, column: 3 },
        };

        assert_eq!(selected_text_block(&snapshot, range), "bcd\nhij\nnop");
        assert_eq!(selected_text(&snapshot, range), "bcdef\nghijkl\nmnop");
    }

    #[test]
    fn block_text_drops_wide_continuation_spacers_in_the_band() {
        // A wide glyph occupies a lead cell plus a `wide_continuation` spacer.
        // Row: 'a' | '世'(wide lead) | spacer | 'b' | 'c' | 'd'. The spacer must
        // never copy as its own cell, so a block band cutting through the glyph
        // yields the wide char exactly once with no phantom space — the same
        // `!wide_continuation` filtering the wrapped path uses.
        let snapshot = Snapshot {
            dimensions: Dimensions::new(6, 1),
            cursor: Position::default(),
            cursor_visible: true,
            colors: crate::core::DynamicColors::default(),
            cells: vec![
                Cell::new('a', Attrs::default()),
                Cell::new('世', Attrs::default()),
                Cell::wide_spacer(Attrs::default()),
                Cell::new('b', Attrs::default()),
                Cell::new('c', Attrs::default()),
                Cell::new('d', Attrs::default()),
            ],
        };

        // Band [1, 3] spans the wide lead, its spacer, and 'b': the spacer
        // drops, so the wide char copies once → "世b" (not "世 b").
        let through = SelectionRange {
            start: CellPoint { row: 0, column: 1 },
            end: CellPoint { row: 0, column: 3 },
        };
        assert_eq!(selected_text_block(&snapshot, through), "世b");

        // Band [2, 3] starts ON the orphaned continuation spacer (its lead is
        // outside the band): the spacer drops, leaving just 'b'.
        let from_spacer = SelectionRange {
            start: CellPoint { row: 0, column: 2 },
            end: CellPoint { row: 0, column: 3 },
        };
        assert_eq!(selected_text_block(&snapshot, from_spacer), "b");
    }

    #[test]
    fn visible_block_range_preserves_the_column_band_when_clipped() {
        // A block whose top rows are scrolled above the viewport must keep its
        // [lo, hi] column band on the visible rows — the wrapped projection
        // would instead zero the top-clipped corner's column.
        let dims = Dimensions::new(8, 3);
        let range = AbsoluteSelectionRange {
            start: AbsoluteCellPoint { row: 5, column: 3 },
            end: AbsoluteCellPoint { row: 7, column: 5 },
        };

        // viewport_offset 4 over scrollback 10: top absolute row = 6, so row 5
        // (with column 3) is clipped above the viewport.
        assert_eq!(
            visible_block_range_from_absolute(range, 4, 10, dims),
            Some(SelectionRange {
                start: CellPoint { row: 0, column: 3 },
                end: CellPoint { row: 1, column: 5 },
            })
        );
        // Contrast: the wrapped projection zeros the clipped corner's column.
        assert_eq!(
            visible_range_from_absolute(range, 4, 10, dims),
            Some(SelectionRange {
                start: CellPoint { row: 0, column: 0 },
                end: CellPoint { row: 1, column: 5 },
            })
        );
        // Entirely outside the viewport returns None, like the wrapped helper.
        assert_eq!(visible_block_range_from_absolute(range, 2, 10, dims), None);
    }

    #[test]
    fn block_highlight_inverts_only_the_column_band() {
        // Only columns [1, 2] on both rows are inverted; columns 0 and 3 stay
        // untouched — the rectangular shape, not the wrapped run.
        let mut snapshot = snapshot(&["abcd", "efgh"], 4);
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 1 },
            end: CellPoint { row: 1, column: 2 },
        };

        apply_highlight_block(&mut snapshot, range, None);

        // Row 0: col 0 untouched, cols 1-2 inverted, col 3 untouched.
        assert!(!snapshot.cells[0].attrs.inverse());
        assert!(snapshot.cells[1].attrs.inverse());
        assert!(snapshot.cells[2].attrs.inverse());
        assert!(!snapshot.cells[3].attrs.inverse());
        // Row 1: same band.
        assert!(!snapshot.cells[4].attrs.inverse());
        assert!(snapshot.cells[5].attrs.inverse());
        assert!(snapshot.cells[6].attrs.inverse());
        assert!(!snapshot.cells[7].attrs.inverse());
    }

    #[test]
    fn apply_selection_highlight_dispatches_block_vs_wrapped() {
        let dims = Dimensions::new(4, 2);
        // Two corners spanning both rows; column band [1, 2].
        let range = AbsoluteSelectionRange {
            start: AbsoluteCellPoint { row: 0, column: 1 },
            end: AbsoluteCellPoint { row: 1, column: 2 },
        };

        // Block: only the column band [1, 2] is inverted on both rows.
        let mut block = snapshot(&["abcd", "efgh"], 4);
        apply_selection_highlight(&mut block, range, true, 0, 0, dims, None);
        assert!(!block.cells[0].attrs.inverse());
        assert!(block.cells[1].attrs.inverse());
        assert!(block.cells[2].attrs.inverse());
        assert!(!block.cells[3].attrs.inverse());
        assert!(!block.cells[4].attrs.inverse());
        assert!(block.cells[5].attrs.inverse());
        assert!(block.cells[6].attrs.inverse());
        assert!(!block.cells[7].attrs.inverse());

        // Wrapped: row 0 runs col 1 to end-of-row, row 1 from col 0 to col 2.
        let mut wrapped = snapshot(&["abcd", "efgh"], 4);
        apply_selection_highlight(&mut wrapped, range, false, 0, 0, dims, None);
        assert!(!wrapped.cells[0].attrs.inverse());
        assert!(wrapped.cells[1].attrs.inverse());
        assert!(wrapped.cells[2].attrs.inverse());
        assert!(wrapped.cells[3].attrs.inverse());
        assert!(wrapped.cells[4].attrs.inverse());
        assert!(wrapped.cells[5].attrs.inverse());
        assert!(wrapped.cells[6].attrs.inverse());
        assert!(!wrapped.cells[7].attrs.inverse());

        // Off-viewport selection is a no-op (nothing inverted) for both modes.
        let off = AbsoluteSelectionRange {
            start: AbsoluteCellPoint { row: 5, column: 1 },
            end: AbsoluteCellPoint { row: 6, column: 2 },
        };
        let mut none = snapshot(&["abcd", "efgh"], 4);
        apply_selection_highlight(&mut none, off, true, 2, 10, dims, None);
        apply_selection_highlight(&mut none, off, false, 2, 10, dims, None);
        assert!(none.cells.iter().all(|cell| !cell.attrs.inverse()));
    }
}
