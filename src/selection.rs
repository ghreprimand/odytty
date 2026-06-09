use crate::core::{Dimensions, Snapshot};
use crate::text::CellSize;

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

pub fn cell_at_physical(x_px: f64, y_px: f64, cell: CellSize, dimensions: Dimensions) -> CellPoint {
    let column = (x_px.max(0.0) as u32 / cell.width.max(1)) as usize;
    let row = (y_px.max(0.0) as u32 / cell.height.max(1)) as usize;
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

pub fn apply_highlight(snapshot: &mut Snapshot, range: SelectionRange) {
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
            cell.attrs.inverse = true;
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
            cells.extend(chars.into_iter().take(columns).map(|ch| Cell {
                ch,
                attrs: Attrs::default(),
                wide_continuation: false,
            }));
        }

        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position::default(),
            cursor_visible: true,
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

        apply_highlight(&mut snapshot, range);

        assert!(!snapshot.cells[1].attrs.inverse);
        assert!(snapshot.cells[2].attrs.inverse);
        assert!(snapshot.cells[3].attrs.inverse);
        assert!(snapshot.cells[4].attrs.inverse);
        assert!(snapshot.cells[5].attrs.inverse);
        assert!(!snapshot.cells[6].attrs.inverse);
    }
}
