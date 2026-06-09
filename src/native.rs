//! Native window lifecycle (Linux-first).
//!
//! This module owns the seam between the OS window/event loop and the rest of
//! OdyTTY. It is intentionally narrow: this packet brings up a real `winit`
//! window that opens and closes cleanly, and nothing else. GPU surface setup
//! (`wgpu`), the glyph atlas / text renderer, PTY wiring, input mapping, and the
//! Odyssey visual layer all land in later packets so each can be reviewed on its
//! own.
//!
//! ## Ownership split (filled in incrementally)
//!
//! The native app keeps the owned terminal core (`crate::core`) separate from
//! windowing, GPU rendering, and any later Odyssey visual layer:
//!
//! - **Event loop** — `winit` owns the OS window, input events, and resize.
//!   *(this packet)*
//! - **GPU surface/device** — `wgpu` will own the surface, device, queue, and
//!   swap chain. *(later)*
//! - **Glyph atlas / text renderer** — a CPU-rasterized monospace glyph atlas
//!   uploaded to a `wgpu` texture; cells drawn as textured quads. *(later)*
//! - **Grid presentation** — maps an owned-core `Snapshot` to positioned cells
//!   via `crate::render` metrics, with no terminal semantics in the renderer.
//!   *(later)*
//!
//! Because there is no renderer yet, the window's surface contents are
//! undefined this packet; the milestone here is purely a clean open/close
//! lifecycle. The window is sized from the requested grid using approximate
//! monospace cell metrics so the dimensions are realistic ahead of real text
//! layout.

use std::time::{Duration, Instant};

use crate::core::Dimensions;
use crate::render::CellMetrics;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Environment variable that, when set to a positive integer of milliseconds,
/// makes the native window auto-close after that delay. This exists so the
/// open/close lifecycle can be exercised non-interactively (smoke checks, CI)
/// without a human closing the window. It is a development affordance, not a
/// product setting.
const AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";

/// Errors from the native app path.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// The OS event loop could not be created or failed while running.
    #[error("native event loop error: {0}")]
    EventLoop(String),
    /// The OS window could not be created.
    #[error("native window creation failed: {0}")]
    WindowCreation(String),
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
    /// Font size in logical pixels.
    pub font_size_px: f32,
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            title: "OdyTTY".to_owned(),
            initial_grid: Dimensions::new(80, 24),
            font_family: "monospace".to_owned(),
            font_size_px: 14.0,
        }
    }
}

impl NativeOptions {
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
        (w.ceil().max(1.0) as u32, h.ceil().max(1.0) as u32)
    }
}

/// Parse the auto-close delay from the environment, if present and valid.
///
/// Returns `None` when the variable is unset, empty, non-numeric, or `0`.
fn autoclose_from_env() -> Option<Duration> {
    let raw = std::env::var(AUTOCLOSE_ENV).ok()?;
    let ms: u64 = raw.trim().parse().ok()?;
    if ms == 0 {
        None
    } else {
        Some(Duration::from_millis(ms))
    }
}

/// Application state driving the `winit` event loop.
///
/// Holds only what the open/close lifecycle needs. The window is created lazily
/// on `resumed` per `winit`'s portability contract, and any startup failure is
/// captured so it can be surfaced after the loop returns.
struct App {
    options: NativeOptions,
    window: Option<Window>,
    autoclose: Option<Duration>,
    deadline: Option<Instant>,
    startup_error: Option<NativeError>,
}

impl App {
    fn new(options: NativeOptions, autoclose: Option<Duration>) -> Self {
        Self {
            options,
            window: None,
            autoclose,
            deadline: None,
            startup_error: None,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let (w, h) = self.options.window_logical_size();
        let attributes = Window::default_attributes()
            .with_title(self.options.title.clone())
            .with_inner_size(LogicalSize::new(w, h));

        match event_loop.create_window(attributes) {
            Ok(window) => {
                window.request_redraw();
                self.window = Some(window);
                if let Some(delay) = self.autoclose {
                    let deadline = Instant::now() + delay;
                    self.deadline = Some(deadline);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            Err(err) => {
                self.startup_error = Some(NativeError::WindowCreation(err.to_string()));
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                // No renderer yet: the GPU surface and text rendering arrive in
                // later packets. Nothing to present this packet.
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(deadline) = self.deadline
            && Instant::now() >= deadline
        {
            event_loop.exit();
        }
    }
}

/// Entry point for the native app.
///
/// Opens a real OS window sized to the requested grid and runs the event loop
/// until the window is closed (or, when `ODYTTY_NATIVE_AUTOCLOSE_MS` is set, the
/// auto-close deadline elapses). Returns once the window has closed cleanly.
pub fn run_native(options: NativeOptions) -> Result<(), NativeError> {
    let event_loop = EventLoop::new().map_err(|err| NativeError::EventLoop(err.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(options, autoclose_from_env());
    event_loop
        .run_app(&mut app)
        .map_err(|err| NativeError::EventLoop(err.to_string()))?;

    if let Some(err) = app.startup_error {
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_linux_first_monospace() {
        let options = NativeOptions::default();
        assert_eq!(options.initial_grid, Dimensions::new(80, 24));
        assert_eq!(options.font_family, "monospace");
        assert!(options.font_size_px > 0.0);
        assert_eq!(options.title, "OdyTTY");
    }

    #[test]
    fn cell_metrics_scale_with_font_size() {
        let options = NativeOptions {
            font_size_px: 20.0,
            ..NativeOptions::default()
        };
        let metrics = options.cell_metrics();
        assert_eq!(metrics.width_px, 12.0);
        assert_eq!(metrics.height_px, 24.0);
    }

    #[test]
    fn window_size_covers_the_grid() {
        let options = NativeOptions {
            initial_grid: Dimensions::new(80, 24),
            font_size_px: 10.0,
            ..NativeOptions::default()
        };
        // 80 cols * (10 * 0.6) = 480 ; 24 rows * (10 * 1.2) = 288
        assert_eq!(options.window_logical_size(), (480, 288));
    }

    #[test]
    fn window_size_is_never_zero() {
        let options = NativeOptions {
            initial_grid: Dimensions::new(1, 1),
            font_size_px: 0.1,
            ..NativeOptions::default()
        };
        let (w, h) = options.window_logical_size();
        assert!(w >= 1 && h >= 1);
    }
}
