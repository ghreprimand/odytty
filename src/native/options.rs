// SPDX-License-Identifier: GPL-3.0-only
use std::ffi::OsString;
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

/// Command to execute as the initial PTY child instead of the user's shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
}

/// Initial native window and text assumptions.
///
/// These are the documented starting defaults for the Linux-first prototype.
/// Appearance/effect defaults live in [`Settings`]; this type carries the
/// window/text subset needed to bootstrap the native renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeOptions {
    /// Window title.
    pub title: String,
    /// Directory used for the initial shell/command.
    pub working_directory: Option<PathBuf>,
    /// Optional command to exec as the initial PTY child.
    pub command: Option<NativeCommand>,
    /// Initial terminal grid size in columns/rows.
    pub initial_grid: Dimensions,
    /// Monospace font family request. Defaults to bundled Victor Mono unless
    /// `font_path` points at a direct file or settings request another family.
    pub font_family: String,
    /// Optional weight-variant suffix appended to `font_family` to select a
    /// lighter or heavier base face (RV7). Empty (the default) loads the
    /// family's regular face exactly as before. Bold/italic discovery always
    /// uses the plain `font_family`, so SGR bold stays distinct.
    pub font_weight: String,
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
    /// Optional detached session id to attach to at startup (Phase 2). `None`
    /// (the default) is the normal local-shell launch and leaves the startup path
    /// byte-identical. When `Some(id)`, the window still opens its normal initial
    /// local session, then attaches the hosted session as a live tab and focuses
    /// it — the `odytty attach <id>` entry the CLI sets.
    pub attach_session: Option<String>,
    /// Whether this process was launched as a bare `odytty` with no CLI
    /// arguments (WP2 restore-on-launch, sub-ODP 8b). Only a bare launch is
    /// eligible to restore the previous workspace shape; any argument (a flag,
    /// `-e`, a working-directory, an attach id) sets this `false` and suppresses
    /// restore for that launch. `false` by default so every non-bare
    /// construction path (attach, tests, config reload) is restore-inert.
    pub bare_launch: bool,
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            title: "OdyTTY".to_owned(),
            working_directory: None,
            command: None,
            initial_grid: Dimensions::new(80, 24),
            font_family: crate::text::BUNDLED_FONT_FAMILY.to_owned(),
            font_weight: String::new(),
            font_path: None,
            font_size_px: DEFAULT_FONT_SIZE_PX,
            text_gamma: crate::settings::DEFAULT_TEXT_GAMMA,
            subpixel: SubpixelMode::Off,
            window_padding_px: DEFAULT_WINDOW_PADDING_PX,
            line_height: DEFAULT_LINE_HEIGHT,
            box_thickness: DEFAULT_BOX_THICKNESS,
            attach_session: None,
            bare_launch: false,
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
            font_weight: settings.font_weight.clone(),
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
