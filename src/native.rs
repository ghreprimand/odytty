//! Native window + GPU surface (Linux-first, Wayland-native).
//!
//! This module owns the seam between the OS window/event loop, the GPU surface,
//! and the rest of OdyTTY. It is built up incrementally so each piece is
//! reviewable on its own:
//!
//! - **Window lifecycle** — a `winit` window that opens and closes cleanly.
//! - **GPU surface bring-up** — a `wgpu` surface/device/queue that clears the
//!   window to a solid color each frame and survives resize.
//!
//! Still deliberately absent this packet: the glyph atlas / text renderer, PTY
//! wiring, keyboard input, and the Odyssey visual/theme layer. The solid clear
//! color here is a neutral placeholder that proves the GPU pipeline opens,
//! presents, and reconfigures on resize — it is **not** the theme system, which
//! lands later as a disableable layer.
//!
//! ## Ownership split (filled in incrementally)
//!
//! The native app keeps the owned terminal core (`crate::core`) separate from
//! windowing, GPU rendering, and any later Odyssey visual layer:
//!
//! - **Event loop** — `winit` owns the OS window, input events, and resize.
//!   *(done)*
//! - **GPU surface/device** — `wgpu` owns the surface, device, queue, and swap
//!   chain, presenting frames to the window. *(this packet: solid clear only)*
//! - **Glyph atlas / text renderer** — a CPU-rasterized monospace glyph atlas
//!   uploaded to a `wgpu` texture; cells drawn as textured quads. *(later)*
//! - **Grid presentation** — maps an owned-core `Snapshot` to positioned cells
//!   via `crate::render` metrics, with no terminal semantics in the renderer.
//!   *(later)*
//!
//! ## Linux / Wayland
//!
//! `winit` compiles in both Wayland and X11 backends and selects Wayland at
//! runtime when `WAYLAND_DISPLAY` is set, so under Hyprland this is a native
//! Wayland surface (no XWayland). `wgpu` presents to that surface via its
//! default backends (Vulkan on Linux), so the GPU path is Wayland-native too.

use std::sync::Arc;
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

/// Placeholder clear color for the window surface, in linear RGBA.
///
/// A neutral near-black so the GPU bring-up is visually obvious without
/// implying a theme. The real background comes from the Odyssey theme layer in
/// a later packet; this is intentionally not that.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.043,
    g: 0.047,
    b: 0.063,
    a: 1.0,
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

/// GPU surface state bound to a single window.
///
/// Owns the `wgpu` surface, device, queue, and surface configuration. This
/// packet only clears the surface to [`CLEAR_COLOR`]; the glyph renderer plugs
/// in at [`GpuState::render`] later. The surface borrows the window for
/// `'static` by holding an `Arc<Window>`.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl GpuState {
    /// Bring up the GPU surface for `window`.
    ///
    /// Synchronous from the caller's perspective: the async adapter/device
    /// requests are driven to completion with `pollster`, since `winit`'s
    /// handler callbacks are synchronous.
    fn new(window: Arc<Window>) -> Result<Self, NativeError> {
        let size = window.inner_size();
        // No display handle is supplied here: the default Linux backend is
        // Vulkan, which ignores it. (It only matters for GLES-on-Wayland.)
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());

        let surface = instance
            .create_surface(window)
            .map_err(|err| NativeError::SurfaceCreation(err.to_string()))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|err| NativeError::NoAdapter(err.to_string()))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("odytty-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|err| NativeError::DeviceRequest(err.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Fifo (vsync) is universally supported and avoids tearing; the
            // present mode can become a setting once frames carry real content.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
        })
    }

    /// Reconfigure the surface for a new physical size. No-op for zero extents
    /// (e.g. a minimized window), which the swap chain rejects.
    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Reapply the current configuration, used to recover a lost/outdated
    /// surface before the next frame.
    fn reconfigure(&self) {
        self.surface.configure(&self.device, &self.config);
    }

    /// Clear the surface to [`CLEAR_COLOR`] and present one frame.
    ///
    /// Returns a [`FrameOutcome`] so the event loop can decide whether to
    /// reconfigure the surface or simply skip the frame. `wgpu` 29 reports
    /// acquisition status through [`wgpu::CurrentSurfaceTexture`] rather than a
    /// `Result`, so there is no fatal out-of-memory path here.
    fn render(&mut self) -> FrameOutcome {
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            // Acquired, but the surface no longer matches: draw this frame, then
            // reconfigure for the next one.
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            // Surface changed/lost or a validation error: reconfigure and retry.
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Validation => return FrameOutcome::NeedsReconfigure,
            // Transient: drop this frame and try again later.
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return FrameOutcome::Skipped;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("odytty-clear-encoder"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("odytty-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        if suboptimal {
            FrameOutcome::NeedsReconfigure
        } else {
            FrameOutcome::Presented
        }
    }
}

/// What the event loop should do after a frame attempt.
enum FrameOutcome {
    /// A frame was presented successfully.
    Presented,
    /// The surface needs reconfiguring before the next frame.
    NeedsReconfigure,
    /// The frame was intentionally skipped (transient surface state).
    Skipped,
}

/// Application state driving the `winit` event loop.
///
/// The window is created lazily on `resumed` per `winit`'s portability
/// contract, and any startup failure is captured so it can be surfaced after
/// the loop returns.
struct App {
    options: NativeOptions,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    autoclose: Option<Duration>,
    deadline: Option<Instant>,
    startup_error: Option<NativeError>,
}

impl App {
    fn new(options: NativeOptions, autoclose: Option<Duration>) -> Self {
        Self {
            options,
            window: None,
            gpu: None,
            autoclose,
            deadline: None,
            startup_error: None,
        }
    }

    /// Record a fatal startup error and ask the loop to exit.
    fn fail(&mut self, event_loop: &ActiveEventLoop, err: NativeError) {
        self.startup_error = Some(err);
        event_loop.exit();
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

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                self.fail(event_loop, NativeError::WindowCreation(err.to_string()));
                return;
            }
        };

        match GpuState::new(window.clone()) {
            Ok(gpu) => self.gpu = Some(gpu),
            Err(err) => {
                self.fail(event_loop, err);
                return;
            }
        }

        window.request_redraw();
        self.window = Some(window);

        if let Some(delay) = self.autoclose {
            let deadline = Instant::now() + delay;
            self.deadline = Some(deadline);
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
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
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(gpu) = self.gpu.as_mut() else {
                    return;
                };
                match gpu.render() {
                    FrameOutcome::Presented | FrameOutcome::Skipped => {}
                    // Surface lost/outdated/suboptimal (e.g. after a resize or
                    // compositor change): reconfigure and try again next frame.
                    FrameOutcome::NeedsReconfigure => gpu.reconfigure(),
                }
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
/// Opens a real OS window sized to the requested grid, brings up a `wgpu`
/// surface, and runs the event loop until the window is closed (or, when
/// `ODYTTY_NATIVE_AUTOCLOSE_MS` is set, the auto-close deadline elapses). The
/// surface is cleared to a neutral placeholder color each frame; readable text
/// rendering lands in a later packet. Returns once the window has closed
/// cleanly.
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

    #[test]
    fn clear_color_is_opaque() {
        assert_eq!(CLEAR_COLOR.a, 1.0);
    }
}
