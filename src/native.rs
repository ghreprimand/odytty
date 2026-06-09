//! Native window + GPU surface (Linux-first, Wayland-native).
//!
//! This module owns the seam between the OS window/event loop, the GPU surface,
//! and the rest of OdyTTY. It is built up incrementally so each piece is
//! reviewable on its own:
//!
//! - **Window lifecycle** — a `winit` window that opens and closes cleanly.
//! - **GPU surface bring-up** — a `wgpu` surface/device/queue that survives
//!   resize.
//! - **Glyph text rendering** — the `crate::text` atlas is uploaded to an
//!   `R8Unorm` texture and the `crate::grid` geometry is drawn as textured
//!   quads with the shared `cell.wgsl` pipeline, so the window shows readable
//!   monospaced text.
//!
//! The surface is still cleared to a neutral placeholder color before the cell
//! geometry is drawn over it; that clear is **not** the theme system, which
//! lands later as a disableable layer.
//!
//! Still deliberately absent this packet: PTY wiring, keyboard input, and the
//! Odyssey visual/theme layer. The text shown is a *static seeded snapshot*
//! (see [`GpuState::new`]) driven through the real owned core so colors/SGR
//! exercise the real path; live PTY output replaces it in the next packet.
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
//!   (`crate::text`) uploaded to a `wgpu` texture; cells drawn as textured
//!   quads. *(this packet)*
//! - **Grid presentation** — maps an owned-core `Snapshot` to positioned cell
//!   quads via `crate::grid`, with no terminal semantics in the renderer.
//!   *(this packet)*
//!
//! ## Linux / Wayland
//!
//! `winit` compiles in both Wayland and X11 backends and selects Wayland at
//! runtime when `WAYLAND_DISPLAY` is set, so under Hyprland this is a native
//! Wayland surface (no XWayland). `wgpu` presents to that surface via its
//! default backends (Vulkan on Linux), so the GPU path is Wayland-native too.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wgpu::util::DeviceExt;

use crate::core::{Dimensions, Terminal};
use crate::grid::{self, Vertex};
use crate::render::CellMetrics;
use crate::text::{self, GlyphAtlas};

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
    /// Font loading or glyph-atlas setup for the text renderer failed.
    #[error("text setup failed: {0}")]
    Text(String),
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

/// Viewport uniform mirroring `Viewport` in `cell.wgsl`: physical surface size
/// in pixels plus padding to a 16-byte std140 slot.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewportUniform {
    size: [f32; 2],
    _pad: [f32; 2],
}

/// GPU surface state bound to a single window.
///
/// Owns the `wgpu` surface, device, queue, and surface configuration, plus the
/// text-rendering resources: the glyph atlas texture, the cell render pipeline,
/// its bind group, the viewport uniform, and the vertex buffer holding the
/// current snapshot's cell quads. The surface borrows the window for `'static`
/// by holding an `Arc<Window>`.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    viewport_buf: wgpu::Buffer,
    vertex_buf: wgpu::Buffer,
    vertex_count: u32,
    // Kept alive for the lifetime of the bind group; never read directly.
    _atlas_texture: wgpu::Texture,
    _atlas_sampler: wgpu::Sampler,
}

impl GpuState {
    /// Bring up the GPU surface for `window`.
    ///
    /// Synchronous from the caller's perspective: the async adapter/device
    /// requests are driven to completion with `pollster`, since `winit`'s
    /// handler callbacks are synchronous.
    fn new(window: Arc<Window>, options: &NativeOptions) -> Result<Self, NativeError> {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
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

        // --- Glyph atlas: rasterize at physical pixels for crisp HiDPI text.
        let font = text::load_font().map_err(|err| NativeError::Text(err.to_string()))?;
        let atlas = GlyphAtlas::build(&font, options.font_size_px * scale.max(1.0));

        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("odytty-atlas"),
            size: wgpu::Extent3d {
                width: atlas.width,
                height: atlas.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width),
                rows_per_image: Some(atlas.height),
            },
            wgpu::Extent3d {
                width: atlas.width,
                height: atlas.height,
                depth_or_array_layers: 1,
            },
        );
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Nearest + clamp: glyph cells map 1:1 to pixels, so no filtering.
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // --- Viewport uniform (physical surface size), updated on resize.
        let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("odytty-viewport"),
            contents: bytemuck::bytes_of(&ViewportUniform {
                size: [config.width as f32, config.height as f32],
                _pad: [0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // --- Bind group layout / group: uniform + atlas texture + sampler.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("odytty-cell-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("odytty-cell-bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: viewport_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
            ],
        });

        // --- Render pipeline from the shared cell shader.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("odytty-cell-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/cell.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("odytty-cell-pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let vertex_attrs = wgpu::vertex_attr_array![
            0 => Float32x2, // pos_px
            1 => Float32x2, // uv
            2 => Float32x4, // color
            3 => Float32,   // is_glyph
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("odytty-cell-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &vertex_attrs,
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // No culling: quad winding is not normalized in the geometry.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    // Straight-alpha blend so glyph coverage composites over the
                    // already-drawn background quad of the same cell.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        // --- Seeded demo snapshot (PLACEHOLDER: replaced by live PTY output in
        // the next packet). Driven through the real owned core so SGR/colors go
        // through the genuine parsing path rather than a hand-built Snapshot.
        let mut term = Terminal::new(options.initial_grid.columns, options.initial_grid.rows);
        term.advance(b"OdyTTY native renderer -- placeholder content\r\n");
        term.advance(b"PTY output, keyboard input, and themes land in later packets.\r\n");
        term.advance(b"\r\n");
        term.advance(
            b"\x1b[31mred \x1b[32mgreen \x1b[33myellow \x1b[34mblue \x1b[35mmagenta \x1b[36mcyan\x1b[0m\r\n",
        );
        term.advance(b"\x1b[1;37;44m bold white on blue \x1b[0m back to normal\r\n");
        term.advance(b"\x1b[7m inverse video sample \x1b[0m\r\n");
        let snapshot = term.snapshot();

        let vertices = grid::build_vertices(&snapshot, &atlas);
        let vertex_count = vertices.len() as u32;
        let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("odytty-cell-vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            viewport_buf,
            vertex_buf,
            vertex_count,
            _atlas_texture: atlas_texture,
            _atlas_sampler: atlas_sampler,
        })
    }

    /// Write the current physical surface size into the viewport uniform so the
    /// vertex shader maps pixel-space geometry to NDC correctly after a resize.
    fn update_viewport(&self) {
        self.queue.write_buffer(
            &self.viewport_buf,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [self.config.width as f32, self.config.height as f32],
                _pad: [0.0, 0.0],
            }),
        );
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
        // Geometry is pixel-space and stable across resize; only the viewport
        // uniform needs the new physical size.
        self.update_viewport();
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
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("odytty-cell-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Keep the neutral clear, then draw cell quads over it.
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if self.vertex_count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
                pass.draw(0..self.vertex_count, 0..1);
            }
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

        match GpuState::new(window.clone(), &self.options) {
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
