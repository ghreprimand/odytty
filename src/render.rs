// SPDX-License-Identifier: GPL-3.0-only
use crate::core::{Dimensions, Snapshot, TerminalModel};

pub trait Renderer {
    fn draw(&mut self, snapshot: &Snapshot);
}

/// Fixed pixel metrics for one monospace cell.
///
/// The future GPU text renderer uses these to place glyph quads. Keeping the
/// presentation math here — independent of any window or device — lets it be
/// unit-tested without `winit`/`wgpu`, and keeps terminal semantics out of the
/// renderer. All values are in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub width_px: f32,
    pub height_px: f32,
}

impl CellMetrics {
    pub fn new(width_px: f32, height_px: f32) -> Self {
        Self {
            width_px: width_px.max(1.0),
            height_px: height_px.max(1.0),
        }
    }

    /// Top-left pixel origin of the cell at `(row, column)`.
    pub fn cell_origin(&self, row: usize, column: usize) -> (f32, f32) {
        (column as f32 * self.width_px, row as f32 * self.height_px)
    }

    /// Pixel size of the full grid surface for the given grid dimensions.
    pub fn surface_size(&self, dimensions: Dimensions) -> (f32, f32) {
        (
            dimensions.columns as f32 * self.width_px,
            dimensions.rows as f32 * self.height_px,
        )
    }
}

#[derive(Debug, Default)]
pub struct NullRenderer {
    pub frames_drawn: usize,
}

impl Renderer for NullRenderer {
    fn draw(&mut self, _snapshot: &Snapshot) {
        self.frames_drawn += 1;
    }
}

impl NullRenderer {
    pub fn draw_model(&mut self, model: &impl TerminalModel) {
        self.draw(&model.snapshot());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_origin_scales_with_row_and_column() {
        let metrics = CellMetrics::new(8.0, 16.0);
        assert_eq!(metrics.cell_origin(0, 0), (0.0, 0.0));
        assert_eq!(metrics.cell_origin(2, 3), (24.0, 32.0));
    }

    #[test]
    fn surface_size_covers_full_grid() {
        let metrics = CellMetrics::new(8.0, 16.0);
        assert_eq!(
            metrics.surface_size(Dimensions::new(80, 24)),
            (640.0, 384.0)
        );
    }

    #[test]
    fn metrics_clamp_to_positive() {
        let metrics = CellMetrics::new(0.0, -4.0);
        assert!(metrics.width_px >= 1.0);
        assert!(metrics.height_px >= 1.0);
    }
}
