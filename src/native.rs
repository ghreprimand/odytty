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
//! The surface is cleared to the active theme's clear color before the cell
//! geometry is drawn over it. The theme (selected by `ODYTTY_THEME`, defaulting
//! to the plain baseline) also sets the default foreground/background used for
//! `Color::Default` cells. Themes are presentation-only: they never touch the
//! terminal core or cell attributes (see [`crate::theme`]).
//!
//! The window now opens a real shell: PTY output is rendered live and keyboard
//! input is encoded and written back to the PTY (via the shared
//! [`crate::input`] encoder), so the read+write loop is complete. Still
//! deliberately absent: richer Odyssey visual treatments (motion/effects) and
//! workflow polish beyond the first daily-loop basics.
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

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wgpu::util::DeviceExt;

use arboard::Clipboard;

use crate::core::{Dimensions, Snapshot, Terminal};
use crate::grid::{self, Vertex};
use crate::input::{self, Key, Modifiers};
use crate::pty::PtySession;
use crate::render::CellMetrics;
use crate::selection::{self, CellPoint, SelectionState};
use crate::text::{self, CellSize, GlyphAtlas};
use crate::theme::{Theme, VisualEffect};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, NamedKey};
use winit::window::{Window, WindowId};

/// Environment variable that, when set to a positive integer of milliseconds,
/// makes the native window auto-close after that delay. This exists so the
/// open/close lifecycle can be exercised non-interactively (smoke checks, CI)
/// without a human closing the window. It is a development affordance, not a
/// product setting.
const AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";

/// Convert a theme's clear color (sRGB bytes) into a linear-RGBA `wgpu::Color`.
///
/// The surface format is sRGB, which applies the linear→sRGB transfer on write,
/// so the clear value must be linear — same conversion the glyph/cell colors use
/// (via [`text::srgb_to_linear`]), keeping the surround and the cell backgrounds
/// in the same color space.
fn theme_clear_color(theme: &Theme) -> wgpu::Color {
    let (r, g, b) = theme.clear;
    wgpu::Color {
        r: text::srgb_to_linear(r) as f64,
        g: text::srgb_to_linear(g) as f64,
        b: text::srgb_to_linear(b) as f64,
        a: 1.0,
    }
}

/// Pack a [`VisualEffect`] into the shader uniform's `effect` slot:
/// `[scanline_strength, scanline_period_px]`. When off, strength is `0.0`, which
/// makes the shader's scanline term vanish (pixel-identical to no effect).
fn effect_params(visual: VisualEffect) -> [f32; 2] {
    [visual.scanline_strength(), visual.scanline_period_px()]
}

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

/// Events the PTY pump thread sends to wake the `winit` event loop.
///
/// The loop otherwise sleeps (`ControlFlow::Wait`) with no input wired this
/// packet, so these proxy events are what drive redraws as shell output
/// arrives and what signals a clean exit when the shell ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserEvent {
    /// New PTY output landed in the shared terminal; rebuild + redraw.
    Redraw,
    /// The shell's PTY reached EOF (shell exited): exit the loop.
    ShellExited,
}

/// The single PTY master writer, shared behind a lock.
///
/// `portable-pty`'s `take_writer` yields the writer once, so it is wrapped here
/// and shared: the pump thread uses it to send host responses (query replies),
/// and the App uses its clone to send encoded keystrokes — both write to the
/// single PTY master.
type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

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

/// Compute the terminal grid (columns × rows) that fits a physical surface.
///
/// `width_px`/`height_px` are the window's physical pixel size (what `winit`
/// reports in `WindowEvent::Resized`) and `cell` is the rasterized per-cell
/// pixel metric from the glyph atlas — the *same* metric the grid geometry uses
/// — so the fit matches what is actually drawn. Integer floor division gives
/// the number of whole cells that fit; both axes are clamped to at least one so
/// a sliver-sized or minimized window can never produce a zero-dimension grid.
/// Cell extents are defensively clamped to `>= 1` to avoid division by zero.
fn grid_dimensions_for(width_px: u32, height_px: u32, cell: CellSize) -> Dimensions {
    let cols = width_px / cell.width.max(1);
    let rows = height_px / cell.height.max(1);
    Dimensions::new(cols as usize, rows as usize)
}

/// Viewport uniform mirroring `Viewport` in `cell.wgsl`: physical surface size
/// in pixels plus the ambient-effect params (strength, period_px) filling the
/// 16-byte std140 slot. `effect` is `[0.0, _]` when the visual treatment is off,
/// which makes the shader a no-op.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewportUniform {
    size: [f32; 2],
    effect: [f32; 2],
}

/// Retains a platform clipboard handle across copy/paste operations.
///
/// On Linux, clipboard contents are commonly served by the application that
/// last set them. Dropping the `Clipboard` immediately after `set_text` can make
/// copied text disappear before the compositor or clipboard manager has had a
/// chance to request it, so the native app keeps the handle alive for its
/// lifetime.
#[derive(Debug)]
struct ClipboardSlot<T> {
    handle: Option<T>,
}

impl<T> ClipboardSlot<T> {
    fn new() -> Self {
        Self { handle: None }
    }

    fn get_or_try_init<E>(&mut self, create: impl FnOnce() -> Result<T, E>) -> Result<&mut T, E> {
        if self.handle.is_none() {
            self.handle = Some(create()?);
        }

        Ok(self.handle.as_mut().expect("clipboard handle initialized"))
    }

    fn clear(&mut self) {
        self.handle = None;
    }

    #[cfg(test)]
    fn is_retaining_handle(&self) -> bool {
        self.handle.is_some()
    }
}

impl<T> Default for ClipboardSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct NativeClipboard {
    slot: ClipboardSlot<Clipboard>,
}

impl NativeClipboard {
    fn read_text(&mut self) -> Option<String> {
        let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
            Ok(clipboard) => clipboard,
            Err(err) => {
                eprintln!("odytty: clipboard unavailable for paste: {err}");
                return None;
            }
        };

        match clipboard.get_text() {
            Ok(text) => Some(text),
            Err(err) => {
                eprintln!("odytty: clipboard paste failed: {err}");
                self.slot.clear();
                None
            }
        }
    }

    fn write_text(&mut self, text: &str) -> Option<()> {
        let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
            Ok(clipboard) => clipboard,
            Err(err) => {
                eprintln!("odytty: clipboard unavailable for copy: {err}");
                return None;
            }
        };

        match clipboard.set_text(text.to_owned()) {
            Ok(()) => Some(()),
            Err(err) => {
                eprintln!("odytty: clipboard copy failed: {err}");
                self.slot.clear();
                None
            }
        }
    }
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
    /// The glyph atlas, kept so vertices can be rebuilt from new snapshots as
    /// live PTY output arrives.
    atlas: GlyphAtlas,
    /// Surface clear color from the active theme (linear RGBA).
    clear_color: wgpu::Color,
    /// Ambient-effect uniform params `[strength, period_px]` ([0,_] == off).
    /// Re-written into the viewport uniform on every resize/reconfigure.
    effect: [f32; 2],
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
    fn new(
        window: Arc<Window>,
        options: &NativeOptions,
        initial_snapshot: &Snapshot,
        theme: Theme,
        visual: VisualEffect,
    ) -> Result<Self, NativeError> {
        let effect = effect_params(visual);
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
                effect,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // --- Bind group layout / group: uniform + atlas texture + sampler.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("odytty-cell-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // VERTEX uses size for NDC mapping; FRAGMENT reads the
                    // effect params for the ambient scanline wash.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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

        // Build the first vertex buffer from the initial (blank) snapshot. Live
        // PTY output replaces this content via `update_from_snapshot` as the
        // pump thread advances the shared terminal. A >=1x1 grid always emits at
        // least one background quad, so this buffer is never zero-sized.
        let vertices = grid::build_vertices(initial_snapshot, &atlas);
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
            atlas,
            clear_color: theme_clear_color(&theme),
            effect,
            _atlas_texture: atlas_texture,
            _atlas_sampler: atlas_sampler,
        })
    }

    /// Rebuild the cell vertex buffer from a fresh terminal snapshot.
    ///
    /// Called on the UI thread after the pump thread signals new PTY output.
    /// The grid is small (e.g. 80×24 → a few thousand vertices), so recreating
    /// the buffer per coalesced update is cheap and avoids tracking capacity.
    /// The caller must already hold the snapshot by value — the terminal mutex
    /// is dropped before this runs so the lock is never held across GPU calls.
    fn update_from_snapshot(&mut self, snapshot: &Snapshot) {
        let vertices = grid::build_vertices(snapshot, &self.atlas);
        self.vertex_count = vertices.len() as u32;
        self.vertex_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("odytty-cell-vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
    }

    /// Write the current physical surface size into the viewport uniform so the
    /// vertex shader maps pixel-space geometry to NDC correctly after a resize.
    fn update_viewport(&self) {
        self.queue.write_buffer(
            &self.viewport_buf,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [self.config.width as f32, self.config.height as f32],
                effect: self.effect,
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

    /// Clear the surface to the active theme's clear color and present one frame.
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
                        load: wgpu::LoadOp::Clear(self.clear_color),
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

/// How many rows a single mouse-wheel notch scrolls the viewport.
const WHEEL_STEP_LINES: usize = 3;

/// Native-side scrollback viewport state.
///
/// Tracks how many rows the rendered viewport is paged upward from the live
/// bottom (`offset == 0` is the live screen). Every mutation clamps against the
/// supplied scrollback length, so the offset can never address rows that do not
/// exist. The core `snapshot_with_scrollback` clamps too; tracking the bound
/// here keeps the UX honest (no "dead" scrolling past the oldest row) and lets
/// the offset logic be unit-tested without a GPU/window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Viewport {
    offset: usize,
}

impl Viewport {
    fn offset(self) -> usize {
        self.offset
    }

    /// Whether the viewport is at the live bottom (offset 0).
    fn is_live(self) -> bool {
        self.offset == 0
    }

    /// Page `lines` rows upward into history, clamped to `scrollback_len`.
    /// Returns whether the offset changed.
    fn scroll_up(&mut self, lines: usize, scrollback_len: usize) -> bool {
        let next = self.offset.saturating_add(lines).min(scrollback_len);
        self.set(next)
    }

    /// Page `lines` rows downward toward the live bottom. Returns whether the
    /// offset changed.
    fn scroll_down(&mut self, lines: usize) -> bool {
        let next = self.offset.saturating_sub(lines);
        self.set(next)
    }

    /// Snap back to the live bottom. Returns whether the offset changed.
    fn reset_to_live(&mut self) -> bool {
        self.set(0)
    }

    /// Keep the scrolled-back view anchored to the same absolute rows when
    /// `added` new rows entered scrollback. This is the "stay scrolled" policy:
    /// fresh PTY output does not scroll the user's view out from under them.
    /// No-op at the live bottom (where new output should appear immediately).
    fn anchor_after_growth(&mut self, added: usize, scrollback_len: usize) {
        if self.offset == 0 || added == 0 {
            return;
        }
        self.offset = self.offset.saturating_add(added).min(scrollback_len);
    }

    /// Re-clamp after the available history may have shrunk (resize reflow,
    /// alternate-screen entry clearing primary scrollback).
    fn clamp(&mut self, scrollback_len: usize) {
        self.offset = self.offset.min(scrollback_len);
    }

    fn set(&mut self, next: usize) -> bool {
        if next == self.offset {
            return false;
        }
        self.offset = next;
        true
    }
}

/// Convert a mouse-wheel delta into a signed row count: positive scrolls up
/// into history, negative scrolls toward the live bottom. Line deltas map each
/// notch to [`WHEEL_STEP_LINES`] rows; pixel deltas convert by the cell height.
fn wheel_lines(delta: MouseScrollDelta, cell_height: u32) -> isize {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => {
            if y == 0.0 {
                return 0;
            }
            let notches = y.abs().ceil().max(1.0) as isize;
            y.signum() as isize * notches * WHEEL_STEP_LINES as isize
        }
        MouseScrollDelta::PixelDelta(pos) => {
            let height = (cell_height.max(1)) as f64;
            (pos.y / height).round() as isize
        }
    }
}

/// Application state driving the `winit` event loop.
///
/// The window is created lazily on `resumed` per `winit`'s portability
/// contract, and any startup failure is captured so it can be surfaced after
/// the loop returns.
struct App {
    options: NativeOptions,
    /// Active presentation theme (selected once from `ODYTTY_THEME`). Used for
    /// the surface clear color; the default cell colors are applied process-wide
    /// at startup via `text::set_default_colors`. Presentation-only.
    theme: Theme,
    /// Active optional visual treatment (selected once from `ODYTTY_VISUAL`,
    /// default off). Drives the ambient scanline uniform; presentation-only and
    /// fully disableable. The core never sees it.
    visual: VisualEffect,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    /// The terminal model shared with the PTY pump thread. Snapshots are taken
    /// under this lock on the UI thread, then the lock is dropped before any GPU
    /// work so it is never held across `wgpu` calls.
    terminal: Arc<Mutex<Terminal>>,
    /// Set when the pump thread reports new output; the next `RedrawRequested`
    /// rebuilds the vertex buffer once (coalescing many wakes into one rebuild).
    needs_rebuild: bool,
    /// The shared PTY writer. Key presses are encoded to bytes and written here,
    /// completing the read+write loop with the pump thread that owns the reader.
    writer: PtyWriter,
    /// The shared PTY session, used to push the new window size to the kernel
    /// (`TIOCSWINSZ`) on resize so shell/TUI programs see the updated `$COLUMNS`
    /// and `$LINES`. Shared with `run_native`, which reaps the child on exit.
    pty: Arc<Mutex<PtySession>>,
    /// The terminal grid size last applied to the model and PTY. Tracked so a
    /// `Resized` event that does not change the whole-cell grid skips redundant
    /// model/PTY resize work (idempotence): only surface reconfigure runs.
    grid: Dimensions,
    /// Latest modifier state, tracked across `ModifiersChanged` events so a key
    /// press can be encoded with the Ctrl/Alt/Shift held at press time. `winit`
    /// delivers modifier changes separately from key events, so this must be
    /// remembered rather than read off each `KeyboardInput`.
    modifiers: Modifiers,
    /// Current visible-cell selection. Native owns this UI state; the terminal
    /// core remains unaware of selections and clipboard operations.
    selection: SelectionState,
    /// Most recent pointer position mapped to a terminal cell. `winit` mouse
    /// button events do not carry coordinates, so press/release use this cached
    /// cell from the latest cursor movement.
    pointer_cell: Option<CellPoint>,
    /// Whether the left mouse button is currently extending a selection.
    selecting: bool,
    /// Scrollback viewport offset (0 == live). Mouse wheel and Shift+PageUp/
    /// PageDown move it; any PTY-bound input snaps it back to live.
    viewport: Viewport,
    /// Scrollback length observed at the last rebuild, used to anchor the
    /// scrolled-back view as new output grows scrollback (the "stay scrolled"
    /// policy in [`Viewport::anchor_after_growth`]).
    last_scrollback_len: usize,
    /// Native-side clipboard owner. Kept alive across copy/paste operations so
    /// Linux clipboard contents remain served after Ctrl+Shift+C.
    clipboard: NativeClipboard,
    autoclose: Option<Duration>,
    deadline: Option<Instant>,
    startup_error: Option<NativeError>,
}

impl App {
    fn new(
        options: NativeOptions,
        theme: Theme,
        visual: VisualEffect,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
        autoclose: Option<Duration>,
    ) -> Self {
        let grid = options.initial_grid;
        Self {
            options,
            theme,
            visual,
            window: None,
            gpu: None,
            terminal,
            needs_rebuild: true,
            writer,
            pty,
            grid,
            modifiers: Modifiers::default(),
            selection: SelectionState::default(),
            pointer_cell: None,
            selecting: false,
            viewport: Viewport::default(),
            last_scrollback_len: 0,
            clipboard: NativeClipboard::default(),
            autoclose,
            deadline: None,
            startup_error: None,
        }
    }

    /// Resize the terminal model and PTY to fit the new physical surface size.
    ///
    /// Idempotent: when the computed whole-cell grid is unchanged (a sub-cell
    /// pixel change, or a duplicate event), no model or PTY resize is performed
    /// and `false` is returned. The GPU surface itself is reconfigured by the
    /// caller regardless, since it tracks pixel size, not the cell grid.
    ///
    /// Lock scopes are kept tight and never nested: the terminal mutex is taken
    /// and dropped for the model resize, then the PTY mutex is taken and dropped
    /// for the (non-blocking) `TIOCSWINSZ`. Neither is held across the other or
    /// across any GPU call.
    fn resize_grid(&mut self, cell: CellSize, width_px: u32, height_px: u32) -> bool {
        let new_grid = grid_dimensions_for(width_px, height_px, cell);
        if new_grid == self.grid {
            return false;
        }
        self.grid = new_grid;

        if let Ok(mut terminal) = self.terminal.lock() {
            terminal.resize(new_grid.columns, new_grid.rows);
        }
        if let Ok(pty) = self.pty.lock() {
            let _ = pty.resize(new_grid);
        }
        true
    }

    /// Record a fatal startup error and ask the loop to exit.
    fn fail(&mut self, event_loop: &ActiveEventLoop, err: NativeError) {
        self.startup_error = Some(err);
        event_loop.exit();
    }

    /// Encode a pressed key and write its bytes to the PTY.
    ///
    /// Maps the `winit` logical key (plus the cached [`Modifiers`]) onto the
    /// neutral [`Key`] model and defers byte production to the shared
    /// [`input::encode_key`], so the native and crossterm front ends emit
    /// identical sequences. Keys the prototype does not encode are dropped. The
    /// PTY writer is flushed after each write so the keystroke reaches the shell
    /// without buffering latency.
    fn handle_key_press(&mut self, logical: WinitKey) {
        let mods = self.modifiers;
        if is_copy_shortcut(&logical, mods) {
            self.handle_copy_shortcut();
            return;
        }
        if is_paste_shortcut(&logical, mods) {
            self.handle_paste_shortcut();
            return;
        }
        // Shift+PageUp/PageDown drive the scrollback viewport rather than the
        // PTY, so plain (unmodified) PageUp/PageDown still reach the shell/TUI.
        if is_scroll_up_key(&logical, mods) {
            self.scroll_viewport(self.page_lines() as isize);
            return;
        }
        if is_scroll_down_key(&logical, mods) {
            self.scroll_viewport(-(self.page_lines() as isize));
            return;
        }

        let mut bytes = Vec::new();
        match logical {
            // `Key::Character` may carry more than one char (composed input);
            // encode each so multi-char text still reaches the shell intact.
            WinitKey::Character(text) => {
                for ch in text.chars() {
                    bytes.extend_from_slice(&input::encode_key(Key::Char(ch), mods));
                }
            }
            WinitKey::Named(named) => {
                if let Some(key) = map_named_key(named, mods.shift) {
                    bytes = input::encode_key(key, mods);
                }
            }
            // Dead keys / unidentified: nothing to send.
            _ => {}
        }

        if bytes.is_empty() {
            return;
        }
        // Any keystroke that reaches the shell snaps the viewport back to live,
        // so typing always returns to the prompt at the bottom.
        self.return_to_live();
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    }

    /// Paste clipboard text into the PTY if the platform clipboard is
    /// reachable. Clipboard failures are deliberately non-fatal: a terminal
    /// should keep running even when the compositor denies clipboard access.
    fn handle_paste_shortcut(&mut self) {
        let Some(text) = self.clipboard.read_text() else {
            return;
        };
        // Paste writes to the PTY, so treat it like typed input: return to live.
        self.return_to_live();
        let _ = write_paste_text(&self.terminal, &self.writer, &text);
    }

    /// Copy the current visible selection to the clipboard. This is kept fully
    /// native-side: the selected text is derived from a snapshot copy and no
    /// terminal state is mutated.
    fn handle_copy_shortcut(&mut self) {
        let Some(range) = self.selection.range() else {
            return;
        };
        let text = {
            let snapshot = self.terminal.lock().expect("terminal mutex").snapshot();
            selection::selected_text(&snapshot, range)
        };
        if text.is_empty() {
            return;
        }
        let _ = self.clipboard.write_text(&text);
    }

    fn update_pointer_cell(&mut self, x_px: f64, y_px: f64) {
        let Some(cell) = self.gpu.as_ref().map(|gpu| gpu.atlas.cell) else {
            return;
        };
        let point = selection::cell_at_physical(x_px, y_px, cell, self.grid);
        self.pointer_cell = Some(point);
        if self.selecting {
            self.selection.update(point);
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    fn begin_selection(&mut self) {
        let Some(point) = self.pointer_cell else {
            return;
        };
        self.selection.begin(point);
        self.selecting = true;
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn finish_selection(&mut self) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Number of rows a Shift+PageUp/PageDown press scrolls: one screenful less
    /// one row of overlap for continuity (at least one row).
    fn page_lines(&self) -> usize {
        self.grid.rows.saturating_sub(1).max(1)
    }

    /// Current scrollback length from the shared model (0 if the lock is
    /// poisoned), used to clamp upward scrolling.
    fn scrollback_len(&self) -> usize {
        self.terminal
            .lock()
            .map(|t| t.screen().scrollback_len())
            .unwrap_or(0)
    }

    /// Adjust the scrollback viewport. `delta > 0` pages up into history,
    /// `delta < 0` pages toward the live bottom. A change clears any selection
    /// (visible-grid ranges are ambiguous across a viewport shift) and asks for
    /// a redraw; with no scrollback this is a clamped no-op (never panics).
    fn scroll_viewport(&mut self, delta: isize) {
        let changed = match delta.cmp(&0) {
            std::cmp::Ordering::Greater => self
                .viewport
                .scroll_up(delta as usize, self.scrollback_len()),
            std::cmp::Ordering::Less => self.viewport.scroll_down((-delta) as usize),
            std::cmp::Ordering::Equal => false,
        };
        if changed {
            self.on_viewport_changed();
        }
    }

    /// Return the viewport to the live bottom (offset 0). Called whenever input
    /// is written to the PTY so typing always jumps back to the prompt.
    fn return_to_live(&mut self) {
        if self.viewport.reset_to_live() {
            self.on_viewport_changed();
        }
    }

    /// Shared side effects of a viewport offset change: drop any selection,
    /// cancel an in-progress drag, and request one rebuild/redraw.
    fn on_viewport_changed(&mut self) {
        self.selection.clear();
        self.selecting = false;
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn is_copy_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    if !(mods.ctrl && mods.shift) || mods.alt {
        return false;
    }

    matches!(logical, WinitKey::Character(text) if text.eq_ignore_ascii_case("c"))
}

fn is_paste_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    if !(mods.ctrl && mods.shift) || mods.alt {
        return false;
    }

    matches!(logical, WinitKey::Character(text) if text.eq_ignore_ascii_case("v"))
}

/// Shift+PageUp pages the scrollback viewport upward. Shift only (no Ctrl/Alt)
/// so plain PageUp still reaches the PTY.
fn is_scroll_up_key(logical: &WinitKey, mods: Modifiers) -> bool {
    mods.shift && !mods.ctrl && !mods.alt && matches!(logical, WinitKey::Named(NamedKey::PageUp))
}

/// Shift+PageDown pages the scrollback viewport toward the live bottom.
fn is_scroll_down_key(logical: &WinitKey, mods: Modifiers) -> bool {
    mods.shift && !mods.ctrl && !mods.alt && matches!(logical, WinitKey::Named(NamedKey::PageDown))
}

fn write_paste_text(
    terminal: &Arc<Mutex<Terminal>>,
    writer: &PtyWriter,
    text: &str,
) -> std::io::Result<()> {
    let bracketed_paste = terminal
        .lock()
        .map(|terminal| terminal.bracketed_paste_enabled())
        .unwrap_or(false);
    let bytes = input::encode_paste(text, bracketed_paste);

    if let Ok(mut writer) = writer.lock() {
        writer.write_all(&bytes)?;
        writer.flush()?;
    }
    Ok(())
}

/// Translate a `winit` [`NamedKey`] into the neutral [`Key`] model.
///
/// `shift` is consulted only to turn Tab into [`Key::BackTab`] (Shift-Tab),
/// matching how the crossterm front end distinguishes the two. `Space` is
/// mapped to [`Key::Char(' ')`] rather than a named key so Ctrl-Space encodes
/// to `NUL` via the shared encoder. Named keys the prototype does not handle
/// (function keys, media keys, etc.) return `None`.
fn map_named_key(named: NamedKey, shift: bool) -> Option<Key> {
    Some(match named {
        NamedKey::Enter => Key::Enter,
        NamedKey::Backspace => Key::Backspace,
        NamedKey::ArrowLeft => Key::Left,
        NamedKey::ArrowRight => Key::Right,
        NamedKey::ArrowUp => Key::Up,
        NamedKey::ArrowDown => Key::Down,
        NamedKey::Home => Key::Home,
        NamedKey::End => Key::End,
        NamedKey::PageUp => Key::PageUp,
        NamedKey::PageDown => Key::PageDown,
        NamedKey::Tab if shift => Key::BackTab,
        NamedKey::Tab => Key::Tab,
        NamedKey::Delete => Key::Delete,
        NamedKey::Insert => Key::Insert,
        NamedKey::Escape => Key::Esc,
        NamedKey::Space => Key::Char(' '),
        _ => return None,
    })
}

impl ApplicationHandler<UserEvent> for App {
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

        // Seed the first buffer from the current shared-terminal snapshot (any
        // PTY output already pumped is picked up by the first redraw below).
        let initial_snapshot = self.terminal.lock().expect("terminal mutex").snapshot();
        match GpuState::new(
            window.clone(),
            &self.options,
            &initial_snapshot,
            self.theme,
            self.visual,
        ) {
            Ok(gpu) => self.gpu = Some(gpu),
            Err(err) => {
                self.fail(event_loop, err);
                return;
            }
        }

        self.needs_rebuild = true;
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
                // Reconfigure the GPU surface (pixel size) and read the real
                // per-cell metric so the grid fit matches what is drawn.
                let cell = self.gpu.as_mut().map(|gpu| {
                    gpu.resize(size.width, size.height);
                    gpu.atlas.cell
                });
                // Resize the model + PTY only when the whole-cell grid changes.
                if let Some(cell) = cell
                    && self.resize_grid(cell, size.width, size.height)
                {
                    self.selection.clear();
                    self.selecting = false;
                    self.pointer_cell = None;
                    // Reflow changes the row/scrollback layout; return to the
                    // live bottom so the offset is never stale against the new
                    // geometry. clamp() in the rebuild guards bounds regardless.
                    self.viewport.reset_to_live();
                    self.needs_rebuild = true;
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                // Rebuild geometry at most once per redraw, no matter how many
                // pump wakes coalesced into this frame. Snapshot under the lock,
                // then drop it before touching the GPU.
                if self.needs_rebuild {
                    let mut snapshot = {
                        let terminal = self.terminal.lock().expect("terminal mutex");
                        let scrollback_len = terminal.screen().scrollback_len();
                        // "Stay scrolled": as new output grows scrollback while
                        // the user is scrolled back, anchor the view to the same
                        // absolute rows instead of letting it scroll away. Only
                        // explicit input (handle_key_press/paste) returns to live.
                        let added = scrollback_len.saturating_sub(self.last_scrollback_len);
                        self.viewport.anchor_after_growth(added, scrollback_len);
                        self.last_scrollback_len = scrollback_len;
                        self.viewport.clamp(scrollback_len);
                        terminal.snapshot_with_scrollback(self.viewport.offset())
                    };
                    // Selection highlighting only applies to the live viewport;
                    // a viewport change clears selection, so this is just
                    // belt-and-suspenders for the offset-0 case.
                    if self.viewport.is_live()
                        && let Some(range) = self.selection.range()
                    {
                        selection::apply_highlight(&mut snapshot, range);
                    }
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.update_from_snapshot(&snapshot);
                    }
                    self.needs_rebuild = false;
                }
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
            // `winit` reports modifier state separately from key presses; cache
            // it so the next `KeyboardInput` encodes with Ctrl/Alt/Shift held.
            WindowEvent::ModifiersChanged(state) => {
                let state = state.state();
                self.modifiers = Modifiers {
                    ctrl: state.control_key(),
                    alt: state.alt_key(),
                    shift: state.shift_key(),
                };
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.update_pointer_cell(position.x, position.y);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => self.begin_selection(),
                ElementState::Released => self.finish_selection(),
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.atlas.cell.height);
                let lines = wheel_lines(delta, cell_height);
                if lines != 0 {
                    self.scroll_viewport(lines);
                }
            }
            // Only act on key-down (ignore key-up). Repeats are kept: holding a
            // key should autorepeat into the shell like a real terminal.
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.handle_key_press(event.logical_key);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            // Coalesce: flag a rebuild and ask for one redraw. Many output
            // chunks between frames collapse into a single snapshot+rebuild
            // because `winit` merges redundant `request_redraw` calls and we
            // only rebuild when `needs_rebuild` is set.
            UserEvent::Redraw => {
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            // The shell exited (PTY EOF): close the window cleanly.
            UserEvent::ShellExited => event_loop.exit(),
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

/// Read loop that pumps PTY output into the shared terminal and wakes the UI.
///
/// Owns its own reader and writer clones of the PTY master: it advances the
/// shared [`Terminal`] with each chunk, writes back any host responses the core
/// produces (e.g. answers to cursor/device queries) so query-driven prompts do
/// not stall, and signals the event loop with [`UserEvent::Redraw`]. On EOF or
/// a read error (the shell exited) it signals [`UserEvent::ShellExited`] and
/// returns, ending the thread.
///
/// Redraw coalescing is intentionally simple: one wake per read chunk. The UI
/// side merges these into a single rebuild per presented frame, so a burst of
/// output never causes one rebuild per byte.
fn spawn_pty_pump(
    mut reader: Box<dyn Read + Send>,
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    proxy: EventLoopProxy<UserEvent>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = proxy.send_event(UserEvent::ShellExited);
                    break;
                }
                Ok(len) => {
                    let host_output = {
                        let mut term = terminal.lock().expect("terminal mutex");
                        term.advance(&buffer[..len]);
                        term.take_host_output()
                    };
                    if !host_output.is_empty()
                        && let Ok(mut writer) = writer.lock()
                    {
                        let _ = writer.write_all(&host_output);
                        let _ = writer.flush();
                    }
                    // If the loop has shut down, the proxy is closed: stop.
                    if proxy.send_event(UserEvent::Redraw).is_err() {
                        break;
                    }
                }
                Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = proxy.send_event(UserEvent::ShellExited);
                    break;
                }
            }
        }
    })
}

/// Entry point for the native app.
///
/// Opens a real OS window sized to the requested grid, brings up a `wgpu`
/// surface, spawns the default shell on a PTY, and renders the shell's live
/// output: a pump thread feeds PTY bytes into a shared [`Terminal`] and wakes
/// the event loop, which rebuilds the cell geometry and presents a frame. The
/// loop runs until the window is closed, the shell exits, or (when
/// `ODYTTY_NATIVE_AUTOCLOSE_MS` is set) the auto-close deadline elapses. On the
/// way out the child shell is killed and reaped and the pump thread is joined,
/// so no zombie process or detached thread is left behind.
///
/// The read+write loop is complete: PTY output flows shell → core → pixels, and
/// keyboard input flows window → [`crate::input`] encoder → PTY → shell.
pub fn run_native(options: NativeOptions) -> Result<(), NativeError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| NativeError::EventLoop(err.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // Select the presentation theme once (ODYTTY_THEME, default plain) and apply
    // its default cell colors process-wide before any rendering. This only
    // affects how Color::Default paints; the terminal core is unaware of it.
    let theme = Theme::from_env();
    text::set_default_colors(theme.foreground, theme.background);
    // Optional visual treatment (ODYTTY_VISUAL, default off). Presentation-only;
    // the terminal core is unaware of it and it is fully disableable.
    let visual = VisualEffect::from_env();

    // Shared terminal model, sized to the initial grid. The pump thread writes
    // to it; the UI thread snapshots from it.
    let terminal = Arc::new(Mutex::new(Terminal::new(
        options.initial_grid.columns,
        options.initial_grid.rows,
    )));

    // Spawn the shell PTY and start pumping its output into the shared terminal.
    let session = PtySession::spawn_default_shell(options.initial_grid)
        .map_err(|err| NativeError::Pty(err.to_string()))?;
    let reader = session
        .try_clone_reader()
        .map_err(|err| NativeError::Pty(err.to_string()))?;
    // One writer, shared: the pump thread sends host responses through it, and
    // the App sends encoded keystrokes through its clone.
    let writer: PtyWriter = Arc::new(Mutex::new(
        session
            .take_writer()
            .map_err(|err| NativeError::Pty(err.to_string()))?,
    ));

    let proxy = event_loop.create_proxy();
    let pump_thread = spawn_pty_pump(reader, writer.clone(), terminal.clone(), proxy);

    // Share the session: the App pushes window-size changes to it on resize,
    // and this function reaps the child on the way out.
    let session = Arc::new(Mutex::new(session));

    let mut app = App::new(
        options,
        theme,
        visual,
        terminal,
        writer,
        session.clone(),
        autoclose_from_env(),
    );
    let run_result = event_loop
        .run_app(&mut app)
        .map_err(|err| NativeError::EventLoop(err.to_string()));

    // Tear down deterministically: kill + reap the shell, which closes the PTY
    // master and unblocks the pump thread's `read`, then join the thread. The
    // App's session clone is dropped with `app` after this; reaping the child
    // is what EOFs the pump's reader, independent of master drop order.
    {
        let mut session = session.lock().expect("pty session");
        let _ = session.kill();
        let _ = session.wait();
    }
    let _ = pump_thread.join();

    run_result?;
    if let Some(err) = app.startup_error {
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;

    #[test]
    fn viewport_scroll_up_clamps_to_scrollback() {
        let mut vp = Viewport::default();
        assert!(vp.is_live());
        // Scroll up within bounds.
        assert!(vp.scroll_up(3, 10));
        assert_eq!(vp.offset(), 3);
        // Scroll up past the available history clamps to scrollback_len.
        assert!(vp.scroll_up(100, 10));
        assert_eq!(vp.offset(), 10);
        // Already at the top: no change.
        assert!(!vp.scroll_up(5, 10));
        assert_eq!(vp.offset(), 10);
    }

    #[test]
    fn viewport_scroll_up_with_no_scrollback_is_noop() {
        let mut vp = Viewport::default();
        assert!(!vp.scroll_up(5, 0));
        assert_eq!(vp.offset(), 0);
        assert!(vp.is_live());
    }

    #[test]
    fn viewport_scroll_down_saturates_at_live() {
        let mut vp = Viewport::default();
        vp.scroll_up(5, 10);
        assert!(vp.scroll_down(2));
        assert_eq!(vp.offset(), 3);
        // Scrolling down past live saturates at 0 and reports live.
        assert!(vp.scroll_down(100));
        assert_eq!(vp.offset(), 0);
        assert!(vp.is_live());
        // Already live: no change.
        assert!(!vp.scroll_down(1));
    }

    #[test]
    fn viewport_to_live_resets_offset() {
        let mut vp = Viewport::default();
        vp.scroll_up(4, 10);
        assert!(vp.reset_to_live());
        assert_eq!(vp.offset(), 0);
        // Returning to live again is a no-op.
        assert!(!vp.reset_to_live());
    }

    #[test]
    fn viewport_anchors_view_when_scrollback_grows() {
        let mut vp = Viewport::default();
        vp.scroll_up(5, 20);
        // Three new rows enter scrollback while scrolled back: offset advances
        // to keep the same absolute rows in view.
        vp.anchor_after_growth(3, 23);
        assert_eq!(vp.offset(), 8);
        // Anchoring clamps to the new scrollback length.
        vp.anchor_after_growth(1000, 30);
        assert_eq!(vp.offset(), 30);
    }

    #[test]
    fn viewport_anchor_is_noop_when_live() {
        let mut vp = Viewport::default();
        // At the live bottom, new output should appear immediately (no anchor).
        vp.anchor_after_growth(5, 10);
        assert_eq!(vp.offset(), 0);
        assert!(vp.is_live());
    }

    #[test]
    fn viewport_clamp_shrinks_offset_to_available_history() {
        let mut vp = Viewport::default();
        vp.scroll_up(15, 20);
        // History shrank (e.g. alternate-screen entry cleared scrollback).
        vp.clamp(4);
        assert_eq!(vp.offset(), 4);
        vp.clamp(0);
        assert_eq!(vp.offset(), 0);
        assert!(vp.is_live());
    }

    #[test]
    fn wheel_line_delta_maps_notches_to_rows() {
        // Positive y scrolls up into history; one notch == WHEEL_STEP_LINES.
        assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 16), 3);
        // Negative y scrolls toward live.
        assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, -1.0), 16), -3);
        // Multi-notch deltas scale.
        assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 2.0), 16), 6);
        // Zero is no scroll.
        assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 0.0), 16), 0);
    }

    #[test]
    fn wheel_pixel_delta_converts_by_cell_height() {
        // 32px / 16px cell == 2 rows up.
        let up = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 32.0));
        assert_eq!(wheel_lines(up, 16), 2);
        // Negative pixels scroll toward live.
        let down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -16.0));
        assert_eq!(wheel_lines(down, 16), -1);
        // A zero cell height must not divide-by-zero (clamped to 1).
        let safe = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 5.0));
        assert_eq!(wheel_lines(safe, 0), 5);
    }

    #[test]
    fn scroll_keys_require_shift_without_ctrl_or_alt() {
        let shift = Modifiers {
            shift: true,
            ctrl: false,
            alt: false,
        };
        let plain = Modifiers::default();
        let up = WinitKey::Named(NamedKey::PageUp);
        let down = WinitKey::Named(NamedKey::PageDown);
        // Shift+PageUp/PageDown drive the viewport.
        assert!(is_scroll_up_key(&up, shift));
        assert!(is_scroll_down_key(&down, shift));
        // Plain PageUp/PageDown do NOT (they reach the PTY).
        assert!(!is_scroll_up_key(&up, plain));
        assert!(!is_scroll_down_key(&down, plain));
        // Ctrl/Alt held disqualifies the scroll binding.
        assert!(!is_scroll_up_key(
            &up,
            Modifiers {
                shift: true,
                ctrl: true,
                alt: false,
            }
        ));
    }

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
    fn theme_clear_color_is_opaque_and_linearized() {
        // Every built-in theme yields an opaque clear color, and the conversion
        // matches the renderer's sRGB→linear transfer (same as cell colors).
        for theme in Theme::ALL {
            let color = theme_clear_color(&theme);
            assert_eq!(color.a, 1.0, "{} clear must be opaque", theme.name);
            assert_eq!(color.r, text::srgb_to_linear(theme.clear.0) as f64);
            assert_eq!(color.g, text::srgb_to_linear(theme.clear.1) as f64);
            assert_eq!(color.b, text::srgb_to_linear(theme.clear.2) as f64);
        }
    }

    #[test]
    fn effect_params_off_is_zero_strength_disable() {
        // Off → zero strength makes the shader scanline term vanish (the effect
        // is disabled and rendering is identical to the pre-effect path).
        let params = effect_params(VisualEffect::Off);
        assert_eq!(params[0], 0.0, "off must have zero strength");
        assert!(params[1] > 0.0, "period stays positive even when off");
    }

    #[test]
    fn effect_params_ambient_is_subtle_and_enabled() {
        let params = effect_params(VisualEffect::Ambient);
        assert!(
            params[0] > 0.0 && params[0] <= 0.15,
            "ambient strength subtle: {}",
            params[0]
        );
        assert!(params[1] > 0.0, "ambient period positive");
        // The packed strength matches the effect's own report (single source).
        assert_eq!(params[0], VisualEffect::Ambient.scanline_strength());
        assert_eq!(params[1], VisualEffect::Ambient.scanline_period_px());
    }

    #[test]
    fn viewport_uniform_is_sixteen_bytes() {
        // std140 slot: vec2 size + vec2 effect == 16 bytes, matching cell.wgsl.
        assert_eq!(std::mem::size_of::<ViewportUniform>(), 16);
    }

    #[test]
    fn clipboard_slot_retains_initialized_handle() {
        let mut slot = ClipboardSlot::default();
        let mut created = 0;

        *slot
            .get_or_try_init(|| {
                created += 1;
                Ok::<_, ()>(41)
            })
            .expect("first handle") += 1;
        let retained = *slot
            .get_or_try_init(|| {
                created += 1;
                Ok::<_, ()>(0)
            })
            .expect("retained handle");

        assert_eq!(created, 1);
        assert_eq!(retained, 42);
        assert!(slot.is_retaining_handle());
    }

    #[test]
    fn clipboard_slot_can_drop_failed_or_stale_handle() {
        let mut slot = ClipboardSlot::default();

        let _ = slot
            .get_or_try_init(|| Ok::<_, ()>("first"))
            .expect("first handle");
        assert!(slot.is_retaining_handle());

        slot.clear();
        assert!(!slot.is_retaining_handle());

        let retained = *slot
            .get_or_try_init(|| Ok::<_, ()>("replacement"))
            .expect("replacement handle");
        assert_eq!(retained, "replacement");
    }

    fn cell(width: u32, height: u32) -> CellSize {
        CellSize {
            width,
            height,
            baseline: 0,
        }
    }

    #[test]
    fn grid_dimensions_floor_divide_pixel_size_by_cell() {
        // 800/8 = 100 cols, 600/16 = 37 rows (592px of 600 used; remainder
        // floored away). Matches the whole cells the geometry can draw.
        let dims = grid_dimensions_for(800, 600, cell(8, 16));
        assert_eq!(dims, Dimensions::new(100, 37));
    }

    #[test]
    fn grid_dimensions_clamp_to_at_least_one() {
        // A window smaller than a single cell still yields a 1x1 grid rather
        // than a zero-dimension (panicking) grid.
        let dims = grid_dimensions_for(4, 4, cell(8, 16));
        assert_eq!(dims, Dimensions::new(1, 1));
    }

    #[test]
    fn grid_dimensions_survive_zero_extents() {
        // A minimized window reports 0x0; clamps to 1x1 without dividing by the
        // (clamped) cell extents incorrectly.
        let dims = grid_dimensions_for(0, 0, cell(8, 16));
        assert_eq!(dims, Dimensions::new(1, 1));
    }

    #[test]
    fn grid_dimensions_tolerate_degenerate_cell() {
        // Defensive: a zero-sized cell metric must not divide by zero.
        let dims = grid_dimensions_for(80, 40, cell(0, 0));
        assert_eq!(dims, Dimensions::new(80, 40));
    }

    /// Drive the idempotence seam directly: resizing to the same whole-cell
    /// grid is a no-op (returns `false`), a different grid applies (returns
    /// `true`) and updates both the tracked grid and the shared model. The PTY
    /// is a real one-shot session so `resize` exercises the actual ioctl path.
    #[test]
    fn resize_grid_is_idempotent_and_updates_model() {
        let dims = Dimensions::new(80, 24);
        let session = match PtySession::spawn_shell_command(dims, "sleep 1") {
            Ok(session) => session,
            Err(_) => {
                eprintln!("skipping: no PTY available");
                return;
            }
        };
        let writer: PtyWriter = match session.take_writer() {
            Ok(writer) => Arc::new(Mutex::new(writer)),
            Err(_) => {
                eprintln!("skipping: could not take PTY writer");
                return;
            }
        };
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(session));
        let mut app = App::new(
            NativeOptions::default(),
            Theme::default(),
            VisualEffect::default(),
            terminal.clone(),
            writer,
            pty.clone(),
            None,
        );

        // 8x16 cell, 800x600 surface -> 100x37 grid: first apply changes state.
        let metric = cell(8, 16);
        assert!(app.resize_grid(metric, 800, 600));
        assert_eq!(app.grid, Dimensions::new(100, 37));
        assert_eq!(
            terminal.lock().expect("terminal").snapshot().dimensions,
            Dimensions::new(100, 37)
        );

        // Same surface again: idempotent no-op.
        assert!(!app.resize_grid(metric, 800, 600));
        assert_eq!(app.grid, Dimensions::new(100, 37));

        // Sub-cell pixel change (still 100x37 whole cells): also a no-op.
        assert!(!app.resize_grid(metric, 807, 607));
        assert_eq!(app.grid, Dimensions::new(100, 37));

        // A genuinely different grid applies.
        assert!(app.resize_grid(metric, 640, 480));
        assert_eq!(app.grid, Dimensions::new(80, 30));

        // Reap the child so no zombie lingers.
        if let Ok(mut session) = pty.lock() {
            let _ = session.kill();
            let _ = session.wait();
        }
    }

    #[test]
    fn named_keys_map_to_neutral_model() {
        assert_eq!(map_named_key(NamedKey::Enter, false), Some(Key::Enter));
        assert_eq!(map_named_key(NamedKey::ArrowUp, false), Some(Key::Up));
        assert_eq!(
            map_named_key(NamedKey::Backspace, false),
            Some(Key::Backspace)
        );
        // Shift-Tab becomes BackTab; plain Tab stays Tab.
        assert_eq!(map_named_key(NamedKey::Tab, false), Some(Key::Tab));
        assert_eq!(map_named_key(NamedKey::Tab, true), Some(Key::BackTab));
        // Space maps to a char so Ctrl-Space can encode to NUL downstream.
        assert_eq!(map_named_key(NamedKey::Space, false), Some(Key::Char(' ')));
        // Unhandled named keys are dropped.
        assert_eq!(map_named_key(NamedKey::F1, false), None);
    }

    #[test]
    fn space_named_key_encodes_nul_under_ctrl() {
        // Full path: Space named key -> neutral Key -> shared encoder, with Ctrl.
        let key = map_named_key(NamedKey::Space, false).expect("space maps");
        assert_eq!(input::encode_key(key, Modifiers::CTRL), vec![0]);
    }

    #[test]
    fn paste_shortcut_requires_ctrl_shift_v() {
        assert!(is_paste_shortcut(
            &WinitKey::Character("v".into()),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            }
        ));
        assert!(is_paste_shortcut(
            &WinitKey::Character("V".into()),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            }
        ));
        assert!(!is_paste_shortcut(
            &WinitKey::Character("v".into()),
            Modifiers::CTRL
        ));
        assert!(!is_paste_shortcut(
            &WinitKey::Character("v".into()),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: true,
            }
        ));
        assert!(!is_paste_shortcut(
            &WinitKey::Named(NamedKey::Enter),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            }
        ));
    }

    #[test]
    fn copy_shortcut_requires_ctrl_shift_c() {
        assert!(is_copy_shortcut(
            &WinitKey::Character("c".into()),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            }
        ));
        assert!(is_copy_shortcut(
            &WinitKey::Character("C".into()),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            }
        ));
        assert!(!is_copy_shortcut(
            &WinitKey::Character("c".into()),
            Modifiers::CTRL
        ));
        assert!(!is_copy_shortcut(
            &WinitKey::Character("c".into()),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: true,
            }
        ));
        assert!(!is_copy_shortcut(
            &WinitKey::Character("v".into()),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            }
        ));
    }

    #[derive(Clone, Default)]
    struct RecordingWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        flushes: Arc<Mutex<usize>>,
    }

    type RecordingWriterParts = (PtyWriter, Arc<Mutex<Vec<u8>>>, Arc<Mutex<usize>>);

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().expect("bytes").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            *self.flushes.lock().expect("flushes") += 1;
            Ok(())
        }
    }

    fn recording_writer() -> RecordingWriterParts {
        let recorder = RecordingWriter::default();
        let bytes = recorder.bytes.clone();
        let flushes = recorder.flushes.clone();
        (Arc::new(Mutex::new(Box::new(recorder))), bytes, flushes)
    }

    #[test]
    fn write_paste_text_sends_plain_clipboard_text() {
        let terminal = Arc::new(Mutex::new(Terminal::new(10, 2)));
        let (writer, bytes, flushes) = recording_writer();

        write_paste_text(&terminal, &writer, "plain\npaste").expect("paste write");

        assert_eq!(&*bytes.lock().expect("bytes"), b"plain\npaste");
        assert_eq!(*flushes.lock().expect("flushes"), 1);
    }

    #[test]
    fn write_paste_text_uses_bracketed_paste_mode() {
        let terminal = Arc::new(Mutex::new(Terminal::new(10, 2)));
        terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
        let (writer, bytes, flushes) = recording_writer();

        write_paste_text(&terminal, &writer, "safe\x1b[201~tail").expect("paste write");

        assert_eq!(
            &*bytes.lock().expect("bytes"),
            b"\x1b[200~safetail\x1b[201~"
        );
        assert_eq!(*flushes.lock().expect("flushes"), 1);
    }

    /// End-to-end PTY → core check: spawn a one-shot command on a real PTY,
    /// pump its bytes into a `Terminal` exactly as the native pump thread does,
    /// and assert the rendered snapshot contains the command's output.
    ///
    /// `#[ignore]`d like the other live-PTY smoke test: it needs a real shell
    /// and a PTY, so it is opt-in (`cargo test -- --ignored`).
    #[test]
    #[ignore = "spawns a real shell on a PTY"]
    fn pty_output_pumps_into_terminal_snapshot() {
        use std::io::Read;

        let dims = Dimensions::new(40, 10);
        let session = PtySession::spawn_shell_command(dims, "printf 'HELLO_ODYTTY'")
            .expect("spawn one-shot pty command");
        let mut reader = session.try_clone_reader().expect("clone reader");
        let mut terminal = Terminal::new(dims.columns, dims.rows);

        // Pump to EOF, mirroring the pump thread's read/advance loop.
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => terminal.advance(&buffer[..len]),
                Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }

        assert!(
            terminal.screen().plain_text().contains("HELLO_ODYTTY"),
            "snapshot should contain the command output"
        );
    }
}
