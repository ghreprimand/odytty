// SPDX-License-Identifier: GPL-3.0-only
use std::path::PathBuf;

use crate::atlas::SubpixelMode;
use crate::core::Dimensions;
use crate::render::CellMetrics;
use crate::settings::{
    DEFAULT_BOX_THICKNESS, DEFAULT_FONT_SIZE_PX, DEFAULT_LINE_HEIGHT, DEFAULT_WINDOW_PADDING_PX,
    Settings,
};

/// Errors from the native app path.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// The OS event loop could not be created or failed while running.
    #[error("native event loop error: {0}")]
    EventLoop(String),
    /// The OS window could not be created.
    #[error("native window creation failed: {0}")]
    WindowCreation(String),
    /// The GPU surface could not be created for the window.
    #[error("gpu surface creation failed: {0}")]
    SurfaceCreation(String),
    /// No compatible GPU adapter was available.
    #[error("no compatible gpu adapter: {0}")]
    NoAdapter(String),
    /// The GPU device/queue could not be acquired.
    #[error("gpu device request failed: {0}")]
    DeviceRequest(String),
    /// Font loading or glyph-atlas setup for the text renderer failed.
    #[error("text setup failed: {0}")]
    Text(String),
    /// The shell PTY could not be spawned.
    #[error("pty spawn failed: {0}")]
    Pty(String),
}

/// Initial native window and text assumptions.
///
/// These are the documented starting defaults for the Linux-first prototype.
/// They are intentionally minimal; richer settings (themes, effects) belong to a
/// later Odyssey layer, not here.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeOptions {
    /// Window title.
    pub title: String,
    /// Initial terminal grid size in columns/rows.
    pub initial_grid: Dimensions,
    /// Monospace font family request. `"monospace"` defers to the system's
    /// default fixed-width face for the first prototype.
    pub font_family: String,
    /// Optional explicit font file from runtime settings.
    pub font_path: Option<PathBuf>,
    /// Font size in logical pixels.
    pub font_size_px: f32,
    /// Glyph coverage gamma used by the cell shader. `1.0` is the legacy
    /// linear blend path; higher values give light-on-dark text more weight.
    pub text_gamma: f32,
    /// Optional RGB/BGR subpixel text coverage. Defaults off for exact
    /// grayscale output compatibility.
    pub subpixel: SubpixelMode,
    /// Logical pixels of inset between the window edge and the terminal grid.
    pub window_padding_px: f32,
    /// Line-height multiplier baked into the glyph atlas cell (LINEHEIGHT).
    /// `1.0` adds zero leading and is byte-identical to the historical cell
    /// geometry; higher values grow the cell box for extra vertical breathing
    /// room.
    pub line_height: f32,
    /// Box-drawing stroke-thickness multiplier (BOXTHICK). `1.0` reproduces the
    /// historical geometric box-drawing weights byte-identically; other values
    /// scale the rule thickness. Inert unless geometric box-drawing is on.
    pub box_thickness: f32,
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            title: "OdyTTY".to_owned(),
            initial_grid: Dimensions::new(80, 24),
            font_family: "monospace".to_owned(),
            font_path: None,
            font_size_px: DEFAULT_FONT_SIZE_PX,
            text_gamma: crate::settings::DEFAULT_TEXT_GAMMA,
            subpixel: SubpixelMode::Off,
            window_padding_px: DEFAULT_WINDOW_PADDING_PX,
            line_height: DEFAULT_LINE_HEIGHT,
            box_thickness: DEFAULT_BOX_THICKNESS,
        }
    }
}

impl NativeOptions {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            font_family: settings
                .font_family
                .clone()
                .unwrap_or_else(|| Self::default().font_family),
            font_path: settings.font_path.clone(),
            font_size_px: settings.font_size_px,
            text_gamma: settings.text_gamma,
            subpixel: settings.subpixel,
            window_padding_px: settings.window_padding_px,
            line_height: settings.line_height,
            box_thickness: settings.box_thickness,
            ..Self::default()
        }
    }

    /// Approximate per-cell pixel metrics derived from the font size.
    ///
    /// These are deliberately coarse stand-ins for real font metrics: a typical
    /// monospace advance width is ~0.6em and line height ~1.2em. The real text
    /// renderer will replace these with measured glyph metrics, but they give a
    /// realistic initial window size without pulling in a font stack yet.
    pub fn cell_metrics(&self) -> CellMetrics {
        CellMetrics::new(self.font_size_px * 0.6, self.font_size_px * 1.2)
    }

    /// Logical window size, in pixels, for the requested grid.
    ///
    /// Returned as integer logical pixels suitable for `winit`'s
    /// [`LogicalSize`]. Always at least `1x1`.
    pub fn window_logical_size(&self) -> (u32, u32) {
        let (w, h) = self.cell_metrics().surface_size(self.initial_grid);
        let pad = self.window_padding_px.max(0.0) * 2.0;
        (
            (w + pad).ceil().max(1.0) as u32,
            (h + pad).ceil().max(1.0) as u32,
        )
    }
}
